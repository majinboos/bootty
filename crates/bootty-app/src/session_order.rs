use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use rusqlite::params;

use crate::workspace::open_db;

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
    binding_id: Option<i64>,
    store: SessionStore,
    membership_initialized: bool,
}

impl SessionOrderStore {
    pub(crate) fn for_binding(config_path: &Path, binding_id: i64) -> Self {
        let path = crate::workspace::sqlite_path(config_path);
        let (store, membership_initialized) = Self::load_store(&path, Some(binding_id));
        Self {
            store,
            path,
            binding_id: Some(binding_id),
            membership_initialized,
        }
    }

    fn load_store(path: &Path, binding_id: Option<i64>) -> (SessionStore, bool) {
        let Some(binding_id) = binding_id else {
            return (SessionStore::default(), false);
        };
        (
            SessionStore::load_sqlite(path, binding_id).unwrap_or_default(),
            Self::membership_initialized(path, binding_id).unwrap_or(false),
        )
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

    pub fn add_session(&mut self, name: &str) {
        if self.store.existing_names().insert(name.to_owned()) {
            self.store.insert_unique(name);
            self.save();
        }
    }

    pub fn remove_session(&mut self, name: &str) -> bool {
        let removed = self.store.remove(name);
        if removed {
            self.save();
        }
        removed
    }

    pub fn rename_session(&mut self, old: &str, new: &str) -> bool {
        let renamed = self.store.rename_session(old, new);
        if renamed {
            self.save();
        }
        renamed
    }

    pub fn session_names(&self) -> Vec<String> {
        self.store.ordered_names()
    }

    pub fn sync_sessions<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        if ordered_alive.is_empty() {
            return Vec::new();
        }
        let alive = ordered_alive
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let existing = self.store.existing_names();
        let mut changed = false;
        if !self.membership_initialized && existing.is_empty() {
            for session in &ordered_alive {
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

    fn save(&mut self) {
        if let Some(binding_id) = self.binding_id
            && self.store.save_sqlite(&self.path, binding_id).is_ok()
        {
            self.membership_initialized = true;
        }
    }
}
