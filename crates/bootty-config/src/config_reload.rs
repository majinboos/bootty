use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::config::{
    BoottyConfig, ConfigFileSnapshot, ConfigResult, config_dependency_snapshot, load_config_attempt,
};

pub const CONFIG_HOT_RELOAD_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct ConfigHotReload {
    path: PathBuf,
    last_check: Instant,
    snapshot: ConfigFileSnapshot,
}

impl ConfigHotReload {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            last_check: Instant::now(),
            snapshot: config_dependency_snapshot(path),
        }
    }

    pub fn changed(&mut self, now: Instant) -> bool {
        if now.duration_since(self.last_check) < CONFIG_HOT_RELOAD_INTERVAL {
            return false;
        }
        self.last_check = now;
        let current = self.snapshot.refresh_known_paths();
        if current == self.snapshot {
            return false;
        }
        self.snapshot = current;
        true
    }

    pub fn reload_config(&mut self) -> ConfigResult<BoottyConfig> {
        let attempt = load_config_attempt(&self.path);
        self.snapshot = attempt.snapshot;
        attempt.config
    }

    pub fn refresh_dependency_graph(&mut self) {
        self.snapshot = config_dependency_snapshot(&self.path);
    }
}
