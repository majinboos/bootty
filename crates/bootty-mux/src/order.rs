use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn session_group(name: &str) -> &str {
    name.split_once('/').map_or("", |(group, _)| group)
}

fn order_file() -> PathBuf {
    home().join(".config/tmux/session-order.json")
}

fn hidden_file() -> PathBuf {
    home().join(".config/tmux/session-hidden")
}

fn temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tmp");
    path.with_file_name(format!("{file_name}.{}.tmp", std::process::id()))
}

fn load_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionGroup {
    name: String,
    sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionStore {
    entries: Vec<SessionGroup>,
}

impl SessionStore {
    fn load() -> Self {
        let path = order_file();
        if let Ok(data) = fs::read_to_string(&path)
            && let Ok(mut store) = serde_json::from_str::<SessionStore>(&data)
        {
            store.normalize_groups();
            return store;
        }

        let legacy = home().join(".config/tmux/session-order");
        if legacy.exists() {
            let lines = load_lines(&legacy);
            let store = Self::from_flat_list(&lines);
            store.save();
            return store;
        }

        Self::default()
    }

    fn save(&self) {
        let path = order_file();
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }

        let tmp = temp_path(&path);
        let json = serde_json::to_string_pretty(self).unwrap_or_default();
        if fs::File::create(&tmp)
            .and_then(|mut file| file.write_all(json.as_bytes()))
            .is_err()
        {
            return;
        }
        let _ = fs::rename(&tmp, &path);
    }

    fn from_flat_list(names: &[String]) -> Self {
        let mut store = Self::default();
        let mut seen = HashSet::new();
        for name in names {
            if seen.insert(name.clone()) {
                store.insert(name);
            }
        }
        store
    }

    fn ordered_names(&self) -> Vec<String> {
        self.entries
            .iter()
            .flat_map(|group| group.sessions.iter().cloned())
            .collect()
    }

    fn insert(&mut self, name: &str) {
        if self.contains(name) {
            return;
        }

        let group = session_group(name);
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

    fn normalize_groups(&mut self) {
        let mut merged = Vec::<SessionGroup>::new();
        for entry in self.entries.drain(..) {
            for session in &entry.sessions {
                let group = session_group(session);
                if let Some(target) = merged.iter_mut().find(|entry| entry.name == group) {
                    if !target.sessions.contains(session) {
                        target.sessions.push(session.clone());
                    }
                } else {
                    merged.push(SessionGroup {
                        name: group.to_owned(),
                        sessions: vec![session.clone()],
                    });
                }
            }
        }
        self.entries = merged;
    }

    fn prune(&mut self, alive: &HashSet<String>) {
        for entry in &mut self.entries {
            entry.sessions.retain(|session| alive.contains(session));
        }
        self.entries.retain(|entry| !entry.sessions.is_empty());
    }

    fn contains(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|group| group.sessions.iter().any(|session| session == name))
    }

    fn move_session(&mut self, name: &str, delta: i32) -> bool {
        let direction = if delta < 0 {
            "up"
        } else if delta > 0 {
            "down"
        } else {
            return false;
        };
        let group = session_group(name);
        let Some((entry_idx, session_idx)) = self.find_session(name) else {
            return false;
        };

        let entry = &self.entries[entry_idx];
        if entry.sessions.len() > 1 {
            match direction {
                "up" if session_idx > 0 => {
                    self.entries[entry_idx]
                        .sessions
                        .swap(session_idx, session_idx - 1);
                    return true;
                }
                "down" if session_idx < entry.sessions.len() - 1 => {
                    self.entries[entry_idx]
                        .sessions
                        .swap(session_idx, session_idx + 1);
                    return true;
                }
                _ => {}
            }
        }

        let n = self.entries.len();
        match direction {
            "up" if entry_idx > 0 => {
                let prev = entry_idx - 1;
                let prev_group = &self.entries[prev].name;
                if group.is_empty()
                    && !prev_group.is_empty()
                    && self.entries[prev].sessions.len() > 1
                {
                    if prev == 0 {
                        return false;
                    }
                    self.entries.swap(entry_idx, prev - 1);
                } else {
                    self.entries.swap(entry_idx, prev);
                }
                true
            }
            "down" if entry_idx < n - 1 => {
                let next = entry_idx + 1;
                let next_group = &self.entries[next].name;
                if group.is_empty()
                    && !next_group.is_empty()
                    && self.entries[next].sessions.len() > 1
                {
                    if next >= n - 1 {
                        return false;
                    }
                    self.entries.swap(entry_idx, next + 1);
                } else {
                    self.entries.swap(entry_idx, next);
                }
                true
            }
            _ => false,
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

pub fn compute_order(alive: &HashSet<String>, include_hidden: bool) -> Vec<String> {
    let hidden = if include_hidden {
        HashSet::new()
    } else {
        load_lines(&hidden_file()).into_iter().collect()
    };

    let mut store = SessionStore::load();
    let mut alive_sorted = alive.iter().collect::<Vec<_>>();
    alive_sorted.sort();
    for session in alive_sorted {
        store.insert(session);
    }
    store.prune(alive);
    store.save();

    store
        .ordered_names()
        .into_iter()
        .filter(|session| alive.contains(session) && !hidden.contains(session))
        .collect()
}

pub fn move_session(name: &str, delta: i32) -> bool {
    let mut store = SessionStore::load();
    let moved = store.move_session(name, delta);
    if moved {
        store.save();
    }
    moved
}
