use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use bootty_config::config::{
    BoottyConfig, MultiplexerBackendConfig, SshRemoteConfig, load_config_from_path,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde::Deserialize;

use super::Backend;

pub(super) struct ImportPlan {
    pub(super) spaces: Vec<ImportedSpace>,
}

pub(super) struct ImportedSpace {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) backend: Backend,
    pub(super) position: i64,
    pub(super) sessions: Vec<String>,
}

fn parse_stored_backend(value: &str, binding_id: i64) -> Result<Option<MultiplexerBackendConfig>> {
    let backend = match value {
        "inherit" => None,
        "native" => Some(MultiplexerBackendConfig::Native),
        "rmux" => Some(MultiplexerBackendConfig::Rmux),
        "tmux" => Some(MultiplexerBackendConfig::Tmux),
        _ => bail!("legacy binding {binding_id} has unknown backend {value:?}"),
    };
    Ok(backend)
}

#[derive(Debug)]
struct LegacySpace {
    database_id: i64,
    remote_id: Option<String>,
    name: String,
    position: i64,
}

#[derive(Debug)]
struct LegacyBinding {
    id: i64,
    space_id: i64,
    backend: Option<MultiplexerBackendConfig>,
    local: bool,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "source",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
enum LegacyRemote {
    Inherit,
    Local,
    Profile(LegacyRemoteSpaceRef),
    Inline(SshRemoteConfig),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyRemoteSpaceRef {
    #[serde(alias = "profile-id")]
    profile_id: String,
    #[serde(alias = "remote-space-id")]
    remote_space_id: String,
    #[serde(alias = "remote-space-name")]
    remote_space_name: String,
    backend: MultiplexerBackendConfig,
}

#[derive(Debug)]
struct LegacyGroup {
    id: i64,
    binding_id: i64,
    name: String,
    position: i64,
}

#[derive(Debug)]
struct LegacySession {
    binding_id: i64,
    name: String,
    position: i64,
}

pub(super) fn load(config_path: &Path, database_path: &Path) -> Result<ImportPlan> {
    let config = load_config_from_path(config_path)
        .with_context(|| format!("load Bootty config {}", config_path.display()))?;
    let mut connection = open_legacy_connection(database_path)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;

    required_table_columns(
        &transaction,
        "workspace_spaces",
        &["id", "remote_id", "name", "position"],
    )?;
    let spaces = load_spaces(&transaction)?;
    // Revision 5 folded each Space's connection into the Space itself, so the Space row is the
    // binding and its id stands in for the binding id everything downstream keys on.
    let bindings = if table_exists(&transaction, "workspace_bindings")? {
        required_table_columns(
            &transaction,
            "workspace_bindings",
            &["id", "space_id", "backend", "remote"],
        )?;
        load_bindings(&transaction, &spaces, &config)?
    } else {
        load_folded_bindings(&transaction, &spaces, &config)?
    };
    let binding_ids = bindings
        .iter()
        .map(|binding| binding.id)
        .collect::<HashSet<_>>();
    let sessions = load_sessions(&transaction, &binding_ids)?;
    let plan = build_plan(spaces, &bindings, &sessions, &config)?;
    transaction.commit()?;
    Ok(plan)
}

fn open_legacy_connection(database_path: &Path) -> Result<Connection> {
    let connection =
        Connection::open_with_flags(database_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open legacy remote catalog {}", database_path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn required_table_columns(
    transaction: &Transaction<'_>,
    table: &str,
    required: &[&str],
) -> Result<HashSet<String>> {
    if !table_exists(transaction, table)? {
        bail!("legacy catalog is missing required table {table}")
    }
    let columns = table_columns(transaction, table)?;
    if let Some(column) = required
        .iter()
        .copied()
        .find(|column| !columns.contains(*column))
    {
        bail!("legacy catalog table {table} is missing required column {column}")
    }
    Ok(columns)
}

fn load_spaces(transaction: &Transaction<'_>) -> Result<Vec<LegacySpace>> {
    let mut statement = transaction.prepare(
        "SELECT id, remote_id, name, position
         FROM workspace_spaces ORDER BY position, id",
    )?;
    let mut database_ids = HashSet::new();
    let mut remote_ids = HashSet::new();
    let mut spaces = Vec::new();
    for row in statement.query_map([], |row| {
        Ok(LegacySpace {
            database_id: row.get(0)?,
            remote_id: row.get(1)?,
            name: row.get(2)?,
            position: row.get(3)?,
        })
    })? {
        let space = row?;
        if space.database_id <= 0 || space.position < 0 {
            bail!("legacy workspace Space has an invalid id or position")
        }
        if !database_ids.insert(space.database_id) {
            bail!("legacy workspace Spaces contain duplicate ids")
        }
        if let Some(remote_id) = &space.remote_id
            && !remote_id.is_empty()
        {
            if space.name.trim().is_empty() || space.name.contains('\0') {
                bail!("legacy workspace Space has an invalid name")
            }
            if !remote_ids.insert(remote_id.clone()) {
                bail!("legacy workspace Spaces contain duplicate remote ids")
            }
        }
        spaces.push(space);
    }
    Ok(spaces)
}

fn load_bindings(
    transaction: &Transaction<'_>,
    spaces: &[LegacySpace],
    config: &BoottyConfig,
) -> Result<Vec<LegacyBinding>> {
    let space_ids = remote_space_ids(spaces);
    if space_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut binding_ids = HashSet::new();
    let mut statement = transaction.prepare(
        "SELECT id, space_id, backend, remote
         FROM workspace_bindings
         WHERE space_id IN (
             SELECT id FROM workspace_spaces
             WHERE remote_id IS NOT NULL AND remote_id != ''
         )
         ORDER BY space_id, id",
    )?;
    let mut bindings = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })? {
        let (id, space_id, backend, remote) = row?;
        if id <= 0 || !space_ids.contains(&space_id) {
            bail!("legacy workspace binding has an invalid Space reference")
        }
        if !binding_ids.insert(id) {
            bail!("legacy workspace bindings contain duplicate ids")
        }
        let backend = parse_stored_backend(&backend, id)?;
        let local = decode_local_placement(remote.as_deref(), config, id)?;
        bindings.push(LegacyBinding {
            id,
            space_id,
            backend,
            local,
        });
    }
    Ok(bindings)
}

/// The connections of a revision 5 database, where each Space holds its own.
fn load_folded_bindings(
    transaction: &Transaction<'_>,
    spaces: &[LegacySpace],
    config: &BoottyConfig,
) -> Result<Vec<LegacyBinding>> {
    required_table_columns(transaction, "workspace_spaces", &["backend", "remote"])?;
    let space_ids = remote_space_ids(spaces);
    if space_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = transaction.prepare(
        "SELECT id, backend, remote
         FROM workspace_spaces
         WHERE remote_id IS NOT NULL AND remote_id != ''
         ORDER BY id",
    )?;
    let mut bindings = Vec::new();
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })? {
        let (space_id, backend, remote) = row?;
        if !space_ids.contains(&space_id) {
            bail!("legacy workspace Space has an invalid connection")
        }
        bindings.push(LegacyBinding {
            id: space_id,
            space_id,
            backend: parse_stored_backend(&backend, space_id)?,
            local: decode_local_placement(remote.as_deref(), config, space_id)?,
        });
    }
    Ok(bindings)
}

