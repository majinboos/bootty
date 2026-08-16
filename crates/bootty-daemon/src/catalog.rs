use std::{
    collections::HashSet,
    fs::File,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use bootty_identity::ApplicationIdentity;
use bootty_mux::{
    MuxBackendKind, RemoteSpaceSummary,
    command::MuxCommand,
    membership::{BackendMembership, MembershipOperation},
    rmux::RmuxBackend,
    snapshot::{MuxSnapshot, session_matches},
    tmux::TmuxBackend,
    zellij::ZellijBackend,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

mod legacy_import;

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

    fn wire_kind(self) -> MuxBackendKind {
        match self {
            Self::Rmux => MuxBackendKind::Rmux,
            Self::Tmux => MuxBackendKind::Tmux,
            Self::Zellij => MuxBackendKind::Zellij,
        }
    }
}

struct LegacyCatalog {
    path: PathBuf,
    config_path: PathBuf,
}

pub struct Catalog {
    connection: Connection,
    lock_directory: PathBuf,
}

/// Backend seam used by catalog mutations and recovery.
pub trait CatalogBackend {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn execute(&mut self, command: MuxCommand) -> Result<()>;
}

impl CatalogBackend for Backend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        match self {
            Self::Rmux => RmuxBackend::new().snapshot(),
            Self::Tmux => TmuxBackend::new().snapshot(),
            Self::Zellij => ZellijBackend::new().snapshot(),
        }
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match self {
            Self::Rmux => RmuxBackend::new().execute(command),
            Self::Tmux => TmuxBackend::new().execute(command),
            Self::Zellij => ZellijBackend::new().execute(command),
        }
    }
}

impl Catalog {
    pub fn open(path: &Path, identity: ApplicationIdentity) -> Result<Self> {
        let legacy = default_legacy_catalog(identity);
        Self::open_with_legacy(path, legacy.as_ref())
    }

