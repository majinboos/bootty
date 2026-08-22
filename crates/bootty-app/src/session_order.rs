use std::collections::HashSet;

fn session_group(name: &str) -> &str {
    name.split_once('/').map_or("", |(group, _)| group)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionGroup {
    pub(crate) name: String,
    pub(crate) sessions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SessionStore {
    entries: Vec<SessionGroup>,
}

impl SessionStore {
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
        if new.is_empty() || new.contains('\0') || old == new || self.existing_names().contains(new)
        {
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
    /// reorders the siblings; across groups a session cannot leave its group, so the whole source
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
}

/// Binding-scoped session membership and ordering.
///
/// This is a persistence-free value type. `WorkspaceRepository` owns its SQLite representation
/// and publishes a replacement only after the corresponding transaction commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOrderStore {
    store: SessionStore,
    membership_initialized: bool,
}

impl SessionOrderStore {
    pub(crate) fn from_groups(groups: Vec<SessionGroup>, membership_initialized: bool) -> Self {
        Self {
            store: SessionStore { entries: groups },
            membership_initialized,
        }
    }

    pub(crate) fn groups(&self) -> &[SessionGroup] {
        &self.store.entries
    }

    pub fn add_session(&mut self, name: &str) -> bool {
        if name.is_empty() || self.store.existing_names().contains(name) {
            return false;
        }
        self.store.insert_unique(name);
        self.membership_initialized = true;
        true
    }

    pub fn remove_session(&mut self, name: &str) -> bool {
        let removed = self.store.remove(name);
        if removed {
            self.membership_initialized = true;
        }
        removed
    }

    pub fn rename_session(&mut self, old: &str, new: &str) -> bool {
        let renamed = self.store.rename_session(old, new);
        if renamed {
            self.membership_initialized = true;
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
            self.membership_initialized = true;
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
            self.membership_initialized = true;
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
            self.membership_initialized = true;
        }
        moved
    }
}
