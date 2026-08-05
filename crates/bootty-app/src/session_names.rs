use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use rusqlite::params;

use crate::workspace::open_db as workspace_open_db;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionNameRecord {
    pub session_id: String,
    pub cwd: String,
    pub generated_name: String,
    pub session_name: String,
    /// What bootty calls this session in its own UI. The backend name has to be unique across a
    /// shared server, so it carries any `-2` disambiguation; this one is the name bootty meant.
    pub display_name: String,
    pub explicit: bool,
}

#[derive(Debug, Clone)]
pub struct SessionNameStore {
    path: PathBuf,
    binding_id: i64,
    records: HashMap<String, SessionNameRecord>,
}

impl SessionNameStore {
    pub fn for_binding(config_path: &Path, binding_id: i64) -> Self {
        let path = crate::workspace::sqlite_path(config_path);
        Self {
            records: Self::load_records(&path, binding_id),
            path,
            binding_id,
        }
    }

    fn load_records(path: &Path, binding_id: i64) -> HashMap<String, SessionNameRecord> {
        let Ok(conn) = workspace_open_db(path) else {
            return HashMap::new();
        };
        let Ok(mut statement) = conn.prepare(
            "SELECT session_id, cwd, generated_name, session_name, display_name, explicit
             FROM workspace_session_name_metadata
             WHERE binding_id = ?1",
        ) else {
            return HashMap::new();
        };
        let Ok(rows) = statement.query_map([binding_id], |row| {
            Ok(SessionNameRecord {
                session_id: row.get(0)?,
                cwd: row.get(1)?,
                generated_name: row.get(2)?,
                session_name: row.get(3)?,
                display_name: row.get(4)?,
                explicit: row.get::<_, i64>(5)? != 0,
            })
        }) else {
            return HashMap::new();
        };

        rows.filter_map(Result::ok)
            .map(|record| (record.session_id.clone(), record))
            .collect()
    }

    fn save(&self) {
        let binding_id = self.binding_id;
        let Ok(mut conn) = workspace_open_db(&self.path) else {
            return;
        };
        let Ok(tx) = conn.transaction() else {
            return;
        };
        if tx
            .execute(
                "DELETE FROM workspace_session_name_metadata WHERE binding_id = ?1",
                [binding_id],
            )
            .is_err()
        {
            return;
        }
        for record in self.records.values() {
            if tx
                .execute(
                    "INSERT INTO workspace_session_name_metadata
                        (binding_id, session_id, cwd, generated_name, session_name, display_name,
                         explicit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        binding_id,
                        record.session_id,
                        record.cwd,
                        record.generated_name,
                        record.session_name,
                        record.display_name,
                        i64::from(record.explicit)
                    ],
                )
                .is_err()
            {
                return;
            }
        }
        let _ = tx.commit();
    }

    fn matching_key(&self, cwd: &str) -> Option<String> {
        self.records
            .values()
            .find(|record| record.cwd == cwd)
            .map(|record| record.session_id.clone())
    }

    pub fn observe_session(
        &mut self,
        session_id: &str,
        session_name: &str,
        cwd: &str,
    ) -> Option<SessionNameRecord> {
        let key = self.matching_key(cwd)?;
        let mut record = self.records.remove(&key)?;
        let changed = record.session_id != session_id || record.session_name != session_name;
        record.session_id = session_id.to_owned();
        record.session_name = session_name.to_owned();
        let result = record.clone();
        self.records.insert(session_id.to_owned(), record);
        if changed {
            self.save();
        }
        Some(result)
    }

    /// The name this binding last saw for `session_id`. A difference from the session's current
    /// name is a rename this binding has yet to account for.
    pub fn last_observed_name(&self, session_id: &str) -> Option<&str> {
        self.records
            .get(session_id)
            .map(|record| record.session_name.as_str())
    }

    /// Record the name bootty generated for the backend, plus the `display_name` it stands for. They
    /// differ whenever the backend name needed a `-2` suffix to stay unique on a shared server.
    pub fn remember_generated(
        &mut self,
        session_id: &str,
        cwd: &str,
        generated_name: &str,
        display_name: &str,
    ) {
        let existing_key = self.matching_key(cwd);
        if existing_key
            .as_ref()
            .is_some_and(|key| self.records.get(key).is_some_and(|record| record.explicit))
        {
            return;
        }
        if let Some(key) = existing_key
            && key != session_id
        {
            self.records.remove(&key);
        }
        self.records.insert(
            session_id.to_owned(),
            SessionNameRecord {
                session_id: session_id.to_owned(),
                cwd: cwd.to_owned(),
                generated_name: generated_name.to_owned(),
                session_name: generated_name.to_owned(),
                display_name: display_name.to_owned(),
                explicit: false,
            },
        );
        self.save();
    }