fn remote_space_ids(spaces: &[LegacySpace]) -> HashSet<i64> {
    spaces
        .iter()
        .filter(|space| {
            space
                .remote_id
                .as_deref()
                .is_some_and(|remote_id| !remote_id.is_empty())
        })
        .map(|space| space.database_id)
        .collect()
}

fn decode_local_placement(
    stored: Option<&str>,
    config: &BoottyConfig,
    binding_id: i64,
) -> Result<bool> {
    let Some(stored) = stored else {
        return Ok(config.multiplexer.remote.is_none());
    };
    let remote = serde_json::from_str::<LegacyRemote>(stored).with_context(|| {
        format!("decode legacy binding {binding_id} remote placement {stored:?}")
    })?;
    match remote {
        LegacyRemote::Inherit => Ok(config.multiplexer.remote.is_none()),
        LegacyRemote::Local => Ok(true),
        LegacyRemote::Profile(remote) => {
            validate_profile_remote(&remote, binding_id)?;
            Ok(false)
        }
        LegacyRemote::Inline(remote) => {
            validate_inline_remote(&remote, binding_id)?;
            Ok(false)
        }
    }
}

fn validate_profile_remote(remote: &LegacyRemoteSpaceRef, binding_id: i64) -> Result<()> {
    if remote.profile_id.trim().is_empty()
        || remote.remote_space_id.trim().is_empty()
        || remote.remote_space_name.trim().is_empty()
        || !remote.backend.supports_remote()
    {
        bail!("legacy binding {binding_id} has a malformed profile remote placement")
    }
    Ok(())
}

fn validate_inline_remote(remote: &SshRemoteConfig, binding_id: i64) -> Result<()> {
    if remote.host.trim().is_empty() {
        bail!("legacy binding {binding_id} has a malformed inline remote placement")
    }
    Ok(())
}

