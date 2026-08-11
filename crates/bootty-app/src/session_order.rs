use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::Serialize;

use crate::{
    config::{MultiplexerBackendConfig, MultiplexerConfig, SshRemoteConfig},
    mux::{config::selected_backend, controller::MuxScope},
    workspace::{WorkspaceStore, open_db},
};

/// Returns the persistence namespace used by a binding's session membership.
///
/// Native sessions are shared by every binding in a workspace, but each Space owns its
/// membership view. Qualifying native namespaces by Space lets two Spaces refer to the same
/// backend session identity without allowing unrelated bindings in one Space to claim it twice.
pub(crate) fn namespace_for_binding(
    scope: MuxScope,
    config: &MultiplexerConfig,
) -> BackendConnectionNamespace {
    let mut namespace = BackendConnectionNamespace::from_multiplexer(config);
    if selected_backend(config) == MultiplexerBackendConfig::Native {
        namespace.remote_space_id = Some(format!(
            "native-space:{}",
            scope.space_id().persistence_value()
        ));
    }
    namespace
}

/// The durable namespace of one backend connection.
///
/// Binding IDs identify persisted UI state, not the multiplexer endpoint. Initial
/// membership claims are isolated by this value so equal backend-local names can
/// be claimed independently by different connections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BackendConnectionNamespace {
    pub backend: MultiplexerBackendConfig,
    pub remote: Option<SshRemoteConfig>,
    pub remote_space_id: Option<String>,
}

impl BackendConnectionNamespace {
    pub fn new(backend: MultiplexerBackendConfig, remote: Option<SshRemoteConfig>) -> Self {
        Self {
            backend,
            remote,
            remote_space_id: None,
        }
    }

    pub fn from_multiplexer(config: &MultiplexerConfig) -> Self {
        Self {
            backend: selected_backend(config),
            remote: config.remote.clone(),
            remote_space_id: config.remote_space_id.clone(),
        }
    }

    pub(crate) fn persistence_key(&self) -> String {
        serde_json::to_string(self).expect("backend connection namespace is serializable")
    }
}

#[derive(Debug)]
pub struct SessionMembershipConflict {
    pub name: String,
    pub namespace: String,
}

impl std::fmt::Display for SessionMembershipConflict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "session '{}' is already owned by another binding on backend connection {}",
            self.name, self.namespace
        )
    }
}

impl std::error::Error for SessionMembershipConflict {}

fn membership_conflict(name: &str, namespace: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(SessionMembershipConflict {
        name: name.to_owned(),
        namespace: namespace.to_owned(),
    }))
}

fn ensure_namespace_table(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "CREATE TABLE IF NOT EXISTS workspace_session_namespaces (
             binding_id INTEGER PRIMARY KEY
                 REFERENCES workspace_bindings(id) ON DELETE CASCADE,
             namespace TEXT NOT NULL
         )",
        [],
    )?;
    Ok(())
}

fn namespace_conflict(
    tx: &Transaction<'_>,
    binding_id: i64,
    namespace_key: &str,
) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT s.name
         FROM workspace_sessions s
         JOIN workspace_session_namespaces n
           ON n.namespace = ?2 AND n.binding_id != ?1
         JOIN workspace_sessions other
           ON other.binding_id = n.binding_id AND other.name = s.name
         WHERE s.binding_id = ?1
         LIMIT 1",
        params![binding_id, namespace_key],
        |row| row.get::<_, String>(0),
    )
    .optional()
}

fn validate_namespace(
    path: &Path,
    binding_id: i64,
    namespace: &BackendConnectionNamespace,
) -> rusqlite::Result<()> {
    let mut conn = open_db(path)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_namespace_table(&tx)?;
    let namespace_key = namespace.persistence_key();
    if let Some(name) = namespace_conflict(&tx, binding_id, &namespace_key)? {
        return Err(membership_conflict(&name, &namespace_key));
    }
    tx.commit()?;
    Ok(())
}

fn register_namespace(
    path: &Path,
    binding_id: i64,
    namespace: &BackendConnectionNamespace,
) -> rusqlite::Result<()> {
    let mut conn = open_db(path)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_namespace_table(&tx)?;
    let namespace_key = namespace.persistence_key();
    if let Some(name) = namespace_conflict(&tx, binding_id, &namespace_key)? {
        return Err(membership_conflict(&name, &namespace_key));
    }
    tx.execute(
        "INSERT INTO workspace_session_namespaces (binding_id, namespace)
         VALUES (?1, ?2)
         ON CONFLICT(binding_id) DO UPDATE SET namespace = excluded.namespace",
        params![binding_id, namespace_key],
    )?;
    tx.commit()?;
    Ok(())
}

