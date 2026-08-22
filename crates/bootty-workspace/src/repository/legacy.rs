#![allow(clippy::assigning_clones)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) fn migrate_legacy_metadata(
    tx: &Transaction<'_>,
    binding_id: i64,
    path: &Path,
) -> rusqlite::Result<()> {
    let imported_sessions = if table_exists(tx, "session_groups")? && table_exists(tx, "sessions")?
    {
        // Imported sessions get a provisional identity, the same as a revision 3 upgrade: the
        // first successful refresh finds each by name and stamps a real one.
        tx.execute(
            "INSERT INTO workspace_sessions
                (identity, binding_id, backend_name, display_name, explicit, cwd, position)
             SELECT
                 'legacy:' || ?1 || ':' || old_session.name,
                 ?1,
                 old_session.name,
                 COALESCE(metadata.generated_name, ''),
                 COALESCE(metadata.explicit, 0),
                 COALESCE(metadata.cwd, ''),
                 ROW_NUMBER() OVER (ORDER BY old_group.position, old_session.position) - 1
             FROM sessions old_session
             JOIN session_groups old_group ON old_group.id = old_session.group_id
             LEFT JOIN session_name_metadata metadata
                 ON metadata.generated_name = old_session.name
                 OR metadata.session_id = old_session.name",
            [binding_id],
        )? > 0
    } else {
        false
    };
    if !imported_sessions {
        migrate_legacy_order_file(tx, binding_id, path)?;
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
    for (position, session) in groups
        .iter()
        .flat_map(|group| group.sessions.iter())
        .enumerate()
    {
        tx.execute(
            "INSERT INTO workspace_sessions
                (identity, binding_id, backend_name, position)
             VALUES ('legacy:' || ?1 || ':' || ?2, ?1, ?2, ?3)",
            params![binding_id, session, position as i64],
        )?;
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
