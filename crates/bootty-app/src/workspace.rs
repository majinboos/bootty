use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

pub use crate::{
    session_names::{SessionNameRecord, SessionNameStore},
    session_order::SessionOrderStore,
};

use crate::{
    config::{MultiplexerBackendConfig, MultiplexerConfig, SshRemoteConfig, default_config_path},
    mux::controller::{BindingId, MuxScope, SpaceId},
};

const WORKSPACE_SNAPSHOT_REVISION: i64 = 1;
const DEFAULT_SPACE_NAME: &str = "Default Space";
pub const DEFAULT_SPACE_ICON: &str = "folder";
pub const DEFAULT_SPACE_COLOR: [u8; 3] = [0x7A, 0xA2, 0xF7];
const DEFAULT_TINT_SIDEBAR: bool = false;
const DEFAULT_BINDING_NAME: &str = "Default Binding";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceMuxOverride {
    pub backend: Option<MultiplexerBackendConfig>,
    pub remote: SpaceRemoteOverride,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteSpaceRef {
    pub profile_id: String,
    pub remote_space_id: String,
    pub remote_space_name: String,
    pub backend: MultiplexerBackendConfig,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", content = "value", rename_all = "kebab-case")]
pub enum SpaceRemoteOverride {
    #[default]
    Inherit,
    Local,
    Profile(RemoteSpaceRef),
    Inline(SshRemoteConfig),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBinding {
    scope: MuxScope,
    name: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    unavailable: bool,
    selection: Option<WorkspaceBindingSelection>,
}

impl WorkspaceBinding {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn backend_override(&self) -> Option<MultiplexerBackendConfig> {
        self.backend_override
    }

    pub fn remote_override(&self) -> &SpaceRemoteOverride {
        &self.remote_override
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.scope
    }

    pub fn unavailable(&self) -> bool {
        self.unavailable
    }

    pub fn selection(&self) -> Option<&WorkspaceBindingSelection> {
        self.selection.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceBindingSelection {
    session_id: String,
    window_id: Option<String>,
}

impl WorkspaceBindingSelection {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn window_id(&self) -> Option<&str> {
        self.window_id.as_deref()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSpace {
    id: SpaceId,
    remote_id: String,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    position: i64,
    bindings: Vec<WorkspaceBinding>,
}

impl WorkspaceSpace {
    pub fn id(&self) -> SpaceId {
        self.id
    }

    pub fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn icon(&self) -> &str {
        &self.icon
    }

    pub fn color(&self) -> [u8; 3] {
        self.color
    }

    pub fn tint_sidebar(&self) -> bool {
        self.tint_sidebar
    }

    pub fn position(&self) -> i64 {
        self.position
    }

    pub fn bindings(&self) -> &[WorkspaceBinding] {
        &self.bindings
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceRepository {
    config_path: PathBuf,
    path: PathBuf,
    spaces: Vec<WorkspaceSpace>,
}

impl WorkspaceRepository {
    pub fn open(config_path: &Path) -> rusqlite::Result<Self> {
        let path = sqlite_path(config_path);
        let spaces = Self::load_or_migrate(&path)?;
        Ok(Self {
            config_path: config_path.to_path_buf(),
            path,
            spaces,
        })
    }

    pub(crate) fn for_config_path(config_path: &Path) -> Self {
        Self::open(config_path).unwrap_or_else(|_| Self {
            config_path: config_path.to_path_buf(),
            path: sqlite_path(config_path),
            spaces: Vec::new(),
        })
    }

    pub(crate) fn binding(&self) -> Option<&WorkspaceBinding> {
        self.spaces.first()?.bindings.first()
    }

    pub fn spaces(&self) -> &[WorkspaceSpace] {
        &self.spaces
    }

    pub fn default_binding_id(&self) -> Option<BindingId> {
        self.binding().map(|binding| binding.scope.binding_id())
    }

    pub fn session_order(&self, binding_id: BindingId) -> SessionOrderStore {
        SessionOrderStore::for_binding(&self.config_path, binding_id.persistence_value())
    }

    pub fn session_names(&self, binding_id: BindingId) -> SessionNameStore {
        SessionNameStore::for_binding(&self.config_path, binding_id.persistence_value())
    }

    pub fn create_space(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
        config: &MultiplexerConfig,
    ) -> rusqlite::Result<Option<WorkspaceSpace>> {
        let name = name.trim();
        let Some(icon) = nonempty_trimmed(icon) else {
            return Ok(None);
        };
        if name.is_empty() {
            return Ok(None);
        }
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction()?;
        let mut names = tx.prepare("SELECT name FROM workspace_spaces")?;
        let existing_names = names
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(names);
        let name = Self::unique_space_name(existing_names.iter().map(String::as_str), name);
        let remote_id = new_remote_space_id(&tx)?;
        let position = tx.query_row(
            "SELECT COALESCE(MAX(position) + 1, 0) FROM workspace_spaces",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO workspace_spaces (remote_id, name, icon, color, tint_sidebar, position)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                remote_id,
                name,
                icon,
                color_to_hex(color),
                i64::from(tint_sidebar),
                position
            ],
        )?;
        let space_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status, remote)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                space_id,
                DEFAULT_BINDING_NAME,
                backend_to_storage(mux.backend),
                i64::from(config.hide_tmux_status),
                remote_to_storage(&mux.remote),
            ],
        )?;
        let binding_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )?;
        tx.commit()?;

        let space = WorkspaceSpace {
            id: SpaceId::from_persistence(space_id),
            remote_id,
            name,
            icon,
            color,
            tint_sidebar,
            position,
            bindings: vec![WorkspaceBinding {
                scope: MuxScope::new(
                    SpaceId::from_persistence(space_id),
                    BindingId::from_persistence(binding_id),
                ),
                name: DEFAULT_BINDING_NAME.to_owned(),
                backend_override: mux.backend,
                remote_override: mux.remote,
                unavailable: false,
                selection: None,
            }],
        };
        self.spaces.push(space.clone());
        self.spaces.sort_by_key(WorkspaceSpace::position);
        Ok(Some(space))
    }

    pub(crate) fn update_space(
        &mut self,
        id: SpaceId,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> rusqlite::Result<bool> {
        let Some(name) = nonempty_trimmed(name) else {
            return Ok(false);
        };
        let Some(icon) = nonempty_trimmed(icon) else {
            return Ok(false);
        };
        let conn = open_db(&self.path)?;
        if conn.execute(
            "UPDATE workspace_spaces
             SET name = ?1, icon = ?2, color = ?3, tint_sidebar = ?4
             WHERE id = ?5",
            params![
                name,
                icon,
                color_to_hex(color),
                i64::from(tint_sidebar),
                id.persistence_value()
            ],
        )? == 0
        {
            return Ok(false);
        }
        conn.execute(
            "UPDATE workspace_bindings
             SET backend = ?1, remote = ?2
             WHERE id = (
                 SELECT id FROM workspace_bindings
                 WHERE space_id = ?3
                 ORDER BY id
                 LIMIT 1
             )",
            params![
                backend_to_storage(mux.backend),
                remote_to_storage(&mux.remote),
                id.persistence_value()
            ],
        )?;
        if let Some(space) = self.spaces.iter_mut().find(|space| space.id == id) {
            space.name = name;
            space.icon = icon;
            space.color = color;
            space.tint_sidebar = tint_sidebar;
            if let Some(binding) = space.bindings.first_mut() {
                binding.backend_override = mux.backend;
                binding.remote_override = mux.remote;
            }
        }
        Ok(true)
    }

    pub(crate) fn delete_space(&mut self, id: SpaceId) -> rusqlite::Result<bool> {
        if self.spaces.len() <= 1 {
            return Ok(false);
        }
        let conn = open_db(&self.path)?;
        if conn.execute(
            "DELETE FROM workspace_spaces WHERE id = ?1",
            [id.persistence_value()],
        )? == 0
        {
            return Ok(false);
        }
        self.spaces.retain(|space| space.id != id);
        Ok(true)
    }

    pub(crate) fn selected_space(&self, window_key: &str) -> rusqlite::Result<Option<SpaceId>> {
        let conn = open_db(&self.path)?;
        conn.query_row(
            "SELECT selected_space_id FROM workspace_window_state WHERE window_key = ?1",
            [window_key],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map(|value| value.map(SpaceId::from_persistence))
    }

    pub(crate) fn set_selected_space(
        &self,
        window_key: &str,
        space_id: SpaceId,
    ) -> rusqlite::Result<()> {
        let conn = open_db(&self.path)?;
        conn.execute(
            "INSERT INTO workspace_window_state (window_key, selected_space_id)
             VALUES (?1, ?2)
             ON CONFLICT(window_key) DO UPDATE SET selected_space_id = excluded.selected_space_id",
            params![window_key, space_id.persistence_value()],
        )?;
        Ok(())
    }

    pub(crate) fn set_binding_restore_state(
        &mut self,
        scope: MuxScope,
        unavailable: bool,
        session_id: Option<&str>,
        window_id: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let conn = open_db(&self.path)?;
        let changed = conn.execute(
            "UPDATE workspace_bindings
             SET unavailable = ?1, selected_session_id = ?2, selected_window_id = ?3
             WHERE id = ?4 AND space_id = ?5",
            params![
                i64::from(unavailable),
                session_id,
                window_id,
                scope.binding_id().persistence_value(),
                scope.space_id().persistence_value(),
            ],
        )? != 0;
        if changed
            && let Some(binding) = self
                .spaces
                .iter_mut()
                .find(|space| space.id == scope.space_id())
                .and_then(|space| {
                    space
                        .bindings
                        .iter_mut()
                        .find(|binding| binding.scope == scope)
                })
        {
            binding.unavailable = unavailable;
            binding.selection = session_id.map(|session_id| WorkspaceBindingSelection {
                session_id: session_id.to_owned(),
                window_id: window_id.map(str::to_owned),
            });
        }
        Ok(changed)
    }

    fn unique_space_name<'a>(
        existing: impl IntoIterator<Item = &'a str>,
        requested: &str,
    ) -> String {
        let existing = existing
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect::<HashSet<_>>();
        if !existing.contains(&requested.to_ascii_lowercase()) {
            return requested.to_owned();
        }
        for suffix in 2.. {
            let candidate = format!("{requested} {suffix}");
            if !existing.contains(&candidate.to_ascii_lowercase()) {
                return candidate;
            }
        }
        unreachable!("unbounded integer suffixes always produce a unique space name")
    }

    fn load_or_migrate(path: &Path) -> rusqlite::Result<Vec<WorkspaceSpace>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let mut conn = open_db(path)?;
        let revision: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if revision > WORKSPACE_SNAPSHOT_REVISION {
            return Err(rusqlite::Error::InvalidQuery);
        }
        migrate_workspace_binding_cardinality(&conn)?;
        let tx = conn.transaction()?;
        create_workspace_schema(&tx)?;
        migrate_workspace_space_icons(&tx)?;
        migrate_workspace_remote_ids(&tx)?;
        migrate_workspace_space_appearance(&tx)?;
        migrate_workspace_session_name_metadata(&tx)?;
        migrate_workspace_snapshot_state(&tx)?;
        let space_count = tx.query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
            row.get::<_, i64>(0)
        })?;
        if space_count == 0 {
            create_default_binding(&tx, path)?;
        } else {
            create_missing_space_bindings(&tx)?;
        }
        let spaces = load_spaces(&tx)?;
        tx.pragma_update(None, "user_version", WORKSPACE_SNAPSHOT_REVISION)?;
        tx.commit()?;
        Ok(spaces)
    }
}

pub(crate) fn sqlite_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("session-order.sqlite3")
}

pub(crate) fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(250))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

