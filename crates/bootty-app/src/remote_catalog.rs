use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    automation::directory::ClaimOwner,
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig, SshRemoteConfig},
    session_order::{BackendConnectionNamespace, SessionOrderStore},
    workspace::{
        DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceBinding, WorkspaceStore,
    },
};
use anyhow::{Context, Result, bail};
use bootty_mux::project::{ProjectPickerEntry, WorktreePickerEntry};
use bootty_mux::{
    backend::{MuxBackend, MuxBackendOperationError},
    capability::BindingOperationOutcome,
    command::MuxCommand,
    process::{CommandRunner, SystemCommandRunner},
    snapshot::{MuxSnapshot, session_matches},
    ssh::{SshRemote, remote_daemon_failure},
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

pub const REMOTE_SPACE_CATALOG_VERSION: u32 = 3;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteSpaceSummary {
    pub catalog_version: u32,
    pub id: String,
    pub name: String,
    pub backend: MultiplexerBackendConfig,
}

#[cfg(not(test))]
const SESSION_RESERVATION_LEASE_SECONDS: i64 = 60;
#[cfg(test)]
const SESSION_RESERVATION_LEASE_SECONDS: i64 = 1;

#[derive(Debug)]
enum SessionReservation {
    Acquired { token: String },
    ExistingActive,
    ExistingReserved,
    ExpiredReserved { token: String },
}

#[derive(Clone, Debug)]
struct PendingSessionMutation {
    binding_id: i64,
    operation: String,
    session_id: String,
    old_name: String,
    new_name: Option<String>,
    token: String,
}

impl SessionReservation {
    fn new_token(&self) -> Option<&str> {
        match self {
            Self::Acquired { token } => Some(token),
            Self::ExistingActive | Self::ExistingReserved | Self::ExpiredReserved { .. } => None,
        }
    }
}

fn reservation_token(nonce: &str) -> Result<String> {
    let owner = ClaimOwner::current("remote-session-reservation")?;
    Ok(format!("{}:{}:{nonce}", owner.pid, owner.started_at_ms))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaseFileIdentity {
    first: u64,
    second: u64,
}

fn lease_file_identity(path: &Path) -> Result<Option<LeaseFileIdentity>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Some(LeaseFileIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        }))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        Ok(Some(LeaseFileIdentity {
            first: metadata.volume_serial_number().unwrap_or_default(),
            second: metadata.file_index().unwrap_or_default(),
        }))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or_default();
        Ok(Some(LeaseFileIdentity {
            first: metadata.len(),
            second: modified,
        }))
    }
}
fn lease_identity_matches(path: &Path, expected: LeaseFileIdentity) -> Result<bool> {
    Ok(lease_file_identity(path)? == Some(expected))
}

fn remove_lease_path_if_identity(path: &Path, expected: LeaseFileIdentity) -> Result<()> {
    if !lease_identity_matches(path, expected)? {
        return Ok(());
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn reclaim_retired_reservation_leases(lease_path: &Path) -> bool {
    let Some(parent) = lease_path.parent() else {
        return false;
    };
    let Some(file_name) = lease_path.file_name().map(|name| name.to_string_lossy()) else {
        return false;
    };
    let prefix = format!("{file_name}.retired-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let path = entry.path();
        let Some(name) = path.file_name().map(|name| name.to_string_lossy()) else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let Some(expected) = lease_file_identity(&path).ok().flatten() else {
            return false;
        };
        let Ok(connection) = rusqlite::Connection::open(&path) else {
            return false;
        };
        if lease_file_identity(&path).ok().flatten() != Some(expected) {
            continue;
        }
        if connection.busy_timeout(Duration::ZERO).is_err() {
            return false;
        }
        match connection.execute_batch("BEGIN IMMEDIATE;") {
            Ok(()) => {
                let _ = connection.execute_batch("ROLLBACK;");
                if lease_file_identity(&path).ok().flatten() != Some(expected) {
                    continue;
                }
                drop(connection);
                if remove_lease_path_if_identity(&path, expected).is_err() {
                    return false;
                }
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                return false;
            }
            Err(_) => return false,
        }
    }
    true
}

fn reservation_owner_is_dead(path: &Path, token: Option<&str>) -> bool {
    let Some(token) = token else {
        return true;
    };
    let lease_path = reservation_lease_path(path, token);
    for _ in 0..3 {
        let expected = match lease_file_identity(&lease_path) {
            Ok(Some(expected)) => expected,
            Ok(None) => return reclaim_retired_reservation_leases(&lease_path),
            Err(_) => return false,
        };
        let Ok(connection) = rusqlite::Connection::open(&lease_path) else {
            if lease_file_identity(&lease_path).ok().flatten() != Some(expected) {
                continue;
            }
            return false;
        };
        if lease_file_identity(&lease_path).ok().flatten() != Some(expected) {
            continue;
        }
        if connection.busy_timeout(Duration::ZERO).is_err() {
            return false;
        }
        match connection.execute_batch("BEGIN IMMEDIATE;") {
            Ok(()) => {
                let _ = connection.execute_batch("ROLLBACK;");
                if lease_file_identity(&lease_path).ok().flatten() == Some(expected) {
                    return true;
                }
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                if lease_file_identity(&lease_path).ok().flatten() == Some(expected) {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    false
}

fn reservation_lease_path(path: &Path, token: &str) -> PathBuf {
    let safe_token = token
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    path.with_extension(format!("remote-reservation-{safe_token}.lease"))
}

static RETIRED_LEASE_NONCE: AtomicU64 = AtomicU64::new(1);

fn retired_reservation_lease_path(lease_path: &Path) -> PathBuf {
    let nonce = RETIRED_LEASE_NONCE.fetch_add(1, Ordering::Relaxed);
    let file_name = lease_path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| std::borrow::Cow::Borrowed("remote-reservation-lease"));
    lease_path.with_file_name(format!(
        "{file_name}.retired-{}-{nonce}",
        std::process::id()
    ))
}

fn retire_reservation_lease_path(
    lease_path: &Path,
    retired_path: &Path,
    expected: LeaseFileIdentity,
) -> Result<()> {
    match lease_file_identity(lease_path)? {
        None => return Ok(()),
        Some(actual) if actual != expected => {
            bail!("remote session reservation lease path changed before retirement")
        }
        Some(_) => {}
    }
    std::fs::rename(lease_path, retired_path).context("retire remote session reservation lease")?;
    if lease_file_identity(retired_path)? != Some(expected) {
        bail!("remote session reservation tombstone identity changed");
    }
    Ok(())
}

fn remove_reservation_lease(path: &Path, token: &str) -> Result<()> {
    let lease_path = reservation_lease_path(path, token);
    for _ in 0..3 {
        let expected = match lease_file_identity(&lease_path)? {
            Some(expected) => expected,
            None => return Ok(()),
        };
        let connection = match rusqlite::Connection::open(&lease_path) {
            Ok(connection) => connection,
            Err(_) if lease_file_identity(&lease_path)?.is_none() => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "open remote session reservation lease {}",
                        lease_path.display()
                    )
                });
            }
        };
        if lease_file_identity(&lease_path)? != Some(expected) {
            continue;
        }
        connection.busy_timeout(Duration::ZERO)?;
        match connection.execute_batch("BEGIN IMMEDIATE;") {
            Ok(()) => {
                if lease_file_identity(&lease_path)? != Some(expected) {
                    let _ = connection.execute_batch("ROLLBACK;");
                    continue;
                }
                let retired_path = retired_reservation_lease_path(&lease_path);
                if let Err(error) =
                    retire_reservation_lease_path(&lease_path, &retired_path, expected)
                {
                    let _ = connection.execute_batch("ROLLBACK;");
                    if lease_file_identity(&lease_path)? != Some(expected) {
                        continue;
                    }
                    return Err(error);
                }
                let _ = connection.execute_batch("ROLLBACK;");
                drop(connection);
                remove_lease_path_if_identity(&retired_path, expected)?;
                return Ok(());
            }
            Err(rusqlite::Error::SqliteFailure(error, _))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) =>
            {
                if lease_file_identity(&lease_path)? == Some(expected) {
                    return Ok(());
                }
            }
            Err(error) => {
                return Err(error).context("lock remote session reservation lease for retirement");
            }
        }
    }
    bail!("remote session reservation lease path changed during retirement")
}

#[derive(Debug)]
struct ReservationHeartbeat {
    stop: mpsc::Sender<()>,
    handle: Option<std::thread::JoinHandle<()>>,
    ownership: RemoteSessionOwnership,
    session_name: String,
    token: String,
    healthy: Arc<AtomicBool>,
    lease_path: PathBuf,
    lease_identity: LeaseFileIdentity,
    retired_lease_path: PathBuf,
}

impl ReservationHeartbeat {
    fn start(
        ownership: &RemoteSessionOwnership,
        session_name: &str,
        reservation: &SessionReservation,
    ) -> Result<Self> {
        let Some(token) = reservation.new_token() else {
            bail!("cannot heartbeat a non-acquired remote session reservation");
        };
        let lease_path = reservation_lease_path(&ownership.path, token);
        let lease_identity_before_open = lease_file_identity(&lease_path)?;
        let owner_connection = rusqlite::Connection::open(&lease_path).with_context(|| {
            format!(
                "open remote session reservation lease {}",
                lease_path.display()
            )
        })?;
        let lease_identity = lease_file_identity(&lease_path)?
            .ok_or_else(|| anyhow::anyhow!("remote session reservation lease disappeared"))?;
        if lease_identity_before_open.is_some_and(|identity| identity != lease_identity) {
            bail!("remote session reservation lease path changed while opening");
        }
        owner_connection.busy_timeout(Duration::ZERO)?;
        owner_connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
             DELETE FROM owner_lock;
             INSERT INTO owner_lock (id) VALUES (1);
             BEGIN IMMEDIATE;",
        )?;
        if !lease_identity_matches(&lease_path, lease_identity)? {
            bail!("remote session reservation lease path changed while locking");
        }
        let (stop, receiver) = mpsc::channel();
        let heartbeat_ownership = ownership.clone();
        let heartbeat_session_name = session_name.to_owned();
        let heartbeat_token = token.to_owned();
        let interval = Duration::from_secs((SESSION_RESERVATION_LEASE_SECONDS / 3).max(1) as u64);
        let healthy = Arc::new(AtomicBool::new(true));
        let worker_healthy = Arc::clone(&healthy);
        let catalog_path = ownership.path.clone();
        let namespace = ownership.namespace.clone();
        let binding_id = ownership.binding_id;
        let thread_token = heartbeat_token.clone();
        let thread_lease_path = lease_path.clone();
        let thread_lease_identity = lease_identity;
        let retired_lease_path = retired_reservation_lease_path(&lease_path);
        let thread_retired_lease_path = retired_lease_path.clone();
        let handle = std::thread::Builder::new()
            .name("bootty-remote-session-heartbeat".to_owned())
            .spawn(move || {
                let owner_connection = owner_connection;
                let connection = match rusqlite::Connection::open(catalog_path) {
                    Ok(connection) => {
                        let _ = connection.busy_timeout(Duration::from_secs(5));
                        connection
                    }
                    Err(_) => {
                        let _ = receiver.recv();
                        let _ = retire_reservation_lease_path(
                            &thread_lease_path,
                            &thread_retired_lease_path,
                            thread_lease_identity,
                        );
                        drop(owner_connection);
                        let _ = remove_lease_path_if_identity(
                            &thread_retired_lease_path,
                            thread_lease_identity,
                        );
                        return;
                    }
                };
                loop {
                    match receiver.recv_timeout(interval) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            let _ = retire_reservation_lease_path(
                                &thread_lease_path,
                                &thread_retired_lease_path,
                                thread_lease_identity,
                            );
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            let Ok(expires_at) = unix_timestamp().and_then(|now| {
                                now.checked_add(SESSION_RESERVATION_LEASE_SECONDS)
                                    .context("remote session reservation lease overflow")
                            }) else {
                                continue;
                            };
                            match connection.execute(
                                "UPDATE workspace_session_ownership
                                 SET reservation_expires_at = ?4
                                 WHERE namespace = ?1 AND binding_id = ?2
                                   AND state = 'reserved'
                                   AND reservation_token = ?3",
                                params![&namespace, binding_id, &thread_token, expires_at],
                            ) {
                                Ok(updated) if updated > 0 => {
                                    worker_healthy.store(true, Ordering::Release);
                                }
                                Ok(_) => {
                                    worker_healthy.store(false, Ordering::Release);
                                }
                                Err(_) => {
                                    // SQLite busy/locked and transient I/O do not prove
                                    // ownership loss while the owner lock remains held.
                                }
                            }
                        }
                    }
                }
                drop(connection);
                drop(owner_connection);
                let _ = remove_lease_path_if_identity(
                    &thread_retired_lease_path,
                    thread_lease_identity,
                );
            });
        let handle = match handle {
            Ok(handle) => handle,
            Err(error) => {
                let _ =
                    retire_reservation_lease_path(&lease_path, &retired_lease_path, lease_identity);
                let _ = remove_lease_path_if_identity(&retired_lease_path, lease_identity);
                return Err(error).context("start remote session reservation heartbeat");
            }
        };
        Ok(Self {
            stop,
            handle: Some(handle),
            ownership: heartbeat_ownership,
            session_name: heartbeat_session_name,
            healthy,
            token: heartbeat_token,
            lease_path,
            lease_identity,
            retired_lease_path,
        })
    }

    fn renew_now(&self) -> Result<()> {
        if !self.healthy.load(Ordering::Acquire) {
            bail!("remote session reservation token is no longer current");
        }
        match self.ownership.renew_token(&self.session_name, &self.token) {
            Ok(true) => {
                self.healthy.store(true, Ordering::Release);
                Ok(())
            }
            Ok(false) => {
                self.healthy.store(false, Ordering::Release);
                bail!("remote session reservation token is no longer current");
            }
            Err(_) => {
                // A transient SQLite busy/locked result cannot prove token loss. The
                // owner-lock-backed heartbeat continues to retry the exact CAS.
                Ok(())
            }
        }
    }
}

