use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    config::{MultiplexerBackendConfig, MultiplexerConfig, SshRemoteConfig, default_config_path},
    mux::{
        command::{MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxWindowLaunchPlan},
        controller::{BindingId, MuxScope, SpaceId},
    },
    session_order::SessionMembershipConflict,
};

const WORKSPACE_SNAPSHOT_REVISION: i64 = 1;
const LEGACY_SESSION_LAUNCH_PLAN_MIGRATION: &str = "legacy-session-launch-plans-v1";
const DEFAULT_SPACE_NAME: &str = "Default Space";
pub(crate) const DEFAULT_SPACE_ICON: &str = "folder";
pub(crate) const DEFAULT_SPACE_COLOR: [u8; 3] = [0x7A, 0xA2, 0xF7];
const DEFAULT_TINT_SIDEBAR: bool = false;
const DEFAULT_BINDING_NAME: &str = "Default Binding";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSessionRename {
    pub(crate) session_id: String,
    pub(crate) old_name: String,
    pub(crate) new_name: String,
    pub(crate) display_name: String,
    pub(crate) cwd: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionRenamePersistenceState {
    NotCommitted,
    AlreadyCommitted,
    Conflict,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceMuxOverride {
    pub backend: Option<MultiplexerBackendConfig>,
    pub remote: SpaceRemoteOverride,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceSpaceUpdate<'a> {
    pub(crate) id: SpaceId,
    pub(crate) name: &'a str,
    pub(crate) icon: &'a str,
    pub(crate) color: [u8; 3],
    pub(crate) tint_sidebar: bool,
    pub(crate) mux: SpaceMuxOverride,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceNamespaceUpdateContext<'a> {
    pub(crate) binding_id: i64,
    pub(crate) namespace: &'a str,
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
pub(crate) struct WorkspaceBinding {
    scope: MuxScope,
    name: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    unavailable: bool,
    selection: Option<WorkspaceBindingSelection>,
}

impl WorkspaceBinding {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn backend_override(&self) -> Option<MultiplexerBackendConfig> {
        self.backend_override
    }

    pub(crate) fn remote_override(&self) -> &SpaceRemoteOverride {
        &self.remote_override
    }

    pub(crate) fn mux_scope(&self) -> MuxScope {
        self.scope
    }

    pub(crate) fn unavailable(&self) -> bool {
        self.unavailable
    }

    pub(crate) fn selection(&self) -> Option<&WorkspaceBindingSelection> {
        self.selection.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceBindingSelection {
    session_id: String,
    window_id: Option<String>,
}

impl WorkspaceBindingSelection {
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn window_id(&self) -> Option<&str> {
        self.window_id.as_deref()
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceSpace {
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
    pub(crate) fn id(&self) -> SpaceId {
        self.id
    }

    pub(crate) fn remote_id(&self) -> &str {
        &self.remote_id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn icon(&self) -> &str {
        &self.icon
    }

    pub(crate) fn color(&self) -> [u8; 3] {
        self.color
    }

    pub(crate) fn tint_sidebar(&self) -> bool {
        self.tint_sidebar
    }

    pub(crate) fn position(&self) -> i64 {
        self.position
    }

    pub(crate) fn bindings(&self) -> &[WorkspaceBinding] {
        &self.bindings
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceStore {
    path: PathBuf,
    spaces: Vec<WorkspaceSpace>,
}

impl WorkspaceStore {
    pub(crate) fn try_for_config_path(config_path: &Path) -> rusqlite::Result<Self> {
        let path = sqlite_path(config_path);
        let spaces = Self::load_or_migrate(&path)?;
        Ok(Self { path, spaces })
    }

    pub(crate) fn for_config_path(config_path: &Path) -> Self {
        Self::try_for_config_path(config_path).unwrap_or_else(|_| Self {
            path: sqlite_path(config_path),
            spaces: Vec::new(),
        })
    }

    pub(crate) fn binding(&self) -> Option<&WorkspaceBinding> {
        self.spaces.first()?.bindings.first()
    }

    #[cfg(test)]
    pub(crate) fn bindings(&self) -> &[WorkspaceBinding] {
        self.spaces
            .first()
            .map(WorkspaceSpace::bindings)
            .unwrap_or_default()
    }

    pub(crate) fn spaces(&self) -> &[WorkspaceSpace] {
        &self.spaces
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn binding_id(&self) -> Option<i64> {
        self.binding()
            .map(|binding| binding.scope.binding_id().persistence_value())
    }

    pub(crate) fn create_space(
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
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
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
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.execute(
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
        tx.execute(
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
        tx.commit()?;
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
    pub(crate) fn update_space_with_namespace(
        &mut self,
        update: WorkspaceSpaceUpdate<'_>,
        context: WorkspaceNamespaceUpdateContext<'_>,
    ) -> rusqlite::Result<bool> {
        let WorkspaceSpaceUpdate {
            id,
            name,
            icon,
            color,
            tint_sidebar,
            mux,
        } = update;
        let WorkspaceNamespaceUpdateContext {
            binding_id,
            namespace,
        } = context;

        let Some(name) = nonempty_trimmed(name) else {
            return Ok(false);
        };
        let Some(icon) = nonempty_trimmed(icon) else {
            return Ok(false);
        };
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.execute(
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
        if tx.execute(
            "UPDATE workspace_bindings
             SET backend = ?1, remote = ?2
             WHERE id = ?3 AND space_id = ?4",
            params![
                backend_to_storage(mux.backend),
                remote_to_storage(&mux.remote),
                binding_id,
                id.persistence_value()
            ],
        )? == 0
        {
            return Ok(false);
        }
        tx.execute(
            "CREATE TABLE IF NOT EXISTS workspace_session_namespaces (
                 binding_id INTEGER PRIMARY KEY
                     REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                 namespace TEXT NOT NULL
             )",
            [],
        )?;
        let conflict = tx
            .query_row(
                "SELECT s.name
                 FROM workspace_sessions s
                 JOIN workspace_session_namespaces n
                   ON n.namespace = ?2 AND n.binding_id != ?1
                 JOIN workspace_sessions other
                   ON other.binding_id = n.binding_id AND other.name = s.name
                 WHERE s.binding_id = ?1
                 LIMIT 1",
                params![binding_id, namespace],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(name) = conflict {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                SessionMembershipConflict {
                    name,
                    namespace: namespace.to_owned(),
                },
            )));
        }
        tx.execute(
            "INSERT INTO workspace_session_namespaces (binding_id, namespace)
             VALUES (?1, ?2)
             ON CONFLICT(binding_id) DO UPDATE SET namespace = excluded.namespace",
            params![binding_id, namespace],
        )?;
        tx.commit()?;
        if let Some(space) = self.spaces.iter_mut().find(|space| space.id == id) {
            space.name = name;
            space.icon = icon;
            space.color = color;
            space.tint_sidebar = tint_sidebar;
            if let Some(binding) = space
                .bindings
                .iter_mut()
                .find(|binding| binding.scope.binding_id().persistence_value() == binding_id)
            {
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
        {
            let tx = conn.transaction()?;
            create_workspace_schema(&tx)?;
            migrate_workspace_snapshot_state(&tx)?;
            tx.commit()?;
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
        migrate_workspace_session_launch_plans(&tx)?;
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
fn ensure_session_launch_plan_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_session_launch_plans (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            plan TEXT NOT NULL,
            PRIMARY KEY(binding_id, session_id)
        )",
    )
}
fn ensure_pending_ditch_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_pending_ditch (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            cwd TEXT,
            action TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY(binding_id, session_id)
        )",
    )?;
    let mut statement = conn.prepare("PRAGMA table_info(workspace_pending_ditch)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    if !columns.iter().any(|column| column == "cwd") {
        conn.execute(
            "ALTER TABLE workspace_pending_ditch ADD COLUMN cwd TEXT",
            [],
        )?;
    }
    if !columns.iter().any(|column| column == "action") {
        conn.execute(
            "ALTER TABLE workspace_pending_ditch ADD COLUMN action TEXT NOT NULL DEFAULT '{}'",
            [],
        )?;
    }
    Ok(())
}

pub(crate) fn persist_pending_ditch(
    config_path: &Path,
    binding_id: i64,
    session_id: &str,
    cwd: Option<&str>,
    action: &str,
) -> rusqlite::Result<()> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_ditch_table(&conn)?;
    conn.execute(
        "INSERT INTO workspace_pending_ditch(binding_id, session_id, cwd, action)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(binding_id, session_id) DO UPDATE SET cwd = excluded.cwd, action = excluded.action",
        params![binding_id, session_id, cwd, action],
    )?;
    Ok(())
}

pub(crate) fn clear_pending_ditch(
    config_path: &Path,
    binding_id: i64,
    session_id: &str,
) -> rusqlite::Result<()> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_ditch_table(&conn)?;
    conn.execute(
        "DELETE FROM workspace_pending_ditch WHERE binding_id = ?1 AND session_id = ?2",
        params![binding_id, session_id],
    )?;
    Ok(())
}

pub(crate) fn load_pending_ditches(
    config_path: &Path,
    binding_id: i64,
) -> rusqlite::Result<Vec<String>> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_ditch_table(&conn)?;
    let mut statement = conn.prepare(
        "SELECT session_id FROM workspace_pending_ditch
         WHERE binding_id = ?1 ORDER BY session_id",
    )?;
    statement
        .query_map(params![binding_id], |row| row.get(0))?
        .collect()
}

fn ensure_pending_session_rename_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS workspace_pending_session_rename (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            command_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            old_name TEXT NOT NULL,
            new_name TEXT NOT NULL,
            display_name TEXT NOT NULL,
            cwd TEXT NOT NULL,
            PRIMARY KEY(binding_id, command_id)
        )",
    )
}

pub(crate) fn persist_pending_session_rename(
    config_path: &Path,
    binding_id: i64,
    command_id: &str,
    rename: &PendingSessionRename,
) -> rusqlite::Result<()> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_session_rename_table(&conn)?;
    conn.execute(
        "INSERT INTO workspace_pending_session_rename
            (binding_id, command_id, session_id, old_name, new_name, display_name, cwd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(binding_id, command_id) DO UPDATE SET
            session_id = excluded.session_id,
            old_name = excluded.old_name,
            new_name = excluded.new_name,
            display_name = excluded.display_name,
            cwd = excluded.cwd",
        params![
            binding_id,
            command_id,
            rename.session_id,
            rename.old_name,
            rename.new_name,
            rename.display_name,
            rename.cwd,
        ],
    )?;
    Ok(())
}

pub(crate) fn clear_pending_session_rename(
    config_path: &Path,
    binding_id: i64,
    command_id: &str,
) -> rusqlite::Result<()> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_session_rename_table(&conn)?;
    conn.execute(
        "DELETE FROM workspace_pending_session_rename
         WHERE binding_id = ?1 AND command_id = ?2",
        params![binding_id, command_id],
    )?;
    Ok(())
}

pub(crate) fn load_pending_session_renames(
    config_path: &Path,
    binding_id: i64,
) -> rusqlite::Result<Vec<(String, PendingSessionRename)>> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_pending_session_rename_table(&conn)?;
    let mut statement = conn.prepare(
        "SELECT command_id, session_id, old_name, new_name, display_name, cwd
         FROM workspace_pending_session_rename
         WHERE binding_id = ?1 ORDER BY command_id",
    )?;
    statement
        .query_map([binding_id], |row| {
            Ok((
                row.get(0)?,
                PendingSessionRename {
                    session_id: row.get(1)?,
                    old_name: row.get(2)?,
                    new_name: row.get(3)?,
                    display_name: row.get(4)?,
                    cwd: row.get(5)?,
                },
            ))
        })?
        .collect()
}

pub(crate) fn persist_session_launch_plan(
    config_path: &Path,
    binding_id: i64,
    plan: &MuxSessionLaunchPlan,
) -> rusqlite::Result<()> {
    plan.validate()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let encoded = serde_json::to_string(plan)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    let existing = conn
        .query_row(
            "SELECT plan FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, plan.session_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing == encoded {
            return Ok(());
        }
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "immutable launch plan already exists for binding {binding_id} session {:?}",
                    plan.session_id
                ),
            ),
        )));
    }
    conn.execute(
        "INSERT INTO workspace_session_launch_plans (binding_id, session_id, plan)
         VALUES (?1, ?2, ?3)",
        params![binding_id, plan.session_id.as_str(), encoded],
    )?;
    Ok(())
}