fn migrate_workspace_binding_cardinality(conn: &Connection) -> rusqlite::Result<()> {
    let table_sql = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'workspace_bindings'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let had_single_binding_constraint = table_sql.is_some_and(|sql| {
        sql.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_lowercase()
            .contains("space_id integer not null unique")
    });
    if !had_single_binding_constraint {
        return Ok(());
    }

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE workspace_bindings_multiple (
             id INTEGER PRIMARY KEY,
             space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
             name TEXT NOT NULL,
             backend TEXT NOT NULL,
             hide_tmux_status INTEGER NOT NULL
         );
         INSERT INTO workspace_bindings_multiple
             (id, space_id, name, backend, hide_tmux_status)
         SELECT id, space_id, name, backend, hide_tmux_status
         FROM workspace_bindings;
         DROP TABLE workspace_bindings;
         ALTER TABLE workspace_bindings_multiple RENAME TO workspace_bindings;
         COMMIT;",
    );
    if migration.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
    }
    let foreign_keys = conn.pragma_update(None, "foreign_keys", "ON");
    migration?;
    foreign_keys
}

fn create_workspace_schema(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_spaces (
            id INTEGER PRIMARY KEY,
            remote_id TEXT UNIQUE,
            name TEXT NOT NULL,
            icon TEXT NOT NULL DEFAULT 'folder',
            color TEXT NOT NULL DEFAULT '#7AA2F7',
            tint_sidebar INTEGER NOT NULL DEFAULT 0,
            position INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS workspace_bindings (
            id INTEGER PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            backend TEXT NOT NULL,
            hide_tmux_status INTEGER NOT NULL,
            remote TEXT,
            unavailable INTEGER NOT NULL DEFAULT 0,
            selected_session_id TEXT,
            selected_window_id TEXT
        );
        CREATE TABLE IF NOT EXISTS workspace_session_groups (
            id INTEGER PRIMARY KEY,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            position INTEGER NOT NULL,
            UNIQUE(binding_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_sessions (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            group_id INTEGER NOT NULL REFERENCES workspace_session_groups(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            PRIMARY KEY(binding_id, name),
            UNIQUE(binding_id, group_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_session_name_metadata (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            cwd TEXT NOT NULL,
            generated_name TEXT NOT NULL,
            session_name TEXT NOT NULL DEFAULT '',
            display_name TEXT NOT NULL DEFAULT '',
            explicit INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(binding_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS workspace_window_state (
            window_key TEXT PRIMARY KEY,
            selected_space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE
        );",
    )
}

fn new_remote_space_id(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row(
        "SELECT lower(hex(randomblob(4))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(2))) || '-' ||
                lower(hex(randomblob(6)))",
        [],
        |row| row.get(0),
    )
}

fn migrate_workspace_remote_ids(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let has_remote_id = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "remote_id");
    drop(statement);
    if !has_remote_id {
        tx.execute("ALTER TABLE workspace_spaces ADD COLUMN remote_id TEXT", [])?;
    }
    tx.execute(
        "UPDATE workspace_spaces
         SET remote_id = lower(hex(randomblob(4))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(2))) || '-' ||
                         lower(hex(randomblob(6)))
         WHERE remote_id IS NULL OR remote_id = ''",
        [],
    )?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS workspace_spaces_remote_id
         ON workspace_spaces(remote_id)",
        [],
    )?;
    Ok(())
}

fn migrate_workspace_space_icons(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let has_icon = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "icon");
    drop(statement);
    if !has_icon {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN icon TEXT NOT NULL DEFAULT 'folder'",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_space_appearance(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_spaces)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("color") {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN color TEXT NOT NULL DEFAULT '#7AA2F7'",
            [],
        )?;
    }
    if !columns.contains("tint_sidebar") {
        tx.execute(
            "ALTER TABLE workspace_spaces ADD COLUMN tint_sidebar INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_session_name_metadata(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_session_name_metadata)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("session_name") {
        tx.execute(
            "ALTER TABLE workspace_session_name_metadata
             ADD COLUMN session_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    if !columns.contains("display_name") {
        tx.execute(
            "ALTER TABLE workspace_session_name_metadata
             ADD COLUMN display_name TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }
    Ok(())
}

fn migrate_workspace_snapshot_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut statement = tx.prepare("PRAGMA table_info(workspace_bindings)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    drop(statement);
    if !columns.contains("unavailable") {
        tx.execute(
            "ALTER TABLE workspace_bindings
             ADD COLUMN unavailable INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains("selected_session_id") {
        tx.execute(
            "ALTER TABLE workspace_bindings ADD COLUMN selected_session_id TEXT",
            [],
        )?;
    }
    if !columns.contains("selected_window_id") {
        tx.execute(
            "ALTER TABLE workspace_bindings ADD COLUMN selected_window_id TEXT",
            [],
        )?;
    }
    if !columns.contains("remote") {
        tx.execute("ALTER TABLE workspace_bindings ADD COLUMN remote TEXT", [])?;
    }
    Ok(())
}

fn create_missing_space_bindings(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
         SELECT s.id, ?1, ?2, 0
         FROM workspace_spaces s
         WHERE NOT EXISTS (
             SELECT 1 FROM workspace_bindings b WHERE b.space_id = s.id
         )",
        params![DEFAULT_BINDING_NAME, backend_to_storage(None)],
    )?;
    Ok(())
}

fn load_spaces(tx: &Transaction<'_>) -> rusqlite::Result<Vec<WorkspaceSpace>> {
    let mut statement = tx.prepare(
        "SELECT s.id, s.remote_id, s.name, s.icon, s.color, s.tint_sidebar, s.position,
                b.id, b.name, b.backend, b.hide_tmux_status, b.unavailable,
                b.selected_session_id, b.selected_window_id, b.remote
         FROM workspace_spaces s
         JOIN workspace_bindings b ON b.space_id = s.id
         ORDER BY s.position, s.id, b.id",
    )?;
    let rows = statement.query_map([], |row| {
        let space_id = row.get::<_, i64>(0)?;
        Ok((
            space_id,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            color_from_hex(&row.get::<_, String>(4)?).unwrap_or(DEFAULT_SPACE_COLOR),
            row.get::<_, i64>(5)? != 0,
            row.get::<_, i64>(6)?,
            WorkspaceBinding {
                scope: MuxScope::new(
                    SpaceId::from_persistence(space_id),
                    BindingId::from_persistence(row.get(7)?),
                ),
                name: row.get(8)?,
                backend_override: backend_from_storage(&row.get::<_, String>(9)?),
                remote_override: remote_from_storage(row.get::<_, Option<String>>(14)?.as_deref()),
                unavailable: row.get::<_, i64>(11)? != 0,
                selection: row.get::<_, Option<String>>(12)?.map(|session_id| {
                    WorkspaceBindingSelection {
                        session_id,
                        window_id: row.get::<_, Option<String>>(13).unwrap_or_default(),
                    }
                }),
            },
        ))
    })?;
    let mut spaces = Vec::<WorkspaceSpace>::new();
    for row in rows {
        let (space_id, remote_id, name, icon, color, tint_sidebar, position, binding) = row?;
        if let Some(space) = spaces.last_mut()
            && space.id.persistence_value() == space_id
        {
            space.bindings.push(binding);
        } else {
            spaces.push(WorkspaceSpace {
                id: SpaceId::from_persistence(space_id),
                remote_id,
                name,
                icon,
                color,
                tint_sidebar,
                position,
                bindings: vec![binding],
            });
        }
    }
    Ok(spaces)
}

fn create_default_binding(tx: &Transaction<'_>, path: &Path) -> rusqlite::Result<WorkspaceBinding> {
    let remote_id = new_remote_space_id(tx)?;
    tx.execute(
        "INSERT INTO workspace_spaces (remote_id, name, icon, color, tint_sidebar, position)
         VALUES (?1, ?2, ?3, ?4, ?5, 0)",
        params![
            remote_id,
            DEFAULT_SPACE_NAME,
            DEFAULT_SPACE_ICON,
            color_to_hex(DEFAULT_SPACE_COLOR),
            i64::from(DEFAULT_TINT_SIDEBAR)
        ],
    )?;
    let space_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            space_id,
            DEFAULT_BINDING_NAME,
            backend_to_storage(None),
            0_i64,
        ],
    )?;
    let binding_id = tx.last_insert_rowid();
    migrate_legacy_metadata(tx, binding_id, path)?;
    Ok(WorkspaceBinding {
        scope: MuxScope::new(
            SpaceId::from_persistence(space_id),
            BindingId::from_persistence(binding_id),
        ),
        name: DEFAULT_BINDING_NAME.to_owned(),
        backend_override: None,
        remote_override: SpaceRemoteOverride::Inherit,
        unavailable: false,
        selection: None,
    })
}

fn migrate_legacy_metadata(
    tx: &Transaction<'_>,
    binding_id: i64,
    path: &Path,
) -> rusqlite::Result<()> {
    let imported_sessions = if table_exists(tx, "session_groups")? && table_exists(tx, "sessions")?
    {
        let session_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        if session_count == 0 {
            false
        } else {
            tx.execute(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 SELECT ?1, name, position FROM session_groups ORDER BY position",
                [binding_id],
            )?;
            tx.execute(
                "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                 SELECT ?1, old_session.name, scoped_group.id, old_session.position
                 FROM sessions old_session
                 JOIN session_groups old_group ON old_group.id = old_session.group_id
                 JOIN workspace_session_groups scoped_group
                   ON scoped_group.binding_id = ?1 AND scoped_group.position = old_group.position
                 ORDER BY old_group.position, old_session.position",
                [binding_id],
            )? > 0
        }
    } else {
        false
    };
    if !imported_sessions {
        migrate_legacy_order_file(tx, binding_id, path)?;
    }
    if table_exists(tx, "session_name_metadata")? {
        tx.execute(
            "INSERT INTO workspace_session_name_metadata
                 (binding_id, session_id, cwd, generated_name, session_name, explicit)
             SELECT ?1, session_id, cwd, generated_name, generated_name, explicit
             FROM session_name_metadata",
            [binding_id],
        )?;
    }
    Ok(())
}

fn migrate_legacy_order_file(
    tx: &Transaction<'_>,
    binding_id: i64,
    database_path: &Path,
) -> rusqlite::Result<()> {
    let Some(names) = legacy_order_paths(database_path)
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())
    else {
        return Ok(());
    };
    let mut groups = Vec::<LegacySessionGroup>::new();
    let mut seen = HashSet::new();
    for name in names.lines().filter(|name| !name.is_empty()) {
        if !seen.insert(name) {
            continue;
        }
        let group_name = name.split_once('/').map_or("", |(group, _)| group);
        if group_name.is_empty() {
            groups.push(LegacySessionGroup {
                name: String::new(),
                sessions: vec![name.to_owned()],
            });
        } else if let Some(group) = groups.iter_mut().find(|group| group.name == group_name) {
            group.sessions.push(name.to_owned());
        } else if let Some(group) = groups
            .iter_mut()
            .find(|group| group.sessions.len() == 1 && group.sessions[0] == group_name)
        {
            group.name = group_name.to_owned();
            group.sessions.push(name.to_owned());
        } else {
            groups.push(LegacySessionGroup {
                name: group_name.to_owned(),
                sessions: vec![name.to_owned()],
            });
        }
    }
    for (group_position, group) in groups.iter().enumerate() {
        tx.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, ?2, ?3)",
            params![binding_id, group.name, group_position as i64],
        )?;
        let group_id = tx.last_insert_rowid();
        for (session_position, session) in group.sessions.iter().enumerate() {
            tx.execute(
                "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                 VALUES (?1, ?2, ?3, ?4)",
                params![binding_id, session, group_id, session_position as i64],
            )?;
        }
    }
    Ok(())
}

fn legacy_order_paths(database_path: &Path) -> Vec<PathBuf> {
    let config_dir = database_path.parent().unwrap_or_else(|| Path::new("."));
    let mut paths = vec![config_dir.join("session-order")];
    if default_config_path().parent() == Some(config_dir) {
        paths.push(
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/"))
                .join(".config/tmux/session-order"),
        );
    }
    paths
}

struct LegacySessionGroup {
    name: String,
    sessions: Vec<String>,
}

fn table_exists(tx: &Transaction<'_>, name: &str) -> rusqlite::Result<bool> {
    tx.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
}

fn backend_to_storage(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "inherit",
        Some(MultiplexerBackendConfig::Rmux) => "rmux",
        Some(MultiplexerBackendConfig::Native) => "native",
        Some(MultiplexerBackendConfig::Tmux) => "tmux",
        Some(MultiplexerBackendConfig::Zellij) => "zellij",
    }
}

/// A binding's remote is stored as JSON rather than as columns of its own: it is one value the app
/// reads and writes whole, and every field it gained would otherwise be another migration.
fn remote_to_storage(remote: &SpaceRemoteOverride) -> Option<String> {
    match remote {
        SpaceRemoteOverride::Inherit => None,
        SpaceRemoteOverride::Inline(remote) => serde_json::to_string(remote).ok(),
        remote => serde_json::to_string(remote).ok(),
    }
}

fn remote_from_storage(stored: Option<&str>) -> SpaceRemoteOverride {
    let Some(stored) = stored else {
        return SpaceRemoteOverride::Inherit;
    };
    serde_json::from_str(stored).unwrap_or_else(|_| {
        serde_json::from_str(stored)
            .map(SpaceRemoteOverride::Inline)
            .unwrap_or_default()
    })
}

fn nonempty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn color_to_hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn color_from_hex(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#')?;
    (value.len() == 6).then_some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}

fn backend_from_storage(backend: &str) -> Option<MultiplexerBackendConfig> {
    match backend {
        "rmux" => Some(MultiplexerBackendConfig::Rmux),
        "native" => Some(MultiplexerBackendConfig::Native),
        "tmux" => Some(MultiplexerBackendConfig::Tmux),
        "zellij" => Some(MultiplexerBackendConfig::Zellij),
        _ => None,
    }
}
