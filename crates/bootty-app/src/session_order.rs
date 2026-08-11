use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use rusqlite::params;

use crate::workspace::{WorkspaceStore, open_db};

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

    fn save_sqlite(&self, path: &Path, binding_id: i64) -> rusqlite::Result<()> {
        let mut conn = open_db(path)?;
        let tx = conn.transaction()?;
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
        tx.commit()
    }
}

#[derive(Debug, Clone)]
pub struct SessionOrderStore {
    path: PathBuf,
    binding_id: i64,
    store: SessionStore,
    membership_initialized: bool,
    #[cfg(test)]
    next_save_failure: Option<String>,
}

impl SessionOrderStore {
    pub fn for_config_path(config_path: &Path) -> rusqlite::Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        let binding_id = workspace
            .binding_id()
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Self::load(workspace.path().to_path_buf(), binding_id)
    }

    pub fn for_binding(config_path: &Path, binding_id: i64) -> rusqlite::Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(config_path)?;
        Self::load(workspace.path().to_path_buf(), binding_id)
    }

    fn load(path: PathBuf, binding_id: i64) -> rusqlite::Result<Self> {
        let store = SessionStore::load_sqlite(&path, binding_id)?;
        let membership_initialized = Self::membership_initialized(&path, binding_id)?;
        Ok(Self {
            path,
            binding_id,
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
    fn names_owned_by_other_bindings(&self) -> rusqlite::Result<HashSet<String>> {
        let conn = open_db(&self.path)?;
        let mut statement =
            conn.prepare("SELECT name FROM workspace_sessions WHERE binding_id != ?1")?;
        statement
            .query_map([self.binding_id], |row| row.get::<_, String>(0))?
            .collect()
    }

    pub fn add_session(&mut self, name: &str) -> rusqlite::Result<bool> {
        if self.store.find_session(name).is_some() {
            return Ok(false);
        }
        let mut store = self.store.clone();
        store.insert_unique(name);
        self.save_store(store)?;
        Ok(true)
    }

    pub fn remove_session(&mut self, name: &str) -> rusqlite::Result<bool> {
        let mut store = self.store.clone();
        if !store.remove(name) {
            return Ok(false);
        }
        self.save_store(store)?;
        Ok(true)
    }

    pub fn rename_session(&mut self, old: &str, new: &str) -> rusqlite::Result<bool> {
        let mut store = self.store.clone();
        if !store.rename_session(old, new) {
            return Ok(false);
        }
        self.save_store(store)?;
        Ok(true)
    }

    pub fn session_names(&self) -> Vec<String> {
        self.store.ordered_names()
    }

    pub fn sync_sessions<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<Vec<String>> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if ordered_alive.is_empty() {
            return Ok(Vec::new());
        }
        let alive = ordered_alive
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let initialize = !self.membership_initialized && self.store.entries.is_empty();
        let prune = self
            .store
            .entries
            .iter()
            .flat_map(|group| &group.sessions)
            .any(|session| !alive.contains(session.as_str()));
        if initialize || prune {
            let mut store = self.store.clone();
            if initialize {
                let claimed = self.names_owned_by_other_bindings()?;
                for session in &ordered_alive {
                    if !claimed.contains(session) {
                        store.insert_unique(session);
                    }
                }
            }
            store.prune(&alive);
            self.save_store(store)?;
        }
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
        self.sync_sessions(sessions)?;
        let mut store = self.store.clone();
        if !store.move_session(name, delta) {
            return Ok(false);
        }
        self.save_store(store)?;
        Ok(true)
    }

    pub fn move_session_before<'a>(
        &mut self,
        source: &str,
        before: Option<&str>,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> rusqlite::Result<bool> {
        self.sync_sessions(sessions)?;
        let mut store = self.store.clone();
        if !store.move_session_before(source, before) {
            return Ok(false);
        }
        self.save_store(store)?;
        Ok(true)
    }

    fn save_store(&mut self, store: SessionStore) -> rusqlite::Result<()> {
        #[cfg(test)]
        if let Some(message) = self.next_save_failure.take() {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::other(message),
            )));
        }

        store.save_sqlite(&self.path, self.binding_id)?;
        self.store = store;
        self.membership_initialized = true;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_save_for_test(&mut self) {
        self.next_save_failure = Some("injected session-order persistence failure".to_owned());
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

    #[test]
    fn sync_sessions_persists_order_in_sqlite_wal_database() {
        let path = temp_config_path("sqlite");
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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

        let mut reloaded = SessionOrderStore::for_config_path(&path).expect("reload session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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

        let mut reloaded = SessionOrderStore::for_config_path(&path).expect("reload session order");
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

        let mut first =
            SessionOrderStore::for_binding(&path, first_binding).expect("open first order");
        let mut second =
            SessionOrderStore::for_binding(&path, second_binding).expect("open second order");
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
    fn detached_session_can_be_attached_again() {
        let path = temp_config_path("detach-attach");
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
        let sessions = ["first", "project/one", "project/two", "last"];
        store.sync_sessions(sessions).expect("sync sessions");

        assert!(
            store
                .rename_session("project/one", "renamed")
                .expect("rename session")
        );

        let mut reloaded = SessionOrderStore::for_config_path(&path).expect("reload session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");
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
        let reloaded = SessionOrderStore::for_config_path(&path).expect("reload session order");
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

        let mut store = SessionOrderStore::for_config_path(&path).expect("open session order");

        assert_eq!(
            store
                .sync_sessions(["first", "second"])
                .expect("sync legacy order"),
            vec!["second", "first"]
        );
    }
}
