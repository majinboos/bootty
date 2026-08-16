#![allow(clippy::assigning_clones)]

use std::collections::HashMap;

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

/// Binding-scoped session naming metadata.
///
/// This is a persistence-free value type. `WorkspaceRepository` owns its `SQLite` representation
/// and publishes a replacement only after the corresponding transaction commits.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionNameStore {
    records: HashMap<String, SessionNameRecord>,
}

impl SessionNameStore {
    pub(crate) fn from_records(records: HashMap<String, SessionNameRecord>) -> Self {
        Self { records }
    }

    pub(crate) fn records(&self) -> &HashMap<String, SessionNameRecord> {
        &self.records
    }

    fn matching_key(&self, cwd: &str) -> Option<String> {
        if cwd.is_empty() {
            return None;
        }
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
        record.session_id = session_id.to_owned();
        record.session_name = session_name.to_owned();
        let result = record.clone();
        self.records.insert(session_id.to_owned(), record);
        Some(result)
    }

    /// The name this binding last saw for `session_id`. A difference from the session's current
    /// name is a rename this binding has yet to account for.
    pub fn last_observed_name(&self, session_id: &str) -> Option<&str> {
        self.records
            .get(session_id)
            .map(|record| record.session_name.as_str())
    }

    pub fn record(&self, session_id: &str) -> Option<&SessionNameRecord> {
        self.records.get(session_id)
    }

    pub(crate) fn remove_identity(&mut self, session_id: &str) -> bool {
        self.records.remove(session_id).is_some()
    }

    /// Record the name bootty generated for the backend, plus the `display_name` it stands for. They
    /// differ whenever the backend name needed a `-2` suffix to stay unique on a shared server.
    pub fn remember_generated(
        &mut self,
        session_id: &str,
        cwd: &str,
        generated_name: &str,
        display_name: &str,
    ) -> bool {
        let existing_key = self.matching_key(cwd);
        if existing_key
            .as_ref()
            .is_some_and(|key| self.records.get(key).is_some_and(|record| record.explicit))
        {
            return false;
        }
        if let Some(key) = existing_key
            && key != session_id
        {
            self.records.remove(&key);
        }
        let next = SessionNameRecord {
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            generated_name: generated_name.to_owned(),
            session_name: generated_name.to_owned(),
            display_name: display_name.to_owned(),
            explicit: false,
        };
        let changed = self.records.get(session_id) != Some(&next);
        self.records.insert(session_id.to_owned(), next);
        changed
    }

    /// Take a record back as generated under `generated_name`. Used when a name that looked like
    /// someone else's rename turns out to be one bootty asked the backend for.
    pub fn reclaim_generated(&mut self, session_id: &str, generated_name: &str) -> bool {
        let Some(record) = self.records.get_mut(session_id) else {
            return false;
        };
        if !record.explicit && record.generated_name == generated_name {
            return false;
        }
        record.generated_name = generated_name.to_owned();
        record.explicit = false;
        true
    }

    /// Set what bootty shows for `session_id`, leaving the rest of the record alone. Fills in records
    /// written before display names existed.
    pub fn set_display_name(&mut self, session_id: &str, display_name: &str) -> bool {
        let Some(record) = self.records.get_mut(session_id) else {
            return false;
        };
        if record.display_name == display_name {
            return false;
        }
        record.display_name = display_name.to_owned();
        true
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
    ) -> bool {
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
        let previous = record.clone();
        record.session_id = session_id.to_owned();
        record.cwd = cwd.to_owned();
        record.session_name = session_name.to_owned();
        record.display_name = display_name.to_owned();
        record.explicit = true;
        let changed = previous != record || self.records.get(session_id) != Some(&record);
        self.records.insert(session_id.to_owned(), record);
        changed
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