impl Drop for ReservationHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let lease_retired = matches!(lease_file_identity(&self.lease_path), Ok(None));
        let _ = retire_reservation_lease_path(
            &self.lease_path,
            &self.retired_lease_path,
            self.lease_identity,
        );
        let _ = remove_lease_path_if_identity(&self.retired_lease_path, self.lease_identity);
        if lease_retired {
            let _ = self
                .ownership
                .release_expired_reservation(&self.session_name, &self.token);
        }
    }
}

#[derive(Clone, Debug)]
struct RemoteSessionOwnership {
    path: PathBuf,
    namespace: String,
    binding_id: i64,
}

impl RemoteSessionOwnership {
    fn new(path: impl Into<PathBuf>, namespace: String, binding_id: i64) -> Result<Self> {
        let ownership = Self {
            path: path.into(),
            namespace,
            binding_id,
        };
        ownership.ensure_schema()?;
        Ok(ownership)
    }

    fn connection(&self) -> Result<rusqlite::Connection> {
        let connection = crate::workspace::open_db(&self.path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        Ok(connection)
    }

    fn ensure_schema(&self) -> Result<()> {
        let connection = self.connection()?;
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS workspace_session_ownership (
                 namespace TEXT NOT NULL,
                 session_name TEXT NOT NULL,
                 binding_id INTEGER NOT NULL
                     REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                 state TEXT NOT NULL DEFAULT 'active'
                     CHECK (state IN ('active', 'reserved')),
                 reservation_token TEXT,
                 reservation_expires_at INTEGER,
                 CHECK (
                     (state = 'active' AND reservation_token IS NULL
                         AND reservation_expires_at IS NULL)
                     OR (state = 'reserved' AND reservation_token IS NOT NULL
                         AND reservation_expires_at IS NOT NULL)
                 ),
                 PRIMARY KEY (namespace, session_name)
             );
             CREATE TABLE IF NOT EXISTS workspace_session_ownership_meta (
                 namespace TEXT PRIMARY KEY,
                 revision INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS workspace_session_mutations (
                 namespace TEXT NOT NULL,
                 binding_id INTEGER NOT NULL
                     REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                 operation TEXT NOT NULL
                     CHECK (operation IN ('rename', 'ditch')),
                 session_id TEXT NOT NULL,
                 old_name TEXT NOT NULL,
                 new_name TEXT,
                 owner TEXT NOT NULL,
                 reservation_token TEXT NOT NULL,
                 revision INTEGER NOT NULL,
                 PRIMARY KEY (namespace, reservation_token),
                 UNIQUE (namespace, old_name),
                 UNIQUE (namespace, new_name)
             );",
        )?;
        connection.execute(
            "INSERT OR IGNORE INTO workspace_session_ownership_meta (namespace, revision)
             VALUES (?1, 0)",
            [&self.namespace],
        )?;
        Ok(())
    }

    fn current_revision(&self) -> Result<i64> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT revision
             FROM workspace_session_ownership_meta
             WHERE namespace = ?1",
            [&self.namespace],
            |row| row.get(0),
        )?)
    }

    fn bump_revision(&self, transaction: &Transaction<'_>) -> Result<()> {
        transaction.execute(
            "UPDATE workspace_session_ownership_meta
             SET revision = revision + 1
             WHERE namespace = ?1",
            [&self.namespace],
        )?;
        Ok(())
    }

    fn reconcile_at_revision(&self, snapshot: &MuxSnapshot, observed_revision: i64) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let leases =
            self.reconcile_in_transaction(&transaction, snapshot, Some(observed_revision), true)?;
        transaction.commit()?;
        for token in leases {
            remove_reservation_lease(&self.path, &token)?;
        }
        Ok(())
    }

    fn reconcile_in_transaction(
        &self,
        transaction: &Transaction<'_>,
        snapshot: &MuxSnapshot,
        observed_revision: Option<i64>,
        reclaim_expired_absent: bool,
    ) -> Result<Vec<String>> {
        let current_revision: i64 = transaction.query_row(
            "SELECT revision
             FROM workspace_session_ownership_meta
             WHERE namespace = ?1",
            [&self.namespace],
            |row| row.get(0),
        )?;
        if observed_revision.is_some_and(|observed| current_revision > observed) {
            return Ok(Vec::new());
        }
        let now = unix_timestamp()?;
        let mut statement = transaction.prepare(
            "SELECT session_name, state, reservation_token, reservation_expires_at
             FROM workspace_session_ownership
             WHERE namespace = ?1",
        )?;
        let rows = statement
            .query_map([&self.namespace], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(statement);
        let mut leases = Vec::new();
        for (session_name, state, reservation_token, reservation_expires_at) in rows {
            let alive = snapshot
                .sessions
                .iter()
                .any(|session| session_matches(session, &session_name));
            let pending = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM workspace_session_mutations
                     WHERE namespace = ?1 AND (old_name = ?2 OR new_name = ?2)
                 )",
                params![&self.namespace, &session_name],
                |row| row.get::<_, bool>(0),
            )?;
            match state.as_str() {
                "active" if !alive && !pending => {
                    let removed = transaction.execute(
                        "DELETE FROM workspace_session_ownership
                         WHERE namespace = ?1 AND session_name = ?2
                           AND state = 'active'",
                        params![&self.namespace, &session_name],
                    )?;
                    if removed == 1 {
                        self.bump_revision(transaction)?;
                    }
                }
                "reserved" if !pending => {
                    let expired =
                        reservation_expires_at.is_some_and(|expires_at| expires_at <= now);
                    let owner_dead =
                        reservation_owner_is_dead(&self.path, reservation_token.as_deref());
                    if expired && owner_dead {
                        let changed = if alive {
                            transaction.execute(
                                "UPDATE workspace_session_ownership
                                 SET state = 'active', reservation_token = NULL,
                                     reservation_expires_at = NULL
                                 WHERE namespace = ?1 AND session_name = ?2
                                   AND state = 'reserved'
                                   AND reservation_token = ?3",
                                params![&self.namespace, &session_name, reservation_token],
                            )?
                        } else if reclaim_expired_absent {
                            transaction.execute(
                                "DELETE FROM workspace_session_ownership
                                 WHERE namespace = ?1 AND session_name = ?2
                                   AND state = 'reserved'
                                   AND reservation_token = ?3",
                                params![&self.namespace, &session_name, reservation_token],
                            )?
                        } else {
                            0
                        };
                        if changed == 1 {
                            self.bump_revision(transaction)?;
                            if !alive && let Some(token) = reservation_token {
                                leases.push(token);
                            }
                        }
                    }
                }
                "active" | "reserved" => {}
                _ => bail!("invalid remote session ownership state {state:?}"),
            }
        }
        Ok(leases)
    }

    #[cfg(test)]
    fn prepare_create(
        &self,
        snapshot: &MuxSnapshot,
        session_name: &str,
        owned_names: &[String],
    ) -> Result<SessionReservation> {
        self.prepare_create_with_observation(snapshot, session_name, owned_names, None)
    }

    fn prepare_create_with_observation(
        &self,
        snapshot: &MuxSnapshot,
        session_name: &str,
        owned_names: &[String],
        observed_revision: Option<i64>,
    ) -> Result<SessionReservation> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = self.reconcile_in_transaction(&transaction, snapshot, observed_revision, false)?;
        if let Some(observed_revision) = observed_revision {
            let current_revision: i64 = transaction.query_row(
                "SELECT revision FROM workspace_session_ownership_meta WHERE namespace = ?1",
                [&self.namespace],
                |row| row.get(0),
            )?;
            if current_revision != observed_revision {
                bail!(
                    "remote session ownership changed during reservation preparation; retry \
                     with a fresh authoritative snapshot"
                );
            }
        }
        let pending_destination: bool = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM workspace_session_mutations
                 WHERE namespace = ?1 AND new_name = ?2
             )",
            params![&self.namespace, session_name],
            |row| row.get(0),
        )?;
        if pending_destination {
            bail!("session creation conflicts with a remote rename in progress");
        }

        let existing = snapshot
            .sessions
            .iter()
            .find(|session| session_matches(session, session_name));
        let ownership = transaction
            .query_row(
                "SELECT binding_id, state, reservation_token, reservation_expires_at
                 FROM workspace_session_ownership
                 WHERE namespace = ?1 AND session_name = ?2",
                params![&self.namespace, session_name],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;

        if let Some((binding_id, state, token, reservation_expires_at)) = ownership {
            if binding_id != self.binding_id {
                if state == "reserved"
                    && token
                        .as_deref()
                        .is_some_and(|token| !reservation_owner_is_dead(&self.path, Some(token)))
                {
                    transaction.commit()?;
                    return Ok(SessionReservation::ExistingReserved);
                }
                bail!("session already belongs to another remote Space");
            }
            match state.as_str() {
                "active" => {
                    transaction.commit()?;
                    return Ok(SessionReservation::ExistingActive);
                }
                "reserved" => {
                    let Some(token) = token else {
                        bail!("invalid remote session ownership reservation");
                    };
                    let expired = reservation_expires_at.is_some_and(|expires_at| {
                        unix_timestamp().is_ok_and(|now| expires_at <= now)
                    }) && reservation_owner_is_dead(&self.path, Some(&token));
                    transaction.commit()?;
                    return Ok(if expired {
                        SessionReservation::ExpiredReserved { token }
                    } else {
                        SessionReservation::ExistingReserved
                    });
                }
                _ => bail!("invalid remote session ownership state {state:?}"),
            }
        }

        if let Some(session) = existing {
            let legacy_owners =
                self.legacy_membership_owners(&transaction, &session.name, session_name)?;
            if legacy_owners
                .iter()
                .any(|binding_id| *binding_id != self.binding_id)
                || !owned_names.iter().any(|name| name == &session.name)
            {
                bail!("session already belongs to another remote Space");
            }
            transaction.execute(
                "INSERT INTO workspace_session_ownership
                 (namespace, session_name, binding_id, state)
                 VALUES (?1, ?2, ?3, 'active')",
                params![&self.namespace, session_name, self.binding_id],
            )?;
            self.bump_revision(&transaction)?;
            transaction.commit()?;
            return Ok(SessionReservation::ExistingActive);
        }

        let nonce: String =
            transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
        let token = reservation_token(&nonce)?;
        let expires_at = unix_timestamp()?
            .checked_add(SESSION_RESERVATION_LEASE_SECONDS)
            .ok_or_else(|| anyhow::anyhow!("remote session reservation lease overflow"))?;
        let inserted = transaction.execute(
            "INSERT INTO workspace_session_ownership
             (namespace, session_name, binding_id, state, reservation_token,
              reservation_expires_at)
             VALUES (?1, ?2, ?3, 'reserved', ?4, ?5)
             ON CONFLICT(namespace, session_name) DO NOTHING",
            params![
                &self.namespace,
                session_name,
                self.binding_id,
                &token,
                expires_at
            ],
        )?;
        if inserted == 1 {
            self.bump_revision(&transaction)?;
            transaction.commit()?;
            return Ok(SessionReservation::Acquired { token });
        }

        let (binding_id, state, existing_token) = transaction
            .query_row(
                "SELECT binding_id, state, reservation_token
                 FROM workspace_session_ownership
                 WHERE namespace = ?1 AND session_name = ?2",
                params![&self.namespace, session_name],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("remote session ownership changed while reserving"))?;
        if binding_id != self.binding_id {
            bail!("session already belongs to another remote Space");
        }
        if state == "reserved" {
            if existing_token.is_none() {
                bail!("invalid remote session ownership reservation");
            }
            return Ok(SessionReservation::ExistingReserved);
        }
        if state != "active" {
            bail!("invalid remote session ownership state {state:?}");
        }
        let _ = self.reconcile_in_transaction(&transaction, snapshot, observed_revision, true)?;
        if let Some(observed_revision) = observed_revision {
            let current_revision: i64 = transaction.query_row(
                "SELECT revision FROM workspace_session_ownership_meta WHERE namespace = ?1",
                [&self.namespace],
                |row| row.get(0),
            )?;
            if current_revision != observed_revision {
                bail!(
                    "remote session ownership changed during mutation preparation; retry \
                     with a fresh authoritative snapshot"
                );
            }
        }
        Ok(SessionReservation::ExistingActive)
    }

    fn prepare_mutation_with_observation(
        &self,
        snapshot: &MuxSnapshot,
        session_name: &str,
        destination_name: Option<&str>,
        owned_names: &[String],
        observed_revision: Option<i64>,
    ) -> Result<SessionReservation> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let _ = self.reconcile_in_transaction(&transaction, snapshot, observed_revision, true)?;
        if let Some(observed_revision) = observed_revision {
            let current_revision: i64 = transaction.query_row(
                "SELECT revision FROM workspace_session_ownership_meta WHERE namespace = ?1",
                [&self.namespace],
                |row| row.get(0),
            )?;
            if current_revision != observed_revision {
                bail!(
                    "remote session ownership changed during mutation preparation; retry \
                     with a fresh authoritative snapshot"
                );
            }
        }

        let destination_name = destination_name.filter(|destination| *destination != session_name);
        let mut operation_names = vec![session_name];
        if let Some(destination_name) = destination_name {
            operation_names.push(destination_name);
        }
        operation_names.sort_unstable();
        for operation_name in &operation_names {
            if Some(*operation_name) != destination_name {
                continue;
            }
            if snapshot
                .sessions
                .iter()
                .any(|session| session_matches(session, operation_name))
            {
                bail!("remote session rename destination already exists");
            }
            let destination_ownership = transaction
                .query_row(
                    "SELECT binding_id, state
                     FROM workspace_session_ownership
                     WHERE namespace = ?1 AND session_name = ?2",
                    params![&self.namespace, operation_name],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            if destination_ownership.is_some() {
                bail!("remote session rename destination is already reserved");
            }
            let pending_destination: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM workspace_session_mutations
                     WHERE namespace = ?1 AND new_name = ?2
                 )",
                params![&self.namespace, operation_name],
                |row| row.get(0),
            )?;
            if pending_destination {
                bail!("remote session rename destination is already reserved");
            }
        }

        let ownership = transaction
            .query_row(
                "SELECT binding_id, state, reservation_token, reservation_expires_at
                 FROM workspace_session_ownership
                 WHERE namespace = ?1 AND session_name = ?2",
                params![&self.namespace, session_name],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((binding_id, state, token, _expires_at)) = ownership {
            if binding_id != self.binding_id {
                bail!("session already belongs to another remote Space");
            }
            if state == "reserved" {
                if token.is_none() {
                    bail!("invalid remote session ownership reservation");
                }
                transaction.commit()?;
                return Ok(SessionReservation::ExistingReserved);
            }
            if state != "active" {
                bail!("invalid remote session ownership state {state:?}");
            }
        } else {
            if !owned_names.iter().any(|name| name == session_name) {
                bail!("session does not belong to this remote Space");
            }
            transaction.execute(
                "INSERT INTO workspace_session_ownership
                 (namespace, session_name, binding_id, state)
                 VALUES (?1, ?2, ?3, 'active')",
                params![&self.namespace, session_name, self.binding_id],
            )?;
            self.bump_revision(&transaction)?;
        }

        let nonce: String =
            transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
        let token = reservation_token(&nonce)?;
        let expires_at = unix_timestamp()?
            .checked_add(SESSION_RESERVATION_LEASE_SECONDS)
            .ok_or_else(|| anyhow::anyhow!("remote session reservation lease overflow"))?;
        for operation_name in operation_names {
            let changed = if operation_name == session_name {
                transaction.execute(
                    "UPDATE workspace_session_ownership
                     SET state = 'reserved', reservation_token = ?4,
                         reservation_expires_at = ?5
                     WHERE namespace = ?1 AND session_name = ?2
                       AND binding_id = ?3 AND state = 'active'
                       AND reservation_token IS NULL",
                    params![
                        &self.namespace,
                        operation_name,
                        self.binding_id,
                        &token,
                        expires_at
                    ],
                )?
            } else {
                transaction.execute(
                    "INSERT INTO workspace_session_ownership
                     (namespace, session_name, binding_id, state,
                      reservation_token, reservation_expires_at)
                     VALUES (?1, ?2, ?3, 'reserved', ?4, ?5)",
                    params![
                        &self.namespace,
                        operation_name,
                        self.binding_id,
                        &token,
                        expires_at
                    ],
                )?
            };
            if changed != 1 {
                bail!("session ownership changed while acquiring mutation lease");
            }
            self.bump_revision(&transaction)?;
        }
        transaction.commit()?;
        Ok(SessionReservation::Acquired { token })
    }

    fn renew_token(&self, _session_name: &str, token: &str) -> Result<bool> {
        let expires_at = unix_timestamp()?
            .checked_add(SESSION_RESERVATION_LEASE_SECONDS)
            .ok_or_else(|| anyhow::anyhow!("remote session reservation lease overflow"))?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE workspace_session_ownership
             SET reservation_expires_at = ?4
             WHERE namespace = ?1 AND binding_id = ?2 AND state = 'reserved'
               AND reservation_token = ?3",
            params![&self.namespace, self.binding_id, token, expires_at],
        )?;
        if updated > 0 {
            self.bump_revision(&transaction)?;
        }
        transaction.commit()?;
        Ok(updated > 0)
    }

    fn begin_mutation(
        &self,
        operation: &str,
        session_id: &str,
        old_name: &str,
        new_name: Option<&str>,
        reservation: &SessionReservation,
    ) -> Result<PendingSessionMutation> {
        let token = reservation
            .new_token()
            .ok_or_else(|| anyhow::anyhow!("missing remote session mutation lease token"))?
            .to_owned();
        let owner = token.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
        let revision = self.current_revision()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(new_name) = new_name
            && new_name != old_name
            && let Some(binding_id) = transaction
                .query_row(
                    "SELECT binding_id
                     FROM workspace_session_ownership
                     WHERE namespace = ?1 AND session_name = ?2",
                    params![&self.namespace, new_name],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            && binding_id != self.binding_id
        {
            bail!("session already belongs to another remote Space");
        }
        transaction.execute(
            "INSERT INTO workspace_session_mutations
             (namespace, binding_id, operation, session_id, old_name, new_name,
              owner, reservation_token, revision)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &self.namespace,
                self.binding_id,
                operation,
                session_id,
                old_name,
                new_name,
                &owner,
                &token,
                revision,
            ],
        )?;
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        Ok(PendingSessionMutation {
            binding_id: self.binding_id,
            operation: operation.to_owned(),
            session_id: session_id.to_owned(),
            old_name: old_name.to_owned(),
            new_name: new_name.map(str::to_owned),
            token,
        })
    }

    fn rollback_mutation(&self, mutation: &PendingSessionMutation) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE workspace_session_ownership
             SET state = 'active', reservation_token = NULL, reservation_expires_at = NULL
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4",
            params![
                &self.namespace,
                &mutation.old_name,
                mutation.binding_id,
                &mutation.token,
            ],
        )?;
        let destination_removed = if let Some(destination_name) = mutation.new_name.as_deref() {
            transaction.execute(
                "DELETE FROM workspace_session_ownership
                 WHERE namespace = ?1 AND session_name = ?2
                   AND binding_id = ?3 AND state = 'reserved'
                   AND reservation_token = ?4",
                params![
                    &self.namespace,
                    destination_name,
                    mutation.binding_id,
                    &mutation.token,
                ],
            )?
        } else {
            0
        };
        let removed = transaction.execute(
            "DELETE FROM workspace_session_mutations
             WHERE namespace = ?1 AND reservation_token = ?2",
            params![&self.namespace, &mutation.token],
        )?;
        if updated != 1 || removed != 1 || (mutation.new_name.is_some() && destination_removed != 1)
        {
            bail!("remote session mutation token is no longer current");
        }
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        remove_reservation_lease(&self.path, &mutation.token)?;
        Ok(())
    }

    fn finalize_rename(&self, mutation: &PendingSessionMutation, new_name: &str) -> Result<()> {
        if mutation.new_name.as_deref() != Some(new_name) {
            bail!("remote session rename destination changed while finalizing");
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let destination_removed = transaction.execute(
            "DELETE FROM workspace_session_ownership
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4",
            params![
                &self.namespace,
                new_name,
                mutation.binding_id,
                &mutation.token,
            ],
        )?;
        let updated = transaction.execute(
            "UPDATE workspace_session_ownership
             SET session_name = ?5, state = 'active',
                 reservation_token = NULL, reservation_expires_at = NULL
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4",
            params![
                &self.namespace,
                &mutation.old_name,
                mutation.binding_id,
                &mutation.token,
                new_name,
            ],
        )?;
        let removed = transaction.execute(
            "DELETE FROM workspace_session_mutations
             WHERE namespace = ?1 AND reservation_token = ?2",
            params![&self.namespace, &mutation.token],
        )?;
        if updated != 1 || removed != 1 || (mutation.new_name.is_some() && destination_removed != 1)
        {
            bail!("remote session rename token is no longer current");
        }
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        remove_reservation_lease(&self.path, &mutation.token)?;
        Ok(())
    }

    fn finalize_ditch(&self, mutation: &PendingSessionMutation) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM workspace_session_ownership
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4",
            params![
                &self.namespace,
                &mutation.old_name,
                mutation.binding_id,
                &mutation.token,
            ],
        )?;
        let intent_removed = transaction.execute(
            "DELETE FROM workspace_session_mutations
             WHERE namespace = ?1 AND reservation_token = ?2",
            params![&self.namespace, &mutation.token],
        )?;
        if removed != 1 || intent_removed != 1 {
            bail!("remote session deletion token is no longer current");
        }
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        remove_reservation_lease(&self.path, &mutation.token)?;
        Ok(())
    }
    fn pending_mutations(&self) -> Result<Vec<PendingSessionMutation>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT binding_id, operation, session_id, old_name, new_name,
                    reservation_token
             FROM workspace_session_mutations
             WHERE namespace = ?1
             ORDER BY revision, reservation_token",
        )?;
        Ok(statement
            .query_map([&self.namespace], |row| {
                Ok(PendingSessionMutation {
                    binding_id: row.get(0)?,
                    operation: row.get(1)?,
                    session_id: row.get(2)?,
                    old_name: row.get(3)?,
                    new_name: row.get(4)?,
                    token: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn mutation_expired_and_dead(&self, mutation: &PendingSessionMutation) -> Result<bool> {
        let connection = self.connection()?;
        let expires_at = connection
            .query_row(
                "SELECT reservation_expires_at
                 FROM workspace_session_ownership
                 WHERE namespace = ?1 AND session_name = ?2
                   AND binding_id = ?3 AND reservation_token = ?4",
                params![
                    &self.namespace,
                    &mutation.old_name,
                    mutation.binding_id,
                    &mutation.token,
                ],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        let now = unix_timestamp()?;
        Ok(expires_at.is_some_and(|expires_at| expires_at <= now)
            && reservation_owner_is_dead(&self.path, Some(&mutation.token)))
    }

    fn recover_pending_mutations(
        &self,
        snapshot: &MuxSnapshot,
        sessions: &mut SessionOrderStore,
    ) -> Result<()> {
        for mutation in self.pending_mutations()? {
            let recoverable = self.mutation_expired_and_dead(&mutation)?;
            let current = snapshot.sessions.iter().find(|session| {
                session.id == mutation.session_id
                    || session.name == mutation.old_name
                    || mutation
                        .new_name
                        .as_deref()
                        .is_some_and(|name| session.name == name)
            });
            match mutation.operation.as_str() {
                "rename" => {
                    let Some(new_name) = mutation.new_name.as_deref() else {
                        continue;
                    };
                    if recoverable
                        && current.is_some_and(|session| {
                            session.id == mutation.session_id && session.name == new_name
                        })
                    {
                        if mutation.binding_id == self.binding_id {
                            let names = sessions.session_names();
                            if names.iter().any(|name| name == &mutation.old_name) {
                                sessions
                                    .rename_session(&mutation.old_name, new_name)
                                    .map_err(|error| {
                                        remote_rename_persistence_failure(
                                            &mutation.old_name,
                                            new_name,
                                            error,
                                            None,
                                        )
                                    })?;
                            } else if !names.iter().any(|name| name == new_name) {
                                sessions.add_session(new_name).map_err(|error| {
                                    remote_membership_persistence_failure(
                                        "session rename recovery",
                                        error,
                                    )
                                })?;
                            }
                        }
                        self.finalize_rename(&mutation, new_name)?;
                    } else if recoverable
                        && current.is_some_and(|session| {
                            session.id == mutation.session_id && session.name == mutation.old_name
                        })
                    {
                        self.rollback_mutation(&mutation)?;
                    }
                }
                "ditch" => {
                    if recoverable && current.is_none() {
                        if mutation.binding_id == self.binding_id
                            && sessions
                                .session_names()
                                .iter()
                                .any(|name| name == &mutation.old_name)
                        {
                            sessions
                                .remove_session(&mutation.old_name)
                                .map_err(|error| {
                                    remote_membership_persistence_failure(
                                        "session deletion recovery",
                                        error,
                                    )
                                })?;
                        }
                        self.finalize_ditch(&mutation)?;
                    } else if recoverable {
                        self.rollback_mutation(&mutation)?;
                    }
                }
                _ => bail!("invalid remote session mutation operation"),
            }
        }
        Ok(())
    }

    fn reclaim_expired_absent(
        &self,
        snapshot: &MuxSnapshot,
        session_name: &str,
        token: &str,
        observed_revision: Option<i64>,
    ) -> Result<bool> {
        if snapshot
            .sessions
            .iter()
            .any(|session| session_matches(session, session_name))
            || !reservation_owner_is_dead(&self.path, Some(token))
        {
            return Ok(false);
        }
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(observed_revision) = observed_revision {
            let current_revision: i64 = transaction.query_row(
                "SELECT revision
                 FROM workspace_session_ownership_meta
                 WHERE namespace = ?1",
                [&self.namespace],
                |row| row.get(0),
            )?;
            if current_revision != observed_revision {
                bail!(
                    "remote session ownership changed during expired reservation reclaim; retry \
                     with a fresh authoritative snapshot"
                );
            }
        }
        let pending = transaction.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM workspace_session_mutations
                 WHERE namespace = ?1 AND (old_name = ?2 OR new_name = ?2)
             )",
            params![&self.namespace, session_name],
            |row| row.get::<_, bool>(0),
        )?;
        if pending {
            transaction.commit()?;
            return Ok(false);
        }
        let removed = transaction.execute(
            "DELETE FROM workspace_session_ownership
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4 AND reservation_expires_at <= ?5",
            params![&self.namespace, session_name, self.binding_id, token, now],
        )?;
        if removed == 1 {
            self.bump_revision(&transaction)?;
        }
        transaction.commit()?;
        if removed == 1 {
            remove_reservation_lease(&self.path, token)?;
        }
        Ok(removed == 1)
    }

    fn legacy_membership_owners(
        &self,
        transaction: &Transaction<'_>,
        session_name: &str,
        session_id: &str,
    ) -> Result<Vec<i64>> {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT sessions.binding_id
             FROM workspace_sessions AS sessions
             JOIN workspace_session_namespaces AS namespaces
               ON namespaces.binding_id = sessions.binding_id
             WHERE namespaces.namespace = ?1
               AND sessions.name IN (?2, ?3)
             ORDER BY sessions.binding_id",
        )?;
        Ok(statement
            .query_map(params![&self.namespace, session_name, session_id], |row| {
                row.get(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn finalize(&self, session_name: &str, reservation: &SessionReservation) -> Result<()> {
        let Some(token) = reservation.new_token() else {
            return Ok(());
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = transaction.execute(
            "UPDATE workspace_session_ownership
             SET state = 'active', reservation_token = NULL, reservation_expires_at = NULL
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4",
            params![&self.namespace, session_name, self.binding_id, token],
        )?;
        if updated != 1 {
            bail!("remote session reservation token is no longer current");
        }
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        remove_reservation_lease(&self.path, token)?;
        Ok(())
    }

    fn rollback(&self, _session_name: &str, reservation: &SessionReservation) -> Result<()> {
        let Some(token) = reservation.new_token() else {
            return Ok(());
        };
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM workspace_session_ownership
             WHERE namespace = ?1 AND binding_id = ?2
               AND state = 'reserved' AND reservation_token = ?3",
            params![&self.namespace, self.binding_id, token],
        )?;
        if removed < 1 {
            bail!("remote session reservation token is no longer current");
        }
        self.bump_revision(&transaction)?;
        transaction.commit()?;
        remove_reservation_lease(&self.path, token)?;
        Ok(())
    }
    fn release_expired_reservation(&self, session_name: &str, token: &str) -> Result<()> {
        let now = unix_timestamp()?;
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let removed = transaction.execute(
            "DELETE FROM workspace_session_ownership
             WHERE namespace = ?1 AND session_name = ?2
               AND binding_id = ?3 AND state = 'reserved'
               AND reservation_token = ?4 AND reservation_expires_at <= ?5
               AND NOT EXISTS (
                   SELECT 1 FROM workspace_session_mutations
                   WHERE namespace = ?1 AND reservation_token = ?4
               )",
            params![&self.namespace, session_name, self.binding_id, token, now],
        )?;
        if removed == 1 {
            self.bump_revision(&transaction)?;
        }
        transaction.commit()?;
        Ok(())
    }

    fn live_session_names(&self, snapshot: &MuxSnapshot) -> Result<HashSet<String>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT session_name
             FROM workspace_session_ownership
             WHERE namespace = ?1 AND binding_id = ?2 AND state = 'active'",
        )?;
        let names = statement
            .query_map(params![&self.namespace, self.binding_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names
            .into_iter()
            .filter(|name| {
                snapshot
                    .sessions
                    .iter()
                    .any(|session| session_matches(session, name))
            })
            .collect())
    }
}
fn backfill_live_membership(
    ownership: &RemoteSessionOwnership,
    snapshot: &MuxSnapshot,
    sessions: &mut SessionOrderStore,
) -> Result<HashSet<String>> {
    let owned = ownership.live_session_names(snapshot)?;
    let mut membership = sessions.session_names();
    for name in &owned {
        if membership.iter().any(|existing| existing == name) {
            continue;
        }
        sessions.add_session(name).map_err(|error| {
            remote_membership_persistence_failure("session membership backfill", error)
        })?;
        membership.push(name.clone());
    }
    Ok(owned)
}

fn unix_timestamp() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock predates Unix epoch")?
        .as_secs()
        .try_into()
        .context("Unix timestamp exceeds SQLite integer range")
}

pub fn list(config: &BoottyConfig) -> Result<Vec<RemoteSpaceSummary>> {
    let workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    Ok(workspace
        .spaces()
        .iter()
        .filter_map(|space| {
            let binding = space.bindings().first()?;
            if !binding_is_local(binding, config) {
                return None;
            }
            let backend = binding
                .backend_override()
                .unwrap_or(config.multiplexer.backend);
            backend.supports_remote().then(|| RemoteSpaceSummary {
                catalog_version: REMOTE_SPACE_CATALOG_VERSION,
                id: space.remote_id().to_owned(),
                name: space.name().to_owned(),
                backend,
            })
        })
        .collect())
}

pub fn create(
    config: &BoottyConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceSummary> {
    if !backend.supports_remote() {
        bail!("remote Spaces need tmux, zellij, or rmux")
    }
    let mut workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    let space = workspace
        .create_space(
            name,
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(backend),
                remote: crate::workspace::SpaceRemoteOverride::Local,
            },
            &config.multiplexer,
        )?
        .ok_or_else(|| anyhow::anyhow!("remote Space name cannot be empty"))?;
    Ok(RemoteSpaceSummary {
        catalog_version: REMOTE_SPACE_CATALOG_VERSION,
        id: space.remote_id().to_owned(),
        name: space.name().to_owned(),
        backend,
    })
}

pub fn snapshot(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<MuxSnapshot> {
    let (mut backend, mut sessions, ownership) =
        remote_space_runtime(config, space_id, expected_backend)?;
    let (mut snapshot, owned_by_ledger, _) =
        authoritative_remote_snapshot(backend.as_mut(), &mut sessions, &ownership)?;
    filter_snapshot_for_space_with_extra(&mut snapshot, &mut sessions, &owned_by_ledger)
}
fn authoritative_remote_snapshot(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    ownership: &RemoteSessionOwnership,
) -> Result<(MuxSnapshot, HashSet<String>, i64)> {
    loop {
        let observed_revision = ownership.current_revision()?;
        let snapshot = backend.snapshot()?;
        ownership.recover_pending_mutations(&snapshot, sessions)?;
        ownership.reconcile_at_revision(&snapshot, observed_revision)?;
        let owned_by_ledger = backfill_live_membership(ownership, &snapshot, sessions)?;
        if ownership.current_revision()? != observed_revision {
            continue;
        }
        return Ok((snapshot, owned_by_ledger, observed_revision));
    }
}

#[cfg(test)]
fn filter_snapshot_for_space(
    mut snapshot: MuxSnapshot,
    sessions: &mut SessionOrderStore,
) -> Result<MuxSnapshot> {
    filter_snapshot_for_space_with_extra(&mut snapshot, sessions, &HashSet::new())
}

fn filter_snapshot_for_space_with_extra(
    snapshot: &mut MuxSnapshot,
    sessions: &mut SessionOrderStore,
    extra_owned: &HashSet<String>,
) -> Result<MuxSnapshot> {
    let alive = snapshot
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect::<Vec<_>>();
    let allowed = sessions
        .sync_sessions(alive)?
        .into_iter()
        .collect::<HashSet<_>>();
    snapshot.sessions.retain(|session| {
        extra_owned.iter().any(|id| session_matches(session, id))
            || allowed.iter().any(|id| session_matches(session, id))
    });
    snapshot.active_session_id = snapshot
        .active_session_id
        .clone()
        .filter(|id| snapshot.sessions.iter().any(|session| &session.id == id));
    Ok(snapshot.clone())
}

pub fn execute(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
    payload: &str,
) -> Result<()> {
    let command = bootty_mux::remote_space::decode_command(payload)?;
    validate_remote_session_launch(&command)?;
    let (mut backend, mut sessions, ownership) =
        remote_space_runtime(config, space_id, expected_backend)?;
    execute_with_ownership(
        backend.as_mut(),
        &mut sessions,
        &ownership,
        command,
        space_id,
    )
}

#[cfg(test)]
fn execute_with_runtime(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    command: MuxCommand,
    space_id: &str,
) -> Result<()> {
    execute_with_runtime_inner(backend, sessions, None, command, space_id)
}

fn execute_with_ownership(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    ownership: &RemoteSessionOwnership,
    command: MuxCommand,
    space_id: &str,
) -> Result<()> {
    execute_with_runtime_inner(backend, sessions, Some(ownership), command, space_id)
}

fn execute_with_runtime_inner(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    ownership: Option<&RemoteSessionOwnership>,
    command: MuxCommand,
    space_id: &str,
) -> Result<()> {
    preflight_remote_session_launch(backend, &command)?;
    if let Some(ownership) = ownership {
        let (snapshot, _, observed_revision) =
            authoritative_remote_snapshot(backend, sessions, ownership)?;
        let owned_names = sessions.session_names();
        return execute_with_ownership_inner(
            backend,
            sessions,
            ownership,
            OwnershipExecutionRequest {
                command,
                space_id,
                snapshot,
                owned_names: &owned_names,
                observed_revision: Some(observed_revision),
            },
        );
    }
    let snapshot = backend.snapshot()?;
    let owned_names = sessions.session_names();

    if let Some(session_id) = created_session_id(&command)
        && let Some(existing) = snapshot
            .sessions
            .iter()
            .find(|session| session_matches(session, session_id))
    {
        if owned_names.iter().any(|name| name == &existing.name) {
            return Ok(());
        }
        bail!("session already belongs to another remote Space");
    }
    let owned_session_name =
        resolve_owned_session_name(&snapshot, &owned_names, &command, space_id)?;
    backend.execute(command.clone())?;
    match command {
        MuxCommand::CreateSession { plan } => {
            persist_created_remote_session(backend, sessions, &plan.session_id)?;
        }
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => {
            persist_created_remote_session(backend, sessions, &session_id)?;
        }
        MuxCommand::RenameSession { session_id, name } => {
            if let Some(old_name) = owned_session_name
                && let Err(persistence_error) = sessions.rename_session(&old_name, &name)
            {
                let rollback = backend.execute(MuxCommand::RenameSession {
                    session_id,
                    name: old_name.clone(),
                });
                return Err(remote_rename_persistence_failure(
                    &old_name,
                    &name,
                    persistence_error,
                    rollback.err(),
                ));
            }
        }
        MuxCommand::DitchSession { .. } => {
            if let Some(name) = owned_session_name {
                sessions.remove_session(&name).map_err(|error| {
                    remote_membership_persistence_failure("session removal", error)
                })?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct OwnershipExecutionRequest<'a> {
    command: MuxCommand,
    space_id: &'a str,
    snapshot: MuxSnapshot,
    owned_names: &'a [String],
    observed_revision: Option<i64>,
}

fn execute_with_ownership_inner(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    ownership: &RemoteSessionOwnership,
    request: OwnershipExecutionRequest<'_>,
) -> Result<()> {
    let OwnershipExecutionRequest {
        command,
        space_id,
        snapshot,
        owned_names,
        observed_revision,
    } = request;
    if let Some(session_id) = created_session_id(&command).map(str::to_owned) {
        let mut reservation = ownership.prepare_create_with_observation(
            &snapshot,
            &session_id,
            owned_names,
            observed_revision,
        )?;
        let expired_token = match &reservation {
            SessionReservation::ExpiredReserved { token } => Some(token.clone()),
            _ => None,
        };
        if let Some(expired_token) = expired_token {
            let revalidated_snapshot = backend.snapshot()?;
            let reclaimed = ownership.reclaim_expired_absent(
                &revalidated_snapshot,
                &session_id,
                &expired_token,
                observed_revision,
            )?;
            let revalidated_revision = if reclaimed {
                Some(ownership.current_revision()?)
            } else {
                observed_revision
            };
            reservation = ownership.prepare_create_with_observation(
                &revalidated_snapshot,
                &session_id,
                owned_names,
                revalidated_revision,
            )?;
        }
        match &reservation {
            SessionReservation::ExistingActive => {
                sessions.add_session(&session_id).map_err(|error| {
                    remote_membership_persistence_failure("session membership", error)
                })?;
                return Ok(());
            }
            SessionReservation::ExistingReserved | SessionReservation::ExpiredReserved { .. } => {
                bail!("session creation is already in progress for this remote Space");
            }
            SessionReservation::Acquired { .. } => {}
        }

        let heartbeat = match ReservationHeartbeat::start(ownership, &session_id, &reservation) {
            Ok(heartbeat) => heartbeat,
            Err(error) => {
                let rollback_error = ownership.rollback(&session_id, &reservation).err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "remote session reservation heartbeat failed to start; \
                         reservation rollback also failed: {rollback_error}"
                    ),
                    None => "remote session reservation heartbeat failed to start; \
                              reservation rolled back"
                        .to_owned(),
                }));
            }
        };
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session reservation heartbeat failed before backend execution; \
                 ownership remains reserved",
            ));
        }
        if let Err(error) = backend.execute(command.clone()) {
            drop(heartbeat);
            return Err(handle_remote_create_backend_failure(
                backend,
                sessions,
                ownership,
                &reservation,
                &session_id,
                error,
            ));
        }
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session reservation heartbeat failed after backend execution; \
                 ownership remains reserved for authoritative reconciliation",
            ));
        }
        if let Err(error) = persist_created_remote_session(backend, sessions, &session_id) {
            drop(heartbeat);
            return Err(handle_remote_create_persistence_failure(
                backend,
                ownership,
                &reservation,
                &session_id,
                error,
            ));
        }
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session reservation heartbeat failed before ownership finalization; \
                 ownership remains reserved for authoritative reconciliation",
            ));
        }
        if let Err(error) = ownership.finalize(&session_id, &reservation) {
            drop(heartbeat);
            return Err(error
                .context("remote session membership persisted but ownership finalization failed"));
        }
        drop(heartbeat);
        return Ok(());
    }
    let observed_revision = observed_revision.unwrap_or(ownership.current_revision()?);
    if matches!(&command, MuxCommand::DitchSession { .. })
        && let Some(session_id) = command_session_id(&command)
        && !snapshot
            .sessions
            .iter()
            .any(|session| session_matches(session, session_id))
    {
        if owned_names.iter().any(|name| name == session_id) {
            sessions
                .remove_session(session_id)
                .map_err(|error| remote_membership_persistence_failure("session removal", error))?;
        }
        ownership.reconcile_at_revision(&snapshot, observed_revision)?;
        return Ok(());
    }

    ownership.reconcile_at_revision(&snapshot, observed_revision)?;
    let owned_session_name =
        resolve_owned_session_name(&snapshot, owned_names, &command, space_id)?;
    if let MuxCommand::DitchSession { session_id } = &command {
        let Some(name) = owned_session_name.clone() else {
            return Ok(());
        };
        let reservation = ownership.prepare_mutation_with_observation(
            &snapshot,
            &name,
            None,
            owned_names,
            Some(observed_revision),
        )?;
        if matches!(&reservation, SessionReservation::ExistingReserved) {
            bail!("session mutation is already in progress for this remote Space");
        }
        let mutation =
            match ownership.begin_mutation("ditch", session_id, &name, None, &reservation) {
                Ok(mutation) => mutation,
                Err(error) => {
                    let rollback_error = ownership.rollback(&name, &reservation).err();
                    return Err(error.context(match rollback_error {
                        Some(rollback_error) => format!(
                            "remote session deletion intent failed and lease rollback failed: \
                         {rollback_error:#}"
                        ),
                        None => {
                            "remote session deletion intent failed; lease rolled back".to_owned()
                        }
                    }));
                }
            };
        let heartbeat = match ReservationHeartbeat::start(ownership, &name, &reservation) {
            Ok(heartbeat) => heartbeat,
            Err(error) => {
                let rollback_error = ownership.rollback_mutation(&mutation).err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "remote session mutation heartbeat failed to start and rollback failed: \
                         {rollback_error:#}"
                    ),
                    None => "remote session mutation heartbeat failed to start; lease rolled back"
                        .to_owned(),
                }));
            }
        };
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session mutation heartbeat failed before backend execution; \
                 ownership remains reserved",
            ));
        }
        let backend_error = match backend.execute(command.clone()) {
            Ok(()) => None,
            Err(error) => {
                let post_snapshot = match backend.snapshot() {
                    Ok(snapshot) => snapshot,
                    Err(snapshot_error) => {
                        let backend_detail = error.to_string();
                        drop(heartbeat);
                        return Err(error.context(format!(
                            "remote session deletion failed: {backend_detail}; \
                             authoritative snapshot failed; ownership remains reserved: \
                             {snapshot_error:#}"
                        )));
                    }
                };
                if post_snapshot.sessions.iter().any(|session| {
                    session.id == session_id.as_str() || session.name == name.as_str()
                }) {
                    let backend_detail = error.to_string();
                    drop(heartbeat);
                    let rollback_error = ownership.rollback_mutation(&mutation).err();
                    return Err(error.context(match rollback_error {
                        Some(rollback_error) => format!(
                            "remote session deletion failed: {backend_detail}; mutation lease \
                             rollback failed: {rollback_error:#}"
                        ),
                        None => format!(
                            "remote session deletion failed: {backend_detail}; mutation lease \
                             rolled back"
                        ),
                    }));
                }
                Some(error)
            }
        };
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session mutation heartbeat failed after backend execution; \
                 ownership remains reserved for authoritative reconciliation",
            ));
        }
        let post_snapshot = match backend.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                drop(heartbeat);
                return Err(error.context(
                    "remote session deletion completed but authoritative snapshot failed; \
                     ownership remains reserved",
                ));
            }
        };
        if post_snapshot
            .sessions
            .iter()
            .any(|session| session.id == session_id.as_str() || session.name == name.as_str())
        {
            drop(heartbeat);
            bail!(
                "remote session deletion identity changed; ownership remains reserved for \
                 authoritative reconciliation"
            );
        }
        if let Err(error) = sessions.remove_session(&name) {
            drop(heartbeat);
            return Err(remote_membership_persistence_failure(
                "session removal",
                error,
            ));
        }
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session mutation heartbeat failed before ownership release; \
                 membership persisted and ownership remains reserved",
            ));
        }
        if let Err(error) = ownership.finalize_ditch(&mutation) {
            drop(heartbeat);
            return Err(error.context(
                "remote session deletion membership persisted but ownership intent finalization failed",
            ));
        }
        if let Some(error) = backend_error {
            let backend_detail = error.to_string();
            return Err(error.context(format!(
                "remote session deletion reported failure: {backend_detail}, but authoritative \
                 absence completed membership and ownership cleanup"
            )));
        }
        return Ok(());
    }

    if let MuxCommand::RenameSession { session_id, name } = &command {
        let Some(old_name) = owned_session_name.clone() else {
            return Ok(());
        };
        if old_name == *name {
            return Ok(());
        }
        let reservation = ownership.prepare_mutation_with_observation(
            &snapshot,
            &old_name,
            Some(name),
            owned_names,
            Some(observed_revision),
        )?;
        if matches!(&reservation, SessionReservation::ExistingReserved) {
            bail!("session mutation is already in progress for this remote Space");
        }
        let mutation = match ownership.begin_mutation(
            "rename",
            session_id,
            &old_name,
            Some(name),
            &reservation,
        ) {
            Ok(mutation) => mutation,
            Err(error) => {
                let rollback_error = ownership.rollback(&old_name, &reservation).err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "remote session rename intent failed and lease rollback failed: \
                         {rollback_error:#}"
                    ),
                    None => "remote session rename intent failed; lease rolled back".to_owned(),
                }));
            }
        };
        let heartbeat = match ReservationHeartbeat::start(ownership, &old_name, &reservation) {
            Ok(heartbeat) => heartbeat,
            Err(error) => {
                let rollback_error = ownership.rollback_mutation(&mutation).err();
                return Err(error.context(match rollback_error {
                    Some(rollback_error) => format!(
                        "remote session rename heartbeat failed to start and rollback failed: \
                         {rollback_error:#}"
                    ),
                    None => "remote session rename heartbeat failed to start; lease rolled back"
                        .to_owned(),
                }));
            }
        };
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session rename heartbeat failed before backend execution; \
                 ownership remains reserved",
            ));
        }
        let mut post_snapshot_after_error = None;
        let backend_error =
            match backend.execute(command.clone()) {
                Ok(()) => None,
                Err(error) => {
                    let post_snapshot = match backend.snapshot() {
                        Ok(snapshot) => snapshot,
                        Err(snapshot_error) => {
                            drop(heartbeat);
                            return Err(error.context(format!(
                                "remote session rename failed and authoritative snapshot failed; \
                             ownership remains reserved: {snapshot_error:#}"
                            )));
                        }
                    };
                    let old_present = post_snapshot.sessions.iter().any(|session| {
                        session.id == session_id.as_str() && session.name == old_name
                    });
                    let new_present = post_snapshot.sessions.iter().any(|session| {
                        session.id == session_id.as_str() && session.name == name.as_str()
                    });
                    if old_present && !new_present {
                        drop(heartbeat);
                        let rollback_error = ownership.rollback_mutation(&mutation).err();
                        return Err(error.context(match rollback_error {
                            Some(rollback_error) => format!(
                                "remote session rename failed and mutation rollback failed: \
                             {rollback_error:#}"
                            ),
                            None => "remote session rename failed; mutation lease rolled back"
                                .to_owned(),
                        }));
                    }
                    if !new_present {
                        drop(heartbeat);
                        return Err(error.context(
                        "remote session rename failed and authoritative identity is unresolved; \
                         ownership remains reserved for reconciliation",
                    ));
                    }
                    post_snapshot_after_error = Some(post_snapshot);
                    Some(error)
                }
            };
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session rename heartbeat failed after backend execution; \
                 ownership remains reserved for authoritative reconciliation",
            ));
        }
        let post_snapshot = match post_snapshot_after_error {
            Some(snapshot) => snapshot,
            None => match backend.snapshot() {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    drop(heartbeat);
                    return Err(error.context(
                        "remote session rename completed but authoritative snapshot failed; \
                         ownership remains reserved",
                    ));
                }
            },
        };
        let renamed = post_snapshot
            .sessions
            .iter()
            .any(|session| session.id == session_id.as_str() && session.name == name.as_str());
        if !renamed {
            drop(heartbeat);
            bail!(
                "remote session rename identity did not reach the requested name; \
                 ownership remains reserved for authoritative reconciliation"
            );
        }
        if let Err(error) = sessions.rename_session(&old_name, name) {
            drop(heartbeat);
            return Err(remote_rename_persistence_failure(
                &old_name, name, error, None,
            ));
        }
        if let Err(error) = heartbeat.renew_now() {
            drop(heartbeat);
            return Err(error.context(
                "remote session rename heartbeat failed before ownership finalization; \
                 membership persisted and ownership remains reserved",
            ));
        }
        if let Err(error) = ownership.finalize_rename(&mutation, name) {
            drop(heartbeat);
            return Err(error.context(
                "remote session rename membership persisted but ownership finalization failed",
            ));
        }
        drop(heartbeat);
        if let Some(error) = backend_error {
            return Err(error.context(
                "remote session rename reported failure, but authoritative renamed identity \
                 completed membership and ownership finalization",
            ));
        }
        return Ok(());
    }
    let Some(session_name) = owned_session_name else {
        return Ok(());
    };
    let reservation = ownership.prepare_mutation_with_observation(
        &snapshot,
        &session_name,
        None,
        owned_names,
        Some(observed_revision),
    )?;
    if matches!(&reservation, SessionReservation::ExistingReserved) {
        bail!("session mutation is already in progress for this remote Space");
    }
    let heartbeat = match ReservationHeartbeat::start(ownership, &session_name, &reservation) {
        Ok(heartbeat) => heartbeat,
        Err(error) => {
            let _ = ownership.finalize(&session_name, &reservation);
            return Err(error.context(
                "remote session operation heartbeat failed to start; lease release attempted",
            ));
        }
    };
    if let Err(error) = heartbeat.renew_now() {
        drop(heartbeat);
        return Err(error.context(
            "remote session operation heartbeat failed before backend execution; \
             ownership remains reserved",
        ));
    }
    if let Err(error) = backend.execute(command.clone()) {
        drop(heartbeat);
        let release_error = ownership.finalize(&session_name, &reservation).err();
        return Err(error.context(match release_error {
            Some(release_error) => format!(
                "remote session operation failed and lease release failed: {release_error:#}"
            ),
            None => "remote session operation failed; lease released".to_owned(),
        }));
    }
    if let Err(error) = heartbeat.renew_now() {
        drop(heartbeat);
        return Err(error.context(
            "remote session operation heartbeat failed after backend execution; \
             ownership remains reserved for authoritative reconciliation",
        ));
    }
    if let Err(error) = ownership.finalize(&session_name, &reservation) {
        drop(heartbeat);
        return Err(
            error.context("remote session operation completed but ownership finalization failed")
        );
    }
    drop(heartbeat);
    Ok(())
}
fn handle_remote_create_backend_failure(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    ownership: &RemoteSessionOwnership,
    reservation: &SessionReservation,
    session_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let post_snapshot = match backend.snapshot() {
        Ok(snapshot) => snapshot,
        Err(snapshot_error) => {
            return error.context(format!(
                "backend session create failed and authoritative post-failure snapshot failed; \
                 ownership remains reserved: {snapshot_error:#}"
            ));
        }
    };
    if post_snapshot
        .sessions
        .iter()
        .any(|session| session_matches(session, session_id))
    {
        if let Err(membership_error) = sessions.add_session(session_id) {
            return error.context(format!(
                "backend session create failed after an authoritative snapshot observed the \
                 session; membership persistence failed and ownership remains reserved: \
                 {membership_error}"
            ));
        }
        if let Err(finalize_error) = ownership.finalize(session_id, reservation) {
            return error.context(format!(
                "backend session create failed after an authoritative snapshot observed the \
                 session; ownership finalization failed and ownership remains reserved: \
                 {finalize_error:#}"
            ));
        }
        return error.context(
            "backend session create failed after an authoritative snapshot observed the session; \
             ownership was retained for reconciliation",
        );
    }
    if let Err(rollback_error) = ownership.rollback(session_id, reservation) {
        return error.context(format!(
            "backend session create failed and reservation rollback failed; ownership remains \
             reserved: {rollback_error:#}"
        ));
    }
    error
}

