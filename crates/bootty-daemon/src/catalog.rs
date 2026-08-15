use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use bootty_mux::{
    command::MuxCommand,
    rmux::RmuxBackend,
    snapshot::{MuxSnapshot, session_matches},
    tmux::TmuxBackend,
    zellij::ZellijBackend,
};
use rusqlite::{Connection, OptionalExtension, params};
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

    fn execute(self, command: MuxCommand) -> Result<()> {
        match self {
            Self::Rmux => RmuxBackend::new().execute(command),
            Self::Tmux => TmuxBackend::new().execute(command),
            Self::Zellij => ZellijBackend::new().execute(command),
        }
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
                 session_name TEXT NOT NULL,
                 position INTEGER NOT NULL,
                 PRIMARY KEY (space_id, session_name)
             );",
        )?;
        let mut catalog = Self { connection };
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
                                "INSERT INTO remote_space_sessions
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
        let mut snapshot = backend.snapshot()?;
        let owned = self.sync_sessions(space_id, &snapshot)?;
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
        let backend_kind = self.space_backend(space_id, expected)?;
        let snapshot = backend_kind.snapshot()?;
        let owned = self.session_names(space_id)?;
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
        backend_kind.execute(command.clone())?;
        match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                self.add_session(space_id, &session_id)?;
            }
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

    fn session_names(&self, space_id: &str) -> Result<HashSet<String>> {
        let mut statement = self.connection.prepare(
            "SELECT session_name FROM remote_space_sessions WHERE space_id = ?1 ORDER BY position",
        )?;
        Ok(statement
            .query_map([space_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    fn sync_sessions(&mut self, space_id: &str, snapshot: &MuxSnapshot) -> Result<HashSet<String>> {
        let alive = snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<HashSet<_>>();
        let owned = self.session_names(space_id)?;
        for missing in owned.iter().filter(|name| !alive.contains(name.as_str())) {
            self.connection.execute(
                "DELETE FROM remote_space_sessions WHERE space_id = ?1 AND session_name = ?2",
                params![space_id, missing],
            )?;
        }
        Ok(owned
            .into_iter()
            .filter(|name| alive.contains(name.as_str()))
            .collect())
    }

    fn add_session(&self, space_id: &str, name: &str) -> Result<()> {
        let position = self.connection.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM remote_space_sessions WHERE space_id = ?1",
            [space_id],
            |row| row.get::<_, i64>(0),
        )?;
        self.connection.execute(
            "INSERT OR IGNORE INTO remote_space_sessions (space_id, session_name, position)
             VALUES (?1, ?2, ?3)",
            params![space_id, name, position],
        )?;
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

fn created_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id),
        _ => None,
    }
}
