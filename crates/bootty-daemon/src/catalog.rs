use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(windows)]
use std::os::windows::fs::MetadataExt;

use anyhow::{Context, Result, bail};
use bootty_mux::{
    backend::MuxBackend,
    capability::BindingOperationOutcome,
    command::MuxCommand,
    operation::MuxBackendCommandCompletion,
    rmux::RmuxBackend,
    snapshot::{MuxSnapshot, session_matches},
    tmux::TmuxBackend,
    zellij::ZellijBackend,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use toml_edit::DocumentMut;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaseFileIdentity {
    first: u64,
    second: u64,
}

#[cfg(unix)]
fn lease_metadata_identity(metadata: &fs::Metadata) -> LeaseFileIdentity {
    LeaseFileIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    }
}

#[cfg(windows)]
fn lease_metadata_identity(metadata: &fs::Metadata) -> LeaseFileIdentity {
    LeaseFileIdentity {
        first: u64::from(metadata.volume_serial_number().unwrap_or_default()),
        second: metadata.file_index().unwrap_or_default(),
    }
}

#[cfg(not(any(unix, windows)))]
fn lease_metadata_identity(metadata: &fs::Metadata) -> LeaseFileIdentity {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_nanos() as u64)
        .unwrap_or_default();
    LeaseFileIdentity {
        first: metadata.len(),
        second: modified,
    }
}

fn lease_path_identity(path: &Path) -> Option<LeaseFileIdentity> {
    fs::metadata(path)
        .ok()
        .map(|metadata| lease_metadata_identity(&metadata))
}

fn lease_path_matches(path: &Path, expected: LeaseFileIdentity) -> bool {
    lease_path_identity(path) == Some(expected)
}

pub const CATALOG_VERSION: u32 = 3;
const SESSION_RESERVATION_LEASE_SECONDS: i64 = 60;
const SESSION_RESERVATION_RENEWAL_SECONDS: u64 = 20;
static LEASE_TOMBSTONE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Rmux,
    Tmux,
    Zellij,
}

impl Backend {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "rmux" => Ok(Self::Rmux),
            "tmux" => Ok(Self::Tmux),
            "zellij" => Ok(Self::Zellij),
            _ => bail!("remote Spaces need tmux, zellij, or rmux"),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Rmux => "rmux",
            Self::Tmux => "tmux",
            Self::Zellij => "zellij",
        }
    }
    fn snapshot(self) -> Result<MuxSnapshot> {
        match self {
            Self::Rmux => RmuxBackend::new().snapshot(),
            Self::Tmux => TmuxBackend::new().snapshot(),
            Self::Zellij => ZellijBackend::new().snapshot(),
        }
    }

    fn execute(self, command: MuxCommand) -> Result<Option<MuxBackendCommandCompletion>> {
        match self {
            Self::Rmux => {
                let mut backend = RmuxBackend::new();
                backend.execute(command)?;
                Ok(backend.take_authoritative_completion())
            }
            Self::Tmux => {
                let mut backend = TmuxBackend::new();
                backend.execute(command)?;
                Ok(backend.take_authoritative_completion())
            }
            Self::Zellij => {
                ZellijBackend::new().execute(command)?;
                Ok(None)
            }
        }
    }

    /// Checks whether this backend can faithfully execute a recursive launch without taking a
    /// snapshot or creating a session.
    fn preflight_session_launch(self, command: &MuxCommand) -> Result<()> {
        let MuxCommand::CreateSession { plan } = command else {
            return Ok(());
        };
        let capability = match self {
            Self::Rmux => RmuxBackend::new().session_launch_capability(plan),
            Self::Tmux => TmuxBackend::new().session_launch_capability(plan),
            Self::Zellij => ZellijBackend::new().session_launch_capability(plan),
        };
        require_session_launch_capability(&capability)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SpaceSummary {
    pub catalog_version: u32,
    pub id: String,
    pub name: String,
    pub backend: Backend,
}

struct LegacyCatalog {
    path: PathBuf,
    inherited_backend: Option<Backend>,
    inherited_remote: bool,
}

#[derive(Default)]
struct LegacyConfig {
    backend: Option<Backend>,
    backend_set: bool,
    remote: bool,
}

pub struct Catalog {
    connection: Connection,
    path: PathBuf,
}

/// A durable claim made before a backend can create a session.
///
/// A token makes rollback specific to the invocation that acquired the reservation: a retry
/// cannot delete another in-flight creator's ownership claim.
#[derive(Debug)]
enum SessionReservation {
    Acquired { token: String },
    ExistingActive,
    ExistingReserved { token: String },
}

impl SessionReservation {
    fn new_token(&self) -> Option<&str> {
        match self {
            Self::Acquired { token } => Some(token),
            Self::ExistingActive | Self::ExistingReserved { .. } => None,
        }
    }

    fn token(&self) -> Option<&str> {
        match self {
            Self::Acquired { token } | Self::ExistingReserved { token } => Some(token),
            Self::ExistingActive => None,
        }
    }
}
struct SessionReservationLease {
    stop: mpsc::Sender<()>,
    healthy: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl SessionReservationLease {
    fn start(
        path: &Path,
        backend: Backend,
        space_id: &str,
        session_id: &str,
        token: &str,
    ) -> Result<Self> {
        let lease_path = reservation_lease_path(path, token);
        let owner_connection = Connection::open(&lease_path)
            .with_context(|| format!("open session reservation lease {}", lease_path.display()))?;
        let owner_identity = lease_path_identity(&lease_path)
            .context("session reservation lease identity disappeared after open")?;
        owner_connection.busy_timeout(Duration::ZERO)?;
        owner_connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
             DELETE FROM owner_lock;
             INSERT INTO owner_lock (id) VALUES (1);
             BEGIN IMMEDIATE;",
        )?;

        let (stop, receiver) = mpsc::channel();
        let healthy = Arc::new(AtomicBool::new(true));
        let worker_healthy = Arc::clone(&healthy);
        let catalog_path = path.to_owned();
        let backend = backend.name().to_owned();
        let space_id = space_id.to_owned();
        let session_id = session_id.to_owned();
        let token = token.to_owned();
        let worker_lease_path = lease_path.clone();
        let worker_identity = owner_identity;
        let worker = std::thread::spawn(move || {
            let owner_connection = owner_connection;
            let connection = if let Ok(connection) = Connection::open(catalog_path) {
                let _ = connection.busy_timeout(Duration::from_secs(5));
                connection
            } else {
                // The owner lock remains held while the heartbeat connection is unavailable.
                // Do not treat a transient catalog-open failure as proof of lease loss.
                let _ = receiver.recv();
                let tombstone = rename_lease_sidecar(&worker_lease_path, worker_identity);
                drop(owner_connection);
                remove_lease_tombstone(tombstone);
                return;
            };
            let interval = Duration::from_secs(SESSION_RESERVATION_RENEWAL_SECONDS);
            loop {
                match receiver.recv_timeout(interval) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        let tombstone = rename_lease_sidecar(&worker_lease_path, worker_identity);
                        drop(owner_connection);
                        remove_lease_tombstone(tombstone);
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        let Ok(expires_at) = unix_timestamp().and_then(|now| {
                            now.checked_add(SESSION_RESERVATION_LEASE_SECONDS)
                                .context("session reservation lease overflow")
                        }) else {
                            // A local clock/overflow failure is not evidence that ownership
                            // changed; leave the live owner lock as the liveness proof.
                            continue;
                        };
                        if let Ok(updated) = connection.execute(
                            "UPDATE remote_space_sessions
                             SET reservation_expires_at = ?4
                             WHERE backend = ?1 AND session_name = ?2
                               AND state = 'reserved' AND reservation_token = ?3",
                            params![backend, session_id, token, expires_at],
                        ) {
                            if updated > 0 {
                                worker_healthy.store(true, Ordering::Release);
                            } else {
                                let ownership = connection
                                    .query_row(
                                        "SELECT space_id, state, reservation_token
                                     FROM remote_space_sessions
                                     WHERE backend = ?1 AND session_name = ?2",
                                        params![backend, session_id],
                                        |row| {
                                            Ok((
                                                row.get::<_, String>(0)?,
                                                row.get::<_, String>(1)?,
                                                row.get::<_, Option<String>>(2)?,
                                            ))
                                        },
                                    )
                                    .optional();
                                match ownership {
                                    Ok(Some((owner, state, row_token)))
                                        if owner == space_id
                                            && ((state == "active" && row_token.is_none())
                                                || (state == "reserved"
                                                    && row_token.as_deref()
                                                        == Some(token.as_str()))) =>
                                    {
                                        // A concurrent authoritative snapshot may have already
                                        // promoted this same owner's reservation.
                                        worker_healthy.store(true, Ordering::Release);
                                    }
                                    Ok(Some(_) | None) => {
                                        // The exact owner/token no longer exists.
                                        worker_healthy.store(false, Ordering::Release);
                                    }
                                    Err(_) => {
                                        // A busy/locked inspection is transient; retry later.
                                    }
                                }
                            }
                        } else {
                            // Busy/locked and other I/O errors are transient. The next
                            // interval retries while the owner lock continues protecting
                            // the reservation from authoritative reclaim.
                        }
                    }
                }
            }
        });
        Ok(Self {
            stop,
            healthy,
            worker: Some(worker),
        })
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
}

impl Drop for SessionReservationLease {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct SessionOperationLease {
    connection: Option<Connection>,
    lease_path: PathBuf,
    identity: LeaseFileIdentity,
}

impl SessionOperationLease {
    fn start(path: &Path, backend: Backend, session_id: &str) -> Result<Self> {
        let lease_path = session_operation_lease_path(path, backend, session_id);
        let connection = Connection::open(&lease_path)
            .with_context(|| format!("open session operation lease {}", lease_path.display()))?;
        let identity = lease_path_identity(&lease_path)
            .context("session operation lease identity disappeared after open")?;
        connection.busy_timeout(Duration::ZERO)?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
             DELETE FROM owner_lock;
             INSERT INTO owner_lock (id) VALUES (1);
             BEGIN IMMEDIATE;",
        )?;
        Ok(Self {
            connection: Some(connection),
            lease_path,
            identity,
        })
    }
    fn start_many(path: &Path, backend: Backend, session_names: &[&str]) -> Result<Vec<Self>> {
        let mut ordered = session_names.to_vec();
        ordered.sort_unstable();
        ordered.dedup();
        let mut leases = Vec::with_capacity(ordered.len());
        for session_name in ordered {
            leases.push(Self::start(path, backend, session_name)?);
        }
        Ok(leases)
    }
}

