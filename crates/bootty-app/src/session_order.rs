use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

fn session_group(name: &str) -> &str {
    name.split_once('/').map_or("", |(group, _)| group)
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

fn legacy_order_paths(config_path: &Path) -> [PathBuf; 2] {
    let config_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
    let bootty_legacy = config_dir.join("session-order");
    let tmux_legacy = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
        .join(".config/tmux/session-order");
    [bootty_legacy, tmux_legacy]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SessionGroup {
    name: String,
    sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct SessionStore {
    entries: Vec<SessionGroup>,
}

impl SessionStore {
    fn load(path: &Path) -> Self {
        if let Ok(data) = fs::read_to_string(path)
            && let Ok(mut store) = serde_json::from_str::<SessionStore>(&data)
        {
            store.normalize_groups();
            return store;
        }

        Self::default()
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

    fn normalize_groups(&mut self) {
        let mut merged = Vec::<SessionGroup>::new();
        for entry in self.entries.drain(..) {
            for session in &entry.sessions {
                let group = session_group(session);
                if group.is_empty() {
                    merged.push(SessionGroup {
                        name: String::new(),
                        sessions: vec![session.clone()],
                    });
                    continue;
                }
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

#[derive(Debug, Clone)]
pub struct SessionOrderStore {
    path: PathBuf,
    store: SessionStore,
}

impl SessionOrderStore {
    pub fn for_config_path(config_path: &Path) -> Self {
        let path = config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("session-order.json");
        let mut store = SessionStore::load(&path);
        if store == SessionStore::default() {
            for legacy in legacy_order_paths(config_path) {
                if !legacy.exists() {
                    continue;
                }
                store = SessionStore::from_flat_list(&load_lines(&legacy));
                break;
            }
        }
        Self { path, store }
    }

    pub fn sync_sessions<'a>(
        &mut self,
        sessions: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        let ordered_alive = sessions.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let alive = ordered_alive.iter().cloned().collect::<HashSet<_>>();
        let previous = self.store.clone();
        for session in ordered_alive {
            self.store.insert(&session);
        }
        self.store.prune(&alive);
        if self.store != previous {
            self.save();
        }
        self.store
            .ordered_names()
            .into_iter()
            .filter(|session| alive.contains(session))
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

    fn save(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }

        let tmp = temp_path(&self.path);
        let json = serde_json::to_string_pretty(&self.store).unwrap_or_default();
        if fs::File::create(&tmp)
            .and_then(|mut file| file.write_all(json.as_bytes()))
            .is_err()
        {
            return;
        }
        let _ = fs::rename(&tmp, &self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