fn session_group(name: &str) -> &str {
    name.split_once('/').map_or("", |(group, _)| group)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionGroup {
    name: String,
    sessions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SessionStore {
    entries: Vec<SessionGroup>,
}

impl SessionStore {
    fn load_sqlite(path: &Path, binding_id: i64) -> rusqlite::Result<Self> {
        let conn = open_db(path)?;
        Self::load_connection(&conn, binding_id)
    }

    fn load_transaction(tx: &Transaction<'_>, binding_id: i64) -> rusqlite::Result<Self> {
        Self::load_connection(tx, binding_id)
    }

    fn load_connection(conn: &Connection, binding_id: i64) -> rusqlite::Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT g.id, g.name, s.name
             FROM workspace_session_groups g
             JOIN workspace_sessions s ON s.group_id = g.id
             WHERE g.binding_id = ?1 AND s.binding_id = ?1
             ORDER BY g.position, s.position",
        )?;
        let rows = stmt.query_map([binding_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut store = Self::default();
        let mut current_group_id = None;
        for row in rows {
            let (group_id, group_name, session_name) = row?;
            if current_group_id != Some(group_id) {
                store.entries.push(SessionGroup {
                    name: group_name,
                    sessions: Vec::new(),
                });
                current_group_id = Some(group_id);
            }
            if let Some(group) = store.entries.last_mut() {
                group.sessions.push(session_name);
            }
        }
        Ok(store)
    }

    fn ordered_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .flat_map(|group| group.sessions.iter().cloned())
            .collect()
    }

    fn existing_names(&self) -> HashSet<String> {
        self.entries
            .iter()
            .flat_map(|group| group.sessions.iter().cloned())
            .collect()
    }

    fn insert_unique(&mut self, name: &str) {
        let group = session_group(name);
        if group.is_empty() {
            self.entries.push(SessionGroup {
                name: String::new(),
                sessions: vec![name.to_owned()],
            });
            return;
        }

        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.name == group) {
            entry.sessions.push(name.to_owned());
        } else if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.sessions.len() == 1 && entry.sessions[0] == group)
        {
            entry.name = group.to_owned();
            entry.sessions.push(name.to_owned());
        } else {
            self.entries.push(SessionGroup {
                name: group.to_owned(),
                sessions: vec![name.to_owned()],
            });
        }
    }

    fn remove(&mut self, name: &str) -> bool {
        let Some((group, session)) = self.find_session(name) else {
            return false;
        };
        self.entries[group].sessions.remove(session);
        if self.entries[group].sessions.is_empty() {
            self.entries.remove(group);
        }
        true
    }

    fn rename_session(&mut self, old: &str, new: &str) -> bool {
        if old == new || self.existing_names().contains(new) {
            return false;
        }
        let Some((entry_index, session_index)) = self.find_session(old) else {
            return false;
        };
        self.entries[entry_index].sessions[session_index] = new.to_owned();
        true
    }

    fn prune(&mut self, alive: &HashSet<&str>) -> bool {
        let mut changed = false;
        for entry in &mut self.entries {
            let before = entry.sessions.len();
            entry
                .sessions
                .retain(|session| alive.contains(session.as_str()));
            changed |= entry.sessions.len() != before;
        }
        let before = self.entries.len();
        self.entries.retain(|entry| !entry.sessions.is_empty());
        changed || self.entries.len() != before
    }

    fn move_session(&mut self, name: &str, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let Some((entry_idx, session_idx)) = self.find_session(name) else {
            return false;
        };

        let entry = &self.entries[entry_idx];
        if entry.sessions.len() > 1 {
            if delta < 0 && session_idx > 0 {
                self.entries[entry_idx]
                    .sessions
                    .swap(session_idx, session_idx - 1);
                return true;
            }
            if delta > 0 && session_idx < entry.sessions.len() - 1 {
                self.entries[entry_idx]
                    .sessions
                    .swap(session_idx, session_idx + 1);
                return true;
            }
        }

        let source = self.entries[entry_idx].sessions[0].clone();
        let target = if delta < 0 {
            self.entries
                .get(entry_idx.saturating_sub(1))
                .and_then(|entry| entry.sessions.first().cloned())
        } else {
            self.entries
                .get(entry_idx + 2)
                .and_then(|entry| entry.sessions.first().cloned())
        };
        self.move_block_before(&source, target.as_deref())
    }

    fn move_block_before(&mut self, source: &str, target: Option<&str>) -> bool {
        let previous = self.entries.clone();
        let Some(source_index) = self
            .entries
            .iter()
            .position(|entry| entry.sessions.first().is_some_and(|name| name == source))
        else {
            return false;
        };

        let entry = self.entries.remove(source_index);
        let insert_index =
            match target {
                Some(target) => {
                    let Some(target_index) = self.entries.iter().position(|entry| {
                        entry.sessions.first().is_some_and(|name| name == target)
                    }) else {
                        self.entries.insert(source_index, entry);
                        return false;
                    };
                    target_index
                }
                None => self.entries.len(),
            };

        self.entries.insert(insert_index, entry);
        self.entries != previous
    }

    /// Moves `source` to sit before `before` (or to the end when `None`). Within one group this
    /// reorders the siblings; across groups a session can't leave its group, so the whole source
    /// group moves before the target's group instead.
    fn move_session_before(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some((src_group, src_idx)) = self.find_session(source) else {
            return false;
        };
        match before {
            Some(before) => {
                let Some((tgt_group, tgt_idx)) = self.find_session(before) else {
                    return false;
                };
                if tgt_group == src_group {
                    let insert_idx = if tgt_idx > src_idx {
                        tgt_idx - 1
                    } else {
                        tgt_idx
                    };
                    if insert_idx == src_idx {
                        return false;
                    }
                    let sessions = &mut self.entries[src_group].sessions;
                    let moved = sessions.remove(src_idx);
                    sessions.insert(insert_idx, moved);
                    true
                } else {
                    let src_leader = self.entries[src_group].sessions[0].clone();
                    let tgt_leader = self.entries[tgt_group].sessions[0].clone();
                    self.move_block_before(&src_leader, Some(&tgt_leader))
                }
            }
            None => {
                let src_leader = self.entries[src_group].sessions[0].clone();
                self.move_block_before(&src_leader, None)
            }
        }
    }

    fn find_session(&self, name: &str) -> Option<(usize, usize)> {
        self.entries
            .iter()
            .enumerate()
            .find_map(|(entry_idx, entry)| {
                entry
                    .sessions
                    .iter()
                    .position(|session| session == name)
                    .map(|session_idx| (entry_idx, session_idx))
            })
    }

    fn save_transaction(&self, tx: &Transaction<'_>, binding_id: i64) -> rusqlite::Result<()> {
        tx.execute(
            "DELETE FROM workspace_sessions WHERE binding_id = ?1",
            [binding_id],
        )?;
        tx.execute(
            "DELETE FROM workspace_session_groups WHERE binding_id = ?1",
            [binding_id],
        )?;
        {
            let mut insert_group = tx.prepare(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 VALUES (?1, ?2, ?3)",
            )?;
            let mut insert_session = tx.prepare(
                "INSERT INTO workspace_sessions (binding_id, name, group_id, position)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (group_position, group) in self.entries.iter().enumerate() {
                insert_group.execute(params![binding_id, group.name, group_position as i64])?;
                let group_id = tx.last_insert_rowid();
                for (session_position, session) in group.sessions.iter().enumerate() {
                    insert_session.execute(params![
                        binding_id,
                        session,
                        group_id,
                        session_position as i64
                    ])?;
                }
            }
        }
        if self.entries.is_empty() {
            tx.execute(
                "INSERT INTO workspace_session_groups (binding_id, name, position)
                 VALUES (?1, '', 0)",
                [binding_id],
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SessionOrderStore {
    path: PathBuf,
    binding_id: i64,
    namespace: BackendConnectionNamespace,
    store: SessionStore,
    membership_initialized: bool,
    #[cfg(test)]
    next_save_failure: Option<String>,
}

impl SessionOrderStore {
    pub fn for_config_path(
        config_path: &Path,
        namespace: BackendConnectionNamespace,
    ) -> rusqlite::Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        let binding_id = workspace
            .binding_id()
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Self::load(workspace.path().to_path_buf(), binding_id, namespace, true)
    }

    pub fn for_binding(
        config_path: &Path,
        binding_id: i64,
        namespace: BackendConnectionNamespace,
    ) -> rusqlite::Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        Self::load(workspace.path().to_path_buf(), binding_id, namespace, true)
    }

    pub(crate) fn for_binding_preflight(
        config_path: &Path,
        binding_id: i64,
        namespace: BackendConnectionNamespace,
    ) -> rusqlite::Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        Self::load(workspace.path().to_path_buf(), binding_id, namespace, false)
    }

    /// Register a batch of binding namespaces in one database transaction. Callers use this
    /// after every replacement has passed preflight so a later conflict or I/O error cannot leave
    /// a partially applied namespace set behind.
    pub(crate) fn register_namespaces(
        config_path: &Path,
        namespaces: impl IntoIterator<Item = (i64, BackendConnectionNamespace)>,
    ) -> rusqlite::Result<()> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        let mut conn = open_db(workspace.path())?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_namespace_table(&tx)?;
        let mut final_namespaces = HashMap::new();
        {
            let mut statement =
                tx.prepare("SELECT binding_id, namespace FROM workspace_session_namespaces")?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })? {
                let (binding_id, namespace) = row?;
                final_namespaces.insert(binding_id, namespace);
            }
        }
        let updates = namespaces
            .into_iter()
            .map(|(binding_id, namespace)| (binding_id, namespace.persistence_key()))
            .collect::<Vec<_>>();
        for (binding_id, namespace) in &updates {
            final_namespaces.insert(*binding_id, namespace.clone());
        }

        let mut claimed = HashMap::<(String, String), i64>::new();
        {
            let mut statement = tx.prepare("SELECT binding_id, name FROM workspace_sessions")?;
            for row in statement.query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })? {
                let (binding_id, name) = row?;
                let Some(namespace) = final_namespaces.get(&binding_id) else {
                    continue;
                };
                let key = (namespace.clone(), name.clone());
                if let Some(previous_binding) = claimed.insert(key, binding_id)
                    && previous_binding != binding_id
                {
                    return Err(membership_conflict(&name, namespace));
                }
            }
        }
        for (binding_id, namespace) in updates {
            tx.execute(
                "INSERT INTO workspace_session_namespaces (binding_id, namespace)
                 VALUES (?1, ?2)
                 ON CONFLICT(binding_id) DO UPDATE SET namespace = excluded.namespace",
                params![binding_id, namespace],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn load(
        path: PathBuf,
        binding_id: i64,
        namespace: BackendConnectionNamespace,
        register: bool,
    ) -> rusqlite::Result<Self> {
        if register {
            register_namespace(&path, binding_id, &namespace)?;
        } else {
            validate_namespace(&path, binding_id, &namespace)?;
        }
        let store = SessionStore::load_sqlite(&path, binding_id)?;
        let membership_initialized = Self::membership_initialized(&path, binding_id)?;
        Ok(Self {
            path,
            binding_id,
            namespace,
            store,
            membership_initialized,
            #[cfg(test)]
            next_save_failure: None,
        })
    }

    fn membership_initialized(path: &Path, binding_id: i64) -> rusqlite::Result<bool> {
        let conn = open_db(path)?;
        conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM workspace_session_groups WHERE binding_id = ?1
             )",
            [binding_id],
            |row| row.get(0),
        )
    }

    fn membership_initialized_transaction(
        tx: &Transaction<'_>,
        binding_id: i64,
    ) -> rusqlite::Result<bool> {
        tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM workspace_session_groups WHERE binding_id = ?1
             )",
            [binding_id],
            |row| row.get(0),
        )
    }

    fn claimed_names(
        tx: &Transaction<'_>,
        binding_id: i64,
        namespace: &BackendConnectionNamespace,
    ) -> rusqlite::Result<HashSet<String>> {
        let mut statement = tx.prepare(
            "SELECT s.name
             FROM workspace_sessions s
             JOIN workspace_session_namespaces n ON n.binding_id = s.binding_id
             WHERE n.namespace = ?1 AND s.binding_id != ?2",
        )?;
        statement
            .query_map(params![namespace.persistence_key(), binding_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<HashSet<_>>>()
    }

    fn check_save_failure(&mut self) -> rusqlite::Result<()> {
        #[cfg(test)]
        if let Some(message) = self.next_save_failure.take() {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other(message),
            )));
        }
        Ok(())
    }
    /// Rebuild the persisted order after a stale concurrent writer adds a session.
    ///
    /// The transaction already contains additions committed by another writer. Keep the
    /// order owned by this store, retain names from that snapshot that are still present,
    /// and merge all names introduced since that snapshot in canonical order.
    fn merge_stale_additions(&self, store: &mut SessionStore) {
        let known = self.store.existing_names();
        let mut current = Vec::new();
        let mut additions = Vec::new();
        for name in store.ordered_names() {
            if known.contains(&name) {
                current.push(name);
            } else {
                additions.push(name);
            }
        }
        additions.sort();
        current.append(&mut additions);

        let mut merged = SessionStore::default();
        for name in current {
            merged.insert_unique(&name);
        }
        *store = merged;
    }

    fn mutate_store(
        &mut self,
        deterministic_stale_add: bool,
        mutate: impl FnOnce(&mut SessionStore, &HashSet<String>) -> rusqlite::Result<bool>,
    ) -> rusqlite::Result<bool> {
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut store = SessionStore::load_transaction(&tx, self.binding_id)?;
        let stale = deterministic_stale_add && self.store != store;
        let initialized = Self::membership_initialized_transaction(&tx, self.binding_id)?;
        let claimed = Self::claimed_names(&tx, self.binding_id, &self.namespace)?;
        let changed = mutate(&mut store, &claimed)?;
        if !changed {
            tx.commit()?;
            self.store = store;
            self.membership_initialized = initialized;
            return Ok(false);
        }
        if stale {
            self.merge_stale_additions(&mut store);
        }
        self.check_save_failure()?;
        store.save_transaction(&tx, self.binding_id)?;
        tx.commit()?;
        self.store = store;
        self.membership_initialized = true;
        Ok(true)
    }

    fn sync_store(
        &self,
        tx: &Transaction<'_>,
        store: &mut SessionStore,
        ordered_alive: &[String],
    ) -> rusqlite::Result<bool> {
        let initialized = Self::membership_initialized_transaction(tx, self.binding_id)?;
        if !initialized && store.entries.is_empty() {
            let claimed = Self::claimed_names(tx, self.binding_id, &self.namespace)?;
            for session in ordered_alive {
                if !claimed.contains(session) {
                    store.insert_unique(session);
                }
            }
            return Ok(true);
        }
        let alive = ordered_alive
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let prune = store
            .entries
            .iter()
            .flat_map(|group| &group.sessions)
            .any(|session| !alive.contains(session.as_str()));
        Ok(prune && store.prune(&alive))
    }

    pub fn add_session(&mut self, name: &str) -> rusqlite::Result<bool> {
        let namespace = self.namespace.persistence_key();
        self.mutate_store(true, move |store, claimed| {
            if claimed.contains(name) {
                return Err(membership_conflict(name, &namespace));
            }
            if store.find_session(name).is_some() {
                return Ok(false);
            }
            store.insert_unique(name);
            Ok(true)
        })
    }

    pub fn remove_session(&mut self, name: &str) -> rusqlite::Result<bool> {
        self.mutate_store(false, |store, _claimed| Ok(store.remove(name)))
    }
    /// Forget a membership after another owner has committed its durable removal transaction.
    /// This only updates the in-memory order; it intentionally performs no I/O or fallible work.
    pub(crate) fn forget_session_cache(&mut self, name: &str) {
        self.store.remove(name);
    }
    /// Rename a membership after another owner has committed the durable transaction.
    /// This only updates the in-memory order and intentionally cannot fail.
    pub(crate) fn rename_session_cache(&mut self, old: &str, new: &str) -> bool {
        self.store.rename_session(old, new)
    }

    pub fn rename_session(&mut self, old: &str, new: &str) -> rusqlite::Result<bool> {
        let namespace = self.namespace.persistence_key();
        self.mutate_store(false, move |store, claimed| {
            if store.find_session(old).is_none() {
                return Ok(false);
            }
            if claimed.contains(new) {
                return Err(membership_conflict(new, &namespace));
            }
            Ok(store.rename_session(old, new))
        })
    }

    pub fn session_names(&self) -> Vec<String> {
        self.store.ordered_names()
    }

    pub fn sync_sessions<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<Vec<String>> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut store = SessionStore::load_transaction(&tx, self.binding_id)?;
        let changed = if ordered_alive.is_empty() {
            false
        } else {
            self.sync_store(&tx, &mut store, &ordered_alive)?
        };
        let initialized = Self::membership_initialized_transaction(&tx, self.binding_id)?;
        if changed {
            self.check_save_failure()?;
            store.save_transaction(&tx, self.binding_id)?;
        }
        tx.commit()?;
        self.store = store;
        self.membership_initialized = changed || initialized;
        let alive = ordered_alive
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        Ok(self
            .store
            .ordered_names()
            .into_iter()
            .filter(|session| alive.contains(session.as_str()))
            .collect())
    }

    pub fn move_session<'a>(
        &mut self,
        name: &str,
        delta: i32,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<bool> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut store = SessionStore::load_transaction(&tx, self.binding_id)?;
        let sync_changed = if ordered_alive.is_empty() {
            false
        } else {
            self.sync_store(&tx, &mut store, &ordered_alive)?
        };
        let moved = store.move_session(name, delta);
        let initialized = Self::membership_initialized_transaction(&tx, self.binding_id)?;
        if sync_changed || moved {
            self.check_save_failure()?;
            store.save_transaction(&tx, self.binding_id)?;
        }
        tx.commit()?;
        self.store = store;
        self.membership_initialized = sync_changed || initialized;
        Ok(moved)
    }

    pub fn move_session_before<'a>(
        &mut self,
        source: &str,
        before: Option<&str>,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<bool> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let mut conn = open_db(&self.path)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut store = SessionStore::load_transaction(&tx, self.binding_id)?;
        let sync_changed = if ordered_alive.is_empty() {
            false
        } else {
            self.sync_store(&tx, &mut store, &ordered_alive)?
        };
        let moved = store.move_session_before(source, before);
        let initialized = Self::membership_initialized_transaction(&tx, self.binding_id)?;
        if sync_changed || moved {
            self.check_save_failure()?;
            store.save_transaction(&tx, self.binding_id)?;
        }
        tx.commit()?;
        self.store = store;
        self.membership_initialized = sync_changed || initialized;
        Ok(moved)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_save_for_test(&mut self) {
        self.next_save_failure = Some("injected session-order persistence failure".to_owned());
    }

    #[cfg(test)]
    pub(crate) fn clear_save_failure_for_test(&mut self) {
        self.next_save_failure = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_config_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bootty-session-order-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create temp session order dir");
        fs::write(dir.join("session-order"), "").expect("write empty legacy order");
        dir.join("config.toml")
    }
    fn namespace(
        backend: MultiplexerBackendConfig,
        host: Option<&str>,
    ) -> BackendConnectionNamespace {
        BackendConnectionNamespace::new(backend, host.map(SshRemoteConfig::for_host))
    }

    fn local_namespace() -> BackendConnectionNamespace {
        namespace(MultiplexerBackendConfig::Native, None)
    }

    #[test]
    fn sync_sessions_persists_order_in_sqlite_wal_database() {
        let path = temp_config_path("sqlite");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store
            .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
            .expect("sync sessions");
        assert!(
            store
                .move_session_before(
                    "agents",
                    Some("arc/migrations"),
                    ["arc/migrations", "arc/readiness", "agents", "bootty"],
                )
                .expect("move session")
        );

        let mut reloaded = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("reload session order");
        assert_eq!(
            reloaded
                .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
                .expect("sync reloaded sessions"),
            vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
        );

        let conn = open_db(&crate::workspace::sqlite_path(&path)).expect("open session order db");
        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("query journal mode");
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn sync_sessions_does_not_overwrite_persisted_order_when_refresh_has_no_sessions() {
        let path = temp_config_path("empty-refresh");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store
            .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
            .expect("sync sessions");
        assert!(
            store
                .move_session_before(
                    "agents",
                    Some("arc/migrations"),
                    ["arc/migrations", "arc/readiness", "agents", "bootty"],
                )
                .expect("move session")
        );

        assert!(
            store
                .sync_sessions(std::iter::empty())
                .expect("sync empty refresh")
                .is_empty()
        );

        let mut reloaded = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("reload session order");
        assert_eq!(
            reloaded
                .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
                .expect("sync reloaded sessions"),
            vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
        );
    }

    #[test]
    fn persisted_binding_sessions_filter_shared_backend_snapshots() {
        let path = temp_config_path("binding-membership");
        let workspace = WorkspaceStore::for_config_path(&path);
        let first_binding = workspace.binding_id().expect("default binding");
        let conn = open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second Space");
        let second_space = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![second_space, "Second Space Binding", "native", 0_i64],
        )
        .expect("insert second Space binding");
        let second_binding = conn.last_insert_rowid();

        let mut first = SessionOrderStore::for_binding(&path, first_binding, local_namespace())
            .expect("open first order");
        let mut second = SessionOrderStore::for_binding(&path, second_binding, local_namespace())
            .expect("open second order");
        first.add_session("first").expect("persist first session");
        second
            .add_session("second")
            .expect("persist second session");

        assert_eq!(
            first
                .sync_sessions(["first", "second"])
                .expect("sync first binding"),
            vec!["first"],
            "a binding must expose only its persisted members"
        );
        assert_eq!(
            second
                .sync_sessions(["first", "second"])
                .expect("sync second binding"),
            vec!["second"],
            "a second binding on the same backend must remain isolated"
        );
    }

    #[test]
    fn initial_sync_allows_same_name_for_distinct_backend_connections() {
        let path = temp_config_path("distinct-backend-connections");
        let workspace = WorkspaceStore::for_config_path(&path);
        let first_binding = workspace.binding_id().expect("default binding");
        let conn = open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second Space");
        let second_space = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![second_space, "Second Space Binding", "tmux", 0_i64],
        )
        .expect("insert second Space binding");
        let second_binding = conn.last_insert_rowid();
        drop(conn);

        let mut first = SessionOrderStore::for_binding(
            &path,
            first_binding,
            namespace(MultiplexerBackendConfig::Tmux, Some("devbox-a")),
        )
        .expect("open first order");
        let mut second = SessionOrderStore::for_binding(
            &path,
            second_binding,
            namespace(MultiplexerBackendConfig::Tmux, Some("devbox-b")),
        )
        .expect("open second order");

        assert_eq!(
            first.sync_sessions(["dev"]).expect("sync first backend"),
            vec!["dev"]
        );
        assert_eq!(
            second.sync_sessions(["dev"]).expect("sync second backend"),
            vec!["dev"]
        );
    }
    #[test]
    fn concurrent_initial_sync_claims_each_session_once() {
        let path = temp_config_path("concurrent-binding-membership");
        let workspace = WorkspaceStore::for_config_path(&path);
        let first_binding = workspace.binding_id().expect("default binding");
        let conn = open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second Space");
        let second_space = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![second_space, "Second Space Binding", "native", 0_i64],
        )
        .expect("insert second Space binding");
        let second_binding = conn.last_insert_rowid();
        drop(conn);

        let first = SessionOrderStore::for_binding(&path, first_binding, local_namespace())
            .expect("open first order");
        let second = SessionOrderStore::for_binding(&path, second_binding, local_namespace())
            .expect("open second order");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let owners = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first = scope.spawn(move || {
                let mut first = first;
                first_barrier.wait();
                first.sync_sessions(["shared"]).expect("sync first binding")
            });
            let second_barrier = barrier.clone();
            let second = scope.spawn(move || {
                let mut second = second;
                second_barrier.wait();
                second
                    .sync_sessions(["shared"])
                    .expect("sync second binding")
            });
            [
                first.join().expect("first thread"),
                second.join().expect("second thread"),
            ]
        });

        assert_eq!(
            owners
                .iter()
                .filter(|sessions| !sessions.is_empty())
                .count(),
            1
        );
    }

    #[test]
    fn concurrent_same_binding_additions_preserve_both_sessions() {
        let path = temp_config_path("same-binding-lost-update");
        let first = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open first session order");
        let second = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open second session order");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));

        let added = std::thread::scope(|scope| {
            let first_barrier = barrier.clone();
            let first = scope.spawn(move || {
                let mut first = first;
                first_barrier.wait();
                first.add_session("first").expect("add first session")
            });
            let second_barrier = barrier.clone();
            let second = scope.spawn(move || {
                let mut second = second;
                second_barrier.wait();
                second.add_session("second").expect("add second session")
            });
            [
                first.join().expect("first thread"),
                second.join().expect("second thread"),
            ]
        });

        assert_eq!(added, [true, true]);
        let reloaded = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("reload session order");
        assert_eq!(reloaded.session_names(), vec!["first", "second"]);
    }

    #[test]
    fn add_session_rejects_name_owned_by_another_binding_on_same_connection() {
        let path = temp_config_path("cross-binding-conflict");
        let workspace = WorkspaceStore::for_config_path(&path);
        let first_binding = workspace.binding_id().expect("default binding");
        let conn = open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second Space");
        let second_space = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![second_space, "Second Space Binding", "native", 0_i64],
        )
        .expect("insert second Space binding");
        let second_binding = conn.last_insert_rowid();
        drop(conn);

        let mut first = SessionOrderStore::for_binding(&path, first_binding, local_namespace())
            .expect("open first binding");
        let mut second = SessionOrderStore::for_binding(&path, second_binding, local_namespace())
            .expect("open second binding");

        assert!(first.add_session("shared").expect("add shared session"));
        let error = second
            .add_session("shared")
            .expect_err("reject conflicting shared session");
        assert!(
            error
                .to_string()
                .contains("already owned by another binding")
        );
        assert!(second.session_names().is_empty());
        assert!(first.add_session("first").expect("add first session"));
        assert!(second.add_session("second").expect("add second session"));
        let error = first
            .rename_session("first", "second")
            .expect_err("reject conflicting rename");
        assert!(
            error
                .to_string()
                .contains("already owned by another binding")
        );
    }

    #[test]
    fn namespace_rebind_rejects_destination_collision_and_preserves_source() {
        let path = temp_config_path("namespace-rebind-collision");
        let workspace = WorkspaceStore::for_config_path(&path);
        let first_binding = workspace.binding_id().expect("default binding");
        let conn = open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second Space");
        let second_space = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            params![second_space, "Second Space Binding", "native", 0_i64],
        )
        .expect("insert second Space binding");
        let second_binding = conn.last_insert_rowid();
        drop(conn);

        let tmux = namespace(MultiplexerBackendConfig::Tmux, Some("devbox"));
        let native = local_namespace();
        let mut first = SessionOrderStore::for_binding(&path, first_binding, tmux.clone())
            .expect("open tmux binding");
        let mut second = SessionOrderStore::for_binding(&path, second_binding, native.clone())
            .expect("open native binding");
        first.add_session("dev").expect("persist tmux session");
        second.add_session("dev").expect("persist native session");

        let error = SessionOrderStore::for_binding(&path, first_binding, native)
            .expect_err("reject namespace rebind collision");
        assert!(
            error
                .to_string()
                .contains("already owned by another binding")
        );
        let preserved = SessionOrderStore::for_binding(&path, first_binding, tmux)
            .expect("reopen original namespace");
        assert_eq!(preserved.session_names(), vec!["dev"]);
    }

    #[test]
    fn detached_session_can_be_attached_again() {
        let path = temp_config_path("detach-attach");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store.add_session("first").expect("persist first session");
        store.add_session("second").expect("persist second session");

        assert!(store.remove_session("first").expect("remove first session"));
        assert_eq!(
            store
                .sync_sessions(["first", "second"])
                .expect("sync after detach"),
            vec!["second"]
        );

        store
            .add_session("first")
            .expect("persist reattached session");
        assert_eq!(
            store
                .sync_sessions(["first", "second"])
                .expect("sync after attach"),
            vec!["second", "first"]
        );
    }

    #[test]
    fn rename_session_preserves_its_persisted_position() {
        let path = temp_config_path("rename-position");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        let sessions = ["first", "project/one", "project/two", "last"];
        store.sync_sessions(sessions).expect("sync sessions");

        assert!(
            store
                .rename_session("project/one", "renamed")
                .expect("rename session")
        );

        let mut reloaded = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("reload session order");
        assert_eq!(
            reloaded
                .sync_sessions(["first", "renamed", "project/two", "last"])
                .expect("sync reloaded sessions"),
            vec!["first", "renamed", "project/two", "last"]
        );
    }

    #[test]
    fn move_session_reorders_entries_within_group() {
        let path = temp_config_path("group");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store
            .sync_sessions(["a/1", "a/2", "b"])
            .expect("sync sessions");

        assert!(
            store
                .move_session("a/2", -1, ["a/1", "a/2", "b"])
                .expect("move session")
        );
        let ordered = store
            .sync_sessions(["a/1", "a/2", "b"])
            .expect("sync reordered sessions");
        let a2_index = ordered
            .iter()
            .position(|name| name == "a/2")
            .expect("a/2 present");
        let a1_index = ordered
            .iter()
            .position(|name| name == "a/1")
            .expect("a/1 present");
        assert!(a2_index < a1_index, "{ordered:?}");
    }

    #[test]
    fn move_session_moves_single_session_one_block_down_past_group() {
        let path = temp_config_path("step");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store
            .sync_sessions(["agents", "arc/migrations", "arc/readiness", "bootty"])
            .expect("sync sessions");

        assert!(
            store
                .move_session(
                    "agents",
                    1,
                    ["agents", "arc/migrations", "arc/readiness", "bootty"],
                )
                .expect("move session")
        );
        assert_eq!(
            store
                .sync_sessions(["agents", "arc/migrations", "arc/readiness", "bootty"])
                .expect("sync reordered sessions"),
            vec!["arc/migrations", "arc/readiness", "agents", "bootty"]
        );
    }

    #[test]
    fn move_session_before_reorders_siblings_within_a_group() {
        let path = temp_config_path("within");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        let alive = ["a/1", "a/2", "a/3", "b"];
        store.sync_sessions(alive).expect("sync sessions");

        assert!(
            store
                .move_session_before("a/3", Some("a/1"), alive)
                .expect("move session")
        );
        assert_eq!(
            store.sync_sessions(alive).expect("sync reordered sessions"),
            vec!["a/3", "a/1", "a/2", "b"],
            "a/3 should slot in front of its siblings without disturbing other groups"
        );
    }

    #[test]
    fn move_session_before_across_groups_moves_the_whole_block() {
        let path = temp_config_path("across");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        let alive = ["a/1", "a/2", "b"];
        store.sync_sessions(alive).expect("sync sessions");

        // Dragging the standalone `b` ahead of an `a` session moves it past the entire group.
        assert!(
            store
                .move_session_before("b", Some("a/1"), alive)
                .expect("move session")
        );
        assert_eq!(
            store.sync_sessions(alive).expect("sync reordered sessions"),
            vec!["b", "a/1", "a/2"]
        );
    }

    #[test]
    fn move_session_before_reorders_top_level_entries() {
        let path = temp_config_path("block");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store
            .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
            .expect("sync sessions");

        assert!(
            store
                .move_session_before(
                    "agents",
                    Some("arc/migrations"),
                    ["arc/migrations", "arc/readiness", "agents", "bootty"],
                )
                .expect("move session")
        );
        assert_eq!(
            store
                .sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"])
                .expect("sync reordered sessions"),
            vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
        );
    }

    #[test]
    fn failed_persistence_is_observable_and_does_not_change_membership() {
        let path = temp_config_path("persistence-failure");
        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");
        store.fail_next_save_for_test();

        let error = store
            .add_session("lost")
            .expect_err("injected persistence failure must be returned");

        assert!(
            error
                .to_string()
                .contains("injected session-order persistence failure")
        );
        assert!(store.session_names().is_empty());
        let reloaded = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("reload session order");
        assert!(reloaded.session_names().is_empty());
    }

    #[test]
    fn legacy_order_file_is_imported_into_the_default_binding() {
        let path = temp_config_path("legacy-file");
        fs::write(
            path.parent()
                .expect("config directory")
                .join("session-order"),
            "second\nfirst\n",
        )
        .expect("write legacy order");

        let mut store = SessionOrderStore::for_config_path(&path, local_namespace())
            .expect("open session order");

        assert_eq!(
            store
                .sync_sessions(["first", "second"])
                .expect("sync legacy order"),
            vec!["second", "first"]
        );
    }
}
