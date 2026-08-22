use std::collections::HashSet;

/// The `/` prefix a label shares with its siblings, or `""` for a session on its own.
///
/// Grouping is read off the label rather than stored, so nothing has to be kept in step with it.
fn label_group(label: &str) -> &str {
    label.split_once('/').map_or("", |(group, _)| group)
}

/// One session a Space claims, keyed by the identity the multiplexer carries for it.
///
/// Nothing here keys on a name: a name is only ever a hint or a label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSession {
    pub identity: String,
    /// The name the backend last reported: the label when bootty has no name of its own, and the
    /// hint for re-finding a session whose server restarted and dropped every tag.
    pub backend_name: String,
    /// What bootty calls the session, or empty for "whatever the backend calls it". This is why a
    /// shared server's `-2` suffix never reaches the sidebar.
    pub display_name: String,
    /// Whether the user chose `display_name` instead of bootty generating it from the directory.
    pub explicit: bool,
    /// Where the session started, so bootty can recreate it on a backend that does not persist.
    pub cwd: String,
}

impl WorkspaceSession {
    /// The name to show. Bootty's own if it has one, otherwise the backend's.
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.backend_name
        } else {
            &self.display_name
        }
    }
}

/// The sessions one Space claims, in sidebar order. Membership and order are one list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionMembership {
    sessions: Vec<WorkspaceSession>,
}

impl SessionMembership {
    pub fn from_sessions(sessions: Vec<WorkspaceSession>) -> Self {
        Self { sessions }
    }

    pub fn sessions(&self) -> &[WorkspaceSession] {
        &self.sessions
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn get(&self, identity: &str) -> Option<&WorkspaceSession> {
        self.sessions
            .iter()
            .find(|session| session.identity == identity)
    }

    pub fn contains(&self, identity: &str) -> bool {
        self.get(identity).is_some()
    }

    /// The claimed sessions' backend names, in order, for applying that order to the backend.
    pub fn backend_names(&self) -> Vec<String> {
        self.sessions
            .iter()
            .map(|session| session.backend_name.clone())
            .collect()
    }

    /// Adds a session to the end of the Space, or does nothing if it is already claimed.
    pub fn claim(&mut self, session: WorkspaceSession) -> bool {
        if session.identity.is_empty() || self.contains(&session.identity) {
            return false;
        }
        let group = label_group(session.label()).to_owned();
        // A session joins its group rather than the end of the list, so `agents/review` lands
        // beside `agents/main` instead of below whatever happens to be last.
        let insert_at = (!group.is_empty())
            .then(|| {
                self.sessions
                    .iter()
                    .rposition(|existing| label_group(existing.label()) == group)
                    .map(|last| last + 1)
            })
            .flatten()
            .unwrap_or(self.sessions.len());
        self.sessions.insert(insert_at, session);
        true
    }

    /// Drops a session from this Space. The session itself is untouched.
    pub fn release(&mut self, identity: &str) -> Option<WorkspaceSession> {
        let position = self
            .sessions
            .iter()
            .position(|session| session.identity == identity)?;
        Some(self.sessions.remove(position))
    }

    /// Records the name the backend now reports, which a rename from anywhere changes.
    pub fn observe_backend_name(&mut self, identity: &str, backend_name: &str) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.backend_name == backend_name {
            return false;
        }
        backend_name.clone_into(&mut session.backend_name);
        true
    }