    fn open_with_legacy(path: &Path, legacy: Option<&LegacyCatalog>) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create daemon state directory {}", parent.display()))?;
        }
        let lock_directory = path.with_extension("locks");
        std::fs::create_dir_all(&lock_directory).with_context(|| {
            format!(
                "create daemon catalog lock directory {}",
                lock_directory.display()
            )
        })?;
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
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 PRIMARY KEY (space_id, session_name)
             );
             CREATE TABLE IF NOT EXISTS remote_space_pending_membership_operations (
                 space_id TEXT PRIMARY KEY REFERENCES remote_spaces(id) ON DELETE CASCADE,
                 operation TEXT NOT NULL CHECK (operation IN ('create', 'rename', 'ditch')),
                 session_id TEXT NOT NULL CHECK (session_id != ''),
                 old_name TEXT,
                 new_name TEXT,
                 CHECK (
                     (operation = 'create' AND old_name IS NULL AND new_name IS NOT NULL)
                     OR (operation = 'rename' AND old_name IS NOT NULL AND new_name IS NOT NULL)
                     OR (operation = 'ditch' AND old_name IS NOT NULL AND new_name IS NULL)
                 )
             );",
        )?;
        let mut catalog = Self {
            connection,
            lock_directory,
        };
        catalog.migrate_legacy(legacy)?;
        Ok(catalog)
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
        if !empty {
            return self.commit_migration(None);
        }
        let Some(legacy) = legacy else {
            return self.commit_migration(None);
        };
        match std::fs::metadata(&legacy.path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => {
                bail!(
                    "legacy remote catalog path is not a regular file: {}",
                    legacy.path.display()
                )
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(&legacy.path) {
                    Ok(_) => bail!(
                        "legacy remote catalog path is not a regular file: {}",
                        legacy.path.display()
                    ),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        return self.commit_migration(None);
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("inspect legacy remote catalog {}", legacy.path.display())
                        });
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("inspect legacy remote catalog {}", legacy.path.display())
                });
            }
        }
        let plan = legacy_import::load(&legacy.config_path, &legacy.path)?;
        self.commit_migration(Some(&plan))
    }

    fn commit_migration(&mut self, plan: Option<&legacy_import::ImportPlan>) -> Result<()> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        if empty && let Some(plan) = plan {
            for space in &plan.spaces {
                transaction.execute(
                    "INSERT INTO remote_spaces (id, name, backend, position)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![space.id, space.name, space.backend.name(), space.position],
                )?;
                for (session_position, session_name) in space.sessions.iter().enumerate() {
                    transaction.execute(
                        "INSERT INTO remote_space_sessions
                         (space_id, session_name, position) VALUES (?1, ?2, ?3)",
                        params![space.id, session_name, i64::try_from(session_position)?],
                    )?;
                }
            }
        }
        transaction.execute(
            "INSERT INTO daemon_metadata (key) VALUES ('legacy_catalog_migrated')",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<RemoteSpaceSummary>> {
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
                Ok(RemoteSpaceSummary {
                    catalog_version: CATALOG_VERSION,
                    id,
                    name,
                    backend: Backend::parse(&backend)?.wire_kind(),
                })
            })
            .collect()
    }

    pub fn create(&mut self, requested_name: &str, backend: Backend) -> Result<RemoteSpaceSummary> {
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
        Ok(RemoteSpaceSummary {
            catalog_version: CATALOG_VERSION,
            id,
            name,
            backend: backend.wire_kind(),
        })
    }

    pub fn snapshot(&mut self, space_id: &str, expected: Backend) -> Result<MuxSnapshot> {
        let mut backend = self.space_backend(space_id, expected)?;
        self.snapshot_with_backend(space_id, expected, &mut backend)
    }

    pub fn snapshot_with_backend(
        &mut self,
        space_id: &str,
        expected: Backend,
        backend: &mut dyn CatalogBackend,
    ) -> Result<MuxSnapshot> {
        let backend_kind = self.space_backend(space_id, expected)?;
        let _lease = self.backend_lease(backend_kind)?;
        let mut snapshot = backend.snapshot()?;
        let owned = self.reconcile_and_sync_sessions(space_id, &snapshot)?;
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
    ) -> Result<()> {
        let mut backend = self.space_backend(space_id, expected)?;
        self.execute_with_backend(space_id, expected, command, &mut backend)
    }

    pub fn execute_with_backend(
        &mut self,
        space_id: &str,
        expected: Backend,
        command: MuxCommand,
        backend: &mut dyn CatalogBackend,
    ) -> Result<()> {
        let backend_kind = self.space_backend(space_id, expected)?;
        let _lease = self.backend_lease(backend_kind)?;
        let snapshot = backend.snapshot()?;
        let owned = self.reconcile_and_sync_sessions(space_id, &snapshot)?;
        if let Some(session_id) = created_session_id(&command)
            && !owned.contains(session_id)
            && snapshot
                .sessions
                .iter()
                .any(|session| session_matches(session, session_id))
        {
            bail!("session already belongs to another remote Space")
        }
        let owned_name = resolve_owned_session_name(&snapshot, &owned, &command, space_id)?;
        let operation = membership_operation(&command, owned_name.as_deref())?;
        if let Some(operation) = operation.as_ref() {
            operation
                .validate()
                .map_err(|error| anyhow::anyhow!(error))?;
            self.journal(space_id, operation)?;
        }
        if let Err(error) = backend.execute(command) {
            if operation.is_some() {
                return Err(anyhow::anyhow!(
                    "remote backend result is ambiguous: {error}; remote Space membership recovery is pending"
                ));
            }
            return Err(error);
        }
        if let Some(operation) = operation {
            self.commit_membership(space_id, &operation).map_err(|error| {
                anyhow::anyhow!(
                    "remote Space membership completed but catalog commit failed: {error}; recovery is pending"
                )
            })?;
        }
        Ok(())
    }

    fn journal(&mut self, space_id: &str, operation: &MembershipOperation) -> Result<()> {
        let (operation_name, session_id, old_name, new_name) = operation_storage(operation);
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO remote_space_pending_membership_operations
             (space_id, operation, session_id, old_name, new_name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![space_id, operation_name, session_id, old_name, new_name],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn commit_membership(&mut self, space_id: &str, operation: &MembershipOperation) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let pending = load_pending_operation(&transaction, space_id)?
            .context("pending remote Space membership operation is missing")?;
        if pending != *operation {
            bail!("pending remote Space membership operation does not match completion")
        }
        apply_membership_operation(&transaction, space_id, operation)?;
        delete_pending_operation(&transaction, space_id)?;
        transaction.commit()?;
        Ok(())
    }

    fn reconcile_and_sync_sessions(
        &mut self,
        space_id: &str,
        snapshot: &MuxSnapshot,
    ) -> Result<HashSet<String>> {
        let memberships = snapshot
            .sessions
            .iter()
            .map(|session| BackendMembership {
                id: session.id.clone(),
                name: session.name.clone(),
            })
            .collect::<Vec<_>>();
        let alive = memberships
            .iter()
            .map(|session| session.name.as_str())
            .collect::<HashSet<_>>();
        let transaction = self.connection.transaction()?;
        if let Some(operation) = load_pending_operation(&transaction, space_id)? {
            if operation.effect_occurred(&memberships) {
                apply_membership_operation(&transaction, space_id, &operation)?;
            }
            delete_pending_operation(&transaction, space_id)?;
        }
        let owned = session_names(&transaction, space_id)?;
        for missing in owned.iter().filter(|name| !alive.contains(name.as_str())) {
            transaction.execute(
                "DELETE FROM remote_space_sessions WHERE space_id = ?1 AND session_name = ?2",
                params![space_id, missing],
            )?;
        }
        let owned = session_names(&transaction, space_id)?;
        transaction.commit()?;
        Ok(owned)
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

    fn backend_lease(&self, backend: Backend) -> Result<BackendLease> {
        BackendLease::acquire(&self.lock_directory, backend.name())
            .with_context(|| format!("claim remote {} catalog operation lease", backend.name()))
    }
}

struct BackendLease {
    _file: File,
}

impl BackendLease {
    fn acquire(directory: &Path, backend_name: &str) -> io::Result<Self> {
        let path = directory.join(format!("{backend_name}.lock"));
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock()?;
        Ok(Self { _file: file })
    }
}

fn session_names(transaction: &Transaction<'_>, space_id: &str) -> Result<HashSet<String>> {
    let mut statement = transaction.prepare(
        "SELECT session_name FROM remote_space_sessions WHERE space_id = ?1 ORDER BY position",
    )?;
    Ok(statement
        .query_map([space_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?)
}

fn apply_membership_operation(
    transaction: &Transaction<'_>,
    space_id: &str,
    operation: &MembershipOperation,
) -> Result<()> {
    operation
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    match operation {
        MembershipOperation::Create { session_name, .. } => {
            let position = transaction.query_row(
                "SELECT COALESCE(MAX(position) + 1, 0)
                 FROM remote_space_sessions WHERE space_id = ?1",
                [space_id],
                |row| row.get::<_, i64>(0),
            )?;
            transaction.execute(
                "INSERT OR IGNORE INTO remote_space_sessions
                 (space_id, session_name, position) VALUES (?1, ?2, ?3)",
                params![space_id, session_name, position],
            )?;
        }
        MembershipOperation::Rename {
            old_name, new_name, ..
        } => {
            let changed = transaction.execute(
                "UPDATE remote_space_sessions SET session_name = ?3
                 WHERE space_id = ?1 AND session_name = ?2",
                params![space_id, old_name, new_name],
            )?;
            if changed == 0 {
                let already_renamed = transaction.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM remote_space_sessions
                         WHERE space_id = ?1 AND session_name = ?2
                     )",
                    params![space_id, new_name],
                    |row| row.get::<_, bool>(0),
                )?;
                if !already_renamed {
                    bail!("pending rename membership is unavailable")
                }
            }
        }
        MembershipOperation::Ditch { old_name, .. } => {
            transaction.execute(
                "DELETE FROM remote_space_sessions WHERE space_id = ?1 AND session_name = ?2",
                params![space_id, old_name],
            )?;
        }
    }
    Ok(())
}

fn load_pending_operation(
    transaction: &Transaction<'_>,
    space_id: &str,
) -> Result<Option<MembershipOperation>> {
    let pending = transaction
        .query_row(
            "SELECT operation, session_id, old_name, new_name
             FROM remote_space_pending_membership_operations WHERE space_id = ?1",
            [space_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()?;
    pending
        .map(|(operation, session_id, old_name, new_name)| {
            operation_from_storage(&operation, session_id, old_name, new_name)
        })
        .transpose()
}

fn delete_pending_operation(transaction: &Transaction<'_>, space_id: &str) -> Result<()> {
    transaction.execute(
        "DELETE FROM remote_space_pending_membership_operations WHERE space_id = ?1",
        [space_id],
    )?;
    Ok(())
}

fn default_legacy_catalog(identity: ApplicationIdentity) -> Option<LegacyCatalog> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let config_path =
        bootty_identity::legacy_config_path_from_env(identity, xdg.as_deref(), home.as_deref())?;
    Some(LegacyCatalog {
        path: config_path.with_file_name("session-order.sqlite3"),
        config_path,
    })
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
        MuxCommand::CreateProjectSession { .. } | MuxCommand::CreateWorktreeSession { .. } => None,
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
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => Some(session_id),
    }
}

fn membership_operation(
    command: &MuxCommand,
    old_name: Option<&str>,
) -> Result<Option<MembershipOperation>> {
    Ok(match command {
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => {
            Some(MembershipOperation::Create {
                session_id: session_id.clone(),
                session_name: session_id.clone(),
            })
        }
        MuxCommand::RenameSession { session_id, name } => Some(MembershipOperation::Rename {
            session_id: session_id.clone(),
            old_name: old_name.context("session is unavailable")?.to_owned(),
            new_name: name.clone(),
        }),
        MuxCommand::DitchSession { session_id } => Some(MembershipOperation::Ditch {
            session_id: session_id.clone(),
            old_name: old_name.context("session is unavailable")?.to_owned(),
        }),
        _ => None,
    })
}

fn operation_storage(operation: &MembershipOperation) -> (&str, &str, Option<&str>, Option<&str>) {
    match operation {
        MembershipOperation::Create {
            session_id,
            session_name,
        } => ("create", session_id, None, Some(session_name)),
        MembershipOperation::Rename {
            session_id,
            old_name,
            new_name,
        } => ("rename", session_id, Some(old_name), Some(new_name)),
        MembershipOperation::Ditch {
            session_id,
            old_name,
        } => ("ditch", session_id, Some(old_name), None),
    }
}

fn operation_from_storage(
    operation: &str,
    session_id: String,
    old_name: Option<String>,
    new_name: Option<String>,
) -> Result<MembershipOperation> {
    let operation = match operation {
        "create" if old_name.is_none() => MembershipOperation::Create {
            session_id,
            session_name: new_name.context("pending create has no session name")?,
        },
        "rename" => MembershipOperation::Rename {
            session_id,
            old_name: old_name.context("pending rename has no old name")?,
            new_name: new_name.context("pending rename has no new name")?,
        },
        "ditch" if new_name.is_none() => MembershipOperation::Ditch {
            session_id,
            old_name: old_name.context("pending ditch has no old name")?,
        },
        _ => bail!("pending membership operation has an invalid shape"),
    };
    operation
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(operation)
}

fn created_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id),
        _ => None,
    }
}