pub(crate) fn load_session_launch_plans(
    config_path: &Path,
    binding_id: i64,
) -> rusqlite::Result<Vec<(String, MuxSessionLaunchPlan)>> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    let mut statement = conn.prepare(
        "SELECT session_id, plan
         FROM workspace_session_launch_plans
         WHERE binding_id = ?1
         ORDER BY session_id",
    )?;
    statement
        .query_map([binding_id], |row| {
            let session_id = row.get::<_, String>(0)?;
            let encoded = row.get::<_, String>(1)?;
            let plan = serde_json::from_str::<MuxSessionLaunchPlan>(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((session_id, plan))
        })?
        .collect()
}

pub(crate) fn delete_session_launch_plan(
    config_path: &Path,
    binding_id: i64,
    session_id: &str,
) -> rusqlite::Result<bool> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    Ok(conn.execute(
        "DELETE FROM workspace_session_launch_plans
         WHERE binding_id = ?1 AND session_id = ?2",
        params![binding_id, session_id],
    )? != 0)
}

pub(crate) fn rekey_session_launch_plan(
    config_path: &Path,
    binding_id: i64,
    old_session_id: &str,
    new_session_id: &str,
) -> rusqlite::Result<bool> {
    if old_session_id == new_session_id {
        return Ok(false);
    }
    let mut conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    let tx = conn.transaction()?;
    let old_plan = tx
        .query_row(
            "SELECT plan FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, old_session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(old_plan) = old_plan else {
        return Ok(false);
    };
    let target_plan = tx
        .query_row(
            "SELECT plan FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, new_session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match target_plan {
        None => {
            tx.execute(
                "UPDATE workspace_session_launch_plans
                 SET session_id = ?1
                 WHERE binding_id = ?2 AND session_id = ?3",
                params![new_session_id, binding_id, old_session_id],
            )?;
        }
        Some(target_plan) if target_plan == old_plan => {
            tx.execute(
                "DELETE FROM workspace_session_launch_plans
                 WHERE binding_id = ?1 AND session_id = ?2",
                params![binding_id, old_session_id],
            )?;
        }
        Some(_) => {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "immutable launch plan already exists for binding {binding_id} session \
                         {new_session_id:?}"
                    ),
                ),
            )));
        }
    }
    tx.commit()?;
    Ok(true)
}
pub(crate) fn session_rename_persistence_state(
    config_path: &Path,
    binding_id: i64,
    command_id: &str,
    rename: &PendingSessionRename,
) -> rusqlite::Result<SessionRenamePersistenceState> {
    let conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    ensure_pending_session_rename_table(&conn)?;

    let pending = conn
        .query_row(
            "SELECT session_id, old_name, new_name, display_name, cwd
             FROM workspace_pending_session_rename
             WHERE binding_id = ?1 AND command_id = ?2",
            params![binding_id, command_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?;
    let pending_matches =
        pending.is_some_and(|(session_id, old_name, new_name, display_name, cwd)| {
            session_id == rename.session_id
                && old_name == rename.old_name
                && new_name == rename.new_name
                && display_name == rename.display_name
                && cwd == rename.cwd
        });
    if !pending_matches {
        return Ok(SessionRenamePersistenceState::NotCommitted);
    }

    let (source_membership, destination_membership) = {
        let membership_exists = |name: &str| {
            conn.query_row(
                "SELECT 1 FROM workspace_sessions
                 WHERE binding_id = ?1 AND name = ?2",
                params![binding_id, name],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
        };
        (
            membership_exists(&rename.old_name)?,
            membership_exists(&rename.new_name)?,
        )
    };

    let mut statement = conn.prepare(
        "SELECT session_id, plan
         FROM workspace_session_launch_plans
         WHERE binding_id = ?1",
    )?;
    let plans = statement
        .query_map([binding_id], |row| {
            let key = row.get::<_, String>(0)?;
            let encoded = row.get::<_, String>(1)?;
            let plan = serde_json::from_str::<MuxSessionLaunchPlan>(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((key, plan))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let source_plan = plans.iter().any(|(key, plan)| {
        key == &rename.old_name
            || key == &rename.session_id
            || plan.session_id == rename.old_name
            || plan.session_id == rename.session_id
    });
    let destination_plan = plans
        .iter()
        .find(|(key, _)| key == &rename.new_name)
        .map(|(_, plan)| plan);
    let destination_exists = destination_membership || destination_plan.is_some();
    if source_membership || source_plan || !destination_exists {
        return Ok(SessionRenamePersistenceState::NotCommitted);
    }

    let metadata = conn
        .query_row(
            "SELECT session_id, session_name, cwd
             FROM workspace_session_name_metadata
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, rename.session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let metadata_matches = metadata.is_some_and(|(session_id, session_name, cwd)| {
        session_id == rename.session_id
            && cwd == rename.cwd
            && (session_name == rename.old_name || session_name == rename.new_name)
    });
    let plan_matches = destination_membership
        && destination_plan.is_some_and(|plan| {
            plan.session_id == rename.new_name && plan.default_cwd == rename.cwd
        });
    if metadata_matches && plan_matches {
        Ok(SessionRenamePersistenceState::AlreadyCommitted)
    } else {
        Ok(SessionRenamePersistenceState::Conflict)
    }
}

/// validates destination collisions before changing either membership or launch-plan JSON.
pub(crate) fn rename_session_membership_and_launch_plans(
    config_path: &Path,
    binding_id: i64,
    old_name: &str,
    new_name: &str,
    plan_ids: &[&str],
) -> rusqlite::Result<bool> {
    if old_name == new_name {
        return Ok(false);
    }
    let mut conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    ensure_pending_ditch_table(&conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let old_membership = tx
        .query_row(
            "SELECT 1 FROM workspace_sessions
             WHERE binding_id = ?1 AND name = ?2",
            params![binding_id, old_name],
            |_| Ok(()),
        )
        .optional()?;
    let destination_membership = tx
        .query_row(
            "SELECT 1 FROM workspace_sessions
             WHERE binding_id = ?1 AND name = ?2",
            params![binding_id, new_name],
            |_| Ok(()),
        )
        .optional()?;
    if destination_membership.is_some() {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("session membership already exists for {new_name:?}"),
            ),
        )));
    }

    let mut statement = tx.prepare(
        "SELECT session_id, plan
         FROM workspace_session_launch_plans
         WHERE binding_id = ?1",
    )?;
    let rows = statement
        .query_map([binding_id], |row| {
            let key = row.get::<_, String>(0)?;
            let encoded = row.get::<_, String>(1)?;
            let plan = serde_json::from_str::<MuxSessionLaunchPlan>(&encoded).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })?;
            Ok((key, plan))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    let mut matching = rows
        .into_iter()
        .filter(|(key, plan)| {
            key == old_name
                || plan.session_id == old_name
                || plan_ids
                    .iter()
                    .any(|id| *id == key || *id == plan.session_id)
        })
        .collect::<Vec<_>>();
    let pending = tx
        .query_row(
            "SELECT 1 FROM workspace_pending_ditch
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, old_name],
            |_| Ok(()),
        )
        .optional()?;

    if old_membership.is_none() && matching.is_empty() && pending.is_none() {
        tx.commit()?;
        return Ok(false);
    }

    let existing_target = tx
        .query_row(
            "SELECT plan FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, new_name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing_target.is_some() && matching.iter().any(|(key, _)| key != new_name) {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("launch plan already exists for {new_name:?}"),
            ),
        )));
    }

    let mut renamed_plan: Option<String> = existing_target;
    for (key, mut plan) in matching.drain(..) {
        plan.session_id = new_name.to_owned();
        plan.validate()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let encoded = serde_json::to_string(&plan)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        if renamed_plan
            .as_deref()
            .is_some_and(|existing| existing != encoded)
        {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("conflicting launch plans for {new_name:?}"),
                ),
            )));
        }
        tx.execute(
            "DELETE FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, key],
        )?;
        renamed_plan = Some(encoded);
    }
    if let Some(encoded) = renamed_plan {
        tx.execute(
            "INSERT INTO workspace_session_launch_plans(binding_id, session_id, plan)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(binding_id, session_id) DO UPDATE SET plan = excluded.plan",
            params![binding_id, new_name, encoded],
        )?;
    }
    if old_membership.is_some() {
        tx.execute(
            "UPDATE workspace_sessions SET name = ?1
             WHERE binding_id = ?2 AND name = ?3",
            params![new_name, binding_id, old_name],
        )?;
    }
    if pending.is_some() {
        tx.execute(
            "UPDATE workspace_pending_ditch SET session_id = ?1
             WHERE binding_id = ?2 AND session_id = ?3",
            params![new_name, binding_id, old_name],
        )?;
    }
    tx.commit()?;
    Ok(true)
}

