use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use rusqlite::params;

use crate::{
    config::MultiplexerConfig,
    workspace::{WorkspaceStore, open_db},
};

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
        tx.commit()
    }
}

#[derive(Debug, Clone)]
pub struct SessionOrderStore {
    config_path: PathBuf,
    multiplexer: MultiplexerConfig,
    path: PathBuf,
    binding_id: Option<i64>,
    store: SessionStore,
    loaded: bool,
}

impl SessionOrderStore {
    pub fn for_config_path(config_path: &Path) -> Self {
        Self::for_config_path_with_multiplexer(config_path, &MultiplexerConfig::default())
    }

    pub fn for_config_path_with_multiplexer(
        config_path: &Path,
        multiplexer: &MultiplexerConfig,
    ) -> Self {
        let workspace = WorkspaceStore::for_config_path(config_path, multiplexer);
        let path = workspace.path().to_path_buf();
        let binding_id = workspace.binding_id();
        let store = Self::load_store(&path, binding_id);
        Self {
            config_path: config_path.to_path_buf(),
            multiplexer: multiplexer.clone(),
            path,
            binding_id,
            store,
            loaded: true,
        }
    }

    pub fn lazy_for_config_path(config_path: &Path) -> Self {
        Self::lazy_for_config_path_with_multiplexer(config_path, &MultiplexerConfig::default())
    }

    pub fn lazy_for_config_path_with_multiplexer(
        config_path: &Path,
        multiplexer: &MultiplexerConfig,
    ) -> Self {
        Self {
            config_path: config_path.to_path_buf(),
            multiplexer: multiplexer.clone(),
            path: crate::workspace::sqlite_path(config_path),
            binding_id: None,
            store: SessionStore::default(),
            loaded: false,
        }
    }

    fn load_store(path: &Path, binding_id: Option<i64>) -> SessionStore {
        binding_id
            .and_then(|binding_id| SessionStore::load_sqlite(path, binding_id).ok())
            .unwrap_or_default()
    }

    fn ensure_loaded(&mut self) {
        if self.loaded {
            return;
        }
        let workspace = WorkspaceStore::for_config_path(&self.config_path, &self.multiplexer);
        self.path = workspace.path().to_path_buf();
        self.binding_id = workspace.binding_id();
        self.store = Self::load_store(&self.path, self.binding_id);
        self.loaded = true;
    }

    pub fn sync_sessions<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        self.ensure_loaded();
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if ordered_alive.is_empty() {
            return Vec::new();
        }
        let alive = ordered_alive
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let mut existing = self.store.existing_names();
        let mut changed = false;
        for session in &ordered_alive {
            if existing.insert(session.clone()) {
                self.store.insert_unique(session);
                changed = true;
            }
        }
        changed |= self.store.prune(&alive);
        if changed {
            self.save();
        }
        self.store
            .ordered_names()
            .into_iter()
            .filter(|session| alive.contains(session.as_str()))
            .collect()
    }

    pub fn move_session<'a>(
        &mut self,
        name: &str,
        delta: i32,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.sync_sessions(sessions);
        let moved = self.store.move_session(name, delta);
        if moved {
            self.save();
        }
        moved
    }

    pub fn move_block_before<'a>(
        &mut self,
        source: &str,
        target: Option<&str>,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.sync_sessions(sessions);
        let moved = self.store.move_block_before(source, target);
        if moved {
            self.save();
        }
        moved
    }

    pub fn move_session_before<'a>(
        &mut self,
        source: &str,
        before: Option<&str>,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.sync_sessions(sessions);
        let moved = self.store.move_session_before(source, before);
        if moved {
            self.save();
        }
        moved
    }

    fn save(&self) {
        if let Some(binding_id) = self.binding_id {
            let _ = self.store.save_sqlite(&self.path, binding_id);
        }
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
        dir.join("config.toml")
    }

    #[test]
    fn sync_sessions_persists_order_in_sqlite_wal_database() {
        let path = temp_config_path("sqlite");
        let mut store = SessionOrderStore::for_config_path(&path);
        store.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]);
        assert!(store.move_block_before(
            "agents",
            Some("arc/migrations"),
            ["arc/migrations", "arc/readiness", "agents", "bootty"],
        ));

        let mut reloaded = SessionOrderStore::for_config_path(&path);
        assert_eq!(
            reloaded.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]),
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
        let mut store = SessionOrderStore::for_config_path(&path);
        store.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]);
        assert!(store.move_block_before(
            "agents",
            Some("arc/migrations"),
            ["arc/migrations", "arc/readiness", "agents", "bootty"],
        ));

        assert!(store.sync_sessions(std::iter::empty()).is_empty());

        let mut reloaded = SessionOrderStore::for_config_path(&path);
        assert_eq!(
            reloaded.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]),
            vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
        );
    }

    #[test]
    fn move_session_reorders_entries_within_group() {
        let path = temp_config_path("group");
        let mut store = SessionOrderStore::for_config_path(&path);
        store.sync_sessions(["a/1", "a/2", "b"]);

        assert!(store.move_session("a/2", -1, ["a/1", "a/2", "b"]));
        let ordered = store.sync_sessions(["a/1", "a/2", "b"]);
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
        let mut store = SessionOrderStore::for_config_path(&path);
        store.sync_sessions(["agents", "arc/migrations", "arc/readiness", "bootty"]);

        assert!(store.move_session(
            "agents",
            1,
            ["agents", "arc/migrations", "arc/readiness", "bootty"],
        ));
        assert_eq!(
            store.sync_sessions(["agents", "arc/migrations", "arc/readiness", "bootty"]),
            vec!["arc/migrations", "arc/readiness", "agents", "bootty"]
        );
    }

    #[test]
    fn move_session_before_reorders_siblings_within_a_group() {
        let path = temp_config_path("within");
        let mut store = SessionOrderStore::for_config_path(&path);
        let alive = ["a/1", "a/2", "a/3", "b"];
        store.sync_sessions(alive);

        assert!(store.move_session_before("a/3", Some("a/1"), alive));
        assert_eq!(
            store.sync_sessions(alive),
            vec!["a/3", "a/1", "a/2", "b"],
            "a/3 should slot in front of its siblings without disturbing other groups"
        );
    }

    #[test]
    fn move_session_before_across_groups_moves_the_whole_block() {
        let path = temp_config_path("across");
        let mut store = SessionOrderStore::for_config_path(&path);
        let alive = ["a/1", "a/2", "b"];
        store.sync_sessions(alive);

        // Dragging the standalone `b` ahead of an `a` session moves it past the entire group.
        assert!(store.move_session_before("b", Some("a/1"), alive));
        assert_eq!(store.sync_sessions(alive), vec!["b", "a/1", "a/2"]);
    }

    #[test]
    fn move_block_before_reorders_top_level_entries() {
        let path = temp_config_path("block");
        let mut store = SessionOrderStore::for_config_path(&path);
        store.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]);

        assert!(store.move_block_before(
            "agents",
            Some("arc/migrations"),
            ["arc/migrations", "arc/readiness", "agents", "bootty"],
        ));
        assert_eq!(
            store.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]),
            vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
        );
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

        let mut store = SessionOrderStore::for_config_path(&path);

        assert_eq!(
            store.sync_sessions(["first", "second"]),
            vec!["second", "first"]
        );
    }
}
