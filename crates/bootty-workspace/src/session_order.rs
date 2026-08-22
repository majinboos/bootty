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
        self.names().map(str::to_owned).collect()
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .flat_map(|group| group.sessions.iter().map(String::as_str))
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
            group.clone_into(&mut entry.name);
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
        if new.is_empty()
            || new.contains('\0')
            || old == new
            || self.names().any(|name| name == new)
        {
            return false;
        }
        let Some((entry_index, session_index)) = self.find_session(old) else {
            return false;
        };
        new.clone_into(&mut self.entries[entry_index].sessions[session_index]);
        true
    }

    fn prune(&mut self, alive: &HashSet<&str>) -> bool {
        let before = (self.entries.len(), self.names().count());
        for entry in &mut self.entries {
            entry
                .sessions
                .retain(|session| alive.contains(session.as_str()));
        }
        self.entries.retain(|entry| !entry.sessions.is_empty());
        before != (self.entries.len(), self.names().count())
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

        let target = if delta < 0 {
            Some(entry_idx.checked_sub(1).unwrap_or(entry_idx))
        } else {
            (entry_idx + 2 < self.entries.len()).then_some(entry_idx + 2)
        };
        self.move_block_before(entry_idx, target)
    }

    fn move_block_before(&mut self, source: usize, target: Option<usize>) -> bool {
        if source >= self.entries.len() || target.is_some_and(|target| target >= self.entries.len())
        {
            return false;
        }
        let entry = self.entries.remove(source);
        let insert = target.map_or(self.entries.len(), |target| {
            target - usize::from(target > source)
        });
        let changed = insert != source;
        self.entries.insert(insert, entry);
        changed
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
                    self.move_block_before(src_group, Some(tgt_group))
                }
            }
            None => self.move_block_before(src_group, None),
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
/// This is a persistence-free value type. `WorkspaceRepository` owns its `SQLite` representation
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
        if name.is_empty() || self.store.names().any(|session| session == name) {
            return false;
        }
        self.store.insert_unique(name);
        self.membership_initialized = true;
        true
    }

    pub fn remove_session(&mut self, name: &str) -> bool {
        let removed = self.store.remove(name);
        self.record_change(removed)
    }

    pub fn rename_session(&mut self, old: &str, new: &str) -> bool {
        let renamed = self.store.rename_session(old, new);
        self.record_change(renamed)
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
        let mut changed = false;
        if !self.membership_initialized && self.store.names().next().is_none() {
            for session in &ordered_alive {
                self.store.insert_unique(session);
                changed = true;
            }
        }
        changed |= self.store.prune(&alive);
        self.record_change(changed);
        self.store.ordered_names()
    }

    pub fn move_session<'a>(
        &mut self,
        name: &str,
        delta: i32,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.sync_sessions(sessions);
        let moved = self.store.move_session(name, delta);
        self.record_change(moved)
    }

    pub fn move_session_before<'a>(
        &mut self,
        source: &str,
        before: Option<&str>,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        self.sync_sessions(sessions);
        let moved = self.store.move_session_before(source, before);
        self.record_change(moved)
    }

    fn record_change(&mut self, changed: bool) -> bool {
        self.membership_initialized |= changed;
        changed
    }
}