fn load_sessions(
    transaction: &Transaction<'_>,
    binding_ids: &HashSet<i64>,
) -> Result<HashMap<i64, Vec<String>>> {
    if binding_ids.is_empty() || !table_exists(transaction, "workspace_sessions")? {
        return Ok(HashMap::new());
    }
    // Revision 4 keys sessions by identity and keeps one flat order, with no groups table.
    // Revision 5 keys them by Space, which is the same id by then.
    let columns = table_columns(transaction, "workspace_sessions")?;
    if columns.contains("backend_name") {
        return load_identity_sessions(transaction, binding_ids, columns.contains("space_id"));
    }
    let columns = required_table_columns(
        transaction,
        "workspace_sessions",
        &["binding_id", "name", "position"],
    )?;
    let grouped = columns.contains("group_id");
    if grouped && !table_exists(transaction, "workspace_session_groups")? {
        bail!("legacy workspace_sessions uses groups but workspace_session_groups is missing")
    }
    if grouped {
        load_grouped_sessions(transaction, binding_ids)
    } else {
        load_ungrouped_sessions(transaction, binding_ids)
    }
}

fn load_identity_sessions(
    transaction: &Transaction<'_>,
    binding_ids: &HashSet<i64>,
    keyed_by_space: bool,
) -> Result<HashMap<i64, Vec<String>>> {
    let key = if keyed_by_space {
        "space_id"
    } else {
        "binding_id"
    };
    required_table_columns(
        transaction,
        "workspace_sessions",
        &[key, "backend_name", "position"],
    )?;
    let owned = if keyed_by_space {
        String::from(
            "SELECT id FROM workspace_spaces WHERE remote_id IS NOT NULL AND remote_id != ''",
        )
    } else {
        String::from(
            "SELECT id FROM workspace_bindings
             WHERE space_id IN (
                 SELECT id FROM workspace_spaces
                 WHERE remote_id IS NOT NULL AND remote_id != ''
             )",
        )
    };
    let mut sessions: HashMap<i64, Vec<String>> = HashMap::new();
    let mut statement = transaction.prepare(&format!(
        "SELECT {key}, backend_name
         FROM workspace_sessions
         WHERE {key} IN ({owned})
         ORDER BY {key}, position",
    ))?;
    for row in statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })? {
        let (binding_id, name) = row?;
        if name.is_empty() || name.contains('\0') || !binding_ids.contains(&binding_id) {
            bail!("legacy workspace session has an invalid row")
        }
        sessions.entry(binding_id).or_default().push(name);
    }
    Ok(sessions)
}

fn load_grouped_sessions(
    transaction: &Transaction<'_>,
    binding_ids: &HashSet<i64>,
) -> Result<HashMap<i64, Vec<String>>> {
    required_table_columns(
        transaction,
        "workspace_session_groups",
        &["id", "binding_id", "name", "position"],
    )?;
    let mut groups = HashMap::new();
    let mut group_positions = HashSet::new();
    let mut statement = transaction.prepare(
        "SELECT id, binding_id, name, position
         FROM workspace_session_groups
         WHERE binding_id IN (
             SELECT id FROM workspace_bindings
             WHERE space_id IN (
                 SELECT id FROM workspace_spaces
                 WHERE remote_id IS NOT NULL AND remote_id != ''
             )
         )
         ORDER BY binding_id, position, id",
    )?;
    for row in statement.query_map([], |row| {
        Ok(LegacyGroup {
            id: row.get(0)?,
            binding_id: row.get(1)?,
            name: row.get(2)?,
            position: row.get(3)?,
        })
    })? {
        let group = row?;
        if group.id <= 0
            || group.position < 0
            || group.name.contains('\0')
            || !binding_ids.contains(&group.binding_id)
            || !group_positions.insert((group.binding_id, group.position))
        {
            bail!("legacy workspace session group has an invalid row")
        }
        if groups.insert(group.id, group).is_some() {
            bail!("legacy workspace session groups contain duplicate ids")
        }
    }

    let mut sessions = Vec::new();
    let mut names = HashSet::new();
    let mut positions = HashSet::new();
    let mut statement = transaction.prepare(
        "SELECT binding_id, name, group_id, position
         FROM workspace_sessions
         WHERE binding_id IN (
             SELECT id FROM workspace_bindings
             WHERE space_id IN (
                 SELECT id FROM workspace_spaces
                 WHERE remote_id IS NOT NULL AND remote_id != ''
             )
         )
         ORDER BY binding_id, group_id, position",
    )?;
    for row in statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(2)?,
            LegacySession {
                binding_id: row.get(0)?,
                name: row.get(1)?,
                position: row.get(3)?,
            },
        ))
    })? {
        let (group_id, session) = row?;
        let Some(group) = groups.get(&group_id) else {
            bail!("legacy workspace session references an unknown group")
        };
        if group.binding_id != session.binding_id
            || session.position < 0
            || !binding_ids.contains(&session.binding_id)
            || session.name.is_empty()
            || session.name.contains('\0')
            || !names.insert((session.binding_id, session.name.clone()))
            || !positions.insert((group_id, session.position))
        {
            bail!("legacy workspace session has an invalid row")
        }
        sessions.push((group.position, group.id, session));
    }
    sessions.sort_by_key(|(group_position, group_id, session)| {
        (*group_position, *group_id, session.position)
    });
    let mut ordered = HashMap::new();
    for (_, _, session) in sessions {
        ordered
            .entry(session.binding_id)
            .or_insert_with(Vec::new)
            .push(session.name);
    }
    Ok(ordered)
}

