#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) fn load_spaces(tx: &Transaction<'_>) -> rusqlite::Result<Vec<WorkspaceSpace>> {
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
        let binding_id = row.get::<_, i64>(7)?;
        if space_id <= 0 || binding_id <= 0 {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let backend = backend_from_storage(&row.get::<_, String>(9)?)?;
        let remote = remote_from_storage(row.get::<_, Option<String>>(14)?.as_deref())?;
        let color = color_from_hex(&row.get::<_, String>(4)?)
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
        let tint_sidebar = bool_from_storage(row.get::<_, i64>(5)?)?;
        let hide_tmux_status = bool_from_storage(row.get::<_, i64>(10)?)?;
        let unavailable = bool_from_storage(row.get::<_, i64>(11)?)?;
        let session_id = row.get::<_, Option<String>>(12)?;
        let window_id = row.get::<_, Option<String>>(13)?;
        if session_id.as_deref().is_some_and(str::is_empty)
            || (session_id.is_none() && window_id.is_some())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        Ok((
            space_id,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            color,
            tint_sidebar,
            row.get::<_, i64>(6)?,
            WorkspaceBinding {
                scope: MuxScope::new(
                    SpaceId::from_persistence(space_id),
                    BindingId::from_persistence(binding_id),
                ),
                name: row.get(8)?,
                backend_override: backend,
                remote_override: remote,
                hide_tmux_status,
                unavailable,
                selection: session_id.map(|session_id| WorkspaceBindingSelection {
                    session_id,
                    window_id,
                }),
                sessions: SessionMembership::default(),
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
    if spaces.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let binding_ids = spaces
        .iter()
        .flat_map(|space| space.bindings.iter())
        .map(|binding| binding.scope.binding_id().persistence_value())
        .collect::<HashSet<_>>();
    let (stored_spaces, stored_bindings) = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM workspace_spaces),
                (SELECT COUNT(*) FROM workspace_bindings)",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    if spaces.len() as i64 != stored_spaces || binding_ids.len() as i64 != stored_bindings {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let mut sessions = HashMap::<i64, Vec<WorkspaceSession>>::new();
    let mut identities = HashSet::new();
    let mut statement = tx.prepare(
        "SELECT identity, binding_id, backend_name, display_name, explicit, cwd, position
         FROM workspace_sessions ORDER BY binding_id, position",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(6)?,
            WorkspaceSession {
                identity: row.get(0)?,
                backend_name: row.get(2)?,
                display_name: row.get(3)?,
                explicit: bool_from_storage(row.get::<_, i64>(4)?)?,
                cwd: row.get(5)?,
            },
        ))
    })? {
        let (binding_id, position, session) = row?;
        if session.identity.is_empty()
            || session.backend_name.is_empty()
            || position < 0
            || !binding_ids.contains(&binding_id)
            || !identities.insert(session.identity.clone())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let claimed = sessions.entry(binding_id).or_default();
        if claimed.len() != position as usize {
            return Err(rusqlite::Error::InvalidQuery);
        }
        claimed.push(session);
    }

    for space in &mut spaces {
        if space.id.persistence_value() <= 0
            || space.name.trim().is_empty()
            || space.icon.trim().is_empty()
            || space.remote_id.trim().is_empty()
            || space.position < 0
            || space.bindings.is_empty()
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for binding in &mut space.bindings {
            let binding_id = binding.scope.binding_id().persistence_value();
            binding.sessions =
                SessionMembership::from_sessions(sessions.remove(&binding_id).unwrap_or_default());
        }
    }
    if !sessions.is_empty() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(spaces)
}

pub(super) fn backend_to_storage(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "inherit",
        Some(MultiplexerBackendConfig::Rmux) => "rmux",
        Some(MultiplexerBackendConfig::Native) => "native",
        Some(MultiplexerBackendConfig::Tmux) => "tmux",
    }
}

/// A binding's remote is stored as JSON rather than as columns of its own: it is one value the app
/// reads and writes whole, and every field it gained would otherwise be another migration.
pub(super) fn remote_to_storage(remote: &SpaceRemoteOverride) -> rusqlite::Result<Option<String>> {
    match remote {
        SpaceRemoteOverride::Inherit => Ok(None),
        remote => serde_json::to_string(remote)
            .map(Some)
            .map_err(|_| rusqlite::Error::InvalidQuery),
    }
}

pub(super) fn remote_from_storage(stored: Option<&str>) -> rusqlite::Result<SpaceRemoteOverride> {
    let Some(stored) = stored else {
        return Ok(SpaceRemoteOverride::Inherit);
    };
    serde_json::from_str(stored)
        .or_else(|_| serde_json::from_str(stored).map(SpaceRemoteOverride::Inline))
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

pub(super) fn nonempty_trimmed(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub(super) fn color_to_hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02X}{green:02X}{blue:02X}")
}

pub(super) fn color_from_hex(value: &str) -> Option<[u8; 3]> {
    let value = value.strip_prefix('#')?;
    let rgb = u32::from_str_radix(value, 16).ok()?;
    (value.len() == 6).then_some([(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8])
}

pub(super) fn backend_from_storage(
    backend: &str,
) -> rusqlite::Result<Option<MultiplexerBackendConfig>> {
    match backend {
        "inherit" => Ok(None),
        "rmux" => Ok(Some(MultiplexerBackendConfig::Rmux)),
        "native" => Ok(Some(MultiplexerBackendConfig::Native)),
        "tmux" => Ok(Some(MultiplexerBackendConfig::Tmux)),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

pub(super) fn bool_from_storage(value: i64) -> rusqlite::Result<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