    /// Sets what bootty calls a session, and whether the user chose that name.
    pub fn set_display_name(&mut self, identity: &str, display_name: &str, explicit: bool) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.display_name == display_name && session.explicit == explicit {
            return false;
        }
        display_name.clone_into(&mut session.display_name);
        session.explicit = explicit;
        true
    }

    pub fn set_cwd(&mut self, identity: &str, cwd: &str) -> bool {
        let Some(session) = self.session_mut(identity) else {
            return false;
        };
        if session.cwd == cwd {
            return false;
        }
        cwd.clone_into(&mut session.cwd);
        true
    }

    /// Drops every claim whose session the backend no longer reports. An empty `alive` means the
    /// backend has not answered yet, not that the Space emptied.
    pub fn retain_alive(&mut self, alive: &HashSet<&str>) -> bool {
        if alive.is_empty() {
            return false;
        }
        let before = self.sessions.len();
        self.sessions
            .retain(|session| alive.contains(session.identity.as_str()));
        before != self.sessions.len()
    }

    /// Moves `source` before `before`, or to the end when `before` is `None`.
    ///
    /// A session in a group cannot leave it, so dragging one past another group takes the whole
    /// group with it. An ungrouped session travels alone.
    pub fn move_before(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some(from) = self.position(source) else {
            return false;
        };
        let anchor = match before {
            Some(before) => match self.position(before) {
                Some(to) => Some(to),
                None => return false,
            },
            None => None,
        };
        let source_group = label_group(self.sessions[from].label()).to_owned();
        if !source_group.is_empty()
            && anchor.is_some_and(|to| label_group(self.sessions[to].label()) == source_group)
        {
            return self.move_within_group(from, anchor);
        }
        self.move_block(&source_group, from, anchor)
    }

    /// Moves a session one place, carrying its group when it steps past a group boundary.
    pub fn move_by(&mut self, identity: &str, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let Some(from) = self.position(identity) else {
            return false;
        };
        let neighbour = if delta < 0 {
            from.checked_sub(1)
        } else {
            (from + 1 < self.sessions.len()).then_some(from + 1)
        };
        let Some(neighbour) = neighbour else {
            return false;
        };
        let group = label_group(self.sessions[from].label()).to_owned();
        if !group.is_empty() && label_group(self.sessions[neighbour].label()) == group {
            self.sessions.swap(from, neighbour);
            return true;
        }
        // Stepping down means landing after the neighbour's block, which is before whatever
        // follows it -- or the end of the list when nothing does.
        let anchor = if delta < 0 {
            Some(self.block_start(neighbour))
        } else {
            self.block_end(neighbour)
        };
        self.move_block(&group, from, anchor)
    }

    /// The span `index` belongs to: its whole group, or just itself when it is ungrouped.
    fn block(&self, group: &str, index: usize) -> (usize, usize) {
        if group.is_empty() {
            return (index, index + 1);
        }
        let member = |position: &usize| label_group(self.sessions[*position].label()) == group;
        let start = (0..=index).rev().take_while(member).last().unwrap_or(index);
        let end = (index..self.sessions.len())
            .take_while(member)
            .last()
            .unwrap_or(index);
        (start, end + 1)
    }

    fn block_start(&self, index: usize) -> usize {
        let group = label_group(self.sessions[index].label()).to_owned();
        self.block(&group, index).0
    }

    fn block_end(&self, index: usize) -> Option<usize> {
        let group = label_group(self.sessions[index].label()).to_owned();
        let (_, end) = self.block(&group, index);
        (end < self.sessions.len()).then_some(end)
    }

    fn move_within_group(&mut self, from: usize, anchor: Option<usize>) -> bool {
        let to = anchor.unwrap_or(self.sessions.len());
        let insert_at = if to > from { to - 1 } else { to };
        if insert_at == from {
            return false;
        }
        let session = self.sessions.remove(from);
        self.sessions.insert(insert_at, session);
        true
    }

    /// Lifts the block containing `from` out and reinserts it at `anchor`.
    fn move_block(&mut self, group: &str, from: usize, anchor: Option<usize>) -> bool {
        let (start, end) = self.block(group, from);
        let anchor = anchor.map(|anchor| self.block_start(anchor));
        let insert_at = anchor.unwrap_or(self.sessions.len());
        if insert_at >= start && insert_at <= end {
            return false;
        }
        let block = self.sessions.drain(start..end).collect::<Vec<_>>();
        let insert_at = if insert_at > start {
            insert_at - block.len()
        } else {
            insert_at
        };
        for (offset, session) in block.into_iter().enumerate() {
            self.sessions.insert(insert_at + offset, session);
        }
        true
    }

    fn session_mut(&mut self, identity: &str) -> Option<&mut WorkspaceSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.identity == identity)
    }

    fn position(&self, identity: &str) -> Option<usize> {
        self.sessions
            .iter()
            .position(|session| session.identity == identity)
    }
}
