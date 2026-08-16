use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WorkspaceSchemaKind {
    Fresh,
    LegacyTables,
    LegacyWorkspace,
    Current,
}

impl WorkspaceSchemaKind {
    pub(super) fn uses_legacy_binding_cardinality(self) -> bool {
        matches!(self, Self::LegacyWorkspace)
    }

    pub(super) fn allows_default_creation(self) -> bool {
        matches!(self, Self::Fresh | Self::LegacyTables)
    }
}

pub(super) fn classify_schema(
    conn: &Connection,
    revision: i64,
) -> rusqlite::Result<WorkspaceSchemaKind> {
    let tables = user_tables(conn)?;
    if tables.is_empty() {
        return Ok(WorkspaceSchemaKind::Fresh);
    }

    let has_spaces = tables.contains("workspace_spaces");
    let has_bindings = tables.contains("workspace_bindings");
    if !has_spaces && !has_bindings {
        let legacy = ["session_groups", "sessions", "session_name_metadata"]
            .iter()
            .all(|table| tables.contains(*table));
        return legacy
            .then_some(WorkspaceSchemaKind::LegacyTables)
            .ok_or(rusqlite::Error::InvalidQuery);
    }
    if !has_spaces || !has_bindings {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let space_columns = table_columns(conn, "workspace_spaces")?;
    let binding_columns = table_columns(conn, "workspace_bindings")?;
    let required_space_columns = ["id", "name", "position"];
    let required_binding_columns = ["id", "space_id", "name", "backend", "hide_tmux_status"];
    if !required_space_columns
        .iter()
        .all(|column| space_columns.contains(*column))
        || !required_binding_columns
            .iter()
            .all(|column| binding_columns.contains(*column))
    {
        return Err(rusqlite::Error::InvalidQuery);
    }

    if revision == WORKSPACE_SNAPSHOT_REVISION {
        for (table, columns) in [
            (
                "workspace_spaces",
                [
                    "id",
                    "remote_id",
                    "name",
                    "icon",
                    "color",
                    "tint_sidebar",
                    "position",
                ]
                .as_slice(),
            ),
            (
                "workspace_bindings",
                [
                    "id",
                    "space_id",
                    "name",
                    "backend",
                    "hide_tmux_status",
                    "remote",
                    "unavailable",
                    "selected_session_id",
                    "selected_window_id",
                ]
                .as_slice(),
            ),
            (
                "workspace_session_groups",
                ["id", "binding_id", "name", "position"].as_slice(),
            ),
            (
                "workspace_sessions",
                ["binding_id", "name", "group_id", "position"].as_slice(),
            ),
            (
                "workspace_session_name_metadata",
                [
                    "binding_id",
                    "session_id",
                    "cwd",
                    "generated_name",
                    "session_name",
                    "display_name",
                    "explicit",
                ]
                .as_slice(),
            ),
            (
                "workspace_window_state",
                ["window_key", "selected_space_id"].as_slice(),
            ),
            (
                "workspace_pending_binding_operations",
                [
                    "space_id",
                    "binding_id",
                    "operation",
                    "session_id",
                    "old_name",
                    "new_name",
                    "display_name",
                    "explicit",
                    "cwd",
                ]
                .as_slice(),
            ),
        ] {
            if !tables.contains(table) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let actual_columns = table_columns(conn, table)?;
            if !columns
                .iter()
                .all(|column| actual_columns.contains(*column))
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        Ok(WorkspaceSchemaKind::Current)
    } else {
        Ok(WorkspaceSchemaKind::LegacyWorkspace)
    }
}

fn user_tables(conn: &Connection) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect()
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}

pub(super) fn migrate_workspace_binding_cardinality(conn: &Connection) -> rusqlite::Result<()> {
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

    let columns = table_columns(conn, "workspace_bindings")?;
    let remote = if columns.contains("remote") {
        "remote"
    } else {
        "NULL"
    };
    let unavailable = if columns.contains("unavailable") {
        "unavailable"
    } else {
        "0"
    };
    let selected_session_id = if columns.contains("selected_session_id") {
        "selected_session_id"
    } else {
        "NULL"
    };
    let selected_window_id = if columns.contains("selected_window_id") {
        "selected_window_id"
    } else {
        "NULL"
    };

    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let migration = conn.execute_batch(&format!(
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
         SELECT id, space_id, name, backend, hide_tmux_status, {remote}, {unavailable},
                {selected_session_id}, {selected_window_id}
         FROM workspace_bindings;
         DROP TABLE workspace_bindings;
         ALTER TABLE workspace_bindings_multiple RENAME TO workspace_bindings;
         COMMIT;"
    ));
    if migration.is_err() {
        let _ = conn.execute_batch("ROLLBACK;");
    }
    let foreign_keys = conn.pragma_update(None, "foreign_keys", "ON");
    migration?;
    foreign_keys
}

pub(super) fn create_workspace_schema(tx: &Transaction<'_>) -> rusqlite::Result<()> {
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
        );
        CREATE TABLE IF NOT EXISTS workspace_pending_binding_operations (
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            session_id TEXT NOT NULL,
            old_name TEXT,
            new_name TEXT,
            display_name TEXT,
            explicit INTEGER,
            cwd TEXT,
            PRIMARY KEY(space_id, binding_id)
        );",
    )
}

pub(super) fn new_remote_space_id(tx: &Transaction<'_>) -> rusqlite::Result<String> {
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

pub(super) fn migrate_workspace_remote_ids(tx: &Transaction<'_>) -> rusqlite::Result<()> {
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

pub(super) fn migrate_workspace_space_icons(tx: &Transaction<'_>) -> rusqlite::Result<()> {
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

pub(super) fn migrate_workspace_space_appearance(tx: &Transaction<'_>) -> rusqlite::Result<()> {
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

pub(super) fn migrate_workspace_session_name_metadata(
    tx: &Transaction<'_>,
) -> rusqlite::Result<()> {
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

pub(super) fn migrate_workspace_snapshot_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
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