fn handle_remote_create_persistence_failure(
    backend: &mut dyn MuxBackend,
    ownership: &RemoteSessionOwnership,
    reservation: &SessionReservation,
    session_id: &str,
    error: anyhow::Error,
) -> anyhow::Error {
    let post_snapshot = match backend.snapshot() {
        Ok(snapshot) => snapshot,
        Err(snapshot_error) => {
            return error.context(format!(
                "authoritative post-cleanup snapshot failed; ownership remains reserved: \
                 {snapshot_error:#}"
            ));
        }
    };
    if post_snapshot
        .sessions
        .iter()
        .any(|session| session_matches(session, session_id))
    {
        return error.context(
            "remote session creation failed after membership persistence failure; the live session \
             remains reserved for authoritative reconciliation",
        );
    }
    if let Err(rollback_error) = ownership.rollback(session_id, reservation) {
        return error.context(format!(
            "remote session cleanup proved the allocation absent, but reservation rollback failed; \
             ownership remains reserved: {rollback_error:#}"
        ));
    }
    error.context(
        "remote session creation failed; exact cleanup and authoritative absence released the \
         reservation",
    )
}

fn persist_created_remote_session(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    session_id: &str,
) -> Result<()> {
    if let Err(error) = sessions.add_session(session_id) {
        return Err(remote_create_persistence_failure(backend, error));
    }
    Ok(())
}

