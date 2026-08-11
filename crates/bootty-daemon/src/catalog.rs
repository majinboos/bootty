use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

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

pub const CATALOG_VERSION: u32 = 3;

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
             CREATE TABLE IF NOT EXISTS remote_spaces (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL UNIQUE,
                 backend TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS remote_space_sessions (
                 space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 session_name TEXT NOT NULL UNIQUE,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL DEFAULT 'active'
                     CHECK (state IN ('active', 'reserved')),
                 reservation_token TEXT,
                 CHECK (
                     (state = 'active' AND reservation_token IS NULL)
                     OR (state = 'reserved' AND reservation_token IS NOT NULL)
                 ),
                 PRIMARY KEY (space_id, session_name)
             );",
        )?;
        let mut catalog = Self { connection };
        catalog.migrate_session_ownership_schema()?;
        catalog.migrate_legacy(legacy)?;
        Ok(catalog)
    }

    /// Upgrades the original per-Space membership table into the durable ownership ledger.
    ///
    /// The old composite key allowed two Spaces to claim the same backend session. Rebuilding is
    /// necessary because `SQLite` cannot add a unique table constraint in place. If old data is
    /// already conflicted, the first Space in catalog order retains the legacy claim.
    fn migrate_session_ownership_schema(&mut self) -> Result<()> {
        let has_state = table_has_column(&self.connection, "remote_space_sessions", "state")?;
        let has_reservation_token = table_has_column(
            &self.connection,
            "remote_space_sessions",
            "reservation_token",
        )?;
        if has_state
            && has_reservation_token
            && table_has_unique_column(&self.connection, "remote_space_sessions", "session_name")?
        {
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE remote_space_sessions RENAME TO remote_space_sessions_legacy;
             CREATE TABLE remote_space_sessions (
                 space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 session_name TEXT NOT NULL UNIQUE,
                 position INTEGER NOT NULL,
                 state TEXT NOT NULL DEFAULT 'active'
                     CHECK (state IN ('active', 'reserved')),
                 reservation_token TEXT,
                 CHECK (
                     (state = 'active' AND reservation_token IS NULL)
                     OR (state = 'reserved' AND reservation_token IS NOT NULL)
                 ),
                 PRIMARY KEY (space_id, session_name)
             );",
        )?;
        transaction.execute(
            &format!(
                "INSERT OR IGNORE INTO remote_space_sessions
                     (space_id, session_name, position, state, reservation_token)
                 SELECT sessions.space_id,
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
                        END
                 FROM remote_space_sessions_legacy AS sessions
                 JOIN remote_spaces AS spaces ON spaces.id = sessions.space_id
                 ORDER BY spaces.position, spaces.id, sessions.position, sessions.rowid"
            ),
            [],
        )?;
        transaction.execute_batch("DROP TABLE remote_space_sessions_legacy;")?;
        transaction.commit()?;
        Ok(())
    }

    fn migrate_legacy(&mut self, legacy: Option<&LegacyCatalog>) -> Result<()> {
        let migrated = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM daemon_metadata WHERE key = 'legacy_catalog_migrated')",
            [],
            |row| row.get::<_, bool>(0),
        )?;
        if migrated {
            return Ok(());
        }
        let empty =
            self.connection
                .query_row("SELECT COUNT(*) = 0 FROM remote_spaces", [], |row| {
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
                let transaction = self.connection.transaction()?;
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
                            transaction.execute(
                                "INSERT OR IGNORE INTO remote_space_sessions
                                 (space_id, session_name, position) VALUES (?1, ?2, ?3)",
                                params![id, session_name, i64::try_from(session_position)?],
                            )?;
                        }
                    }
                }
                transaction.commit()?;
            }
        }
        self.connection.execute(
            "INSERT INTO daemon_metadata (key) VALUES ('legacy_catalog_migrated')",
            [],
        )?;
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
        let transaction = self.connection.transaction()?;
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

    pub fn snapshot(&mut self, space_id: &str, expected: Backend) -> Result<MuxSnapshot> {
        let backend = self.space_backend(space_id, expected)?;
        // The write transaction is the serialization boundary for the backend snapshot and its
        // ledger reconciliation. A concurrent creator cannot finalize an ownership row between
        // observing the backend and pruning a missing record.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut snapshot = backend.snapshot()?;
        Self::reconcile_backend_sessions(&transaction, backend, &snapshot)?;
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
        snapshot: Snapshot,
        execute: Execute,
    ) -> Result<Option<MuxBackendCommandCompletion>>
    where
        Preflight: FnOnce(&MuxCommand) -> Result<()>,
        Snapshot: FnOnce() -> Result<MuxSnapshot>,
        Execute: FnOnce(MuxCommand) -> Result<Option<MuxBackendCommandCompletion>>,
    {
        // The launch plan and backend capability are untrusted remote input. Do this before
        // taking a snapshot, which can start a backend process, and before every mutation.
        preflight(&command)?;
        // Keep the ownership ledger locked from immediately before the snapshot until the
        // reservation is durable. This is SQLite serialization, not a process-local mutex.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let snapshot = snapshot()?;
        let created_session = created_session_id(&command).map(str::to_owned);
        let (owned_name, reservation) = if let Some(session_id) = created_session.as_deref() {
            (
                None,
                Some(Self::prepare_create_session_in_transaction(
                    &transaction,
                    space_id,
                    backend,
                    session_id,
                    &snapshot,
                )?),
            )
        } else {
            Self::reconcile_backend_sessions(&transaction, backend, &snapshot)?;
            let owned = Self::session_names_in(&transaction, space_id)?;
            (
                resolve_owned_session_name(&snapshot, &owned, &command, space_id)?,
                None,
            )
        };
        transaction.commit()?;
        if matches!(&reservation, Some(SessionReservation::ExistingActive)) {
            // A completed same-Space request is idempotent. Do not ask the backend to create a
            // duplicate session merely to repeat the request.
            return Ok(None);
        }

        let completion = match execute(command.clone()) {
            Ok(completion) => completion,
            Err(error) => {
                if let Some(session_id) = created_session.as_deref()
                    && let Some(token) =
                        reservation.as_ref().and_then(SessionReservation::new_token)
                    && let Err(rollback_error) =
                        self.rollback_session_reservation(space_id, session_id, token)
                {
                    return Err(error.context(format!(
                        "backend session create failed and catalog reservation rollback failed; \
                         ownership remains reserved: {rollback_error:#}"
                    )));
                }
                return Err(error);
            }
        };

        if let (Some(session_id), Some(reservation)) =
            (created_session.as_deref(), reservation.as_ref())
        {
            self.finalize_session_reservation(space_id, session_id, reservation)
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
                        self.rename_session(space_id, &old_name, &name)?;
                    }
                }
                MuxCommand::DitchSession { .. } => {
                    if let Some(name) = owned_name {
                        self.remove_session(space_id, &name)?;
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let reservation = Self::prepare_create_session_in_transaction(
            &transaction,
            space_id,
            backend,
            session_id,
            snapshot,
        )?;
        transaction.commit()?;
        Ok(reservation)
    }

    fn prepare_create_session_in_transaction(
        transaction: &Transaction<'_>,
        space_id: &str,
        backend: Backend,
        session_id: &str,
        snapshot: &MuxSnapshot,
    ) -> Result<SessionReservation> {
        Self::reconcile_backend_sessions(transaction, backend, snapshot)?;
        let owned = Self::session_names_in(transaction, space_id)?;
        if snapshot
            .sessions
            .iter()
            .any(|session| session_matches(session, session_id) && !owned.contains(&session.name))
        {
            bail!("session already belongs to another remote Space")
        }
        Self::reserve_session_ownership(transaction, space_id, session_id)
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

    /// Reconciles every Space that addresses the same backend. A session name is globally owned,
    /// but only an authoritative snapshot from its matching backend may expire an active claim.
    /// Pending reservations survive a negative snapshot and become active only once observed.
    fn reconcile_backend_sessions(
        transaction: &Transaction<'_>,
        backend: Backend,
        snapshot: &MuxSnapshot,
    ) -> Result<()> {
        let alive = snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<HashSet<_>>();
        let sessions = {
            let mut statement = transaction.prepare(
                "SELECT sessions.space_id, sessions.session_name, sessions.state
                 FROM remote_space_sessions AS sessions
                 JOIN remote_spaces AS spaces ON spaces.id = sessions.space_id
                 WHERE spaces.backend = ?1
                 ORDER BY spaces.position, spaces.id, sessions.position",
            )?;
            statement
                .query_map([backend.name()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (space_id, name, state) in sessions {
            match state.as_str() {
                "active" if !alive.contains(name.as_str()) => {
                    transaction.execute(
                        "DELETE FROM remote_space_sessions
                         WHERE space_id = ?1 AND session_name = ?2 AND state = 'active'",
                        params![space_id, name],
                    )?;
                }
                "reserved" if alive.contains(name.as_str()) => {
                    transaction.execute(
                        "UPDATE remote_space_sessions
                         SET state = 'active', reservation_token = NULL
                         WHERE space_id = ?1 AND session_name = ?2 AND state = 'reserved'",
                        params![space_id, name],
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
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::reconcile_backend_sessions(&transaction, backend, snapshot)?;
        let owned = Self::session_names_in(&transaction, space_id)?;
        transaction.commit()?;
        Ok(owned)
    }
    fn reserve_session_ownership(
        transaction: &Transaction<'_>,
        space_id: &str,
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
        let inserted = transaction.execute(
            "INSERT INTO remote_space_sessions
             (space_id, session_name, position, state, reservation_token)
             VALUES (?1, ?2, ?3, 'reserved', ?4)
             ON CONFLICT(session_name) DO NOTHING",
            params![space_id, session_id, position, &token],
        )?;
        if inserted == 1 {
            return Ok(SessionReservation::Acquired { token });
        }

        let (owner, state, token) = transaction
            .query_row(
                "SELECT space_id, state, reservation_token
                 FROM remote_space_sessions WHERE session_name = ?1",
                [session_id],
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

    fn finalize_session_reservation(
        &mut self,
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
        let updated = transaction.execute(
            "UPDATE remote_space_sessions
             SET state = 'active', reservation_token = NULL
             WHERE space_id = ?1 AND session_name = ?2
               AND state = 'reserved' AND reservation_token = ?3",
            params![space_id, session_id, token],
        )?;
        if updated == 0 {
            let existing = transaction
                .query_row(
                    "SELECT space_id, state, reservation_token
                     FROM remote_space_sessions WHERE session_name = ?1",
                    [session_id],
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

    fn rollback_session_reservation(
        &mut self,
        space_id: &str,
        session_id: &str,
        token: &str,
    ) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "DELETE FROM remote_space_sessions
             WHERE space_id = ?1 AND session_name = ?2
               AND state = 'reserved' AND reservation_token = ?3",
            params![space_id, session_id, token],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn rename_session(&self, space_id: &str, old_name: &str, name: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE remote_space_sessions SET session_name = ?3
             WHERE space_id = ?1 AND session_name = ?2",
            params![space_id, old_name, name],
        )?;
        Ok(())
    }

    fn remove_session(&self, space_id: &str, name: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM remote_space_sessions WHERE space_id = ?1 AND session_name = ?2",
            params![space_id, name],
        )?;
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

fn table_has_unique_column(
    connection: &Connection,
    table: &str,
    column: &str,
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
        if columns.len() == 1 && columns[0] == column {
            return Ok(true);
        }
    }
    Ok(false)
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
    use std::{cell::Cell, collections::BTreeMap};

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
    fn legacy_daemon_membership_schema_becomes_a_global_ownership_ledger() {
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
                     ('first', 'first-only', 1),
                     ('second', 'shared', 0),
                     ('second', 'second-only', 1);",
            )
            .expect("legacy schema");
        drop(legacy);

        let catalog = Catalog::open_with_legacy(&path, None).expect("migrate catalog");

        assert!(
            table_has_unique_column(&catalog.connection, "remote_space_sessions", "session_name")
                .expect("unique ownership")
        );
        assert_eq!(
            catalog.session_names("first").expect("first sessions"),
            HashSet::from(["shared".to_owned(), "first-only".to_owned()])
        );
        assert_eq!(
            catalog.session_names("second").expect("second sessions"),
            HashSet::from(["second-only".to_owned()])
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
                "SELECT state FROM remote_space_sessions WHERE session_name = 'shared'",
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
    fn stale_same_space_ownership_is_pruned_before_create_authorization() {
        let (_dir, mut catalog) = catalog();
        let space = catalog.create("Lab", Backend::Tmux).expect("space");
        let reservation = catalog
            .prepare_create_session(&space.id, Backend::Tmux, "stale", &empty_snapshot())
            .expect("reservation");
        catalog
            .finalize_session_reservation(&space.id, "stale", &reservation)
            .expect("simulate completed backend create");

        assert!(matches!(
            catalog
                .prepare_create_session(&space.id, Backend::Tmux, "stale", &empty_snapshot())
                .expect("recreate after authoritative absence"),
            SessionReservation::Acquired { .. }
        ));
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
            .finalize_session_reservation(&first_space.id, "reclaimable", &reservation)
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
    fn snapshot_and_reservation_use_one_sqlite_serialization_boundary() {
        let (_dir, mut first, mut second, first_space, second_space) = two_catalogs();
        second
            .connection
            .busy_timeout(std::time::Duration::ZERO)
            .expect("disable retry while lock is held");
        let blocked = Cell::new(false);

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
                    blocked.set(
                        second
                            .prepare_create_session(
                                &second_space.id,
                                Backend::Tmux,
                                "second-create",
                                &empty_snapshot(),
                            )
                            .is_err(),
                    );
                    Ok(empty_snapshot())
                },
                |_| Err(anyhow::anyhow!("simulated backend failure")),
            )
            .expect_err("backend failure");

        assert!(error.to_string().contains("simulated backend failure"));
        assert!(blocked.get());
        assert!(matches!(
            second
                .prepare_create_session(
                    &second_space.id,
                    Backend::Tmux,
                    "second-create",
                    &empty_snapshot(),
                )
                .expect("reservation after snapshot transaction commits"),
            SessionReservation::Acquired { .. }
        ));
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
