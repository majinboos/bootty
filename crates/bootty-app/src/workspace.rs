use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    config::{MultiplexerBackendConfig, MultiplexerConfig},
    mux::controller::{BindingId, MuxScope, SpaceId},
};

const DEFAULT_SPACE_NAME: &str = "Default Space";
const DEFAULT_BINDING_NAME: &str = "Default Binding";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceBinding {
    scope: MuxScope,
    name: String,
    multiplexer: MultiplexerConfig,
}

impl WorkspaceBinding {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }
    pub(crate) fn multiplexer_config(&self) -> MultiplexerConfig {
        self.multiplexer.clone()
    }

    pub(crate) fn mux_scope(&self) -> MuxScope {
        self.scope
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceStore {
    path: PathBuf,
    bindings: Vec<WorkspaceBinding>,
}

impl WorkspaceStore {
    pub(crate) fn for_config_path(config_path: &Path, config: &MultiplexerConfig) -> Self {
        let path = sqlite_path(config_path);
        let bindings = Self::load_or_migrate(&path, config).unwrap_or_default();
        Self { path, bindings }
    }

    pub(crate) fn binding(&self) -> Option<&WorkspaceBinding> {
        self.bindings.first()
    }

    pub(crate) fn bindings(&self) -> &[WorkspaceBinding] {
        &self.bindings
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn binding_id(&self) -> Option<i64> {
        self.binding()
            .map(|binding| binding.scope.binding_id().persistence_value())
    }

    fn load_or_migrate(
        path: &Path,
        config: &MultiplexerConfig,
    ) -> rusqlite::Result<Vec<WorkspaceBinding>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        let mut conn = open_db(path)?;
        migrate_workspace_binding_cardinality(&conn)?;
        let tx = conn.transaction()?;
        create_workspace_schema(&tx)?;
        let bindings = match load_bindings(&tx)? {
            bindings if !bindings.is_empty() => bindings,
            _ => vec![create_default_binding(&tx, path, config)?],
        };
        tx.commit()?;
        Ok(bindings)
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
            name TEXT NOT NULL,
            position INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS workspace_bindings (
            id INTEGER PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            backend TEXT NOT NULL,
            hide_tmux_status INTEGER NOT NULL
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
            explicit INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(binding_id, session_id)
        );",
    )
}

fn load_bindings(tx: &Transaction<'_>) -> rusqlite::Result<Vec<WorkspaceBinding>> {
    let mut statement = tx.prepare(
        "SELECT s.id, b.id, b.name, b.backend, b.hide_tmux_status
         FROM workspace_spaces s
         JOIN workspace_bindings b ON b.space_id = s.id
         WHERE s.id = (
             SELECT id FROM workspace_spaces ORDER BY position, id LIMIT 1
         )
         ORDER BY b.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(WorkspaceBinding {
            scope: MuxScope::new(
                SpaceId::from_persistence(row.get(0)?),
                BindingId::from_persistence(row.get(1)?),
            ),
            name: row.get(2)?,
            multiplexer: MultiplexerConfig {
                backend: backend_from_storage(&row.get::<_, String>(3)?),
                hide_tmux_status: row.get::<_, i64>(4)? != 0,
            },
        })
    })?;
    rows.collect()
}

fn create_default_binding(
    tx: &Transaction<'_>,
    path: &Path,
    config: &MultiplexerConfig,
) -> rusqlite::Result<WorkspaceBinding> {
    tx.execute(
        "INSERT INTO workspace_spaces (name, position) VALUES (?1, 0)",
        [DEFAULT_SPACE_NAME],
    )?;
    let space_id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            space_id,
            DEFAULT_BINDING_NAME,
            backend_to_storage(config.backend),
            i64::from(config.hide_tmux_status),
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
        multiplexer: config.clone(),
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
                 (binding_id, session_id, cwd, generated_name, explicit)
             SELECT ?1, session_id, cwd, generated_name, explicit
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

fn legacy_order_paths(database_path: &Path) -> [PathBuf; 2] {
    let config_dir = database_path.parent().unwrap_or_else(|| Path::new("."));
    let bootty_legacy = config_dir.join("session-order");
    let tmux_legacy = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config/tmux/session-order");
    [bootty_legacy, tmux_legacy]
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

fn backend_to_storage(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}

fn backend_from_storage(backend: &str) -> MultiplexerBackendConfig {
    match backend {
        "rmux" => MultiplexerBackendConfig::Rmux,
        "tmux" => MultiplexerBackendConfig::Tmux,
        "zellij" => MultiplexerBackendConfig::Zellij,
        _ => MultiplexerBackendConfig::Native,
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
    fn fresh_configuration_creates_default_space_and_binding() {
        let config_path = temp_config_path("fresh");
        let config = MultiplexerConfig {
            backend: MultiplexerBackendConfig::Tmux,
            hide_tmux_status: true,
        };

        let store = WorkspaceStore::for_config_path(&config_path, &config);
        let binding = store.binding().expect("default binding");
        assert_eq!(binding.multiplexer_config(), config);

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

        let reopened = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());
        assert_eq!(
            reopened
                .binding()
                .expect("persisted default binding")
                .multiplexer_config(),
            config
        );
    }

    #[test]
    fn reopening_loads_multiple_bindings_in_the_same_space() {
        let config_path = temp_config_path("multiple-bindings");
        let store = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());
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
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Other Space"],
        )
        .expect("insert other space");
        let other_space_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![other_space_id, "Other Space Binding", "zellij", 0_i64],
        )
        .expect("insert other space binding");

        let reopened = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());

        assert_eq!(reopened.bindings().len(), 2);
        assert_eq!(reopened.bindings()[0].name(), DEFAULT_BINDING_NAME);
        assert_eq!(reopened.bindings()[1].name(), "Remote");
        assert_eq!(
            reopened.bindings()[1].multiplexer_config(),
            MultiplexerConfig {
                backend: MultiplexerBackendConfig::Tmux,
                hide_tmux_status: true,
            }
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
                hide_tmux_status INTEGER NOT NULL
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
            VALUES (1, '$1', '/repo', 'repo', 0);",
        )
        .expect("create old single-binding schema");

        let store = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());
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

        assert_eq!(store.bindings().len(), 1);
        assert_eq!(metadata_count, 1);
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

        let first = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());
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
        let config = MultiplexerConfig::default();
        let mut order = crate::session_order::SessionOrderStore::for_config_path_with_multiplexer(
            &config_path,
            &config,
        );
        assert_eq!(order.sync_sessions(["project/main"]), vec!["project/main"]);
        let mut names =
            crate::session_names::SessionNameStore::lazy_for_config_path_with_multiplexer(
                &config_path,
                &config,
            );
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

        let reopened = WorkspaceStore::for_config_path(&config_path, &MultiplexerConfig::default());
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
}