    /// Set what bootty shows for `session_id`, leaving the rest of the record alone. Fills in records
    /// written before display names existed.
    pub fn set_display_name(&mut self, session_id: &str, display_name: &str) {
        let Some(record) = self.records.get_mut(session_id) else {
            return;
        };
        if record.display_name == display_name {
            return;
        }
        record.display_name = display_name.to_owned();
        self.save();
    }

    /// The name bootty shows for `session_id`, which is the backend name only when bootty never had
    /// a name of its own for it.
    pub fn display_name(&self, session_id: &str) -> Option<&str> {
        self.records
            .get(session_id)
            .map(|record| record.display_name.as_str())
            .filter(|display_name| !display_name.is_empty())
    }

    /// Record `session_name` as chosen rather than generated, shown as `display_name`. The two differ
    /// when the backend needed a uniqueness suffix the name bootty shows does not carry.
    pub fn mark_explicit(
        &mut self,
        session_id: &str,
        session_name: &str,
        display_name: &str,
        cwd: &str,
    ) {
        let existing_key = self.matching_key(cwd);
        let mut record = existing_key
            .and_then(|key| self.records.remove(&key))
            .unwrap_or_else(|| SessionNameRecord {
                session_id: session_id.to_owned(),
                cwd: cwd.to_owned(),
                generated_name: session_name.to_owned(),
                session_name: session_name.to_owned(),
                display_name: display_name.to_owned(),
                explicit: false,
            });
        record.session_id = session_id.to_owned();
        record.cwd = cwd.to_owned();
        record.session_name = session_name.to_owned();
        record.display_name = display_name.to_owned();
        record.explicit = true;
        self.records.insert(session_id.to_owned(), record);
        self.save();
    }

    pub fn persisted_sessions(&self, names: &[String]) -> Vec<(String, String, String)> {
        names
            .iter()
            .filter_map(|name| {
                self.records
                    .values()
                    .find(|record| {
                        record.session_name == *name
                            || record.session_id == *name
                            || record.generated_name == *name
                    })
                    .map(|record| (record.session_id.clone(), name.clone(), record.cwd.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use crate::workspace::WorkspaceStore;

    use super::*;
    fn temp_config_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bootty-session-names-{name}-{unique}"));
        fs::create_dir_all(&dir).expect("create metadata directory");
        dir.join("config.toml")
    }

    fn store(config_path: &Path) -> SessionNameStore {
        let workspace = WorkspaceStore::for_config_path(config_path);
        SessionNameStore::for_binding(
            config_path,
            workspace.binding_id().expect("default workspace binding"),
        )
    }

    #[test]
    fn generated_name_survives_session_id_discovery() {
        let config = temp_config_path("id");
        let mut store = store(&config);
        store.remember_generated("bootty/main", "/repo", "bootty/main", "bootty/main");

        let record = store
            .observe_session("$1", "bootty/main", "/repo")
            .expect("stored session");

        assert_eq!(record.session_id, "$1");
        assert_eq!(record.cwd, "/repo");
    }

    #[test]
    fn explicit_name_survives_reload() {
        let config = temp_config_path("explicit");
        let mut names = store(&config);
        names.remember_generated("$1", "/repo", "bootty/main", "bootty/main");
        names.mark_explicit("$1", "release", "release", "/repo");

        let mut reloaded = store(&config);
        let record = reloaded
            .observe_session("$1", "release", "/repo")
            .expect("stored session");

        assert!(record.explicit);
        assert_eq!(record.generated_name, "bootty/main");
    }
    #[test]
    fn explicit_name_blocks_later_generated_name_updates() {
        let config = temp_config_path("protected");
        let mut store = store(&config);
        store.remember_generated("$1", "/repo", "project/main", "project/main");
        store.mark_explicit("$1", "release", "release", "/repo");
        store.remember_generated("$1", "/repo", "project/feature", "project/feature");

        let record = store
            .observe_session("$1", "release", "/repo")
            .expect("stored session");

        assert!(record.explicit);
        assert_eq!(record.generated_name, "project/main");
    }

    #[test]
    fn reused_mux_id_does_not_transfer_explicit_name_to_another_worktree() {
        let config = temp_config_path("reused-id");
        let mut store = store(&config);
        store.remember_generated("$1", "/old", "project/main", "project/main");
        store.mark_explicit("$1", "release", "release", "/old");
        store.remember_generated("$1", "/new", "other/main", "other/main");

        let record = store
            .observe_session("$1", "other/main", "/new")
            .expect("new worktree metadata");

        assert!(!record.explicit);
        assert_eq!(record.generated_name, "other/main");
        assert_eq!(record.cwd, "/new");
    }
}
