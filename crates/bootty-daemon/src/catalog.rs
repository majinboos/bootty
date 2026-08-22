use std::{
    collections::HashSet,
    fs::File,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use bootty_identity::ApplicationIdentity;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig, RemoteSpaceSummary,
    backend::MuxBackend,
    command::MuxCommand,
    provider::MuxBackendRegistry,
    snapshot::{MuxSessionTag, MuxSnapshot, new_session_identity, session_matches},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

mod legacy_import;

pub const CATALOG_VERSION: u32 = 3;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Rmux,
    Tmux,
}

impl Backend {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "rmux" => Ok(Self::Rmux),
            "tmux" => Ok(Self::Tmux),
            _ => bail!("remote Spaces need tmux or rmux"),
        }
    }

    fn name(self) -> &'static str {
        self.identity().0
    }

    fn wire_kind(self) -> MuxBackendKind {
        self.identity().1
    }

    fn identity(self) -> (&'static str, MuxBackendKind) {
        match self {
            Self::Rmux => ("rmux", MuxBackendKind::Rmux),
            Self::Tmux => ("tmux", MuxBackendKind::Tmux),
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
    backends: Arc<MuxBackendRegistry>,
}

impl Catalog {
    pub fn open(
        path: &Path,
        identity: ApplicationIdentity,
        backends: Arc<MuxBackendRegistry>,
    ) -> Result<Self> {
        let legacy = default_legacy_catalog(identity);
        Self::open_with_legacy(path, legacy.as_ref(), backends)
    }

    fn open_with_legacy(
        path: &Path,
        legacy: Option<&LegacyCatalog>,
        backends: Arc<MuxBackendRegistry>,
    ) -> Result<Self> {
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
             DROP TABLE IF EXISTS remote_space_pending_membership_operations;",
        )?;
        let mut catalog = Self {
            connection,
            lock_directory,
            backends,
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
        let mut backend = self.backend(expected);
        self.snapshot_with_backend(space_id, expected, backend.as_mut())
    }

    /// The sessions this Space holds, which each session says for itself.
    ///
    /// The daemon reads the `@bootty_space` tag rather than keeping a catalog of names beside the
    /// multiplexer, so there is no second copy to fall out of step and nothing to journal.
    pub fn snapshot_with_backend(
        &mut self,
        space_id: &str,
        expected: Backend,
        backend: &mut dyn MuxBackend,
    ) -> Result<MuxSnapshot> {
        let backend_kind = self.space_backend(space_id, expected)?;
        let _lease = self.backend_lease(backend_kind)?;
        self.adopt_membership_recorded_by_name(space_id, backend)?;
        let mut snapshot = backend.snapshot()?;
        snapshot
            .sessions
            .retain(|session| session.tag.space.as_deref() == Some(space_id));
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
        let mut backend = self.backend(expected);
        self.execute_with_backend(space_id, expected, command, backend.as_mut())
    }

    pub fn execute_with_backend(
        &mut self,
        space_id: &str,
        expected: Backend,
        command: MuxCommand,
        backend: &mut dyn MuxBackend,
    ) -> Result<()> {
        let backend_kind = self.space_backend(space_id, expected)?;
        let _lease = self.backend_lease(backend_kind)?;
        // A command may only touch a session this Space holds. Asking the session itself is the
        // whole check, and it cannot disagree with what the client sees.
        if let Some(session_id) = command_session_id(&command) {
            let snapshot = backend.snapshot()?;
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session_matches(session, session_id))
                .with_context(|| format!("session {session_id} is unavailable"))?;
            if session.tag.space.as_deref() != Some(space_id) {
                bail!("session does not belong to remote Space {space_id}")
            }
        }
        backend.execute(command)
    }

    /// Stamp the sessions the old name-keyed table claims, then drop its rows. Runs once per
    /// Space; a name the backend no longer has goes away with the rows.
    fn adopt_membership_recorded_by_name(
        &mut self,
        space_id: &str,
        backend: &mut dyn MuxBackend,
    ) -> Result<()> {
        let recorded = {
            let mut statement = self.connection.prepare(
                "SELECT session_name FROM remote_space_sessions
                 WHERE space_id = ?1 ORDER BY position",
            )?;
            statement
                .query_map([space_id], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?
        };
        if recorded.is_empty() {
            return Ok(());
        }
        for session in backend.snapshot()?.sessions {
            if !session.tag.is_empty() || !recorded.contains(&session.name) {
                continue;
            }
            backend.execute(MuxCommand::StampSession {
                session_id: session.id.clone(),
                tag: MuxSessionTag {
                    identity: Some(new_session_identity()),
                    space: Some(space_id.to_owned()),
                },
            })?;
        }
        self.connection.execute(
            "DELETE FROM remote_space_sessions WHERE space_id = ?1",
            [space_id],
        )?;
        Ok(())
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

    fn backend(&self, backend: Backend) -> Box<dyn MuxBackend> {
        self.backends.build_backend_for_kind(
            backend.wire_kind(),
            &MuxBindingConfig {
                backend: backend.wire_kind(),
                ..MuxBindingConfig::default()
            },
            None,
        )
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
        | MuxCommand::DitchSession { session_id }
        | MuxCommand::StampSession { session_id, .. } => Some(session_id),
    }
}
