#![allow(clippy::wildcard_imports)]

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

    if !table_has_columns(conn, "workspace_spaces", &["id", "name", "position"])?
        || !table_has_columns(
            conn,
            "workspace_bindings",
            &["id", "space_id", "name", "backend", "hide_tmux_status"],
        )?
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
                "workspace_sessions",
                [
                    "identity",
                    "binding_id",
                    "backend_name",
                    "display_name",
                    "explicit",
                    "cwd",
                    "position",
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
                    "identity",
                    "space_id",
                    "binding_id",
                    "operation",
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
            if !table_has_columns(conn, table, columns)? {
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

fn table_has_columns(conn: &Connection, table: &str, required: &[&str]) -> rusqlite::Result<bool> {
    let columns = table_columns(conn, table)?;
    Ok(required.iter().all(|column| columns.contains(*column)))
}

fn add_column_if_missing(
    tx: &Transaction<'_>,
    columns: &HashSet<String>,
    column: &str,
    sql: &str,
) -> rusqlite::Result<()> {
    if !columns.contains(column) {
        tx.execute(sql, [])?;
    }
    Ok(())
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
    let column_or = |column, fallback| {
        if columns.contains(column) {
            column
        } else {
            fallback
        }
    };
    let remote = column_or("remote", "NULL");
    let unavailable = column_or("unavailable", "0");
    let selected_session_id = column_or("selected_session_id", "NULL");
    let selected_window_id = column_or("selected_window_id", "NULL");

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
    migration.and(foreign_keys)
}

/// Move name-keyed session state onto the identity-keyed table revision 4 uses.
///
/// Each session gets a provisional `legacy:<binding>:<name>` identity. Nothing carries that value,
/// so the first successful refresh finds the session by its name hint and stamps a real one.
///
/// Pending journal rows are dropped: they were keyed by a backend session id nothing can be
/// matched against now, and reconciliation would discard them on the next refresh anyway.
pub(super) fn migrate_workspace_sessions_to_identities(
    tx: &Transaction<'_>,
) -> rusqlite::Result<()> {
    let tables = user_tables(tx)?;
    if !tables.contains("workspace_session_groups") {
        return Ok(());
    }

    tx.execute_batch(
        "CREATE TABLE workspace_sessions_by_identity (
            identity TEXT PRIMARY KEY,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            backend_name TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            explicit INTEGER NOT NULL DEFAULT 0,
            cwd TEXT NOT NULL DEFAULT '',
            position INTEGER NOT NULL,
            UNIQUE(binding_id, position)
        );
        INSERT INTO workspace_sessions_by_identity
            (identity, binding_id, backend_name, display_name, explicit, cwd, position)
        SELECT
            'legacy:' || s.binding_id || ':' || s.name,
            s.binding_id,
            s.name,
            COALESCE(m.display_name, ''),
            COALESCE(m.explicit, 0),
            COALESCE(m.cwd, ''),
            ROW_NUMBER() OVER (
                PARTITION BY s.binding_id ORDER BY g.position, g.id, s.position
            ) - 1
        FROM workspace_sessions s
        JOIN workspace_session_groups g ON g.id = s.group_id
        LEFT JOIN workspace_session_name_metadata m
            ON m.binding_id = s.binding_id
           AND (m.session_name = s.name OR m.generated_name = s.name OR m.session_id = s.name);
        DROP TABLE workspace_sessions;
        DROP TABLE workspace_session_groups;
        DROP TABLE workspace_session_name_metadata;
        ALTER TABLE workspace_sessions_by_identity RENAME TO workspace_sessions;",
    )?;

    // The journal is rebuilt rather than altered: it is keyed by identity now, one row per
    // session instead of one per binding. A database old enough not to have one yet gets the
    // current shape from `create_workspace_schema` instead.
    if tables.contains("workspace_pending_binding_operations") {
        tx.execute_batch("DROP TABLE workspace_pending_binding_operations")?;
    }
    Ok(())
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
        CREATE TABLE IF NOT EXISTS workspace_sessions (
            identity TEXT PRIMARY KEY,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            backend_name TEXT NOT NULL,
            display_name TEXT NOT NULL DEFAULT '',
            explicit INTEGER NOT NULL DEFAULT 0,
            cwd TEXT NOT NULL DEFAULT '',
            position INTEGER NOT NULL,
            UNIQUE(binding_id, position)
        );
        CREATE TABLE IF NOT EXISTS workspace_window_state (
            window_key TEXT PRIMARY KEY,
            selected_space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS workspace_pending_binding_operations (
            identity TEXT PRIMARY KEY,
            space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
            binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
            operation TEXT NOT NULL,
            old_name TEXT,
            new_name TEXT,
            display_name TEXT,
            explicit INTEGER,
            cwd TEXT
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
    let columns = table_columns(tx, "workspace_spaces")?;
    add_column_if_missing(
        tx,
        &columns,
        "remote_id",
        "ALTER TABLE workspace_spaces ADD COLUMN remote_id TEXT",
    )?;
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
    let columns = table_columns(tx, "workspace_spaces")?;
    add_column_if_missing(
        tx,
        &columns,
        "icon",
        "ALTER TABLE workspace_spaces ADD COLUMN icon TEXT NOT NULL DEFAULT 'folder'",
    )
}

pub(super) fn migrate_workspace_space_appearance(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let columns = table_columns(tx, "workspace_spaces")?;
    add_column_if_missing(
        tx,
        &columns,
        "color",
        "ALTER TABLE workspace_spaces ADD COLUMN color TEXT NOT NULL DEFAULT '#7AA2F7'",
    )?;
    add_column_if_missing(
        tx,
        &columns,
        "tint_sidebar",
        "ALTER TABLE workspace_spaces ADD COLUMN tint_sidebar INTEGER NOT NULL DEFAULT 0",
    )
}

pub(super) fn migrate_workspace_snapshot_state(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    let columns = table_columns(tx, "workspace_bindings")?;
    add_column_if_missing(
        tx,
        &columns,
        "unavailable",
        "ALTER TABLE workspace_bindings
         ADD COLUMN unavailable INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        tx,
        &columns,
        "selected_session_id",
        "ALTER TABLE workspace_bindings ADD COLUMN selected_session_id TEXT",
    )?;
    add_column_if_missing(
        tx,
        &columns,
        "selected_window_id",
        "ALTER TABLE workspace_bindings ADD COLUMN selected_window_id TEXT",
    )?;
    add_column_if_missing(
        tx,
        &columns,
        "remote",
        "ALTER TABLE workspace_bindings ADD COLUMN remote TEXT",
    )
}