impl Drop for SessionOperationLease {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let tombstone = rename_lease_sidecar(&self.lease_path, self.identity);
            drop(connection);
            remove_lease_tombstone(tombstone);
        }
    }
}
impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        let legacy = default_legacy_catalog();
        Self::open_with_legacy(path, legacy.as_ref())
    }

    fn open_with_legacy(path: &Path, legacy: Option<&LegacyCatalog>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create daemon state directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open daemon catalog {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS daemon_metadata (
                 key TEXT PRIMARY KEY
             );
             CREATE TABLE IF NOT EXISTS catalog_revisions (
                 id INTEGER PRIMARY KEY CHECK (id = 1),
                 revision INTEGER NOT NULL
             );
             INSERT INTO catalog_revisions (id, revision)
             SELECT 1, 0
             WHERE NOT EXISTS (SELECT 1 FROM catalog_revisions WHERE id = 1);
             CREATE TABLE IF NOT EXISTS remote_spaces (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL UNIQUE,
                 backend TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS remote_space_sessions (
                 space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 backend TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL DEFAULT 'active'
                     CHECK (state IN ('active', 'reserved')),
                 reservation_token TEXT,
                 ownership_revision INTEGER NOT NULL DEFAULT 0,
                 reservation_expires_at INTEGER,
                 CHECK (
                     (state = 'active' AND reservation_token IS NULL
                         AND reservation_expires_at IS NULL)
                     OR (state = 'reserved' AND reservation_token IS NOT NULL
                         AND reservation_expires_at IS NOT NULL)
                 ),
                 UNIQUE (backend, session_name),
                 PRIMARY KEY (space_id, session_name)
             );
             CREATE TABLE IF NOT EXISTS remote_space_session_intents (
                 token TEXT PRIMARY KEY,
                 space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 backend TEXT NOT NULL,
                 kind TEXT NOT NULL CHECK (kind IN ('rename', 'ditch')),
                 old_name TEXT NOT NULL,
                 new_name TEXT,
                 ownership_revision INTEGER NOT NULL,
                 intent_revision INTEGER NOT NULL,
                 CHECK (
                     (kind = 'rename' AND new_name IS NOT NULL)
                     OR (kind = 'ditch' AND new_name IS NULL)
                 ),
                 UNIQUE (backend, old_name)
             );
             CREATE TABLE IF NOT EXISTS remote_space_session_conflicts (
                 id INTEGER PRIMARY KEY,
                 source_rowid INTEGER NOT NULL,
                 space_id TEXT NOT NULL,
                 backend TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 reservation_token TEXT,
                 reservation_expires_at INTEGER,
                 reason TEXT NOT NULL
             );",
        )?;
        let mut catalog = Self {
            connection,
            path: path.to_owned(),
        };
        catalog.migrate_session_ownership_schema()?;
        catalog.migrate_legacy(legacy)?;
        Ok(catalog)
    }

    /// Upgrades the original per-Space membership table into the durable ownership ledger.
    ///
    /// Ownership is unique within a backend namespace, not across unrelated backends. The
    /// rebuild is necessary because `SQLite` cannot add or remove a table constraint in place.
    /// Same-backend legacy conflicts retain the catalog-order winner in the live ledger and keep
    /// every losing claim in a durable conflict archive instead of silently dropping it.
    fn migrate_session_ownership_schema(&mut self) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_backend = table_has_column(&transaction, "remote_space_sessions", "backend")?;
        let has_state = table_has_column(&transaction, "remote_space_sessions", "state")?;
        let has_reservation_token =
            table_has_column(&transaction, "remote_space_sessions", "reservation_token")?;
        let has_reservation_expires_at = table_has_column(
            &transaction,
            "remote_space_sessions",
            "reservation_expires_at",
        )?;
        let has_ownership_revision =
            table_has_column(&transaction, "remote_space_sessions", "ownership_revision")?;
        if has_backend
            && has_state
            && has_reservation_token
            && has_reservation_expires_at
            && has_ownership_revision
            && table_has_unique_columns(
                &transaction,
                "remote_space_sessions",
                &["backend", "session_name"],
            )?
            && !table_has_unique_columns(&transaction, "remote_space_sessions", &["session_name"])?
        {
            transaction.execute(
                "UPDATE catalog_revisions
                 SET revision = MAX(
                     revision,
                     COALESCE((SELECT MAX(ownership_revision) FROM remote_space_sessions), 0)
                 )
                 WHERE id = 1",
                [],
            )?;
            transaction.commit()?;
            return Ok(());
        }

        let source_state = if has_state {
            "sessions.state"
        } else {
            "'active'"
        };
        let source_token = if has_reservation_token {
            "sessions.reservation_token"
        } else {
            "NULL"
        };
        let source_expires_at = if has_reservation_expires_at {
            "sessions.reservation_expires_at"
        } else {
            "NULL"
        };
        let source_ownership_revision = if has_ownership_revision {
            "COALESCE(sessions.ownership_revision, 0)"
        } else {
            "0"
        };
        transaction.execute_batch(
            "ALTER TABLE remote_space_sessions RENAME TO remote_space_sessions_legacy;",
        )?;
        let legacy_rows = {
            let mut statement = transaction.prepare(&format!(
                "SELECT sessions.space_id,
                        spaces.backend,
                        sessions.session_name,
                        sessions.position,
                        CASE
                            WHEN {source_state} = 'reserved' AND {source_token} IS NOT NULL
                                THEN 'reserved'
                            ELSE 'active'
                        END,
                        CASE
                            WHEN {source_state} = 'reserved' AND {source_token} IS NOT NULL
                                THEN {source_token}
                            ELSE NULL
                        END,
                        CASE
                            WHEN {source_state} = 'reserved' AND {source_token} IS NOT NULL
                                THEN COALESCE(
                                    {source_expires_at},
                                    CAST(strftime('%s', 'now') AS INTEGER)
                                        + {SESSION_RESERVATION_LEASE_SECONDS}
                                )
                            ELSE NULL
                        END,
                        {source_ownership_revision},
                        sessions.rowid
                 FROM remote_space_sessions_legacy AS sessions
                 JOIN remote_spaces AS spaces ON spaces.id = sessions.space_id
                 ORDER BY spaces.position, spaces.id, sessions.position, sessions.rowid"
            ))?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<i64>>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.execute_batch(
            "CREATE TABLE remote_space_sessions (
                 space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 backend TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL DEFAULT 'active'
                     CHECK (state IN ('active', 'reserved')),
                 reservation_token TEXT,
                 reservation_expires_at INTEGER,
                 ownership_revision INTEGER NOT NULL DEFAULT 0,
                 CHECK (
                     (state = 'active' AND reservation_token IS NULL
                         AND reservation_expires_at IS NULL)
                     OR (state = 'reserved' AND reservation_token IS NOT NULL
                         AND reservation_expires_at IS NOT NULL)
                 ),
                 UNIQUE (backend, session_name),
                 PRIMARY KEY (space_id, session_name)
             );
             CREATE TABLE IF NOT EXISTS remote_space_session_conflicts (
                 id INTEGER PRIMARY KEY,
                 source_rowid INTEGER NOT NULL,
                 space_id TEXT NOT NULL,
                 backend TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 reservation_token TEXT,
                 reservation_expires_at INTEGER,
                 reason TEXT NOT NULL
             );",
        )?;
        let mut owned = HashSet::new();
        for (
            space_id,
            backend,
            session_name,
            position,
            state,
            reservation_token,
            reservation_expires_at,
            ownership_revision,
            source_rowid,
        ) in legacy_rows
        {
            if owned.insert((backend.clone(), session_name.clone())) {
                transaction.execute(
                    "INSERT INTO remote_space_sessions
                         (space_id, backend, session_name, position, state,
                          reservation_token, reservation_expires_at, ownership_revision)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        space_id,
                        backend,
                        session_name,
                        position,
                        state,
                        reservation_token,
                        reservation_expires_at,
                        ownership_revision
                    ],
                )?;
            } else {
                transaction.execute(
                    "INSERT INTO remote_space_session_conflicts
                         (source_rowid, space_id, backend, session_name, position, state,
                          reservation_token, reservation_expires_at, reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        source_rowid,
                        space_id,
                        backend,
                        session_name,
                        position,
                        state,
                        reservation_token,
                        reservation_expires_at,
                        "duplicate backend/session ownership during catalog migration"
                    ],
                )?;
            }
        }
        transaction.execute(
            "UPDATE catalog_revisions
             SET revision = MAX(
                 revision,
                 COALESCE((SELECT MAX(ownership_revision) FROM remote_space_sessions), 0)
             )
             WHERE id = 1",
            [],
        )?;
        transaction.execute_batch("DROP TABLE remote_space_sessions_legacy;")?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_legacy(&mut self, legacy: Option<&LegacyCatalog>) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let migrated = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM daemon_metadata WHERE key = 'legacy_catalog_migrated')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if migrated {
            transaction.commit()?;
            return Ok(());
        }
        let empty = transaction.query_row("SELECT COUNT(*) = 0 FROM remote_spaces", [], |row| {
            row.get::<_, bool>(0)
        })?;
        if empty && let Some(legacy) = legacy.filter(|legacy| legacy.path.is_file()) {
            let legacy_connection = Connection::open(&legacy.path)
                .with_context(|| format!("open legacy remote catalog {}", legacy.path.display()))?;
            legacy_connection.busy_timeout(Duration::from_secs(5))?;
            if table_exists(&legacy_connection, "workspace_spaces")?
                && table_exists(&legacy_connection, "workspace_bindings")?
                && table_has_column(&legacy_connection, "workspace_spaces", "remote_id")?
                && table_has_column(&legacy_connection, "workspace_bindings", "remote")?
                && table_has_column(&legacy_connection, "workspace_bindings", "backend")?
            {
                let mut statement = legacy_connection.prepare(
                    "SELECT spaces.remote_id, spaces.name, bindings.backend,
                            spaces.position, bindings.id
                     FROM workspace_spaces AS spaces
                     JOIN workspace_bindings AS bindings ON bindings.id = (
                         SELECT candidate.id
                         FROM workspace_bindings AS candidate
                         WHERE candidate.space_id = spaces.id
                         ORDER BY candidate.id
                         LIMIT 1
                     )
                     WHERE spaces.remote_id IS NOT NULL
                       AND spaces.remote_id != ''
                       AND (
                           bindings.remote = '{\"source\":\"local\"}'
                           OR (bindings.remote IS NULL AND ?1 = 0)
                       )
                     ORDER BY spaces.position",
                )?;
                let rows = statement
                    .query_map([i64::from(legacy.inherited_remote)], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                        ))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                drop(statement);
                let mut owned_sessions = HashSet::new();
                for (id, name, stored_backend, position, binding_id) in rows {
                    let backend = if stored_backend == "inherit" {
                        legacy.inherited_backend
                    } else {
                        Backend::parse(&stored_backend).ok()
                    };
                    let Some(backend) = backend else {
                        continue;
                    };
                    transaction.execute(
                        "INSERT INTO remote_spaces (id, name, backend, position)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![id, name, backend.name(), position],
                    )?;
                    if table_exists(&legacy_connection, "workspace_sessions")? {
                        for (session_position, session_name) in
                            legacy_session_names(&legacy_connection, binding_id)?
                                .into_iter()
                                .enumerate()
                        {
                            let position = i64::try_from(session_position)?;
                            if owned_sessions
                                .insert((backend.name().to_owned(), session_name.clone()))
                            {
                                transaction.execute(
                                    "INSERT INTO remote_space_sessions
                                     (space_id, backend, session_name, position)
                                     VALUES (?1, ?2, ?3, ?4)",
                                    params![id, backend.name(), session_name, position],
                                )?;
                            } else {
                                transaction.execute(
                                    "INSERT INTO remote_space_session_conflicts
                                     (source_rowid, space_id, backend, session_name, position,
                                      state, reason)
                                     VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)",
                                    params![
                                        position,
                                        id,
                                        backend.name(),
                                        session_name,
                                        position,
                                        "duplicate backend/session ownership during legacy app migration"
                                    ],
                                )?;
                            }
                        }
                    }
                }
            }
        }
        transaction.execute(
            "INSERT OR IGNORE INTO daemon_metadata (key) VALUES ('legacy_catalog_migrated')",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<SpaceSummary>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, name, backend FROM remote_spaces ORDER BY position, id")?;
        statement
            .query_map([], |row| {
                let backend = row.get::<_, String>(2)?;
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, backend))
            })?
            .map(|row| {
                let (id, name, backend) = row?;
                Ok(SpaceSummary {
                    catalog_version: CATALOG_VERSION,
                    id,
                    name,
                    backend: Backend::parse(&backend)?,
                })
            })
            .collect()
    }

    pub fn create(&mut self, requested_name: &str, backend: Backend) -> Result<SpaceSummary> {
        let requested_name = requested_name.trim();
        if requested_name.is_empty() {
            bail!("remote Space name cannot be empty")
        }
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut names = transaction.prepare("SELECT name FROM remote_spaces")?;
        let existing = names
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        drop(names);
        let name = unique_name(requested_name, &existing);
        let id = transaction.query_row(
            "SELECT lower(hex(randomblob(4))) || '-' ||
                    lower(hex(randomblob(2))) || '-' ||
                    lower(hex(randomblob(2))) || '-' ||
                    lower(hex(randomblob(2))) || '-' ||
                    lower(hex(randomblob(6)))",
            [],
            |row| row.get::<_, String>(0),
        )?;
        let position = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM remote_spaces",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        transaction.execute(
            "INSERT INTO remote_spaces (id, name, backend, position) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, backend.name(), position],
        )?;
        transaction.commit()?;
        Ok(SpaceSummary {
            catalog_version: CATALOG_VERSION,
            id,
            name,
            backend,
        })
    }

    fn begin_session_intent(
        &mut self,
        backend: Backend,
        space_id: &str,
        kind: &str,
        old_name: &str,
        new_name: Option<&str>,
        ownership_revision: i64,
    ) -> Result<String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let intent_revision = Self::next_catalog_revision(&transaction)?;
        let token = transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
            row.get::<_, String>(0)
        })?;
        transaction.execute(
            "INSERT INTO remote_space_session_intents
             (token, space_id, backend, kind, old_name, new_name,
              ownership_revision, intent_revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                token,
                space_id,
                backend.name(),
                kind,
                old_name,
                new_name,
                ownership_revision,
                intent_revision
            ],
        )?;
        transaction.commit()?;
        Ok(token)
    }

    fn clear_session_intent(transaction: &Transaction<'_>, token: &str) -> Result<()> {
        let deleted = transaction.execute(
            "DELETE FROM remote_space_session_intents WHERE token = ?1",
            [token],
        )?;
        if deleted != 1 {
            bail!("session mutation intent disappeared before finalization");
        }
        Ok(())
    }
    fn clear_recovered_session_intent(
        transaction: &Transaction<'_>,
        catalog_path: &Path,
        backend: Backend,
        token: &str,
        old_name: &str,
        new_name: Option<&str>,
    ) -> Result<()> {
        Self::clear_session_intent(transaction, token)?;
        let mut names = vec![old_name];
        if let Some(new_name) = new_name {
            names.push(new_name);
        }
        names.sort_unstable();
        names.dedup();
        for name in names {
            remove_stale_session_operation_lease(catalog_path, backend, name)?;
        }
        Ok(())
    }

    fn catalog_revision(&self) -> Result<i64> {
        self.connection
            .query_row(
                "SELECT revision FROM catalog_revisions WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .context("catalog revision row is missing")
    }

    fn next_catalog_revision(transaction: &Transaction<'_>) -> Result<i64> {
        transaction.execute(
            "UPDATE catalog_revisions SET revision = revision + 1 WHERE id = 1",
            [],
        )?;
        transaction
            .query_row(
                "SELECT revision FROM catalog_revisions WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .context("catalog revision row is missing")
    }

    pub fn snapshot(&mut self, space_id: &str, expected: Backend) -> Result<MuxSnapshot> {
        let backend = self.space_backend(space_id, expected)?;
        let observed_revision = self.catalog_revision()?;
        // Backend I/O stays outside SQLite write transactions. Reconciliation serializes the
        // authoritative observation after it returns, without blocking lease renewals while a
        // remote snapshot is slow.
        let mut snapshot = backend.snapshot()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::reconcile_backend_sessions(
            &transaction,
            &self.path,
            backend,
            &snapshot,
            observed_revision,
        )?;
        let owned = Self::session_names_in(&transaction, space_id)?;
        transaction.commit()?;
        snapshot
            .sessions
            .retain(|session| owned.contains(&session.name));
        snapshot.active_session_id = snapshot
            .active_session_id
            .filter(|id| snapshot.sessions.iter().any(|session| session.id == *id));
        Ok(snapshot)
    }

    pub fn execute(
        &mut self,
        space_id: &str,
        expected: Backend,
        command: MuxCommand,
    ) -> Result<Option<MuxBackendCommandCompletion>> {
        let backend_kind = self.space_backend(space_id, expected)?;
        self.execute_with_backend(
            backend_kind,
            space_id,
            command,
            |command| {
                validate_remote_session_launch(command)?;
                backend_kind.preflight_session_launch(command)
            },
            || backend_kind.snapshot(),
            |command| backend_kind.execute(command),
        )
    }

    fn execute_with_backend<Preflight, Snapshot, Execute>(
        &mut self,
        backend: Backend,
        space_id: &str,
        command: MuxCommand,
        preflight: Preflight,
        mut snapshot: Snapshot,
        execute: Execute,
    ) -> Result<Option<MuxBackendCommandCompletion>>
    where
        Preflight: FnOnce(&MuxCommand) -> Result<()>,
        Snapshot: FnMut() -> Result<MuxSnapshot>,
        Execute: FnOnce(MuxCommand) -> Result<Option<MuxBackendCommandCompletion>>,
    {
        // The launch plan and backend capability are untrusted remote input. Do this before
        // taking a snapshot, which can start a backend process, and before every mutation.
        preflight(&command)?;
        // Backend I/O stays outside SQLite write transactions so a slow remote cannot block
        // reservation lease renewal or a competing catalog operation.
        let observed_revision = self.catalog_revision()?;
        let initial_snapshot = snapshot()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created_session = created_session_id(&command).map(str::to_owned);
        let (owned_name, reservation) = if let Some(session_id) = created_session.as_deref() {
            (
                None,
                Some(Self::prepare_create_session_in_transaction(
                    &transaction,
                    &self.path,
                    space_id,
                    backend,
                    session_id,
                    &initial_snapshot,
                    observed_revision,
                )?),
            )
        } else {
            Self::reconcile_backend_sessions(
                &transaction,
                &self.path,
                backend,
                &initial_snapshot,
                observed_revision,
            )?;
            let owned = Self::session_names_in(&transaction, space_id)?;
            (
                resolve_owned_session_name(&initial_snapshot, &owned, &command, space_id)?,
                None,
            )
        };
        let owned_revision = owned_name
            .as_deref()
            .map(|name| {
                transaction.query_row(
                    "SELECT ownership_revision
                     FROM remote_space_sessions
                     WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3",
                    params![space_id, backend.name(), name],
                    |row| row.get::<_, i64>(0),
                )
            })
            .transpose()?;
        transaction.commit()?;
        match &reservation {
            Some(SessionReservation::ExistingActive) => {
                // A completed same-Space request is idempotent. Do not ask the backend to create
                // a duplicate session merely to repeat the request.
                return Ok(None);
            }
            Some(SessionReservation::ExistingReserved { .. }) => {
                bail!("session creation is already in progress for this remote Space");
            }
            Some(SessionReservation::Acquired { .. }) | None => {}
        }
        let mut reservation_lease =
            match reservation.as_ref().and_then(SessionReservation::new_token) {
                Some(token) => {
                    let session_id = created_session
                        .as_deref()
                        .context("created session reservation has no session id")?;
                    match SessionReservationLease::start(
                        &self.path, backend, space_id, session_id, token,
                    ) {
                        Ok(lease) => Some(lease),
                        Err(error) => {
                            let rollback_result = self
                                .rollback_session_reservation(backend, space_id, session_id, token);
                            if let Err(rollback_error) = rollback_result {
                                return Err(error.context(format!(
                                "reservation lease startup failed and exact reservation rollback \
                                 failed; ownership remains reserved: {rollback_error:#}"
                            )));
                            }
                            match reservation_owner_is_live(&self.path, token) {
                                Ok(false) => {}
                                Ok(true) => {
                                    return Err(error.context(
                                    "reservation lease startup failed; owner lease remains live \
                                     after rollback",
                                ));
                                }
                                Err(cleanup_error) => {
                                    return Err(error.context(format!(
                                        "reservation lease startup rollback succeeded but lease \
                                     cleanup failed: {cleanup_error:#}"
                                    )));
                                }
                            }
                            return Err(error);
                        }
                    }
                }
                None => None,
            };
        if let (Some(session_id), Some(token)) = (
            created_session.as_deref(),
            reservation.as_ref().and_then(SessionReservation::new_token),
        ) && let Err(error) = self.renew_session_reservation(backend, session_id, token)
        {
            let rollback_result =
                self.rollback_session_reservation(backend, space_id, session_id, token);
            drop(reservation_lease.take());
            let cleanup_result = reservation_owner_is_live(&self.path, token);
            if let Err(rollback_error) = rollback_result {
                return Err(error.context(format!(
                    "reservation renewal failed and exact reservation rollback failed; \
                     ownership remains reserved: {rollback_error:#}"
                )));
            }
            match cleanup_result {
                Ok(false) => {}
                Ok(true) => {
                    return Err(error.context(
                        "reservation renewal failed and owner lease cleanup remains \
                         indeterminate",
                    ));
                }
                Err(cleanup_error) => {
                    return Err(error.context(format!(
                        "reservation renewal rollback succeeded but lease cleanup failed: \
                         {cleanup_error:#}"
                    )));
                }
            }
            return Err(error);
        }

        let mut operation_names = Vec::new();
        if let Some(old_name) = owned_name.as_deref() {
            operation_names.push(old_name);
            if let MuxCommand::RenameSession { name, .. } = &command {
                operation_names.push(name.as_str());
            }
        }
        let _operation_leases = if operation_names.is_empty() {
            None
        } else {
            Some(SessionOperationLease::start_many(
                &self.path,
                backend,
                &operation_names,
            )?)
        };
        if let Some(session_name) = owned_name.as_deref() {
            let expected_revision =
                owned_revision.context("owned session revision is unavailable")?;
            self.verify_session_ownership(backend, space_id, session_name, expected_revision)?;
        }
        if let (Some(old_name), MuxCommand::RenameSession { name, .. }) =
            (owned_name.as_deref(), &command)
        {
            self.verify_session_rename_target(backend, old_name, name)?;
        }

        let operation_intent = match (&command, owned_name.as_deref(), owned_revision) {
            (MuxCommand::RenameSession { name, .. }, Some(old_name), Some(revision)) => {
                Some(self.begin_session_intent(
                    backend,
                    space_id,
                    "rename",
                    old_name,
                    Some(name),
                    revision,
                )?)
            }
            (MuxCommand::DitchSession { .. }, Some(old_name), Some(revision)) => Some(
                self.begin_session_intent(backend, space_id, "ditch", old_name, None, revision)?,
            ),
            _ => None,
        };

        let completion = match execute(command.clone()) {
            Ok(completion) => completion,
            Err(error) => {
                let Some(session_id) = created_session.as_deref() else {
                    return Err(error);
                };
                let Some(reservation) = reservation.as_ref() else {
                    return Err(error);
                };
                if reservation_lease
                    .as_ref()
                    .is_some_and(|lease| !lease.is_healthy())
                {
                    return Err(error.context(
                        "backend session create failed after its catalog reservation lease was \
                         lost; ownership remains reserved",
                    ));
                }
                let Some(token) = reservation.new_token() else {
                    return Err(error);
                };
                let post_snapshot = match snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(snapshot_error) => {
                        return Err(error.context(format!(
                            "backend session create failed and authoritative post-failure \
                             snapshot failed; ownership remains reserved: {snapshot_error:#}"
                        )));
                    }
                };
                if post_snapshot
                    .sessions
                    .iter()
                    .any(|session| session_matches(session, session_id))
                {
                    if let Err(finalize_error) = self.finalize_session_reservation(
                        backend,
                        space_id,
                        session_id,
                        reservation,
                    ) {
                        return Err(error.context(format!(
                            "backend session create failed after an authoritative snapshot \
                             observed the session; catalog finalization failed and ownership \
                             remains reserved: {finalize_error:#}"
                        )));
                    }
                    return Err(error.context(
                        "backend session create failed after an authoritative snapshot observed \
                         the session; ownership was retained for reconciliation",
                    ));
                }
                if let Err(rollback_error) =
                    self.rollback_session_reservation(backend, space_id, session_id, token)
                {
                    return Err(error.context(format!(
                        "backend session create failed and catalog reservation rollback failed; \
                         ownership remains reserved: {rollback_error:#}"
                    )));
                }
                return Err(error);
            }
        };
        if reservation_lease
            .as_ref()
            .is_some_and(|lease| !lease.is_healthy())
        {
            bail!("backend session reservation lease was lost; ownership remains reserved");
        }

        if let (Some(session_id), Some(reservation)) =
            (created_session.as_deref(), reservation.as_ref())
        {
            self.finalize_session_reservation(backend, space_id, session_id, reservation)
                .with_context(|| {
                    format!(
                        "backend created remote session {session_id}, but catalog finalization \
                         failed; ownership remains active or reserved and retry will reconcile it"
                    )
                })?;
        } else {
            match command {
                MuxCommand::RenameSession { name, .. } => {
                    if let Some(old_name) = owned_name {
                        let expected_revision =
                            owned_revision.context("owned session revision is unavailable")?;
                        let intent_token = operation_intent
                            .as_deref()
                            .context("rename operation intent is unavailable")?;
                        self.rename_session(
                            backend,
                            space_id,
                            &old_name,
                            &name,
                            expected_revision,
                            intent_token,
                        )?;
                    }
                }
                MuxCommand::DitchSession { .. } => {
                    if let Some(name) = owned_name {
                        let expected_revision =
                            owned_revision.context("owned session revision is unavailable")?;
                        let intent_token = operation_intent
                            .as_deref()
                            .context("ditch operation intent is unavailable")?;
                        self.remove_session(
                            backend,
                            space_id,
                            &name,
                            expected_revision,
                            intent_token,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(completion)
    }

    #[cfg(test)]
    fn prepare_create_session(
        &mut self,
        space_id: &str,
        backend: Backend,
        session_id: &str,
        snapshot: &MuxSnapshot,
    ) -> Result<SessionReservation> {
        let observed_revision = self.catalog_revision()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = Self::prepare_create_session_in_transaction(
            &transaction,
            &self.path,
            space_id,
            backend,
            session_id,
            snapshot,
            observed_revision,
        )?;
        transaction.commit()?;
        Ok(reservation)
    }
    fn prepare_create_session_in_transaction(
        transaction: &Transaction<'_>,
        catalog_path: &Path,
        space_id: &str,
        backend: Backend,
        session_id: &str,
        snapshot: &MuxSnapshot,
        observed_revision: i64,
    ) -> Result<SessionReservation> {
        Self::reconcile_backend_sessions(
            transaction,
            catalog_path,
            backend,
            snapshot,
            observed_revision,
        )?;
        let owned = Self::session_names_in(transaction, space_id)?;
        if snapshot
            .sessions
            .iter()
            .any(|session| session_matches(session, session_id) && !owned.contains(&session.name))
        {
            bail!("session already belongs to another remote Space")
        }
        if session_operation_owner_is_live(catalog_path, backend, session_id)? {
            bail!("session operation is already in progress for this backend session");
        }
        Self::reserve_session_ownership(transaction, space_id, backend, session_id)
    }

    fn space_backend(&self, space_id: &str, expected: Backend) -> Result<Backend> {
        let stored = self
            .connection
            .query_row(
                "SELECT backend FROM remote_spaces WHERE id = ?1",
                [space_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .with_context(|| format!("remote Space {space_id} is unavailable"))?;
        let stored = Backend::parse(&stored)?;
        if stored != expected {
            bail!(
                "Remote Space now uses {} instead of {}. Edit this Space and select it again.",
                stored.name(),
                expected.name()
            )
        }
        Ok(stored)
    }

    #[cfg(test)]
    fn session_names(&self, space_id: &str) -> Result<HashSet<String>> {
        let mut statement = self.connection.prepare(
            "SELECT session_name FROM remote_space_sessions WHERE space_id = ?1 ORDER BY position",
        )?;
        Ok(statement
            .query_map([space_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn session_names_in(transaction: &Transaction<'_>, space_id: &str) -> Result<HashSet<String>> {
        let mut statement = transaction.prepare(
            "SELECT session_name FROM remote_space_sessions WHERE space_id = ?1 ORDER BY position",
        )?;
        Ok(statement
            .query_map([space_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn recover_session_intents(
        transaction: &Transaction<'_>,
        catalog_path: &Path,
        backend: Backend,
        snapshot: &MuxSnapshot,
        observed_revision: i64,
    ) -> Result<HashSet<String>> {
        let alive = snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<HashSet<_>>();
        let mut statement = transaction.prepare(
            "SELECT token, space_id, kind, old_name, new_name,
                    ownership_revision, intent_revision
             FROM remote_space_session_intents
             WHERE backend = ?1
             ORDER BY token",
        )?;
        let intents = statement
            .query_map([backend.name()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut protected = HashSet::new();
        for (token, space_id, kind, old_name, new_name, ownership_revision, intent_revision) in
            intents
        {
            let operation_owner_live = if intent_revision > observed_revision {
                false
            } else {
                session_operation_owner_is_live(catalog_path, backend, &old_name)?
                    || new_name
                        .as_deref()
                        .map(|name| session_operation_owner_is_live(catalog_path, backend, name))
                        .transpose()?
                        .unwrap_or(false)
            };
            if intent_revision > observed_revision || operation_owner_live {
                protected.insert(old_name);
                if let Some(new_name) = new_name {
                    protected.insert(new_name);
                }
                continue;
            }
            let old_alive = alive.contains(old_name.as_str());
            match (kind.as_str(), new_name.as_deref(), old_alive) {
                ("ditch", None, false) => {
                    transaction.execute(
                        "DELETE FROM remote_space_sessions
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'active' AND ownership_revision = ?4",
                        params![space_id, backend.name(), old_name, ownership_revision],
                    )?;
                    Self::clear_recovered_session_intent(
                        transaction,
                        catalog_path,
                        backend,
                        &token,
                        &old_name,
                        None,
                    )?;
                }
                ("ditch", None, true) => {
                    Self::clear_recovered_session_intent(
                        transaction,
                        catalog_path,
                        backend,
                        &token,
                        &old_name,
                        None,
                    )?;
                }
                ("rename", Some(new_name), true) if !alive.contains(new_name) => {
                    Self::clear_recovered_session_intent(
                        transaction,
                        catalog_path,
                        backend,
                        &token,
                        &old_name,
                        Some(new_name),
                    )?;
                }
                ("rename", Some(new_name), false) if !alive.contains(new_name) => {
                    let (source_rowid, position) = transaction
                        .query_row(
                            "SELECT rowid, position FROM remote_space_sessions
                             WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                               AND state = 'active' AND ownership_revision = ?4",
                            params![space_id, backend.name(), old_name, ownership_revision],
                            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                        )
                        .optional()?
                        .unwrap_or((ownership_revision, 0));
                    transaction.execute(
                        "DELETE FROM remote_space_sessions
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'active' AND ownership_revision = ?4",
                        params![space_id, backend.name(), old_name, ownership_revision],
                    )?;
                    transaction.execute(
                        "INSERT INTO remote_space_session_conflicts
                         (source_rowid, space_id, backend, session_name, position,
                          state, reason)
                         VALUES (?1, ?2, ?3, ?4, ?5, 'active', ?6)",
                        params![
                            source_rowid,
                            space_id,
                            backend.name(),
                            old_name,
                            position,
                            "authoritative_absent mutation intent recovery"
                        ],
                    )?;
                    Self::clear_recovered_session_intent(
                        transaction,
                        catalog_path,
                        backend,
                        &token,
                        &old_name,
                        Some(new_name),
                    )?;
                }
                ("rename", Some(new_name), false) if alive.contains(new_name) => {
                    let target_exists = transaction
                        .query_row(
                            "SELECT 1 FROM remote_space_sessions
                             WHERE backend = ?1 AND session_name = ?2",
                            params![backend.name(), new_name],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?
                        .is_some();
                    if target_exists {
                        protected.insert(old_name);
                        protected.insert(new_name.to_owned());
                        continue;
                    }
                    let revision = Self::next_catalog_revision(transaction)?;
                    let updated = transaction.execute(
                        "UPDATE remote_space_sessions
                         SET session_name = ?4, ownership_revision = ?5
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'active' AND ownership_revision = ?6",
                        params![
                            space_id,
                            backend.name(),
                            old_name,
                            new_name,
                            revision,
                            ownership_revision
                        ],
                    )?;
                    if updated == 1 {
                        Self::clear_recovered_session_intent(
                            transaction,
                            catalog_path,
                            backend,
                            &token,
                            &old_name,
                            Some(new_name),
                        )?;
                    } else {
                        protected.insert(old_name);
                        protected.insert(new_name.to_owned());
                    }
                }
                _ => {
                    protected.insert(old_name);
                    if let Some(new_name) = new_name {
                        protected.insert(new_name);
                    }
                }
            }
        }
        Ok(protected)
    }

    /// Reconciles every Space that addresses the same backend. A session name is owned within
    /// that backend namespace, and only an authoritative snapshot from the matching backend may
    /// expire an active claim. A committed reservation survives a negative snapshot while its
    /// owner lease is live; after the owner dies, its expired reservation is reclaimable.
    fn reconcile_backend_sessions(
        transaction: &Transaction<'_>,
        catalog_path: &Path,
        backend: Backend,
        snapshot: &MuxSnapshot,
        observed_revision: i64,
    ) -> Result<()> {
        let protected_names = Self::recover_session_intents(
            transaction,
            catalog_path,
            backend,
            snapshot,
            observed_revision,
        )?;
        let alive = snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<HashSet<_>>();
        let now = unix_timestamp()?;
        let sessions = {
            let mut statement = transaction.prepare(
                "SELECT sessions.space_id, sessions.backend, sessions.session_name,
                        sessions.state, sessions.reservation_token,
                        sessions.reservation_expires_at, sessions.ownership_revision
                 FROM remote_space_sessions AS sessions
                 JOIN remote_spaces AS spaces ON spaces.id = sessions.space_id
                 WHERE sessions.backend = ?1 AND spaces.backend = ?1
                 ORDER BY spaces.position, spaces.id, sessions.position",
            )?;
            statement
                .query_map([backend.name()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (
            space_id,
            row_backend,
            name,
            state,
            reservation_token,
            reservation_expires_at,
            row_revision,
        ) in sessions
        {
            let eligible =
                row_revision <= observed_revision && !protected_names.contains(name.as_str());
            let expired_absent_reservation = eligible
                && state == "reserved"
                && !alive.contains(name.as_str())
                && reservation_expires_at.is_some_and(|expires_at| expires_at <= now)
                && reservation_token
                    .as_deref()
                    .map(|token| reservation_owner_is_live(catalog_path, token).map(|live| !live))
                    .transpose()?
                    .unwrap_or(false);
            match state.as_str() {
                "active"
                    if eligible
                        && !alive.contains(name.as_str())
                        && !session_operation_owner_is_live(catalog_path, backend, &name)? =>
                {
                    transaction.execute(
                        "DELETE FROM remote_space_sessions
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'active' AND ownership_revision = ?4",
                        params![space_id, row_backend, name, row_revision],
                    )?;
                    let _revision = Self::next_catalog_revision(transaction)?;
                }
                "reserved" if eligible && alive.contains(name.as_str()) => {
                    let revision = Self::next_catalog_revision(transaction)?;
                    transaction.execute(
                        "UPDATE remote_space_sessions
                         SET state = 'active', reservation_token = NULL,
                             reservation_expires_at = NULL, ownership_revision = ?4
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'reserved' AND ownership_revision = ?5",
                        params![space_id, row_backend, name, revision, row_revision],
                    )?;
                }
                "reserved" if expired_absent_reservation => {
                    let _revision = Self::next_catalog_revision(transaction)?;
                    transaction.execute(
                        "DELETE FROM remote_space_sessions
                         WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
                           AND state = 'reserved'
                           AND reservation_token = ?4
                           AND reservation_expires_at <= ?5
                           AND ownership_revision = ?6",
                        params![
                            space_id,
                            row_backend,
                            name,
                            reservation_token,
                            now,
                            row_revision
                        ],
                    )?;
                }
                "active" | "reserved" => {}
                _ => bail!("invalid remote session ownership state {state:?}"),
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn sync_sessions(
        &mut self,
        space_id: &str,
        backend: Backend,
        snapshot: &MuxSnapshot,
    ) -> Result<HashSet<String>> {
        let observed_revision = self.catalog_revision()?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::reconcile_backend_sessions(
            &transaction,
            &self.path,
            backend,
            snapshot,
            observed_revision,
        )?;
        let owned = Self::session_names_in(&transaction, space_id)?;
        transaction.commit()?;
        Ok(owned)
    }
    fn reserve_session_ownership(
        transaction: &Transaction<'_>,
        space_id: &str,
        backend: Backend,
        session_id: &str,
    ) -> Result<SessionReservation> {
        let position = transaction.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM remote_space_sessions WHERE space_id = ?1",
            [space_id],
            |row| row.get::<_, i64>(0),
        )?;
        let token = transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| {
            row.get::<_, String>(0)
        })?;
        let reservation_expires_at = unix_timestamp()?
            .checked_add(SESSION_RESERVATION_LEASE_SECONDS)
            .context("session reservation lease overflow")?;
        let ownership_revision = Self::next_catalog_revision(transaction)?;
        let inserted = transaction.execute(
            "INSERT INTO remote_space_sessions
             (space_id, backend, session_name, position, state, reservation_token,
              reservation_expires_at, ownership_revision)
             VALUES (?1, ?2, ?3, ?4, 'reserved', ?5, ?6, ?7)
             ON CONFLICT(backend, session_name) DO NOTHING",
            params![
                space_id,
                backend.name(),
                session_id,
                position,
                &token,
                reservation_expires_at,
                ownership_revision,
            ],
        )?;
        if inserted == 1 {
            return Ok(SessionReservation::Acquired { token });
        }

        let (owner, state, token) = transaction
            .query_row(
                "SELECT space_id, state, reservation_token
                 FROM remote_space_sessions
                 WHERE backend = ?1 AND session_name = ?2",
                params![backend.name(), session_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .context("session ownership changed while reserving")?;
        if owner != space_id {
            bail!("session already belongs to another remote Space")
        }
        match (state.as_str(), token) {
            ("active", None) => Ok(SessionReservation::ExistingActive),
            ("reserved", Some(token)) => Ok(SessionReservation::ExistingReserved { token }),
            _ => bail!("invalid remote session ownership record for {session_id}"),
        }
    }

    fn renew_session_reservation(
        &self,
        backend: Backend,
        session_id: &str,
        token: &str,
    ) -> Result<()> {
        let expires_at = unix_timestamp()?
            .checked_add(SESSION_RESERVATION_LEASE_SECONDS)
            .context("session reservation lease overflow")?;
        let updated = self.connection.execute(
            "UPDATE remote_space_sessions
             SET reservation_expires_at = ?4
             WHERE backend = ?1 AND session_name = ?2
               AND state = 'reserved' AND reservation_token = ?3",
            params![backend.name(), session_id, token, expires_at],
        )?;
        if updated != 1 {
            bail!("session reservation was reclaimed before backend execution");
        }
        Ok(())
    }

    fn finalize_session_reservation(
        &mut self,
        backend: Backend,
        space_id: &str,
        session_id: &str,
        reservation: &SessionReservation,
    ) -> Result<()> {
        let Some(token) = reservation.token() else {
            return Ok(());
        };
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = Self::next_catalog_revision(&transaction)?;
        let updated = transaction.execute(
            "UPDATE remote_space_sessions
             SET state = 'active', reservation_token = NULL, reservation_expires_at = NULL,
                 ownership_revision = ?5
             WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
               AND state = 'reserved' AND reservation_token = ?4",
            params![space_id, backend.name(), session_id, token, revision],
        )?;
        if updated == 0 {
            let existing = transaction
                .query_row(
                    "SELECT space_id, state, reservation_token
                     FROM remote_space_sessions
                     WHERE backend = ?1 AND session_name = ?2",
                    params![backend.name(), session_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()?;
            match existing {
                Some((owner, state, None)) if owner == space_id && state == "active" => {
                    transaction.commit()?;
                    return Ok(());
                }
                Some((owner, _, _)) if owner != space_id => {
                    bail!("session ownership changed to another remote Space")
                }
                Some(_) => bail!("session ownership reservation changed before finalization"),
                None => bail!("session ownership reservation disappeared before finalization"),
            }
        }
        transaction.commit()?;
        Ok(())
    }

    fn verify_session_ownership(
        &self,
        backend: Backend,
        space_id: &str,
        session_name: &str,
        expected_revision: i64,
    ) -> Result<()> {
        let current = self
            .connection
            .query_row(
                "SELECT state, ownership_revision
                 FROM remote_space_sessions
                 WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3",
                params![space_id, backend.name(), session_name],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        if !matches!(
            current,
            Some((state, revision)) if state == "active" && revision == expected_revision
        ) {
            bail!("session ownership changed before backend mutation");
        }
        let intent_pending = self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM remote_space_session_intents
                 WHERE backend = ?1 AND old_name = ?2
             )",
            params![backend.name(), session_name],
            |row| row.get::<_, bool>(0),
        )?;
        if intent_pending {
            bail!("session mutation intent is pending");
        }
        Ok(())
    }

    fn verify_session_rename_target(
        &self,
        backend: Backend,
        old_name: &str,
        new_name: &str,
    ) -> Result<()> {
        if old_name == new_name {
            return Ok(());
        }
        let target_exists = self.connection.query_row(
            "SELECT EXISTS(
                     SELECT 1 FROM remote_space_sessions
                     WHERE backend = ?1 AND session_name = ?2
                 )",
            params![backend.name(), new_name],
            |row| row.get::<_, bool>(0),
        )?;
        if target_exists {
            bail!("session rename target {new_name} is already owned or reserved");
        }
        let intent_pending = self.connection.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM remote_space_session_intents
                 WHERE backend = ?1 AND (old_name = ?2 OR new_name = ?2)
             )",
            params![backend.name(), new_name],
            |row| row.get::<_, bool>(0),
        )?;
        if intent_pending {
            bail!("session rename target {new_name} has a pending mutation");
        }
        Ok(())
    }

    fn rollback_session_reservation(
        &mut self,
        backend: Backend,
        space_id: &str,
        session_id: &str,
        token: &str,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _revision = Self::next_catalog_revision(&transaction)?;
        let deleted = transaction.execute(
            "DELETE FROM remote_space_sessions
             WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
               AND state = 'reserved' AND reservation_token = ?4",
            params![space_id, backend.name(), session_id, token],
        )?;
        if deleted != 1 {
            bail!("session ownership changed before reservation rollback");
        }
        transaction.commit()?;
        Ok(())
    }

    fn rename_session(
        &mut self,
        backend: Backend,
        space_id: &str,
        old_name: &str,
        name: &str,
        expected_revision: i64,
        intent_token: &str,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision = Self::next_catalog_revision(&transaction)?;
        let updated = transaction.execute(
            "UPDATE remote_space_sessions
             SET session_name = ?4, ownership_revision = ?5
             WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
               AND state = 'active' AND ownership_revision = ?6",
            params![
                space_id,
                backend.name(),
                old_name,
                name,
                revision,
                expected_revision
            ],
        )?;
        if updated != 1 {
            bail!("session ownership changed before rename finalization");
        }
        Self::clear_session_intent(&transaction, intent_token)?;
        transaction.commit()?;
        Ok(())
    }

    fn remove_session(
        &mut self,
        backend: Backend,
        space_id: &str,
        name: &str,
        expected_revision: i64,
        intent_token: &str,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _revision = Self::next_catalog_revision(&transaction)?;
        let deleted = transaction.execute(
            "DELETE FROM remote_space_sessions
             WHERE space_id = ?1 AND backend = ?2 AND session_name = ?3
               AND state = 'active' AND ownership_revision = ?4",
            params![space_id, backend.name(), name, expected_revision],
        )?;
        if deleted != 1 {
            bail!("session ownership changed before ditch finalization");
        }
        Self::clear_session_intent(&transaction, intent_token)?;
        transaction.commit()?;
        Ok(())
    }
}

fn legacy_session_names(connection: &Connection, binding_id: i64) -> Result<Vec<String>> {
    if table_exists(connection, "workspace_session_groups")?
        && table_has_column(connection, "workspace_sessions", "group_id")?
    {
        let mut statement = connection.prepare(
            "SELECT sessions.name
             FROM workspace_sessions AS sessions
             JOIN workspace_session_groups AS groups ON groups.id = sessions.group_id
             WHERE sessions.binding_id = ?1 AND groups.binding_id = ?1
             ORDER BY groups.position, sessions.position",
        )?;
        return Ok(statement
            .query_map([binding_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?);
    }
    let mut statement = connection.prepare(
        "SELECT name FROM workspace_sessions
         WHERE binding_id = ?1 ORDER BY position",
    )?;
    Ok(statement
        .query_map([binding_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn load_legacy_config(path: &Path) -> Result<LegacyConfig> {
    if !path.exists() {
        return Ok(LegacyConfig::default());
    }
    load_legacy_config_file(path, &mut Vec::new(), &mut HashSet::new())
}

fn load_legacy_config_file(
    path: &Path,
    stack: &mut Vec<PathBuf>,
    loaded: &mut HashSet<PathBuf>,
) -> Result<LegacyConfig> {
    let id = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if stack.contains(&id) {
        bail!("config include cycle detected at {}", path.display())
    }
    if loaded.contains(&id) {
        return Ok(LegacyConfig::default());
    }
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("read legacy config {}", path.display()))?;
    let document = source
        .parse::<DocumentMut>()
        .with_context(|| format!("parse legacy config {}", path.display()))?;
    let backend = document
        .get("multiplexer")
        .and_then(|multiplexer| multiplexer.get("backend"))
        .and_then(|backend| backend.as_str());
    let mut config = LegacyConfig {
        backend: match backend {
            None | Some("native") => None,
            Some(backend) => Some(Backend::parse(backend)?),
        },
        backend_set: backend.is_some(),
        remote: document
            .get("multiplexer")
            .and_then(|multiplexer| multiplexer.get("remote"))
            .is_some(),
    };
    let includes = document
        .get("include")
        .map(|item| {
            item.as_array()
                .context("legacy config include must be an array")?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .context("legacy config include must contain strings")
                })
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    stack.push(id.clone());
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for include in includes {
        let (optional, include) = include
            .strip_prefix('?')
            .map_or((false, include.as_str()), |include| (true, include));
        let include = Path::new(include);
        let include = if include.is_absolute() {
            include.to_path_buf()
        } else {
            base.join(include)
        };
        if optional && !include.exists() {
            continue;
        }
        let child = load_legacy_config_file(&include, stack, loaded)?;
        if child.backend_set {
            config.backend = child.backend;
            config.backend_set = true;
        }
        config.remote |= child.remote;
    }
    stack.pop();
    loaded.insert(id);
    Ok(config)
}

fn default_legacy_catalog() -> Option<LegacyCatalog> {
    let config_root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    let config_path = config_root.join("bootty/config.toml");
    let config = load_legacy_config(&config_path).unwrap_or(LegacyConfig {
        backend: None,
        backend_set: false,
        remote: true,
    });
    Some(LegacyCatalog {
        path: config_path.with_file_name("session-order.sqlite3"),
        inherited_backend: config.backend,
        inherited_remote: config.remote,
    })
}

fn table_exists(connection: &Connection, name: &str) -> rusqlite::Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    Ok(statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column))
}

fn table_has_unique_columns(
    connection: &Connection,
    table: &str,
    expected: &[&str],
) -> rusqlite::Result<bool> {
    let indexes = {
        let mut statement = connection.prepare(&format!("PRAGMA index_list({table})"))?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (index, is_unique) in indexes {
        if is_unique == 0 {
            continue;
        }
        let escaped_index = index.replace('\'', "''");
        let columns = {
            let mut statement =
                connection.prepare(&format!("PRAGMA index_info('{escaped_index}')"))?;
            statement
                .query_map([], |row| row.get::<_, String>(2))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if columns.len() == expected.len()
            && columns
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.as_str() == *expected)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reservation_lease_path(catalog_path: &Path, token: &str) -> PathBuf {
    let catalog_name = catalog_path.file_name().map_or_else(
        || "catalog".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    catalog_path.with_file_name(format!("{catalog_name}.lease-{token}.sqlite3"))
}
fn lease_tombstone_path(lease_path: &Path) -> PathBuf {
    let file_name = lease_path.file_name().map_or_else(
        || "lease.sqlite3".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = LEASE_TOMBSTONE_COUNTER.fetch_add(1, Ordering::Relaxed);
    lease_path.with_file_name(format!(
        "{file_name}.retired-{}-{timestamp}-{counter}",
        std::process::id()
    ))
}

#[derive(Debug)]
struct RetiredLease {
    path: PathBuf,
    identity: LeaseFileIdentity,
}

fn rename_lease_sidecar(
    lease_path: &Path,
    expected_identity: LeaseFileIdentity,
) -> Option<RetiredLease> {
    if !lease_path_matches(lease_path, expected_identity) {
        return None;
    }
    let tombstone = lease_tombstone_path(lease_path);
    fs::rename(lease_path, &tombstone)
        .ok()
        .map(|()| RetiredLease {
            path: tombstone,
            identity: expected_identity,
        })
}

fn remove_lease_tombstone(tombstone: Option<RetiredLease>) {
    if let Some(tombstone) = tombstone
        && lease_path_matches(&tombstone.path, tombstone.identity)
    {
        let _ = fs::remove_file(tombstone.path);
    }
}

/// Checks retired lease sidecars after their canonical path has been renamed while the owner
/// still holds the `SQLite` lock. A tombstone with a free lock is stale and can be removed; a
/// locked tombstone proves the owner is still live.
fn retired_lease_owner_is_live(lease_path: &Path) -> Result<bool> {
    let Some(parent) = lease_path.parent() else {
        return Ok(true);
    };
    let Some(file_name) = lease_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(true);
    };
    let prefix = format!("{file_name}.retired-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "inspect retired session lease sidecars in {}",
                    parent.display()
                )
            });
        }
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(&prefix) {
            continue;
        }
        let tombstone = entry.path();
        let Some(identity) = lease_path_identity(&tombstone) else {
            return Ok(true);
        };
        let Ok(connection) = Connection::open(&tombstone) else {
            return Ok(true);
        };
        connection.busy_timeout(Duration::ZERO)?;
        match connection.execute_batch("BEGIN IMMEDIATE;") {
            Ok(()) => {
                if !lease_path_matches(&tombstone, identity) {
                    let _ = connection.execute_batch("ROLLBACK;");
                    drop(connection);
                    return Ok(true);
                }
                let rollback = connection.execute_batch("ROLLBACK;");
                drop(connection);
                rollback?;
                if !lease_path_matches(&tombstone, identity) {
                    return Ok(true);
                }
                match fs::remove_file(&tombstone) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("remove stale retired session lease {}", tombstone.display())
                        });
                    }
                }
            }
            Err(_) => return Ok(true),
        }
    }
    Ok(false)
}

/// Removes an unlocked operation sidecar after durable intent recovery.
fn remove_stale_session_operation_lease(
    catalog_path: &Path,
    backend: Backend,
    session_id: &str,
) -> Result<()> {
    let lease_path = session_operation_lease_path(catalog_path, backend, session_id);
    if !lease_path.exists() {
        if retired_lease_owner_is_live(&lease_path)? {
            bail!("session operation lease became live during recovery");
        }
        return Ok(());
    }
    let identity = lease_path_identity(&lease_path)
        .context("session operation lease identity disappeared before recovery")?;
    let connection = Connection::open(&lease_path)
        .with_context(|| format!("open session operation lease {}", lease_path.display()))?;
    connection.busy_timeout(Duration::ZERO)?;
    let tombstone = match connection.execute_batch("BEGIN IMMEDIATE;") {
        Ok(()) => {
            if !lease_path_matches(&lease_path, identity) {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                bail!("session operation lease was replaced during recovery");
            }
            let Some(tombstone) = rename_lease_sidecar(&lease_path, identity) else {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                bail!("session operation lease was replaced during recovery");
            };
            Some(tombstone)
        }
        Err(error) => {
            return Err(error).context("session operation lease became live during recovery");
        }
    };
    let rollback = connection.execute_batch("ROLLBACK;");
    drop(connection);
    remove_lease_tombstone(tombstone);
    rollback?;
    Ok(())
}

/// Returns true when the owner process still holds its per-reservation `SQLite` lock. An inability
/// to prove that the lock is free fails closed so a live creator is never stolen.
fn reservation_owner_is_live(catalog_path: &Path, token: &str) -> Result<bool> {
    let lease_path = reservation_lease_path(catalog_path, token);
    if !lease_path.exists() {
        return retired_lease_owner_is_live(&lease_path);
    }
    let Some(identity) = lease_path_identity(&lease_path) else {
        return Ok(true);
    };
    let Ok(connection) = Connection::open(&lease_path) else {
        return Ok(true);
    };
    connection.busy_timeout(Duration::ZERO)?;
    match connection.execute_batch("BEGIN IMMEDIATE;") {
        Ok(()) => {
            if !lease_path_matches(&lease_path, identity) {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                return Ok(true);
            }
            let Some(tombstone) = rename_lease_sidecar(&lease_path, identity) else {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                return Ok(true);
            };
            let rollback = connection.execute_batch("ROLLBACK;");
            drop(connection);
            if !lease_path_matches(&tombstone.path, tombstone.identity) {
                return Ok(true);
            }
            remove_lease_tombstone(Some(tombstone));
            rollback?;
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}
fn session_operation_lease_path(
    catalog_path: &Path,
    backend: Backend,
    session_id: &str,
) -> PathBuf {
    let catalog_name = catalog_path.file_name().map_or_else(
        || "catalog".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let mut session_hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in session_id.as_bytes() {
        session_hash ^= u64::from(*byte);
        session_hash = session_hash.wrapping_mul(0x0100_0000_01b3);
    }
    let encoded_session = format!("{session_hash:016x}");
    catalog_path.with_file_name(format!(
        "{catalog_name}.operation-{}-{encoded_session}.sqlite3",
        backend.name()
    ))
}

/// Returns true when a rename/ditch operation still owns its per-session lock. Failure to prove
/// that the lock is free fails closed so an in-flight backend mutation cannot be reclaimed.
fn session_operation_owner_is_live(
    catalog_path: &Path,
    backend: Backend,
    session_id: &str,
) -> Result<bool> {
    let lease_path = session_operation_lease_path(catalog_path, backend, session_id);
    if !lease_path.exists() {
        return retired_lease_owner_is_live(&lease_path);
    }
    let Some(identity) = lease_path_identity(&lease_path) else {
        return Ok(true);
    };
    let Ok(connection) = Connection::open(&lease_path) else {
        return Ok(true);
    };
    connection.busy_timeout(Duration::ZERO)?;
    match connection.execute_batch("BEGIN IMMEDIATE;") {
        Ok(()) => {
            if !lease_path_matches(&lease_path, identity) {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                return Ok(true);
            }
            if session_operation_intent_exists(catalog_path, backend, session_id)? {
                connection.execute_batch("ROLLBACK;")?;
                drop(connection);
                return Ok(false);
            }
            let Some(tombstone) = rename_lease_sidecar(&lease_path, identity) else {
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                return Ok(true);
            };
            let rollback = connection.execute_batch("ROLLBACK;");
            drop(connection);
            if !lease_path_matches(&tombstone.path, tombstone.identity) {
                return Ok(true);
            }
            remove_lease_tombstone(Some(tombstone));
            rollback?;
            Ok(false)
        }
        Err(_) => Ok(true),
    }
}

fn session_operation_intent_exists(
    catalog_path: &Path,
    backend: Backend,
    session_id: &str,
) -> Result<bool> {
    let connection = Connection::open(catalog_path)?;
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM remote_space_session_intents
                 WHERE backend = ?1 AND (old_name = ?2 OR new_name = ?2)
             )",
            params![backend.name(), session_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn unix_timestamp() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs()
        .try_into()
        .context("system clock exceeds the supported timestamp range")
}

fn unique_name(requested: &str, existing: &HashSet<String>) -> String {
    if !existing.contains(requested) {
        return requested.to_owned();
    }
    (2..=u32::MAX)
        .map(|suffix| format!("{requested} {suffix}"))
        .find(|candidate| !existing.contains(candidate))
        .expect("all numbered remote Space names are occupied")
}

/// Reject untrusted recursive launch data before a snapshot can construct or touch a backend.
fn validate_remote_session_launch(command: &MuxCommand) -> Result<()> {
    if let MuxCommand::CreateSession { plan } = command
        && let Err(error) = plan.validate()
    {
        bail!("invalid recursive session launch: {error}");
    }
    Ok(())
}

fn require_session_launch_capability(capability: &BindingOperationOutcome<()>) -> Result<()> {
    match capability {
        BindingOperationOutcome::Supported(()) => Ok(()),
        BindingOperationOutcome::Unsupported => {
            bail!("recursive session launch is unsupported by this remote Space backend")
        }
        BindingOperationOutcome::Unavailable => {
            bail!("remote Space backend is unavailable for recursive session launch")
        }
        BindingOperationOutcome::Denied => {
            bail!("remote Space backend denied recursive session launch")
        }
        BindingOperationOutcome::Stale => {
            bail!("remote Space backend capability is stale for recursive session launch")
        }
    }
}

fn resolve_owned_session_name(
    snapshot: &MuxSnapshot,
    owned_names: &HashSet<String>,
    command: &MuxCommand,
    space_id: &str,
) -> Result<Option<String>> {
    let Some(session_id) = command_session_id(command) else {
        return Ok(None);
    };
    let name = snapshot
        .sessions
        .iter()
        .find(|session| session_matches(session, session_id))
        .map(|session| session.name.clone())
        .context("session is unavailable")?;
    if !owned_names.contains(&name) {
        bail!("session does not belong to remote Space {space_id}")
    }
    Ok(Some(name))
}

fn command_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateSession { .. }
        | MuxCommand::CreateProjectSession { .. }
        | MuxCommand::CreateWorktreeSession { .. } => None,
        MuxCommand::ActivateWindow { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::RenameWindow { session_id, .. }
        | MuxCommand::ActivateNextWindow { session_id }
        | MuxCommand::ActivatePreviousWindow { session_id }
        | MuxCommand::ActivateLastWindow { session_id }
        | MuxCommand::ActivateWindowIndex { session_id, .. }
        | MuxCommand::MoveWindow { session_id, .. }
        | MuxCommand::MoveWindowPreservingSelection { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::SelectPane { session_id, .. }
        | MuxCommand::SelectNextPane { session_id, .. }
        | MuxCommand::SelectPreviousPane { session_id, .. }
        | MuxCommand::SelectLastPane { session_id, .. }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::ResizePane { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => Some(session_id),
    }
}

fn created_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateSession { plan } => Some(&plan.session_id),
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::BTreeMap,
    };

    use super::*;
    use bootty_mux::{
        command::{MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxWindowLaunchPlan},
        snapshot::{MuxPaneAnchor, MuxSession},
    };

    fn catalog() -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog =
            Catalog::open_with_legacy(&dir.path().join("daemon.sqlite"), None).expect("catalog");
        (dir, catalog)
    }

    fn empty_snapshot() -> MuxSnapshot {
        MuxSnapshot {
            sessions: Vec::new(),
            active_session_id: None,
        }
    }

    fn snapshot_with_session(name: &str) -> MuxSnapshot {
        MuxSnapshot {
            sessions: vec![MuxSession {
                id: name.to_owned(),
                name: name.to_owned(),
                active: true,
                anchor: MuxPaneAnchor {
                    session_id: name.to_owned(),
                    ..Default::default()
                },
                active_window_id: None,
                windows: Vec::new(),
            }],
            active_session_id: Some(name.to_owned()),
        }
    }

    fn launch_plan(session_id: &str) -> MuxSessionLaunchPlan {
        MuxSessionLaunchPlan {
            session_id: session_id.to_owned(),
            focus: false,
            default_cwd: "/repo".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: "/repo".to_owned(),
                    command: None,
                    argv: None,
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        }
    }

    fn two_catalogs() -> (
        tempfile::TempDir,
        Catalog,
        Catalog,
        SpaceSummary,
        SpaceSummary,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("daemon.sqlite");
        let mut first = Catalog::open_with_legacy(&path, None).expect("first catalog");
        let first_space = first.create("First", Backend::Tmux).expect("first space");
        let second_space = first.create("Second", Backend::Tmux).expect("second space");
        let second = Catalog::open_with_legacy(&path, None).expect("second catalog");
        (dir, first, second, first_space, second_space)
    }

    #[test]
    fn spaces_have_stable_ids_and_unique_names() {
        let (_dir, mut catalog) = catalog();
        let first = catalog.create("Lab", Backend::Tmux).expect("first");
        let second = catalog.create("Lab", Backend::Rmux).expect("second");

        assert_ne!(first.id, second.id);
        assert_eq!(first.name, "Lab");
        assert_eq!(second.name, "Lab 2");
        assert_eq!(catalog.list().expect("list"), vec![first, second]);
    }

    #[test]
    fn backend_ids_resolve_to_owned_session_names() {
        let snapshot = MuxSnapshot {
            sessions: vec![MuxSession {
                id: "$7".to_owned(),
                name: "owned".to_owned(),
                active: true,
                anchor: MuxPaneAnchor {
                    session_id: "$7".to_owned(),
                    ..Default::default()
                },
                active_window_id: None,
                windows: Vec::new(),
            }],
            active_session_id: Some("$7".to_owned()),
        };
        let command = MuxCommand::DitchSession {
            session_id: "$7".to_owned(),
        };

        assert_eq!(
            resolve_owned_session_name(
                &snapshot,
                &HashSet::from(["owned".to_owned()]),
                &command,
                "space-3"
            )
            .expect("resolve"),
            Some("owned".to_owned())
        );
    }

    #[test]
    fn legacy_config_includes_override_backend_and_preserve_remote_inheritance() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config = directory.path().join("config.toml");
        std::fs::write(
            &config,
            "include = [\"local.toml\", \"?missing.toml\"]\n[multiplexer]\nbackend = \"tmux\"\n",
        )
        .expect("root config");
        std::fs::write(
            directory.path().join("local.toml"),
            "[multiplexer]\nbackend = \"zellij\"\n[multiplexer.remote]\nhost = \"devbox\"\n",
        )
        .expect("included config");

        let config = load_legacy_config(&config).expect("load config");

        assert_eq!(config.backend, Some(Backend::Zellij));
        assert!(config.remote);

        let native = directory.path().join("native.toml");
        std::fs::write(
            &native,
            "include = [\"native-override.toml\"]\n[multiplexer]\nbackend = \"tmux\"\n",
        )
        .expect("native root config");
        std::fs::write(
            directory.path().join("native-override.toml"),
            "[multiplexer]\nbackend = \"native\"\n",
        )
        .expect("native override");
        let native = load_legacy_config(&native).expect("load native config");
        assert_eq!(native.backend, None);
        assert!(native.backend_set);
        assert!(!native.remote);
    }

    #[test]
    fn first_run_migrates_remote_spaces_and_membership_from_the_app_catalog() {
        let directory = tempfile::tempdir().expect("tempdir");
        let legacy_path = directory.path().join("session-order.sqlite3");
        let legacy = Connection::open(&legacy_path).expect("legacy database");
        legacy
            .execute_batch(
                "CREATE TABLE workspace_spaces (
                     id INTEGER PRIMARY KEY,
                     remote_id TEXT,
                     name TEXT,
                     position INTEGER
                 );
                 CREATE TABLE workspace_bindings (
                     id INTEGER PRIMARY KEY,
                     space_id INTEGER,
                     backend TEXT,
                     remote TEXT
                 );
                 CREATE TABLE workspace_session_groups (
                     id INTEGER PRIMARY KEY,
                     binding_id INTEGER,
                     name TEXT,
                     position INTEGER
                 );
                 CREATE TABLE workspace_sessions (
                     binding_id INTEGER,
                     name TEXT,
                     group_id INTEGER,
                     position INTEGER
                 );
                 INSERT INTO workspace_spaces VALUES
                     (1, 'stable-space-id', 'Production', 4);
                 INSERT INTO workspace_bindings VALUES
                     (7, 1, 'tmux', NULL),
                     (8, 1, 'zellij', NULL);
                 INSERT INTO workspace_session_groups VALUES
                     (10, 7, 'Later', 1),
                     (11, 7, 'First', 0),
                     (12, 8, 'Foreign', 0);
                 INSERT INTO workspace_sessions VALUES
                     (7, 'later-session', 10, 0),
                     (7, 'first-session', 11, 0),
                     (7, 'second-session', 11, 1),
                     (8, 'foreign-session', 12, 0);",
            )
            .expect("legacy schema");
        let catalog = Catalog::open_with_legacy(
            &directory.path().join("daemon.sqlite"),
            Some(&LegacyCatalog {
                path: legacy_path.clone(),
                inherited_backend: None,
                inherited_remote: false,
            }),
        )
        .expect("migrate catalog");

        assert_eq!(
            catalog.list().expect("list"),
            vec![SpaceSummary {
                catalog_version: CATALOG_VERSION,
                id: "stable-space-id".to_owned(),
                name: "Production".to_owned(),
                backend: Backend::Tmux,
            }]
        );
        assert_eq!(
            catalog.session_names("stable-space-id").expect("sessions"),
            HashSet::from([
                "first-session".to_owned(),
                "second-session".to_owned(),
                "later-session".to_owned(),
            ])
        );
        let mut sessions = catalog
            .connection
            .prepare(
                "SELECT session_name FROM remote_space_sessions
                 WHERE space_id = ?1 ORDER BY position",
            )
            .expect("session order");
        assert_eq!(
            sessions
                .query_map(["stable-space-id"], |row| row.get::<_, String>(0))
                .expect("query order")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("collect order"),
            ["first-session", "second-session", "later-session"]
        );

        let catalog = Catalog::open_with_legacy(
            &directory.path().join("remote-daemon.sqlite"),
            Some(&LegacyCatalog {
                path: legacy_path.clone(),
                inherited_backend: None,
                inherited_remote: true,
            }),
        )
        .expect("skip inherited remote catalog");
        assert!(catalog.list().expect("list").is_empty());

        legacy
            .execute(
                "UPDATE workspace_bindings SET remote = ?1 WHERE id = 7",
                [r#"{"source":"local"}"#],
            )
            .expect("explicit local binding");
        let catalog = Catalog::open_with_legacy(
            &directory.path().join("explicit-local-daemon.sqlite"),
            Some(&LegacyCatalog {
                path: legacy_path,
                inherited_backend: None,
                inherited_remote: true,
            }),
        )
        .expect("migrate explicit local catalog");
        assert_eq!(
            catalog
                .list()
                .expect("list")
                .into_iter()
                .map(|space| space.id)
                .collect::<Vec<_>>(),
            ["stable-space-id"]
        );
    }

    #[test]
    fn legacy_daemon_membership_schema_becomes_a_backend_scoped_ownership_ledger() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon.sqlite");
        let legacy = Connection::open(&path).expect("legacy database");
        legacy
            .execute_batch(
                "CREATE TABLE daemon_metadata (key TEXT PRIMARY KEY);
                 CREATE TABLE remote_spaces (
                     id TEXT PRIMARY KEY,
                     name TEXT NOT NULL UNIQUE,
                     backend TEXT NOT NULL,
                     position INTEGER NOT NULL
                 );
                 CREATE TABLE remote_space_sessions (
                     space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                     session_name TEXT NOT NULL,
                     position INTEGER NOT NULL,
                     PRIMARY KEY (space_id, session_name)
                 );
                 INSERT INTO remote_spaces VALUES
                     ('first', 'First', 'tmux', 0),
                     ('second', 'Second', 'rmux', 1);
                 INSERT INTO remote_space_sessions VALUES
                     ('first', 'shared', 0),
                     ('first', 'first-only', 1),
                     ('second', 'shared', 0),
                     ('second', 'second-only', 1);",
            )
            .expect("legacy schema");
        drop(legacy);

        let catalog = Catalog::open_with_legacy(&path, None).expect("migrate catalog");

        assert!(
            table_has_unique_columns(
                &catalog.connection,
                "remote_space_sessions",
                &["backend", "session_name"],
            )
            .expect("unique ownership")
        );
        assert_eq!(
            catalog.session_names("first").expect("first sessions"),
            HashSet::from(["shared".to_owned(), "first-only".to_owned()])
        );
        assert_eq!(
            catalog.session_names("second").expect("second sessions"),
            HashSet::from(["shared".to_owned(), "second-only".to_owned()])
        );
    }
    #[test]
    fn same_backend_legacy_conflicts_are_archived_without_data_loss() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("daemon.sqlite");
        let legacy = Connection::open(&path).expect("legacy database");
        legacy
            .execute_batch(
                "CREATE TABLE daemon_metadata (key TEXT PRIMARY KEY);
                 CREATE TABLE remote_spaces (
                     id TEXT PRIMARY KEY,
                     name TEXT NOT NULL UNIQUE,
                     backend TEXT NOT NULL,
                     position INTEGER NOT NULL
                 );
                 CREATE TABLE remote_space_sessions (
                     space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                     session_name TEXT NOT NULL,
                     position INTEGER NOT NULL,
                     PRIMARY KEY (space_id, session_name)
                 );
                 INSERT INTO remote_spaces VALUES
                     ('first', 'First', 'tmux', 0),
                     ('second', 'Second', 'tmux', 1);
                 INSERT INTO remote_space_sessions VALUES
                     ('first', 'shared', 0),
                     ('second', 'shared', 0);",
            )
            .expect("legacy schema");
        drop(legacy);

        let catalog = Catalog::open_with_legacy(&path, None).expect("migrate catalog");
        assert_eq!(
            catalog.session_names("first").expect("first sessions"),
            HashSet::from(["shared".to_owned()])
        );
        assert!(
            catalog
                .session_names("second")
                .expect("second sessions")
                .is_empty()
        );
        let archived = catalog
            .connection
            .query_row(
                "SELECT space_id, backend, session_name
                 FROM remote_space_session_conflicts",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("archived conflict");
        assert_eq!(
            archived,
            ("second".to_owned(), "tmux".to_owned(), "shared".to_owned())
        );
    }

    #[test]
    fn same_session_name_can_be_reserved_on_independent_backends() {
        let (_dir, mut catalog) = catalog();
        let tmux = catalog.create("Tmux", Backend::Tmux).expect("tmux space");
        let rmux = catalog.create("Rmux", Backend::Rmux).expect("rmux space");

        assert!(matches!(
            catalog
                .prepare_create_session(&tmux.id, Backend::Tmux, "shared", &empty_snapshot())
                .expect("tmux reservation"),
            SessionReservation::Acquired { .. }
        ));
        assert!(matches!(
            catalog
                .prepare_create_session(&rmux.id, Backend::Rmux, "shared", &empty_snapshot())
                .expect("rmux reservation"),
            SessionReservation::Acquired { .. }
        ));
        let rows = catalog
            .connection
            .prepare(
                "SELECT backend, session_name FROM remote_space_sessions
                 WHERE session_name = 'shared' ORDER BY backend",
            )
            .expect("ownership rows")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query ownership rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect ownership rows");
        assert_eq!(
            rows,
            vec![
                ("rmux".to_owned(), "shared".to_owned()),
                ("tmux".to_owned(), "shared".to_owned())
            ]
        );
    }

    #[test]
    fn expired_absent_reservation_is_reclaimed_but_live_reservation_is_protected() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        first
            .prepare_create_session(
                &first_space.id,
                Backend::Tmux,
                "reclaimable",
                &empty_snapshot(),
            )
            .expect("first reservation");

        assert!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "reclaimable",
                    &empty_snapshot(),
                )
                .is_err(),
            "a live lease must protect the reservation"
        );
        first
            .connection
            .execute(
                "UPDATE remote_space_sessions
                 SET reservation_expires_at = 0
                 WHERE backend = 'tmux' AND session_name = 'reclaimable'",
                [],
            )
            .expect("expire reservation");

        assert!(matches!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "reclaimable",
                    &empty_snapshot(),
                )
                .expect("reclaim expired reservation"),
            SessionReservation::Acquired { .. }
        ));
    }
    #[test]
    fn expired_reservation_with_live_owner_lock_cannot_be_reclaimed() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(
                &first_space.id,
                Backend::Tmux,
                "live-owner",
                &empty_snapshot(),
            )
            .expect("first reservation");
        let token = match &reservation {
            SessionReservation::Acquired { token } => token.clone(),
            _ => panic!("expected acquired reservation"),
        };
        let lease = SessionReservationLease::start(
            &first.path,
            Backend::Tmux,
            &first_space.id,
            "live-owner",
            &token,
        )
        .expect("start owner lease");
        first
            .connection
            .execute(
                "UPDATE remote_space_sessions
                 SET reservation_expires_at = 0
                 WHERE backend = 'tmux' AND session_name = 'live-owner'",
                [],
            )
            .expect("expire reservation");

        assert!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "live-owner",
                    &empty_snapshot(),
                )
                .is_err(),
            "owner lock must protect a live reservation after lease expiry"
        );
        drop(lease);
        assert!(matches!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "live-owner",
                    &empty_snapshot(),
                )
                .expect("reclaim after owner exits"),
            SessionReservation::Acquired { .. }
        ));
    }

    #[test]
    fn stale_open_cannot_reclaim_a_replacement_operation_lease() {
        let (_dir, catalog) = catalog();
        let lease_path = session_operation_lease_path(&catalog.path, Backend::Tmux, "replacement");

        // B opens the old canonical sidecar while A still owns its lock.
        let owner = Connection::open(&lease_path).expect("open original owner");
        owner
            .busy_timeout(Duration::ZERO)
            .expect("owner busy timeout");
        owner
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
                 DELETE FROM owner_lock;
                 INSERT INTO owner_lock (id) VALUES (1);
                 BEGIN IMMEDIATE;",
            )
            .expect("lock original owner");
        let stale_connection = Connection::open(&lease_path).expect("open stale reader");
        let stale_identity = lease_path_identity(&lease_path).expect("original identity");

        // A retires/releases the old sidecar; C then creates and locks a replacement at the
        // canonical path before B finishes its proof.
        let retired =
            rename_lease_sidecar(&lease_path, stale_identity).expect("retire original owner");
        owner
            .execute_batch("ROLLBACK;")
            .expect("release original owner");
        drop(owner);
        let replacement = Connection::open(&lease_path).expect("open replacement owner");
        replacement
            .busy_timeout(Duration::ZERO)
            .expect("replacement busy timeout");
        replacement
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
                 DELETE FROM owner_lock;
                 INSERT INTO owner_lock (id) VALUES (1);
                 BEGIN IMMEDIATE;",
            )
            .expect("lock replacement owner");
        let replacement_identity = lease_path_identity(&lease_path).expect("replacement identity");
        assert_ne!(stale_identity, replacement_identity);

        // The owner proof fails closed while C holds the replacement lock.
        assert!(
            session_operation_owner_is_live(&catalog.path, Backend::Tmux, "replacement")
                .expect("replacement owner proof"),
            "replacement owner must remain protected"
        );
        assert!(!lease_path_matches(&lease_path, stale_identity));
        assert!(
            rename_lease_sidecar(&lease_path, stale_identity).is_none(),
            "stale B must not retire C's replacement"
        );
        remove_lease_tombstone(Some(RetiredLease {
            path: lease_path.clone(),
            identity: stale_identity,
        }));
        assert_eq!(lease_path_identity(&lease_path), Some(replacement_identity));

        drop(stale_connection);
        drop(replacement);
        remove_lease_tombstone(Some(retired));
    }

    #[test]
    fn rename_destination_operation_lease_blocks_concurrent_create() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(&first_space.id, Backend::Tmux, "old", &empty_snapshot())
            .expect("reserve source session");
        first
            .finalize_session_reservation(Backend::Tmux, &first_space.id, "old", &reservation)
            .expect("finalize source session");

        let leases = SessionOperationLease::start_many(&first.path, Backend::Tmux, &["new", "old"])
            .expect("acquire rename source and destination leases");
        let error = second
            .prepare_create_session(&second_space.id, Backend::Tmux, "new", &empty_snapshot())
            .expect_err("destination create must wait for rename operation");
        assert!(
            error
                .to_string()
                .contains("session operation is already in progress")
        );
        drop(leases);
        second
            .prepare_create_session(
                &second_space.id,
                Backend::Tmux,
                "new",
                &snapshot_with_session("old"),
            )
            .expect("reserve destination before rename");
        let error = first
            .verify_session_rename_target(Backend::Tmux, "old", "new")
            .expect_err("rename must reject an existing destination claim");
        assert!(
            error
                .to_string()
                .contains("session rename target new is already owned or reserved")
        );
    }

    #[test]
    fn two_connections_cannot_reserve_the_same_backend_session() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(&first_space.id, Backend::Tmux, "shared", &empty_snapshot())
            .expect("first reservation");
        assert!(matches!(reservation, SessionReservation::Acquired { .. }));

        let error = second
            .prepare_create_session(&second_space.id, Backend::Tmux, "shared", &empty_snapshot())
            .expect_err("second Space must not claim the session");
        assert!(
            error
                .to_string()
                .contains("session already belongs to another remote Space")
        );

        // A finalization failure after the backend created the session leaves the reservation in
        // place. It cannot be captured by another Space and an authoritative snapshot repairs it.
        first
            .sync_sessions(
                &first_space.id,
                Backend::Tmux,
                &snapshot_with_session("shared"),
            )
            .expect("reconcile durable reservation");
        let state = first
            .connection
            .query_row(
                "SELECT state FROM remote_space_sessions
                 WHERE backend = 'tmux' AND session_name = 'shared'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("ownership state");
        assert_eq!(state, "active");
        assert!(matches!(
            first
                .prepare_create_session(
                    &first_space.id,
                    Backend::Tmux,
                    "shared",
                    &snapshot_with_session("shared")
                )
                .expect("same-Space retry"),
            SessionReservation::ExistingActive
        ));
        assert!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "shared",
                    &snapshot_with_session("shared")
                )
                .is_err()
        );
    }

    #[test]
    fn project_and_worktree_backend_failures_release_only_their_new_reservations() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        for command in [
            MuxCommand::CreateProjectSession {
                session_id: "project-retryable".to_owned(),
                cwd: "/repo".to_owned(),
            },
            MuxCommand::CreateWorktreeSession {
                session_id: "worktree-retryable".to_owned(),
                cwd: "/repo-worktree".to_owned(),
            },
        ] {
            let session_id = created_session_id(&command)
                .expect("create command")
                .to_owned();
            let error = first
                .execute_with_backend(
                    Backend::Tmux,
                    &first_space.id,
                    command,
                    |_| Ok(()),
                    || Ok(empty_snapshot()),
                    |_| Err(anyhow::anyhow!("simulated backend failure")),
                )
                .expect_err("backend failure");
            assert!(error.to_string().contains("simulated backend failure"));
            assert!(
                !first
                    .session_names(&first_space.id)
                    .expect("first sessions")
                    .contains(&session_id)
            );
            assert!(matches!(
                second
                    .prepare_create_session(
                        &second_space.id,
                        Backend::Tmux,
                        &session_id,
                        &empty_snapshot()
                    )
                    .expect("reservation after rollback"),
                SessionReservation::Acquired { .. }
            ));
        }
    }
    #[test]
    fn backend_failure_with_live_post_snapshot_retains_ownership() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        let snapshots = Cell::new(0);
        let error = catalog
            .execute_with_backend(
                Backend::Tmux,
                &space.id,
                MuxCommand::CreateProjectSession {
                    session_id: "partially-created".to_owned(),
                    cwd: "/repo".to_owned(),
                },
                |_| Ok(()),
                || {
                    let snapshot = if snapshots.replace(snapshots.get() + 1) == 0 {
                        empty_snapshot()
                    } else {
                        snapshot_with_session("partially-created")
                    };
                    Ok(snapshot)
                },
                |_| Err(anyhow::anyhow!("simulated partial backend failure")),
            )
            .expect_err("backend failure");

        assert!(
            error
                .to_string()
                .contains("ownership was retained for reconciliation")
        );
        assert!(
            catalog
                .session_names(&space.id)
                .expect("owned sessions")
                .contains("partially-created")
        );
        let state = catalog
            .connection
            .query_row(
                "SELECT state FROM remote_space_sessions
                 WHERE backend = 'tmux' AND session_name = 'partially-created'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("ownership state");
        assert_eq!(state, "active");
    }

    #[test]
    fn stale_same_space_ownership_is_pruned_before_create_authorization() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        let reservation = catalog
            .prepare_create_session(&space.id, Backend::Tmux, "stale", &empty_snapshot())
            .expect("reservation");
        catalog
            .finalize_session_reservation(Backend::Tmux, &space.id, "stale", &reservation)
            .expect("simulate completed backend create");

        assert!(matches!(
            catalog
                .prepare_create_session(&space.id, Backend::Tmux, "stale", &empty_snapshot())
                .expect("recreate after authoritative absence"),
            SessionReservation::Acquired { .. }
        ));
    }

    #[test]
    fn same_space_retry_does_not_duplicate_an_in_flight_backend_create() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        catalog
            .prepare_create_session(&space.id, Backend::Tmux, "pending", &empty_snapshot())
            .expect("first reservation");
        let executed = Cell::new(false);

        let error = catalog
            .execute_with_backend(
                Backend::Tmux,
                &space.id,
                MuxCommand::CreateProjectSession {
                    session_id: "pending".to_owned(),
                    cwd: "/repo".to_owned(),
                },
                |_| Ok(()),
                || Ok(empty_snapshot()),
                |_| {
                    executed.set(true);
                    Ok(None)
                },
            )
            .expect_err("an in-flight create must not be issued twice");

        assert!(
            error
                .to_string()
                .contains("session creation is already in progress")
        );
        assert!(!executed.get(), "retry reached the backend");
    }

    #[test]
    fn stale_other_space_ownership_is_reclaimed_from_the_same_backend_snapshot() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(
                &first_space.id,
                Backend::Tmux,
                "reclaimable",
                &empty_snapshot(),
            )
            .expect("first reservation");
        first
            .finalize_session_reservation(
                Backend::Tmux,
                &first_space.id,
                "reclaimable",
                &reservation,
            )
            .expect("simulate completed backend create");

        assert!(matches!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "reclaimable",
                    &empty_snapshot(),
                )
                .expect("reclaim stale ownership"),
            SessionReservation::Acquired { .. }
        ));
        assert!(
            !first
                .session_names(&first_space.id)
                .expect("first sessions")
                .contains("reclaimable")
        );
    }

    #[test]
    fn backend_io_does_not_hold_the_catalog_write_lock() {
        let (_dir, mut first, second, first_space, second_space) = two_catalogs();
        second
            .connection
            .busy_timeout(std::time::Duration::ZERO)
            .expect("disable retry while lock is held");
        let second = RefCell::new(second);
        let snapshots = Cell::new(0);
        let reservation_during_snapshot = Cell::new(false);
        let reservation_during_execute = Cell::new(false);

        let error = first
            .execute_with_backend(
                Backend::Tmux,
                &first_space.id,
                MuxCommand::CreateProjectSession {
                    session_id: "first-create".to_owned(),
                    cwd: "/repo".to_owned(),
                },
                |_| Ok(()),
                || {
                    if snapshots.replace(snapshots.get() + 1) == 0 {
                        reservation_during_snapshot.set(
                            second
                                .borrow_mut()
                                .prepare_create_session(
                                    &second_space.id,
                                    Backend::Tmux,
                                    "during-snapshot",
                                    &empty_snapshot(),
                                )
                                .is_ok(),
                        );
                    }
                    Ok(empty_snapshot())
                },
                |_| {
                    reservation_during_execute.set(
                        second
                            .borrow_mut()
                            .prepare_create_session(
                                &second_space.id,
                                Backend::Tmux,
                                "during-execute",
                                &empty_snapshot(),
                            )
                            .is_ok(),
                    );
                    Err(anyhow::anyhow!("simulated backend failure"))
                },
            )
            .expect_err("backend failure");

        assert!(error.to_string().contains("simulated backend failure"));
        assert!(reservation_during_snapshot.get());
        assert!(reservation_during_execute.get());
    }
    #[test]
    fn rename_does_not_mutate_a_recreated_session_after_snapshot() {
        let (_dir, mut first, mut second, first_space, _second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(&first_space.id, Backend::Tmux, "old", &empty_snapshot())
            .expect("initial reservation");
        first
            .finalize_session_reservation(Backend::Tmux, &first_space.id, "old", &reservation)
            .expect("initial active ownership");
        let injected = Cell::new(false);
        let executed = Cell::new(false);
        let error = first
            .execute_with_backend(
                Backend::Tmux,
                &first_space.id,
                MuxCommand::RenameSession {
                    session_id: "old".to_owned(),
                    name: "new".to_owned(),
                },
                |_| Ok(()),
                || {
                    if !injected.replace(true) {
                        second
                            .prepare_create_session(
                                &first_space.id,
                                Backend::Tmux,
                                "old",
                                &empty_snapshot(),
                            )
                            .expect("concurrent recreate");
                    }
                    Ok(snapshot_with_session("old"))
                },
                |_| {
                    executed.set(true);
                    Ok(None)
                },
            )
            .expect_err("stale rename must abort before backend mutation");
        assert!(error.to_string().contains("ownership changed"));
        assert!(!executed.get());
    }

    #[test]
    fn ditch_does_not_delete_a_recreated_session_after_snapshot() {
        let (_dir, mut first, mut second, first_space, _second_space) = two_catalogs();
        let reservation = first
            .prepare_create_session(&first_space.id, Backend::Tmux, "old", &empty_snapshot())
            .expect("initial reservation");
        first
            .finalize_session_reservation(Backend::Tmux, &first_space.id, "old", &reservation)
            .expect("initial active ownership");
        let injected = Cell::new(false);
        let executed = Cell::new(false);
        let error = first
            .execute_with_backend(
                Backend::Tmux,
                &first_space.id,
                MuxCommand::DitchSession {
                    session_id: "old".to_owned(),
                },
                |_| Ok(()),
                || {
                    if !injected.replace(true) {
                        second
                            .prepare_create_session(
                                &first_space.id,
                                Backend::Tmux,
                                "old",
                                &empty_snapshot(),
                            )
                            .expect("concurrent recreate");
                    }
                    Ok(snapshot_with_session("old"))
                },
                |_| {
                    executed.set(true);
                    Ok(None)
                },
            )
            .expect_err("stale ditch must abort before backend mutation");
        assert!(error.to_string().contains("ownership changed"));
        assert!(!executed.get());
    }

    #[test]
    fn absent_snapshot_does_not_reclaim_a_concurrent_finalized_create() {
        let (_dir, mut first, mut second, first_space, _second_space) = two_catalogs();
        let injected = Cell::new(false);
        let error = first
            .execute_with_backend(
                Backend::Tmux,
                &first_space.id,
                MuxCommand::CreateProjectSession {
                    session_id: "new".to_owned(),
                    cwd: "/repo".to_owned(),
                },
                |_| Ok(()),
                || {
                    if !injected.replace(true) {
                        let reservation = second
                            .prepare_create_session(
                                &first_space.id,
                                Backend::Tmux,
                                "concurrent",
                                &empty_snapshot(),
                            )
                            .expect("concurrent reservation");
                        second
                            .finalize_session_reservation(
                                Backend::Tmux,
                                &first_space.id,
                                "concurrent",
                                &reservation,
                            )
                            .expect("concurrent finalization");
                    }
                    Ok(empty_snapshot())
                },
                |_| Err(anyhow::anyhow!("simulated backend failure")),
            )
            .expect_err("backend failure");
        assert!(error.to_string().contains("simulated backend failure"));
        assert!(
            first
                .session_names(&first_space.id)
                .expect("owned sessions")
                .contains("concurrent")
        );
    }

    #[test]
    fn invalid_recursive_launch_never_snapshots_or_mutates_a_backend() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        let snapshots = Cell::new(0);
        let mutations = Cell::new(0);
        let mut plan = launch_plan("invalid");
        plan.default_cwd.clear();

        let error = catalog
            .execute_with_backend(
                Backend::Tmux,
                &space.id,
                MuxCommand::CreateSession { plan },
                validate_remote_session_launch,
                || {
                    snapshots.set(snapshots.get() + 1);
                    Ok(empty_snapshot())
                },
                |_| {
                    mutations.set(mutations.get() + 1);
                    Ok(None)
                },
            )
            .expect_err("invalid plan");

        assert!(
            error
                .to_string()
                .contains("invalid recursive session launch")
        );
        assert_eq!(snapshots.get(), 0);
        assert_eq!(mutations.get(), 0);
    }

    #[test]
    fn unsupported_recursive_launch_never_snapshots_or_mutates_a_backend() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        let snapshots = Cell::new(0);
        let mutations = Cell::new(0);

        let error = catalog
            .execute_with_backend(
                Backend::Tmux,
                &space.id,
                MuxCommand::CreateSession {
                    plan: launch_plan("unsupported"),
                },
                |command| {
                    validate_remote_session_launch(command)?;
                    require_session_launch_capability(&BindingOperationOutcome::Unsupported)
                },
                || {
                    snapshots.set(snapshots.get() + 1);
                    Ok(empty_snapshot())
                },
                |_| {
                    mutations.set(mutations.get() + 1);
                    Ok(None)
                },
            )
            .expect_err("unsupported backend");

        assert!(
            error
                .to_string()
                .contains("recursive session launch is unsupported")
        );
        assert_eq!(snapshots.get(), 0);
        assert_eq!(mutations.get(), 0);
    }
}