fn load_ungrouped_sessions(
    transaction: &Transaction<'_>,
    binding_ids: &HashSet<i64>,
) -> Result<HashMap<i64, Vec<String>>> {
    let mut ordered = HashMap::new();
    let mut names = HashSet::new();
    let mut positions = HashSet::new();
    let mut statement = transaction.prepare(
        "SELECT binding_id, name, position
         FROM workspace_sessions
         WHERE binding_id IN (
             SELECT id FROM workspace_bindings
             WHERE space_id IN (
                 SELECT id FROM workspace_spaces
                 WHERE remote_id IS NOT NULL AND remote_id != ''
             )
         )
         ORDER BY binding_id, position",
    )?;
    for row in statement.query_map([], |row| {
        Ok(LegacySession {
            binding_id: row.get(0)?,
            name: row.get(1)?,
            position: row.get(2)?,
        })
    })? {
        let session = row?;
        if session.position < 0
            || !binding_ids.contains(&session.binding_id)
            || session.name.is_empty()
            || session.name.contains('\0')
            || !names.insert((session.binding_id, session.name.clone()))
            || !positions.insert((session.binding_id, session.position))
        {
            bail!("legacy workspace session has an invalid row")
        }
        ordered
            .entry(session.binding_id)
            .or_insert_with(Vec::new)
            .push(session.name);
    }
    Ok(ordered)
}

fn build_plan(
    spaces: Vec<LegacySpace>,
    bindings: &[LegacyBinding],
    sessions: &HashMap<i64, Vec<String>>,
    config: &BoottyConfig,
) -> Result<ImportPlan> {
    let mut bindings_by_space = HashMap::<i64, Vec<&LegacyBinding>>::new();
    for binding in bindings {
        bindings_by_space
            .entry(binding.space_id)
            .or_default()
            .push(binding);
    }

    let mut imported_names = HashSet::new();
    let mut imported = Vec::new();
    for space in spaces {
        let Some(remote_id) = space.remote_id.filter(|remote_id| !remote_id.is_empty()) else {
            continue;
        };
        let mut candidates = bindings_by_space
            .get(&space.database_id)
            .into_iter()
            .flatten()
            .filter(|binding| binding.local)
            .filter_map(|binding| {
                destination_backend(binding.backend.unwrap_or(config.multiplexer.backend))
                    .map(|backend| (binding.id, backend))
            });
        let Some((binding_id, backend)) = candidates.next() else {
            continue;
        };
        if candidates.next().is_some() {
            bail!("legacy workspace Space {remote_id:?} has ambiguous local bindings")
        }
        if !imported_names.insert(space.name.clone()) {
            bail!("legacy workspace Spaces contain duplicate imported names")
        }
        imported.push(ImportedSpace {
            id: remote_id,
            name: space.name,
            backend,
            position: space.position,
            sessions: sessions.get(&binding_id).cloned().unwrap_or_default(),
        });
    }
    Ok(ImportPlan { spaces: imported })
}

fn destination_backend(backend: MultiplexerBackendConfig) -> Option<Backend> {
    match backend {
        MultiplexerBackendConfig::Native => None,
        MultiplexerBackendConfig::Rmux => Some(Backend::Rmux),
        MultiplexerBackendConfig::Tmux => Some(Backend::Tmux),
    }
}

fn table_exists(transaction: &Transaction<'_>, name: &str) -> rusqlite::Result<bool> {
    transaction
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()
        .map(|found| found.is_some())
}

fn table_columns(transaction: &Transaction<'_>, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = transaction.prepare(&format!("PRAGMA table_info({table})"))?;
    statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect()
}