/// Atomically retire a backend session's workspace membership and every immutable launch plan
/// identifying it. The transaction intentionally parses all candidate plans before commit: a
/// malformed or otherwise unwritable plan rolls back membership removal rather than leaving a
/// successfully ditched backend session restorable on the next reopen.
pub(crate) fn remove_session_membership_and_launch_plan(
    config_path: &Path,
    binding_id: i64,
    membership_name: &str,
    plan_ids: &[&str],
) -> rusqlite::Result<()> {
    let mut conn = open_db(&sqlite_path(config_path))?;
    ensure_session_launch_plan_table(&conn)?;
    ensure_pending_ditch_table(&conn)?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM workspace_sessions
         WHERE binding_id = ?1 AND name = ?2",
        params![binding_id, membership_name],
    )?;
    tx.execute(
        "DELETE FROM workspace_session_groups
         WHERE binding_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM workspace_sessions
               WHERE workspace_sessions.binding_id = workspace_session_groups.binding_id
                 AND workspace_sessions.group_id = workspace_session_groups.id
           )",
        [binding_id],
    )?;
    tx.execute(
        "INSERT INTO workspace_session_groups (binding_id, name, position)
         SELECT ?1, '', 0
         WHERE NOT EXISTS (
             SELECT 1 FROM workspace_session_groups WHERE binding_id = ?1
         )",
        [binding_id],
    )?;

    let mut statement = tx.prepare(
        "SELECT session_id, plan
         FROM workspace_session_launch_plans
         WHERE binding_id = ?1",
    )?;
    let mut plan_keys = Vec::new();
    let rows = statement.query_map([binding_id], |row| {
        let key = row.get::<_, String>(0)?;
        let encoded = row.get::<_, String>(1)?;
        let plan = serde_json::from_str::<MuxSessionLaunchPlan>(&encoded).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
        Ok((key, plan))
    })?;
    for row in rows {
        let (key, plan) = row?;
        if plan_ids
            .iter()
            .any(|plan_id| *plan_id == key || *plan_id == plan.session_id)
        {
            plan_keys.push(key);
        }
    }
    drop(statement);
    for key in plan_keys {
        tx.execute(
            "DELETE FROM workspace_session_launch_plans
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, key],
        )?;
    }
    for plan_id in plan_ids {
        tx.execute(
            "DELETE FROM workspace_pending_ditch
             WHERE binding_id = ?1 AND session_id = ?2",
            params![binding_id, plan_id],
        )?;
    }
    tx.execute(
        "DELETE FROM workspace_pending_ditch
         WHERE binding_id = ?1 AND session_id = ?2",
        params![binding_id, membership_name],
    )?;
    tx.commit()
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
             hide_tmux_status INTEGER NOT NULL,
             remote TEXT,
             unavailable INTEGER NOT NULL DEFAULT 0,
             selected_session_id TEXT,
             selected_window_id TEXT
         );
         INSERT INTO workspace_bindings_multiple
             (id, space_id, name, backend, hide_tmux_status, remote, unavailable,
              selected_session_id, selected_window_id)
         SELECT id, space_id, name, backend, hide_tmux_status, remote, unavailable,
                selected_session_id, selected_window_id
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
        CREATE TABLE IF NOT EXISTS workspace_session_launch_plans (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            plan TEXT NOT NULL,
            PRIMARY KEY(binding_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS workspace_migrations (
            name TEXT PRIMARY KEY
        );
        CREATE TABLE IF NOT EXISTS workspace_pending_ditch (
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            session_id TEXT NOT NULL,
            cwd TEXT,
            action TEXT NOT NULL DEFAULT '{}',
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
fn migrate_workspace_session_launch_plans(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let migrated = tx
        .query_row(
            "SELECT 1 FROM workspace_migrations WHERE name = ?1",
            [LEGACY_SESSION_LAUNCH_PLAN_MIGRATION],
            |_| Ok(()),
        )
        .optional()?;
    if migrated.is_some() {
        return Ok(());
    }

    let mut statement = tx.prepare(
        "SELECT binding_id, session_id, cwd
         FROM workspace_session_name_metadata",
    )?;
    let legacy = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);

    for (binding_id, session_id, cwd) in legacy {
        let plan = simple_session_launch_plan(&session_id, &cwd);
        let encoded = serde_json::to_string(&plan)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        tx.execute(
            "INSERT OR IGNORE INTO workspace_session_launch_plans
                (binding_id, session_id, plan)
             VALUES (?1, ?2, ?3)",
            params![binding_id, session_id, encoded],
        )?;
    }
    // The marker is written in the same transaction, after every legacy row has been inserted.
    // A failed insert rolls back both the plans and this marker, so migration can safely retry.
    tx.execute(
        "INSERT INTO workspace_migrations(name) VALUES (?1)",
        [LEGACY_SESSION_LAUNCH_PLAN_MIGRATION],
    )?;
    Ok(())
}

pub(crate) fn simple_session_launch_plan(session_id: &str, cwd: &str) -> MuxSessionLaunchPlan {
    MuxSessionLaunchPlan {
        session_id: session_id.to_owned(),
        focus: true,
        default_cwd: cwd.to_owned(),
        environment: std::collections::BTreeMap::new(),
        windows: vec![MuxWindowLaunchPlan {
            name: None,
            focus: true,
            layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                cwd: cwd.to_owned(),
                command: None,
                argv: None,
                environment: std::collections::BTreeMap::new(),
                title: None,
            }),
        }],
        focused_window: 0,
    }
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

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use rusqlite::params;

    use super::*;

    fn temp_config_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bootty-workspace-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create workspace directory");
        dir.join("config.toml")
    }

    #[test]
    fn isolated_workspace_does_not_read_global_tmux_legacy_order() {
        let config_path = temp_config_path("isolated-legacy-order");
        let database_path = sqlite_path(&config_path);

        let paths = legacy_order_paths(&database_path);
        assert_eq!(paths.len(), 1);
        assert_eq!(
            paths[0],
            database_path.parent().unwrap().join("session-order")
        );
    }

    /// The host a space's sessions live on has to survive storage whole: bootty reads it back to
    /// decide which machine to attach, and a detail lost on the way would attach a different one —
    /// or none, once the host itself is gone.
    #[test]
    fn a_space_remembers_the_host_its_sessions_live_on() {
        let config_path = temp_config_path("space-remote");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let remote = SshRemoteConfig {
            host: "devbox".to_owned(),
            user: Some("dev".to_owned()),
            port: Some(2222),
            program: "ssh".to_owned(),
            args: vec!["-i".to_owned(), "~/.ssh/devbox".to_owned()],
        };
        let space = store
            .create_space(
                "Remote",
                "terminal",
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Inline(remote.clone()),
                },
                &MultiplexerConfig::default(),
            )
            .expect("create remote space")
            .expect("space");

        let reopened = WorkspaceStore::for_config_path(&config_path);
        let stored = reopened
            .spaces()
            .iter()
            .find(|candidate| candidate.id() == space.id())
            .expect("persisted space");
        assert_eq!(
            stored.bindings()[0].remote_override(),
            &SpaceRemoteOverride::Inline(remote)
        );

        assert!(
            store
                .update_space(
                    space.id(),
                    "Remote",
                    "terminal",
                    DEFAULT_SPACE_COLOR,
                    false,
                    SpaceMuxOverride {
                        backend: Some(MultiplexerBackendConfig::Tmux),
                        remote: SpaceRemoteOverride::Local,
                    },
                )
                .expect("clear remote")
        );

        let reopened = WorkspaceStore::for_config_path(&config_path);
        let stored = reopened
            .spaces()
            .iter()
            .find(|candidate| candidate.id() == space.id())
            .expect("persisted space");
        assert_eq!(
            stored.bindings()[0].remote_override(),
            &SpaceRemoteOverride::Local
        );
    }

    #[test]
    fn update_space_rolls_back_space_when_binding_update_fails() {
        let config_path = temp_config_path("atomic-space-update");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let space = store
            .create_space(
                "Atomic",
                "terminal",
                [1, 2, 3],
                false,
                SpaceMuxOverride::default(),
                &MultiplexerConfig::default(),
            )
            .expect("create space")
            .expect("created space");

        let conn = open_db(store.path()).expect("open workspace database");
        conn.execute_batch(
            "CREATE TRIGGER fail_space_binding_update
             BEFORE UPDATE OF backend ON workspace_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'injected binding update failure');
             END;",
        )
        .expect("install binding update failure");

        let result = store.update_space(
            space.id(),
            "Changed",
            "calendar",
            [4, 5, 6],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Local,
            },
        );
        assert!(result.is_err(), "the injected second update must fail");

        let reopened = WorkspaceStore::for_config_path(&config_path);
        let persisted = reopened
            .spaces()
            .iter()
            .find(|candidate| candidate.id() == space.id())
            .expect("persisted space");
        assert_eq!(persisted.name(), "Atomic");
        assert_eq!(persisted.icon(), "terminal");
        assert_eq!(persisted.color(), [1, 2, 3]);
        assert!(!persisted.tint_sidebar());
        assert_eq!(persisted.bindings()[0].backend_override(), None);
        assert_eq!(
            persisted.bindings()[0].remote_override(),
            &SpaceRemoteOverride::Inherit
        );
    }

    #[test]
    fn a_space_remembers_its_remote_space_reference() {
        let config_path = temp_config_path("remote-space-reference");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let reference = RemoteSpaceRef {
            profile_id: "home-lab".to_owned(),
            remote_space_id: "remote-42".to_owned(),
            remote_space_name: "Production".to_owned(),
            backend: MultiplexerBackendConfig::Tmux,
        };
        store
            .create_space(
                "Production",
                "terminal",
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(reference.backend),
                    remote: SpaceRemoteOverride::Profile(reference.clone()),
                },
                &MultiplexerConfig::default(),
            )
            .expect("create remote Space")
            .expect("Space");

        let reopened = WorkspaceStore::for_config_path(&config_path);

        assert_eq!(
            reopened.spaces()[1].bindings()[0].remote_override(),
            &SpaceRemoteOverride::Profile(reference)
        );
    }

    #[test]
    fn legacy_inline_remote_json_still_loads() {
        let remote = SshRemoteConfig::for_host("legacy-host");
        let stored = serde_json::to_string(&remote).expect("legacy JSON");

        assert_eq!(
            remote_from_storage(Some(&stored)),
            SpaceRemoteOverride::Inline(remote)
        );
    }

    #[test]
    fn fresh_configuration_creates_default_space_and_binding() {
        let config_path = temp_config_path("fresh");

        let store = WorkspaceStore::for_config_path(&config_path);
        let binding = store.binding().expect("default binding");
        assert_eq!(binding.backend_override(), None);

        let conn = open_db(&sqlite_path(&config_path)).expect("open workspace database");
        let names: (String, String) = conn
            .query_row(
                "SELECT s.name, b.name
                 FROM workspace_spaces s
                 JOIN workspace_bindings b ON b.space_id = s.id",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("default scope names");
        assert_eq!(
            names,
            (
                DEFAULT_SPACE_NAME.to_owned(),
                DEFAULT_BINDING_NAME.to_owned()
            )
        );
    }

    #[test]
    fn reopening_loads_multiple_bindings_in_the_same_space() {
        let config_path = temp_config_path("multiple-bindings");
        let store = WorkspaceStore::for_config_path(&config_path);
        let default = store.binding().expect("default binding");
        let space_id = default.mux_scope().space_id().persistence_value();
        let conn = open_db(store.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![space_id, "Remote", "tmux", 1_i64],
        )
        .expect("insert second binding");
        conn.execute(
            "INSERT INTO workspace_spaces (remote_id, name, position) VALUES (?1, ?2, 1)",
            ["other-space", "Other Space"],
        )
        .expect("insert other space");
        let other_space_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![other_space_id, "Other Space Binding", "zellij", 0_i64],
        )
        .expect("insert other space binding");

        let reopened = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(reopened.spaces().len(), 2);
        assert_eq!(reopened.spaces()[0].name(), DEFAULT_SPACE_NAME);
        assert_eq!(reopened.spaces()[0].bindings().len(), 2);
        assert_eq!(reopened.spaces()[1].name(), "Other Space");
        assert_eq!(reopened.spaces()[1].bindings().len(), 1);
        assert_eq!(
            reopened.spaces()[1].bindings()[0].name(),
            "Other Space Binding"
        );

        assert_eq!(reopened.bindings().len(), 2);
        assert_eq!(reopened.bindings()[0].name(), DEFAULT_BINDING_NAME);
        assert_eq!(reopened.bindings()[1].name(), "Remote");
        assert_eq!(
            reopened.bindings()[1].backend_override(),
            Some(MultiplexerBackendConfig::Tmux)
        );
        assert_ne!(
            reopened.bindings()[0].mux_scope(),
            reopened.bindings()[1].mux_scope()
        );
        assert!(
            reopened
                .bindings()
                .iter()
                .all(|binding| { binding.mux_scope().space_id().persistence_value() == space_id })
        );
    }

    #[test]
    fn reopening_repairs_a_configured_space_without_a_binding() {
        let config_path = temp_config_path("empty-space");
        let store = WorkspaceStore::for_config_path(&config_path);
        let conn = open_db(store.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (remote_id, name, position) VALUES (?1, ?2, 1)",
            ["empty-space", "Empty Space"],
        )
        .expect("insert empty space");

        let reopened = WorkspaceStore::for_config_path(&config_path);

        assert_eq!(reopened.spaces().len(), 2);
        assert_eq!(reopened.spaces()[1].name(), "Empty Space");
        assert_eq!(reopened.spaces()[1].bindings().len(), 1);
        assert_eq!(reopened.spaces()[1].bindings()[0].backend_override(), None);
    }

    #[test]
    fn creating_spaces_persists_a_default_binding_and_unique_name() {
        let config_path = temp_config_path("create-space");
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            hide_tmux_status: true,
            ..MultiplexerConfig::default()
        };
        let mut store = WorkspaceStore::for_config_path(&config_path);

        assert!(
            store
                .create_space(
                    "   ",
                    DEFAULT_SPACE_ICON,
                    DEFAULT_SPACE_COLOR,
                    false,
                    SpaceMuxOverride::default(),
                    &config,
                )
                .expect("ignore blank name")
                .is_none()
        );
        let review = store
            .create_space(
                " Review ",
                "terminal",
                [1, 2, 3],
                true,
                SpaceMuxOverride::default(),
                &config,
            )
            .expect("create review space")
            .expect("nonblank space");
        let duplicate = store
            .create_space(
                "review",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride::default(),
                &config,
            )
            .expect("create duplicate space")
            .expect("nonblank space");

        assert_eq!(review.name(), "Review");
        assert_eq!(review.icon(), "terminal");
        assert_eq!(review.color(), [1, 2, 3]);
        assert!(review.tint_sidebar());
        assert_eq!(review.bindings().len(), 1);
        assert_eq!(review.bindings()[0].name(), DEFAULT_BINDING_NAME);
        assert_eq!(review.bindings()[0].backend_override(), None);
        assert_eq!(duplicate.name(), "review 2");

        let reopened = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(
            reopened
                .spaces()
                .iter()
                .map(WorkspaceSpace::name)
                .collect::<Vec<_>>(),
            vec![DEFAULT_SPACE_NAME, "Review", "review 2"]
        );
        assert!(
            reopened
                .spaces()
                .iter()
                .all(|space| space.bindings().len() == 1)
        );
    }

    #[test]
    fn concurrent_space_creation_chooses_distinct_unique_names() {
        let config_path = temp_config_path("concurrent-create-space");
        let config = MultiplexerConfig::default();
        let first = WorkspaceStore::for_config_path(&config_path);
        let second = WorkspaceStore::for_config_path(&config_path);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let first_config = config.clone();
        let second_config = config.clone();

        let results = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first = scope.spawn(move || {
                let mut store = first;
                first_barrier.wait();
                store.create_space(
                    "Review",
                    DEFAULT_SPACE_ICON,
                    DEFAULT_SPACE_COLOR,
                    false,
                    SpaceMuxOverride::default(),
                    &first_config,
                )
            });
            let second_barrier = barrier.clone();
            let second = scope.spawn(move || {
                let mut store = second;
                second_barrier.wait();
                store.create_space(
                    "Review",
                    DEFAULT_SPACE_ICON,
                    DEFAULT_SPACE_COLOR,
                    false,
                    SpaceMuxOverride::default(),
                    &second_config,
                )
            });
            [
                first.join().expect("first create thread"),
                second.join().expect("second create thread"),
            ]
        });

        assert!(results.iter().all(|result| result.is_ok()));
        assert!(
            results
                .iter()
                .all(|result| result.as_ref().unwrap().is_some())
        );
        let reopened = WorkspaceStore::for_config_path(&config_path);
        let names = reopened
            .spaces()
            .iter()
            .map(WorkspaceSpace::name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"Review"));
        assert!(names.contains(&"Review 2"));
    }

    #[test]
    fn icon_migration_round_trips_edits_and_cascades_deleted_space_bindings() {
        let config_path = temp_config_path("space-icon-migration");
        let db_path = sqlite_path(&config_path);
        let conn = open_db(&db_path).expect("open old workspace database");
        conn.execute_batch(
            "CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL
            );
            INSERT INTO workspace_spaces (id, name, position) VALUES (1, 'Default Space', 0);
            INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status)
            VALUES (1, 1, 'Default Binding', 'native', 0);",
        )
        .expect("create pre-icon workspace");

        let mut store = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(store.spaces()[0].icon(), DEFAULT_SPACE_ICON);
        assert_eq!(store.spaces()[0].color(), DEFAULT_SPACE_COLOR);
        assert!(!store.spaces()[0].tint_sidebar());
        let review = store
            .create_space(
                "Review",
                "terminal",
                [1, 2, 3],
                true,
                SpaceMuxOverride::default(),
                &MultiplexerConfig::default(),
            )
            .expect("create space")
            .expect("space");
        assert!(
            store
                .update_space(
                    review.id(),
                    "Planning",
                    "calendar",
                    [4, 5, 6],
                    false,
                    SpaceMuxOverride {
                        backend: Some(MultiplexerBackendConfig::Zellij),
                        remote: SpaceRemoteOverride::Inherit,
                    },
                )
                .expect("update space")
        );
        let mut reopened = WorkspaceStore::for_config_path(&config_path);
        let planning = reopened
            .spaces()
            .iter()
            .find(|space| space.id() == review.id())
            .expect("persisted Space");
        assert_eq!(planning.name(), "Planning");
        assert_eq!(planning.icon(), "calendar");
        assert_eq!(planning.color(), [4, 5, 6]);
        assert!(!planning.tint_sidebar());
        assert_eq!(
            planning.bindings()[0].backend_override(),
            Some(MultiplexerBackendConfig::Zellij)
        );
        assert!(reopened.delete_space(review.id()).expect("delete"));

        let reopened = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(reopened.spaces().len(), 1);
        let conn = open_db(reopened.path()).expect("open migrated database");
        let binding_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_bindings WHERE space_id = ?1",
                [review.id().persistence_value()],
                |row| row.get(0),
            )
            .expect("binding count");
        assert_eq!(binding_count, 0);
    }

    #[test]
    fn existing_single_binding_schema_migrates_without_losing_scoped_metadata() {
        let config_path = temp_config_path("binding-cardinality-migration");
        let db_path = sqlite_path(&config_path);
        let conn = open_db(&db_path).expect("open old workspace database");
        conn.execute_batch(
            "CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL UNIQUE REFERENCES workspace_spaces(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL,
                remote TEXT,
                unavailable INTEGER NOT NULL DEFAULT 0,
                selected_session_id TEXT,
                selected_window_id TEXT
            );
            CREATE TABLE workspace_session_name_metadata (
                binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                generated_name TEXT NOT NULL,
                explicit INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(binding_id, session_id)
            );
            INSERT INTO workspace_spaces (id, name, position)
            VALUES (1, 'Default Space', 0);
            INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status, remote, unavailable,
                 selected_session_id, selected_window_id)
            VALUES (1, 1, 'Default Binding', 'native', 0, 'devbox', 1, '$1', '@2');
            INSERT INTO workspace_session_name_metadata
                (binding_id, session_id, cwd, generated_name, explicit)
            VALUES (1, '$1', '/repo', 'repo', 0);",
        )
        .expect("create old single-binding schema");

        let store = WorkspaceStore::for_config_path(&config_path);
        let conn = open_db(store.path()).expect("open migrated database");
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (1, 'Remote', 'tmux', 1)",
            [],
        )
        .expect("single-binding constraint removed");
        let metadata_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_session_name_metadata
                 WHERE binding_id = 1 AND session_id = '$1'",
                [],
                |row| row.get(0),
            )
            .expect("preserved metadata count");
        let migrated_plans =
            load_session_launch_plans(&config_path, 1).expect("migrated launch plan");
        let preserved_binding: (Option<String>, bool, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT remote, unavailable, selected_session_id, selected_window_id
                 FROM workspace_bindings WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, i64>(1)? != 0,
                        row.get(2)?,
                        row.get(3)?,
                    ))
                },
            )
            .expect("preserved binding state");

        assert_eq!(store.bindings().len(), 1);
        assert_eq!(metadata_count, 1);
        assert_eq!(
            preserved_binding,
            (
                Some("devbox".to_owned()),
                true,
                Some("$1".to_owned()),
                Some("@2".to_owned())
            )
        );
        assert_eq!(
            migrated_plans,
            vec![("$1".to_owned(), simple_session_launch_plan("$1", "/repo"))]
        );
    }
    #[test]
    fn migrated_launch_plan_is_not_recreated_after_rename_and_reopen() {
        let config_path = temp_config_path("migration-rename-reopen");
        let db_path = sqlite_path(&config_path);
        let conn = open_db(&db_path).expect("open old workspace database");
        conn.execute_batch(
            "CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL UNIQUE REFERENCES workspace_spaces(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL,
                remote TEXT,
                unavailable INTEGER NOT NULL DEFAULT 0,
                selected_session_id TEXT,
                selected_window_id TEXT
            );
            CREATE TABLE workspace_session_name_metadata (
                binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                generated_name TEXT NOT NULL,
                explicit INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(binding_id, session_id)
            );
            INSERT INTO workspace_spaces (id, name, position)
            VALUES (1, 'Default Space', 0);
            INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status)
            VALUES (1, 1, 'Default Binding', 'native', 0);
            INSERT INTO workspace_session_name_metadata
                (binding_id, session_id, cwd, generated_name, explicit)
            VALUES (1, '$7', '/repo', 'repo', 0);",
        )
        .expect("create old workspace schema");
        drop(conn);

        let store = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(
            load_session_launch_plans(&config_path, 1).expect("load migrated plan"),
            vec![("$7".to_owned(), simple_session_launch_plan("$7", "/repo"))]
        );
        let conn = open_db(store.path()).expect("open migrated workspace database");
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_migrations
                 WHERE name = 'legacy-session-launch-plans-v1'",
                [],
                |row| row.get(0),
            )
            .expect("read launch-plan migration marker");
        assert_eq!(migration_count, 1);
        conn.execute(
            "UPDATE workspace_session_name_metadata
             SET session_name = 'release', display_name = 'release', explicit = 1
             WHERE binding_id = 1 AND session_id = '$7'",
            [],
        )
        .expect("record explicit rename metadata");
        drop(conn);

        assert!(
            rename_session_membership_and_launch_plans(&config_path, 1, "$7", "release", &["$7"],)
                .expect("rename migrated plan")
        );

        let reopened = WorkspaceStore::for_config_path(&config_path);
        let plans = load_session_launch_plans(&config_path, 1).expect("reload renamed plan");
        assert_eq!(
            plans,
            vec![(
                "release".to_owned(),
                simple_session_launch_plan("release", "/repo")
            )]
        );
        assert!(!plans.iter().any(|(session_id, _)| session_id == "$7"));
        assert_eq!(reopened.bindings().len(), 1);
        let conn = open_db(reopened.path()).expect("reopen migrated database");
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM workspace_migrations
                 WHERE name = 'legacy-session-launch-plans-v1'",
                [],
                |row| row.get(0),
            )
            .expect("read retained launch-plan migration marker");
        assert_eq!(migration_count, 1);
    }

    #[test]
    fn committed_session_rename_is_idempotently_recognized() {
        let config_path = temp_config_path("rename-recovery");
        let store = WorkspaceStore::for_config_path(&config_path);
        let binding_id = store
            .binding()
            .expect("default binding")
            .mux_scope()
            .binding_id()
            .persistence_value();
        let conn = open_db(store.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )
        .expect("insert session group");
        let group_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
             VALUES (?1, 'alpha', ?2, 0)",
            params![binding_id, group_id],
        )
        .expect("insert source membership");
        conn.execute(
            "INSERT INTO workspace_session_name_metadata
                (binding_id, session_id, cwd, generated_name, session_name, display_name, explicit)
             VALUES (?1, 's1', '/repo/alpha', 'alpha', 'alpha', 'alpha', 0)",
            [binding_id],
        )
        .expect("insert session metadata");
        persist_session_launch_plan(
            &config_path,
            binding_id,
            &simple_session_launch_plan("alpha", "/repo/alpha"),
        )
        .expect("persist source plan");
        let rename = PendingSessionRename {
            session_id: "s1".to_owned(),
            old_name: "alpha".to_owned(),
            new_name: "release".to_owned(),
            display_name: "release".to_owned(),
            cwd: "/repo/alpha".to_owned(),
        };
        persist_pending_session_rename(&config_path, binding_id, "rename-token", &rename)
            .expect("persist pending rename");
        drop(conn);

        assert!(
            rename_session_membership_and_launch_plans(
                &config_path,
                binding_id,
                &rename.old_name,
                &rename.new_name,
                &[rename.session_id.as_str()],
            )
            .expect("commit rename before simulated crash")
        );
        let conn = open_db(store.path()).expect("reopen after committed rename");
        conn.execute(
            "UPDATE workspace_session_name_metadata
             SET session_name = 'release', display_name = 'release', explicit = 1
             WHERE binding_id = ?1 AND session_id = 's1'",
            [binding_id],
        )
        .expect("persist post-rename metadata");
        drop(conn);

        assert_eq!(
            session_rename_persistence_state(&config_path, binding_id, "rename-token", &rename,)
                .expect("inspect committed rename"),
            SessionRenamePersistenceState::AlreadyCommitted
        );
        clear_pending_session_rename(&config_path, binding_id, "rename-token")
            .expect("clear recovered rename");
        assert!(
            load_pending_session_renames(&config_path, binding_id)
                .expect("load pending renames")
                .is_empty()
        );
        assert_eq!(
            load_session_launch_plans(&config_path, binding_id).expect("load committed plan"),
            vec![(
                "release".to_owned(),
                simple_session_launch_plan("release", "/repo/alpha")
            )]
        );
    }

    #[test]
    fn ditch_cleanup_rolls_back_membership_when_plan_cleanup_fails() {
        let config_path = temp_config_path("ditch-atomicity");
        let store = WorkspaceStore::for_config_path(&config_path);
        let binding_id = store
            .binding()
            .expect("default binding")
            .mux_scope()
            .binding_id()
            .persistence_value();
        let db_path = sqlite_path(&config_path);
        let conn = open_db(&db_path).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )
        .expect("insert session group");
        let group_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
             VALUES (?1, '$1', ?2, 0)",
            params![binding_id, group_id],
        )
        .expect("insert session membership");
        persist_session_launch_plan(
            &config_path,
            binding_id,
            &simple_session_launch_plan("$1", "/repo"),
        )
        .expect("persist launch plan");
        conn.execute(
            "INSERT INTO workspace_session_launch_plans (binding_id, session_id, plan)
             VALUES (?1, 'malformed', '{')",
            [binding_id],
        )
        .expect("inject second-step failure");

        assert!(
            remove_session_membership_and_launch_plan(&config_path, binding_id, "$1", &["$1"])
                .is_err()
        );

        let reopened = open_db(&db_path).expect("reopen workspace database");
        let membership_count: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM workspace_sessions
                 WHERE binding_id = ?1 AND name = '$1'",
                [binding_id],
                |row| row.get(0),
            )
            .expect("read membership");
        let plan_count: i64 = reopened
            .query_row(
                "SELECT COUNT(*) FROM workspace_session_launch_plans
                 WHERE binding_id = ?1 AND session_id = '$1'",
                [binding_id],
                |row| row.get(0),
            )
            .expect("read launch plan");
        assert_eq!(membership_count, 1);
        assert_eq!(plan_count, 1);
    }

    #[test]
    fn renaming_membership_rekeys_launch_plan_atomically() {
        let config_path = temp_config_path("rename-launch-plan");
        let store = WorkspaceStore::for_config_path(&config_path);
        let binding_id = store
            .binding()
            .expect("default binding")
            .mux_scope()
            .binding_id()
            .persistence_value();
        let conn = open_db(store.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )
        .expect("insert session group");
        let group_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
             VALUES (?1, '$1', ?2, 0)",
            params![binding_id, group_id],
        )
        .expect("insert session membership");
        persist_session_launch_plan(
            &config_path,
            binding_id,
            &simple_session_launch_plan("$1", "/repo"),
        )
        .expect("persist launch plan");

        assert!(
            rename_session_membership_and_launch_plans(
                &config_path,
                binding_id,
                "$1",
                "release",
                &["$1"],
            )
            .expect("rename membership and plan")
        );

        let reopened = open_db(store.path()).expect("reopen workspace database");
        let membership: String = reopened
            .query_row(
                "SELECT name FROM workspace_sessions
                 WHERE binding_id = ?1",
                [binding_id],
                |row| row.get(0),
            )
            .expect("read renamed membership");
        assert_eq!(membership, "release");
        assert_eq!(
            load_session_launch_plans(&config_path, binding_id).expect("load renamed plan"),
            vec![(
                "release".to_owned(),
                simple_session_launch_plan("release", "/repo")
            )]
        );
    }

    #[test]
    fn reopening_keeps_scoped_metadata_when_legacy_tables_change() {
        let config_path = temp_config_path("repeat");
        let db_path = sqlite_path(&config_path);
        let mut conn = open_db(&db_path).expect("open legacy database");
        let tx = conn.transaction().expect("legacy transaction");
        tx.execute_batch(
            "CREATE TABLE session_groups (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE sessions (
                name TEXT PRIMARY KEY,
                group_id INTEGER NOT NULL,
                position INTEGER NOT NULL
            );
            CREATE TABLE session_name_metadata (
                session_id TEXT PRIMARY KEY,
                cwd TEXT NOT NULL,
                generated_name TEXT NOT NULL,
                explicit INTEGER NOT NULL
            );",
        )
        .expect("create legacy schema");
        tx.execute(
            "INSERT INTO session_groups (name, position) VALUES (?1, 0)",
            ["project"],
        )
        .expect("insert legacy group");
        let group_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO sessions (name, group_id, position) VALUES (?1, ?2, 0)",
            params!["project/main", group_id],
        )
        .expect("insert legacy session");
        tx.execute(
            "INSERT INTO session_name_metadata (session_id, cwd, generated_name, explicit)
             VALUES (?1, ?2, ?3, ?4)",
            params!["$1", "/repo", "project/main", 0_i64],
        )
        .expect("insert legacy name");
        tx.execute(
            "INSERT INTO session_name_metadata (session_id, cwd, generated_name, explicit)
             VALUES (?1, ?2, ?3, ?4)",
            params!["$2", "/release", "project/release", 1_i64],
        )
        .expect("insert explicit legacy name");
        tx.commit().expect("commit legacy state");

        let first = WorkspaceStore::for_config_path(&config_path);
        let binding_id = first.binding_id().expect("default binding");
        let conn = open_db(&db_path).expect("open scoped database");
        let imported_name: String = conn
            .query_row(
                "SELECT name FROM workspace_sessions WHERE binding_id = ?1",
                [binding_id],
                |row| row.get(0),
            )
            .expect("imported session");
        assert_eq!(imported_name, "project/main");
        let mut order = crate::session_order::SessionOrderStore::for_binding(
            &config_path,
            binding_id,
            crate::session_order::BackendConnectionNamespace::new(
                MultiplexerBackendConfig::Native,
                None,
            ),
        )
        .expect("open imported session order");
        assert_eq!(
            order
                .sync_sessions(["project/main"])
                .expect("sync imported session order"),
            vec!["project/main"]
        );
        let mut names =
            crate::session_names::SessionNameStore::for_binding(&config_path, binding_id);
        let name = names
            .observe_session("$1", "project/main", "/repo")
            .expect("imported generated name");
        assert!(!name.explicit);
        assert_eq!(name.generated_name, "project/main");
        let explicit_name = names
            .observe_session("$2", "release", "/release")
            .expect("imported explicit name");
        assert!(explicit_name.explicit);
        assert_eq!(explicit_name.generated_name, "project/release");

        conn.execute(
            "UPDATE sessions SET name = 'changed' WHERE name = 'project/main'",
            [],
        )
        .expect("mutate legacy session");
        conn.execute(
            "UPDATE session_name_metadata SET generated_name = 'changed' WHERE session_id = '$1'",
            [],
        )
        .expect("mutate legacy metadata");

        let reopened = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(reopened.binding_id(), Some(binding_id));
        let conn = open_db(&db_path).expect("open unchanged scoped database");
        let scoped_name: String = conn
            .query_row(
                "SELECT name FROM workspace_sessions WHERE binding_id = ?1",
                [binding_id],
                |row| row.get(0),
            )
            .expect("scoped session");
        let generated_name: String = conn
            .query_row(
                "SELECT generated_name FROM workspace_session_name_metadata WHERE binding_id = ?1",
                [binding_id],
                |row| row.get(0),
            )
            .expect("scoped metadata");

        assert_eq!(scoped_name, "project/main");
        assert_eq!(generated_name, "project/main");
    }

    #[test]
    fn host_snapshot_round_trips_window_and_binding_selection() {
        let config_path = temp_config_path("host-snapshot");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let first = store.spaces()[0].id();
        let second = store
            .create_space(
                "Review",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Inherit,
                },
                &MultiplexerConfig::default(),
            )
            .expect("create second space")
            .expect("space");
        let scope = second.bindings()[0].mux_scope();

        store
            .set_selected_space("window-a", second.id())
            .expect("select second space");
        store
            .set_selected_space("window-b", first)
            .expect("select first space");
        assert!(
            store
                .set_binding_restore_state(scope, true, Some("$review"), Some("@2"))
                .expect("persist binding state")
        );

        let reopened = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(
            reopened.selected_space("window-a").expect("window a"),
            Some(second.id())
        );
        assert_eq!(
            reopened.selected_space("window-b").expect("window b"),
            Some(first)
        );
        let binding = &reopened.spaces()[1].bindings()[0];
        assert!(binding.unavailable());
        let selection = binding.selection().expect("selection");
        assert_eq!(selection.session_id(), "$review");
        assert_eq!(selection.window_id(), Some("@2"));
    }

    #[test]
    fn unavailable_binding_metadata_survives_reopen_and_reconnect() {
        let config_path = temp_config_path("unavailable");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let scope = store.binding().expect("binding").mux_scope();
        store
            .set_binding_restore_state(scope, true, Some("$1"), None)
            .expect("mark unavailable");

        let mut reopened = WorkspaceStore::for_config_path(&config_path);
        assert!(reopened.binding().expect("binding").unavailable());
        reopened
            .set_binding_restore_state(scope, false, Some("$1"), Some("@1"))
            .expect("mark available");

        let restored = WorkspaceStore::for_config_path(&config_path);
        let binding = restored.binding().expect("binding");
        assert!(!binding.unavailable());
        assert_eq!(
            binding
                .selection()
                .and_then(WorkspaceBindingSelection::window_id),
            Some("@1")
        );
    }

    #[test]
    fn future_snapshot_revision_fails_without_mutating_the_database() {
        let config_path = temp_config_path("future-revision");
        let store = WorkspaceStore::for_config_path(&config_path);
        let path = store.path().to_path_buf();
        let conn = open_db(&path).expect("open workspace database");
        conn.pragma_update(None, "user_version", WORKSPACE_SNAPSHOT_REVISION + 1)
            .expect("set future revision");
        let before: i64 = conn
            .query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
                row.get(0)
            })
            .expect("space count");
        drop(conn);

        assert!(WorkspaceStore::load_or_migrate(&path).is_err());
        assert!(WorkspaceStore::try_for_config_path(&config_path).is_err());
        let conn = open_db(&path).expect("reopen workspace database");
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM workspace_spaces", [], |row| {
                row.get(0)
            })
            .expect("space count");
        assert_eq!(after, before);
    }

    #[test]
    fn deleted_space_remote_identity_is_not_reused() {
        let config_path = temp_config_path("remote-id");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let first = store
            .create_space(
                "First",
                "folder",
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride::default(),
                &MultiplexerConfig::default(),
            )
            .unwrap()
            .unwrap();
        let first_remote_id = first.remote_id().to_owned();
        assert!(store.delete_space(first.id()).unwrap());
        let second = store
            .create_space(
                "Second",
                "folder",
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride::default(),
                &MultiplexerConfig::default(),
            )
            .unwrap()
            .unwrap();

        assert_ne!(second.remote_id(), first_remote_id);
    }
    #[test]
    fn update_space_with_namespace_rolls_back_on_destination_conflict() {
        let config_path = temp_config_path("atomic-namespace-update");
        let mut store = WorkspaceStore::for_config_path(&config_path);
        let first_space = store.spaces()[0].clone();
        let first_binding = store.binding_id().expect("default binding");
        let second_space = store
            .create_space(
                "Second",
                "folder",
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Local,
                },
                &MultiplexerConfig::default(),
            )
            .expect("create second space")
            .expect("second space");
        let second_binding = second_space.bindings()[0]
            .scope
            .binding_id()
            .persistence_value();
        let source_namespace = crate::session_order::BackendConnectionNamespace::new(
            MultiplexerBackendConfig::Native,
            None,
        );
        let destination_namespace = crate::session_order::BackendConnectionNamespace::new(
            MultiplexerBackendConfig::Tmux,
            None,
        );
        crate::session_order::SessionOrderStore::for_binding(
            &config_path,
            first_binding,
            source_namespace.clone(),
        )
        .expect("open source session order")
        .add_session("dev")
        .expect("add source session");
        crate::session_order::SessionOrderStore::for_binding(
            &config_path,
            second_binding,
            destination_namespace.clone(),
        )
        .expect("open destination session order")
        .add_session("dev")
        .expect("add destination session");

        let result = store.update_space_with_namespace(
            WorkspaceSpaceUpdate {
                id: first_space.id(),
                name: "Changed",
                icon: "changed",
                color: [0xFF, 0x00, 0x00],
                tint_sidebar: true,
                mux: SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Local,
                },
            },
            WorkspaceNamespaceUpdateContext {
                binding_id: first_binding,
                namespace: &destination_namespace.persistence_key(),
            },
        );
        assert!(
            result
                .expect_err("destination namespace conflict must fail")
                .to_string()
                .contains("already owned by another binding")
        );

        let reopened = WorkspaceStore::for_config_path(&config_path);
        let unchanged = reopened
            .spaces()
            .iter()
            .find(|space| space.id() == first_space.id())
            .expect("source space");
        assert_eq!(unchanged.name(), first_space.name());
        assert_eq!(unchanged.icon(), first_space.icon());
        assert_eq!(unchanged.color(), first_space.color());
        assert_eq!(unchanged.tint_sidebar(), first_space.tint_sidebar());
        assert_eq!(unchanged.bindings()[0].backend_override(), None);
        let conn = open_db(&sqlite_path(&config_path)).expect("open workspace database");
        let namespace: String = conn
            .query_row(
                "SELECT namespace FROM workspace_session_namespaces WHERE binding_id = ?1",
                [first_binding],
                |row| row.get(0),
            )
            .expect("source namespace");
        assert_eq!(namespace, source_namespace.persistence_key());
    }
}