fn remote_membership_persistence_failure(operation: &str, error: rusqlite::Error) -> anyhow::Error {
    MuxBackendOperationError::Failed(format!(
        "remote Space {operation} completed in the backend, but membership persistence failed: \
         {error}; authoritative reconciliation is required"
    ))
    .into()
}

fn remote_rename_persistence_failure(
    old_name: &str,
    new_name: &str,
    persistence_error: rusqlite::Error,
    rollback_error: Option<anyhow::Error>,
) -> anyhow::Error {
    let rollback = rollback_error.map_or_else(
        || format!("restored backend session name to {old_name:?}"),
        |error| format!("backend rename rollback also failed: {error}"),
    );
    MuxBackendOperationError::Failed(format!(
        "remote Space session rename from {old_name:?} to {new_name:?} completed in the backend, \
         but membership persistence failed: {persistence_error}; {rollback}; authoritative \
         reconciliation is required"
    ))
    .into()
}

fn remote_create_persistence_failure(
    backend: &mut dyn MuxBackend,
    persistence_error: rusqlite::Error,
) -> anyhow::Error {
    let detail = format!(
        "remote Space session creation completed in the backend, but membership persistence \
         failed: {persistence_error}"
    );
    let Some(session_id) = backend
        .take_authoritative_completion()
        .and_then(|completion| completion.allocated)
        .map(|allocated| allocated.session_id)
    else {
        return MuxBackendOperationError::Failed(format!(
            "{detail}; the backend did not report an exact newly allocated session, so cleanup \
             was unsafe; creation is reported as failed and authoritative reconciliation is \
             required"
        ))
        .into();
    };

    match backend.execute(MuxCommand::DitchSession {
        session_id: session_id.clone(),
    }) {
        Ok(()) => MuxBackendOperationError::Failed(format!(
            "{detail}; removed exact newly allocated session {session_id:?}; creation is \
             reported as failed and authoritative reconciliation is required"
        ))
        .into(),
        Err(cleanup_error) => MuxBackendOperationError::Failed(format!(
            "{detail}; cleanup of exact newly allocated session {session_id:?} also failed: \
             {cleanup_error}; creation is reported as failed and authoritative reconciliation \
             is required"
        ))
        .into(),
    }
}

/// Reject an untrusted recursive plan before backend construction, snapshot traversal, or process
/// creation. Every later backend boundary revalidates the same immutable plan before mutation.
fn validate_remote_session_launch(command: &MuxCommand) -> Result<()> {
    if let MuxCommand::CreateSession { plan } = command
        && let Err(error) = plan.validate()
    {
        bail!("invalid recursive session launch: {error}");
    }
    Ok(())
}

/// Check backend fidelity before snapshot traversal or a backend process is started. This is
/// separate from structural validation because a valid recursive plan can still be unsupported by
/// a particular backend.
fn preflight_remote_session_launch(backend: &dyn MuxBackend, command: &MuxCommand) -> Result<()> {
    let MuxCommand::CreateSession { plan } = command else {
        return Ok(());
    };
    match backend.session_launch_capability(plan) {
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
    owned_names: &[String],
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
        .or_else(|| {
            owned_names
                .iter()
                .find(|name| name.as_str() == session_id)
                .cloned()
        })
        .ok_or_else(|| anyhow::anyhow!("session is unavailable"))?;
    if !owned_names.contains(&name) {
        bail!("session does not belong to remote Space {space_id}");
    }
    Ok(Some(name))
}

fn remote_space_runtime(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<(
    Box<dyn bootty_mux::backend::MuxBackend>,
    SessionOrderStore,
    RemoteSessionOwnership,
)> {
    let workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    let space = workspace
        .spaces()
        .iter()
        .find(|space| space.remote_id() == space_id)
        .ok_or_else(|| anyhow::anyhow!("remote Space {space_id} is unavailable"))?;
    let binding = space
        .bindings()
        .first()
        .ok_or_else(|| anyhow::anyhow!("remote Space {space_id} has no backend binding"))?;
    if !binding_is_local(binding, config) {
        bail!("remote Space {space_id} points to another SSH host")
    }
    let mut multiplexer = config.multiplexer.clone();
    multiplexer.backend = binding
        .backend_override()
        .unwrap_or(config.multiplexer.backend);
    if multiplexer.backend != expected_backend {
        bail!(
            "Remote Space now uses {} instead of {}. Edit this Space and select it again.",
            backend_name(multiplexer.backend),
            backend_name(expected_backend)
        )
    }
    multiplexer.remote = None;
    multiplexer.remote_space_id = None;
    let namespace = BackendConnectionNamespace::from_multiplexer(&multiplexer);
    let namespace_key =
        serde_json::to_string(&namespace).context("serialize backend connection namespace")?;
    let backend =
        bootty_mux::config::build_backend_for_workspace(&multiplexer, Some(&config.config_path));
    let binding_id = binding.mux_scope().binding_id().persistence_value();
    let sessions = SessionOrderStore::for_binding(&config.config_path, binding_id, namespace)?;
    let ownership = RemoteSessionOwnership::new(workspace.path(), namespace_key, binding_id)?;
    Ok((backend, sessions, ownership))
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

fn backend_name(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}

fn binding_is_local(binding: &WorkspaceBinding, config: &BoottyConfig) -> bool {
    match binding.remote_override() {
        SpaceRemoteOverride::Local => true,
        SpaceRemoteOverride::Inherit => config.multiplexer.remote.is_none(),
        SpaceRemoteOverride::Profile(_) | SpaceRemoteOverride::Inline(_) => false,
    }
}

pub fn list_remote(profile: &SshProfileConfig) -> Result<Vec<RemoteSpaceSummary>> {
    list_remote_with_runner(profile, &SystemCommandRunner)
}

pub fn create_remote(
    profile: &SshProfileConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceSummary> {
    create_remote_with_runner(profile, name, backend, &SystemCommandRunner)
}

pub fn list_remote_projects_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    runner: &R,
) -> Result<Vec<ProjectPickerEntry>> {
    let output = run_remote_config(remote, &["remote-project", "list"], runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn toggle_remote_project_favorite_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    path: &str,
    runner: &R,
) -> Result<bool> {
    let output = run_remote_config(
        remote,
        &["remote-project", "favorite", "--path", path],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
}

pub fn list_remote_worktrees_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    open_cwds: &[String],
    runner: &R,
) -> Result<Vec<WorktreePickerEntry>> {
    let mut args = vec![
        "remote-worktree".to_owned(),
        "list".to_owned(),
        "--project".to_owned(),
        project.to_owned(),
    ];
    for cwd in open_cwds {
        args.extend(["--open-cwd".to_owned(), cwd.clone()]);
    }
    let output = run_remote_config_owned(remote, &args, runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn create_remote_worktree_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    branch: &str,
    runner: &R,
) -> Result<String> {
    let output = run_remote_config(
        remote,
        &[
            "remote-worktree",
            "create",
            "--project",
            project,
            "--branch",
            branch,
        ],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
}

fn list_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    runner: &R,
) -> Result<Vec<RemoteSpaceSummary>> {
    let output = run_remote(profile, &["remote-space", "list"], runner)?;
    let spaces = serde_json::from_str::<Vec<RemoteSpaceSummary>>(&output)?;
    validate_versions(&spaces)?;
    Ok(spaces)
}

fn create_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
    runner: &R,
) -> Result<RemoteSpaceSummary> {
    let backend = match backend {
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
        MultiplexerBackendConfig::Native => bail!("remote Spaces need tmux, zellij, or rmux"),
    };
    let output = run_remote(
        profile,
        &[
            "remote-space",
            "create",
            "--name",
            name,
            "--backend",
            backend,
        ],
        runner,
    )?;
    let space = serde_json::from_str::<RemoteSpaceSummary>(&output)?;
    validate_versions(std::slice::from_ref(&space))?;
    Ok(space)
}

fn run_remote<R: CommandRunner>(
    profile: &SshProfileConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    run_remote_config(&profile.to_remote(), args, runner)
}

fn run_remote_config<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    run_remote_config_owned(remote, &args, runner)
}

fn run_remote_config_owned<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[String],
    runner: &R,
) -> Result<String> {
    let remote = SshRemote::new(remote.clone());
    remote.ensure_daemon_with(runner)?;
    let host = remote.host().to_owned();
    let (program, args) = remote.proxy_command(bootty_mux::ssh::REMOTE_DAEMON_PROGRAM, args)?;
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(output.stdout);
    }
    bail!("{}", remote_daemon_failure(&host, &output.stderr))
}

fn validate_versions(spaces: &[RemoteSpaceSummary]) -> Result<()> {
    if let Some(space) = spaces
        .iter()
        .find(|space| space.catalog_version != REMOTE_SPACE_CATALOG_VERSION)
    {
        bail!(
            "remote Space catalog version {} is not supported",
            space.catalog_version
        )
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootty_mux::{
        backend::{MuxAllocatedResources, MuxBackendCommandCompletion},
        capability::BindingOperationOutcome,
        command::{MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxWindowLaunchPlan},
        process::CommandOutput,
    };
    use std::{cell::RefCell, collections::BTreeMap};

    struct FakeRunner {
        output: CommandOutput,
        command: RefCell<Option<(String, Vec<String>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.command
                .replace(Some((program.to_owned(), args.to_vec())));
            if args.last().is_some_and(|arg| arg.ends_with("remote-ping")) {
                return Ok(CommandOutput {
                    success: true,
                    stdout: format!(
                        "{}:{}",
                        bootty_mux::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
                        env!("CARGO_PKG_VERSION")
                    ),
                    stderr: String::new(),
                });
            }
            Ok(self.output.clone())
        }
    }

    fn profile() -> SshProfileConfig {
        SshProfileConfig {
            name: "Lab".to_owned(),
            host: "lab".to_owned(),
            user: None,
            port: None,
            authentication: Default::default(),
            host_key_policy: Default::default(),
            identity_file: None,
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        }
    }

    fn config(name: &str) -> BoottyConfig {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        BoottyConfig {
            config_path: dir.join(format!("{name}.toml")),
            ..BoottyConfig::default()
        }
    }

    fn launch_plan(cwd: &str) -> MuxSessionLaunchPlan {
        MuxSessionLaunchPlan {
            session_id: "remote-launch".to_owned(),
            focus: true,
            default_cwd: "/remote".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: cwd.to_owned(),
                    command: None,
                    argv: None,
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        }
    }

    struct UnsupportedLaunchBackend;

    impl MuxBackend for UnsupportedLaunchBackend {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            unreachable!("preflight must not snapshot")
        }

        fn execute(&mut self, _command: MuxCommand) -> Result<()> {
            unreachable!("preflight must not execute")
        }

        fn execute_checked(
            &mut self,
            _scope: bootty_mux::controller::MuxScope,
            _command: MuxCommand,
            _precondition: Option<&bootty_mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<Result<()>> {
            BindingOperationOutcome::Unsupported
        }
    }

    fn test_session(id: &str, name: &str) -> bootty_mux::snapshot::MuxSession {
        bootty_mux::snapshot::MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: bootty_mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn session_order(config: &BoottyConfig) -> SessionOrderStore {
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        SessionOrderStore::for_binding(
            &config.config_path,
            binding_id,
            BackendConnectionNamespace::from_multiplexer(&config.multiplexer),
        )
        .expect("open session order")
    }

    fn additional_binding(config: &BoottyConfig, name: &str) -> i64 {
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let connection = crate::workspace::open_db(workspace.path()).expect("open workspace db");
        let remote_id = format!("{name}-remote-id");
        connection
            .execute(
                "INSERT INTO workspace_spaces (remote_id, name, position) VALUES (?1, ?2, ?3)",
                params![remote_id, name, 1_i64],
            )
            .expect("insert second space");
        let space_id = connection.last_insert_rowid();
        connection
            .execute(
                "INSERT INTO workspace_bindings
                 (space_id, name, backend, hide_tmux_status)
                 VALUES (?1, ?2, ?3, 0)",
                params![space_id, name, "tmux"],
            )
            .expect("insert second binding");
        connection.last_insert_rowid()
    }

    fn ownership(config: &BoottyConfig, binding_id: i64) -> RemoteSessionOwnership {
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let namespace = BackendConnectionNamespace::new(MultiplexerBackendConfig::Tmux, None);
        RemoteSessionOwnership::new(
            workspace.path(),
            serde_json::to_string(&namespace).expect("namespace"),
            binding_id,
        )
        .expect("open session ownership")
    }

    struct FakeRemoteBackend {
        snapshot: MuxSnapshot,
        completion: Option<MuxBackendCommandCompletion>,
        commands: Vec<MuxCommand>,
        cleanup_error: Option<String>,
        execute_delay: Option<Duration>,
    }
    impl FakeRemoteBackend {
        fn with_completion(completion: Option<MuxBackendCommandCompletion>) -> Self {
            Self {
                snapshot: MuxSnapshot::default(),
                completion,
                commands: Vec::new(),
                cleanup_error: None,
                execute_delay: None,
            }
        }

        fn with_execute_delay(mut self, delay: Duration) -> Self {
            self.execute_delay = Some(delay);
            self
        }
    }

    impl MuxBackend for FakeRemoteBackend {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            Ok(self.snapshot.clone())
        }

        fn execute(&mut self, command: MuxCommand) -> Result<()> {
            if let Some(delay) = self.execute_delay.take() {
                std::thread::sleep(delay);
            }
            if matches!(&command, MuxCommand::DitchSession { .. })
                && let Some(error) = self.cleanup_error.take()
            {
                self.commands.push(command);
                anyhow::bail!("{error}");
            }
            match &command {
                MuxCommand::CreateSession { plan } => {
                    let allocated_id = self
                        .completion
                        .as_ref()
                        .and_then(|completion| completion.allocated.as_ref())
                        .map(|allocated| allocated.session_id.clone())
                        .unwrap_or_else(|| plan.session_id.clone());
                    self.snapshot
                        .sessions
                        .push(test_session(&allocated_id, &plan.session_id));
                }
                MuxCommand::CreateProjectSession { session_id, .. }
                | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                    self.snapshot
                        .sessions
                        .push(test_session(session_id, session_id));
                }
                MuxCommand::DitchSession { session_id } => {
                    self.snapshot
                        .sessions
                        .retain(|session| session.id != session_id.as_str());
                }
                MuxCommand::RenameSession { session_id, name } => {
                    if let Some(session) = self
                        .snapshot
                        .sessions
                        .iter_mut()
                        .find(|session| session.id == session_id.as_str())
                    {
                        session.name.clone_from(name);
                    }
                }
                _ => {}
            }
            self.commands.push(command);
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: bootty_mux::controller::MuxScope,
            command: MuxCommand,
            precondition: Option<&bootty_mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<Result<()>> {
            if let Some(precondition) = precondition {
                if precondition.scope != scope {
                    return BindingOperationOutcome::Supported(Err(
                        MuxBackendOperationError::stale("remote binding scope changed").into(),
                    ));
                }
                return BindingOperationOutcome::Supported(Err(
                    MuxBackendOperationError::unsupported(
                        "remote backend lacks an atomic checked mutation protocol",
                    )
                    .into(),
                ));
            }
            BindingOperationOutcome::Supported(self.execute(command))
        }

        fn session_launch_capability(
            &self,
            _plan: &MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<()> {
            BindingOperationOutcome::Supported(())
        }

        fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
            self.completion.take()
        }
    }

    #[test]
    fn remote_launch_limits_fail_before_backend_construction() {
        let error = validate_remote_session_launch(&MuxCommand::CreateSession {
            plan: launch_plan(""),
        })
        .expect_err("empty pane cwd must fail");

        assert!(error.to_string().contains("pane cwd"));
    }

    #[test]
    fn remote_launch_fidelity_fails_before_snapshot_or_execution() {
        let error = preflight_remote_session_launch(
            &UnsupportedLaunchBackend,
            &MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
        )
        .expect_err("unsupported backend must fail before work");

        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn remote_create_persistence_failure_cleans_exact_allocation_and_returns_failed() {
        let config = config("remote-create-persistence");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(Some(MuxBackendCommandCompletion {
            allocated: Some(MuxAllocatedResources {
                session_id: "$42".to_owned(),
                windows: Vec::new(),
            }),
            target: None,
        }));

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(_))
        ));
        assert!(
            error
                .to_string()
                .contains("removed exact newly allocated session \"$42\"")
        );
        assert!(backend.snapshot.sessions.is_empty());
        assert!(matches!(
            &backend.commands[..],
            [
                MuxCommand::CreateSession { .. },
                MuxCommand::DitchSession { session_id }
            ] if session_id == "$42"
        ));
        assert!(session_order(&config).session_names().is_empty());
    }

    #[test]
    fn remote_create_persistence_failure_without_allocation_reports_partial_failure() {
        let config = config("remote-create-unknown-allocation");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(None);

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateProjectSession {
                session_id: "project".to_owned(),
                cwd: "/remote/project".to_owned(),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(_))
        ));
        assert!(
            error
                .to_string()
                .contains("did not report an exact newly allocated session")
        );
        assert!(
            error
                .to_string()
                .contains("authoritative reconciliation is required")
        );
        assert_eq!(
            backend
                .snapshot
                .sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["project"]
        );
        assert!(matches!(
            &backend.commands[..],
            [MuxCommand::CreateProjectSession { .. }]
        ));
        assert!(session_order(&config).session_names().is_empty());
    }

    #[test]
    fn remote_ditch_failure_rolls_back_mutation_lease_for_retry() {
        let config = config("remote-ditch-rollback");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        let ownership = ownership(&config, binding_id);
        let mut sessions = session_order(&config);
        let mut backend = FakeRemoteBackend::with_completion(None);
        execute_with_ownership(
            &mut backend,
            &mut sessions,
            &ownership,
            MuxCommand::CreateProjectSession {
                session_id: "retry-project".to_owned(),
                cwd: "/remote/project".to_owned(),
            },
            "space-1",
        )
        .expect("create session");

        backend.cleanup_error = Some("remote cleanup failed".to_owned());
        let error = execute_with_ownership(
            &mut backend,
            &mut sessions,
            &ownership,
            MuxCommand::DitchSession {
                session_id: "retry-project".to_owned(),
            },
            "space-1",
        )
        .expect_err("failed deletion must be reported");
        assert!(error.to_string().contains("remote cleanup failed"));
        assert_eq!(sessions.session_names(), vec!["retry-project".to_owned()]);

        execute_with_ownership(
            &mut backend,
            &mut sessions,
            &ownership,
            MuxCommand::DitchSession {
                session_id: "retry-project".to_owned(),
            },
            "space-1",
        )
        .expect("retry deletion");
        assert!(sessions.session_names().is_empty());
        assert!(backend.snapshot.sessions.is_empty());
    }

    #[test]
    fn remote_create_persistence_failure_reports_cleanup_failure() {
        let config = config("remote-create-cleanup-failure");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(Some(MuxBackendCommandCompletion {
            allocated: Some(MuxAllocatedResources {
                session_id: "$42".to_owned(),
                windows: Vec::new(),
            }),
            target: None,
        }));
        backend.cleanup_error = Some("injected cleanup failure".to_owned());

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(
            error
                .to_string()
                .contains("cleanup of exact newly allocated session \"$42\" also failed")
        );
        assert!(
            backend
                .snapshot
                .sessions
                .iter()
                .any(|session| session.id == "$42")
        );
    }

    #[test]
    fn remote_rename_persistence_failure_restores_backend_name() {
        let config = config("remote-rename-persistence");
        let mut sessions = session_order(&config);
        sessions
            .add_session("before")
            .expect("persist original membership");
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(None);
        backend
            .snapshot
            .sessions
            .push(test_session("$42", "before"));

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::RenameSession {
                session_id: "$42".to_owned(),
                name: "after".to_owned(),
            },
            "space-1",
        )
        .expect_err("membership failure must fail and compensate the rename");

        assert!(error.to_string().contains("restored backend session name"));
        assert_eq!(backend.snapshot.sessions[0].name, "before");
        assert!(matches!(
            &backend.commands[..],
            [
                MuxCommand::RenameSession {
                    name: first_name,
                    ..
                },
                MuxCommand::RenameSession {
                    name: second_name,
                    ..
                }
            ] if first_name == "after" && second_name == "before"
        ));
        assert_eq!(session_order(&config).session_names(), vec!["before"]);
    }

    #[test]
    fn remote_rename_destination_lease_blocks_cross_space_create() {
        let config = config("remote-rename-destination-race");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let first_binding = workspace.binding_id().expect("default binding");
        let second_binding = additional_binding(&config, "Second");
        let first = ownership(&config, first_binding);
        let second = ownership(&config, second_binding);
        let empty = MuxSnapshot::default();
        let initial = first
            .prepare_create(&empty, "before", &[])
            .expect("reserve original session");
        first
            .finalize("before", &initial)
            .expect("persist original ownership");
        let snapshot = MuxSnapshot {
            sessions: vec![test_session("$42", "before")],
            active_session_id: Some("$42".to_owned()),
        };
        let owned_names = vec!["before".to_owned()];
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let (first_result, second_result) = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_snapshot = snapshot.clone();
            let first_owned_names = owned_names.clone();
            let first_thread = scope.spawn(move || {
                let reservation = first
                    .prepare_mutation_with_observation(
                        &first_snapshot,
                        "before",
                        Some("after"),
                        &first_owned_names,
                        None,
                    )
                    .expect("reserve rename source and destination");
                let mutation = first
                    .begin_mutation("rename", "$42", "before", Some("after"), &reservation)
                    .expect("persist rename intent");
                first_barrier.wait();
                first_barrier.wait();
                first.rollback_mutation(&mutation)
            });
            let second_barrier = barrier.clone();
            let second_thread = scope.spawn(move || {
                second_barrier.wait();
                let result = second.prepare_create(&snapshot, "after", &[]);
                second_barrier.wait();
                result
            });
            (
                first_thread.join().expect("rename lease thread"),
                second_thread.join().expect("cross-space create thread"),
            )
        });

        first_result.expect("roll back rename reservation");
        let error = second_result.expect_err("create must conflict with active rename");
        assert!(
            error.to_string().contains("remote rename"),
            "unexpected conflict: {error:#}"
        );
    }

    #[test]
    fn remote_create_for_existing_owned_session_is_idempotent() {
        let config = config("remote-create-idempotent");
        let mut sessions = session_order(&config);
        sessions
            .add_session("remote-launch")
            .expect("persist owned session");
        let mut backend = FakeRemoteBackend::with_completion(None);
        backend
            .snapshot
            .sessions
            .push(test_session("$42", "remote-launch"));

        execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect("existing owned session must be idempotent");

        assert!(backend.commands.is_empty());
    }

    #[test]
    fn concurrent_remote_session_reservations_have_one_owner() {
        let config = config("remote-ownership-race");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let first_binding = workspace.binding_id().expect("default binding");
        let second_binding = additional_binding(&config, "Second");
        let first = ownership(&config, first_binding);
        let second = ownership(&config, second_binding);
        let snapshot = MuxSnapshot::default();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first_snapshot = &snapshot;
            let first = scope.spawn(move || {
                first_barrier.wait();
                first.prepare_create(first_snapshot, "shared", &[])
            });
            let second_barrier = barrier.clone();
            let second_snapshot = &snapshot;
            let second = scope.spawn(move || {
                second_barrier.wait();
                second.prepare_create(second_snapshot, "shared", &[])
            });
            [
                first.join().expect("first reservation thread"),
                second.join().expect("second reservation thread"),
            ]
        });

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| { matches!(outcome, Ok(SessionReservation::Acquired { .. })) })
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    outcome
                        .as_ref()
                        .is_err_and(|error| error.to_string().contains("another remote Space"))
                })
                .count(),
            1
        );
    }

    #[test]
    fn slow_remote_create_keeps_reservation_owned_until_finalize() {
        let config = config("remote-ownership-slow-create");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let first_binding = workspace.binding_id().expect("default binding");
        let second_binding = additional_binding(&config, "Second");
        let first_ownership = ownership(&config, first_binding);
        let second_ownership = ownership(&config, second_binding);
        let namespace = BackendConnectionNamespace::new(MultiplexerBackendConfig::Tmux, None);
        let mut first_sessions =
            SessionOrderStore::for_binding(&config.config_path, first_binding, namespace.clone())
                .expect("first session order");
        let mut second_sessions =
            SessionOrderStore::for_binding(&config.config_path, second_binding, namespace)
                .expect("second session order");
        let observed_revision = first_ownership
            .current_revision()
            .expect("initial revision");
        let mut first_backend =
            FakeRemoteBackend::with_completion(None).with_execute_delay(Duration::from_secs(2));
        let first_command = MuxCommand::CreateProjectSession {
            session_id: "slow-shared".to_owned(),
            cwd: "/remote/project".to_owned(),
        };

        let first = std::thread::scope(|scope| {
            let first_ownership = &first_ownership;
            let first_command_for_thread = first_command.clone();
            let first = scope.spawn(move || {
                execute_with_ownership(
                    &mut first_backend,
                    &mut first_sessions,
                    first_ownership,
                    first_command_for_thread,
                    "space-1",
                )
            });
            std::thread::sleep(Duration::from_millis(1_100));
            first_ownership
                .reconcile_at_revision(&MuxSnapshot::default(), observed_revision)
                .expect("stale snapshot must not clear the live reservation");
            let mut second_backend = FakeRemoteBackend::with_completion(None);
            let conflict = execute_with_ownership(
                &mut second_backend,
                &mut second_sessions,
                &second_ownership,
                first_command,
                "space-2",
            )
            .expect_err("a competing create must not pass while the slow owner executes");
            let completed = first.join().expect("slow create thread");
            (conflict, completed)
        });

        first
            .1
            .expect("slow create must complete after heartbeat renewal");
        assert!(
            first
                .0
                .to_string()
                .contains("session creation is already in progress")
        );
    }

    #[test]
    fn expired_reservation_reclaims_after_invocation_lock_drop() {
        let config = config("remote-ownership-lock-drop");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        let ownership = ownership(&config, binding_id);
        let snapshot = MuxSnapshot::default();
        let reservation = ownership
            .prepare_create(&snapshot, "lock-drop", &[])
            .expect("reserve session");
        let heartbeat =
            ReservationHeartbeat::start(&ownership, "lock-drop", &reservation).expect("heartbeat");
        drop(heartbeat);
        let connection = crate::workspace::open_db(workspace.path()).expect("open workspace db");
        connection
            .execute(
                "UPDATE workspace_session_ownership
                 SET reservation_expires_at = 0
                 WHERE namespace = ?1 AND session_name = ?2 AND binding_id = ?3",
                params![&ownership.namespace, "lock-drop", binding_id],
            )
            .expect("expire reservation");

        assert!(matches!(
            ownership
                .prepare_create(&snapshot, "lock-drop", &[])
                .expect("reconcile expired reservation"),
            SessionReservation::ExpiredReserved { .. }
        ));
    }

    #[test]
    fn concurrent_heartbeat_retirement_allows_a_new_lease() {
        let config = config("remote-ownership-lease-barrier");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        let ownership = ownership(&config, binding_id);
        let snapshot = MuxSnapshot::default();
        let reservation = ownership
            .prepare_create(&snapshot, "lease-barrier", &[])
            .expect("reserve session");
        let heartbeat = ReservationHeartbeat::start(&ownership, "lease-barrier", &reservation)
            .expect("start heartbeat");
        let connection = crate::workspace::open_db(workspace.path()).expect("open workspace db");
        connection
            .execute(
                "UPDATE workspace_session_ownership
                 SET reservation_expires_at = 0
                 WHERE namespace = ?1 AND session_name = ?2 AND binding_id = ?3",
                params![&ownership.namespace, "lease-barrier", binding_id],
            )
            .expect("expire reservation");

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let replacement = std::thread::scope(|scope| {
            let contender_barrier = barrier.clone();
            let contender = scope.spawn(move || {
                contender_barrier.wait();
                for _ in 0..500 {
                    match ownership.prepare_create(&snapshot, "lease-barrier", &[]) {
                        Ok(reservation @ SessionReservation::Acquired { .. }) => {
                            return ReservationHeartbeat::start(
                                &ownership,
                                "lease-barrier",
                                &reservation,
                            );
                        }
                        Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
                Err(anyhow::anyhow!(
                    "replacement lease did not become available"
                ))
            });
            barrier.wait();
            drop(heartbeat);
            contender.join().expect("replacement heartbeat thread")
        });

        let replacement = replacement.expect("replacement heartbeat");
        drop(replacement);
    }

    #[test]
    fn stale_lease_identity_cannot_remove_replacement_path() {
        let config = config("remote-ownership-lease-identity");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let canonical = workspace
            .path()
            .with_extension("remote-reservation-identity.lease");
        let retired = canonical.with_extension("retired");
        std::fs::write(&canonical, b"old lease").expect("write old lease");
        let expected = lease_file_identity(&canonical)
            .expect("read old lease identity")
            .expect("old lease identity");
        std::fs::rename(&canonical, &retired).expect("retire old lease");
        std::fs::write(&canonical, b"new lease").expect("write replacement lease");

        remove_lease_path_if_identity(&canonical, expected).expect("identity-checked cleanup");

        assert!(
            canonical.exists(),
            "replacement canonical lease must survive"
        );
        assert!(retired.exists(), "stale retired lease must survive");
        std::fs::remove_file(canonical).expect("remove replacement lease");
        std::fs::remove_file(retired).expect("remove retired lease");
    }

    #[test]
    fn lease_retirement_keeps_a_concurrent_new_canonical_linked() {
        let config = config("remote-ownership-lease-link");
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        let ownership = ownership(&config, binding_id);
        let snapshot = MuxSnapshot::default();
        let reservation = ownership
            .prepare_create(&snapshot, "lease-link", &[])
            .expect("reserve session");
        let token = reservation.new_token().expect("reservation token");
        let lease_path = reservation_lease_path(&ownership.path, token);
        let retired_path = retired_reservation_lease_path(&lease_path);
        let owner_connection = rusqlite::Connection::open(&lease_path).expect("open owner lease");
        let expected = lease_file_identity(&lease_path)
            .expect("read owner lease identity")
            .expect("owner lease identity");
        owner_connection
            .busy_timeout(Duration::ZERO)
            .expect("owner busy timeout");
        owner_connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS owner_lock (id INTEGER PRIMARY KEY);
                 DELETE FROM owner_lock;
                 INSERT INTO owner_lock (id) VALUES (1);
                 BEGIN IMMEDIATE;",
            )
            .expect("hold owner lease lock");

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let contender_lease_path = lease_path.clone();
        let contender_barrier = barrier.clone();
        let contender = std::thread::scope(|scope| {
            let contender = scope.spawn(move || -> Result<rusqlite::Connection> {
                contender_barrier.wait();
                let connection = rusqlite::Connection::open(&contender_lease_path)?;
                connection.busy_timeout(Duration::ZERO)?;
                connection.execute_batch("BEGIN IMMEDIATE;")?;
                contender_barrier.wait();
                Ok(connection)
            });
            retire_reservation_lease_path(&lease_path, &retired_path, expected)
                .expect("retire canonical lease while owner lock is held");
            barrier.wait();
            barrier.wait();
            drop(owner_connection);
            std::fs::remove_file(&retired_path).expect("remove retired lease after release");
            contender.join().expect("new lease contender")
        });

        let new_owner = contender.expect("new owner lock");
        assert!(
            lease_path.exists(),
            "new canonical lease must remain linked"
        );
        drop(new_owner);
        ownership
            .rollback("lease-link", &reservation)
            .expect("clean up test reservation");
    }

    #[test]
    fn catalog_lists_the_default_space_and_creates_remote_spaces() {
        let config = config("catalog");
        assert!(list(&config).expect("list").is_empty());

        let created = create(&config, "Production", MultiplexerBackendConfig::Tmux)
            .expect("create remote Space");

        assert_eq!(created.name, "Production");
        assert_eq!(created.backend, MultiplexerBackendConfig::Tmux);
        assert!(list(&config).expect("reload").contains(&created));
    }

    #[test]
    fn catalog_rejects_native_remote_space() {
        let error =
            create(&config("native"), "Wrong", MultiplexerBackendConfig::Native).unwrap_err();
        assert!(error.to_string().contains("tmux, zellij, or rmux"));
    }

    #[test]
    fn ssh_catalog_uses_the_cross_platform_bootty_proxy_and_parses_json() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"catalog_version":3,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        let spaces = list_remote_with_runner(&profile(), &runner).expect("remote list");

        assert_eq!(spaces[0].id, "remote-7");
        let (_, args) = runner.command.into_inner().expect("command");
        let command = args.last().expect("remote command");
        assert!(command.starts_with(&format!(
            "./.bootty/bin/bootty-daemon-{}-{}.exe remote-exec ",
            bootty_mux::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(
            command
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '/'))
        );
    }

    #[test]
    fn ssh_project_catalog_returns_remote_paths_through_the_daemon_proxy() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"path":"/srv/projects/bootty","favorite":false}]"#.to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        let output =
            run_remote_config(&profile().to_remote(), &["remote-project", "list"], &runner)
                .expect("remote projects");
        let projects =
            serde_json::from_str::<Vec<ProjectPickerEntry>>(&output).expect("project JSON");

        assert_eq!(projects[0].path, "/srv/projects/bootty");
        let (_, args) = runner.command.into_inner().expect("command");
        assert!(
            args.last()
                .expect("remote command")
                .contains(" remote-exec ")
        );
    }

    #[test]
    fn ssh_catalog_rejects_unknown_versions() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"catalog_version":4,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        assert!(
            list_remote_with_runner(&profile(), &runner)
                .unwrap_err()
                .to_string()
                .contains("version 4")
        );
    }

    #[test]
    fn remote_space_snapshot_only_contains_owned_sessions() {
        let config = config("remote-snapshot");
        let mut order = session_order(&config);
        order.add_session("owned").expect("persist owned session");
        let session = |id: &str| bootty_mux::snapshot::MuxSession {
            id: id.to_owned(),
            name: id.to_owned(),
            active: id == "other",
            anchor: bootty_mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        };
        let snapshot = MuxSnapshot {
            sessions: vec![session("owned"), session("other")],
            active_session_id: Some("other".to_owned()),
        };

        let filtered = filter_snapshot_for_space(snapshot, &mut order).expect("filter snapshot");

        assert_eq!(
            filtered
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["owned"]
        );
        assert_eq!(filtered.active_session_id, None);
    }

    #[test]
    fn remote_space_command_resolves_backend_id_to_owned_name() {
        let snapshot = MuxSnapshot {
            sessions: vec![bootty_mux::snapshot::MuxSession {
                id: "$7".to_owned(),
                name: "owned".to_owned(),
                active: true,
                anchor: bootty_mux::snapshot::MuxPaneAnchor {
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
            resolve_owned_session_name(&snapshot, &["owned".to_owned()], &command, "space-3")
                .unwrap(),
            Some("owned".to_owned())
        );
    }

    #[test]
    fn remote_space_rejects_a_stale_cached_backend() {
        let config = config("backend-authority");
        let space = create(&config, "Remote", MultiplexerBackendConfig::Tmux).unwrap();

        assert_eq!(
            snapshot(&config, &space.id, MultiplexerBackendConfig::Zellij)
                .unwrap_err()
                .to_string(),
            "Remote Space now uses tmux instead of zellij. Edit this Space and select it again."
        );
    }

    #[test]
    fn catalog_excludes_spaces_that_point_to_another_ssh_host() {
        let config = config("nested-remote");
        let mut workspace = WorkspaceStore::for_config_path(&config.config_path);
        let nested = workspace
            .create_space(
                "Nested",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Inline(crate::config::SshRemoteConfig::for_host(
                        "other-host",
                    )),
                },
                &config.multiplexer,
            )
            .unwrap()
            .unwrap();

        assert!(list(&config).unwrap().is_empty());
        assert_eq!(
            snapshot(&config, nested.remote_id(), MultiplexerBackendConfig::Tmux)
                .unwrap_err()
                .to_string(),
            format!(
                "remote Space {} points to another SSH host",
                nested.remote_id()
            )
        );
    }
}
