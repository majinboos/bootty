use std::{
    path::Path,
    time::{Duration, Instant},
};

use crate::config::{BoottyConfig, ConfigFileSnapshot, config_file_snapshot};

pub const CONFIG_HOT_RELOAD_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct ConfigHotReload {
    last_check: Instant,
    snapshot: ConfigFileSnapshot,
}

impl ConfigHotReload {
    pub fn new(path: &Path) -> Self {
        Self {
            last_check: Instant::now(),
            snapshot: snapshot_for_config_path(path),
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

    pub fn refresh_after_reload(&mut self, path: &Path) {
        self.snapshot = snapshot_for_config_path(path);
    }
}

pub fn new_session_only_config_changed(previous: &BoottyConfig, next: &BoottyConfig) -> bool {
    previous.session != next.session
        || previous.window.width != next.window.width
        || previous.window.height != next.window.height
        || previous.window.macos_titlebar_style != next.window.macos_titlebar_style
}

fn snapshot_for_config_path(path: &Path) -> ConfigFileSnapshot {
    config_file_snapshot(path)
        .unwrap_or_else(|_| ConfigFileSnapshot::from_paths([path.to_path_buf()]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChromeConfig, MacosTitlebarStyle};

    #[test]
    fn reload_scope_treats_session_and_window_size_as_new_session_only() {
        let previous = BoottyConfig::default();
        let mut next = previous.clone();
        next.chrome = ChromeConfig {
            sidebar: false,
            ..next.chrome
        };
        assert!(!new_session_only_config_changed(&previous, &next));

        next.session.shell = Some("/bin/bash".to_owned());
        assert!(new_session_only_config_changed(&previous, &next));

        let mut next = previous.clone();
        next.window.width = 900.0;
        assert!(new_session_only_config_changed(&previous, &next));

        let mut next = previous.clone();
        next.window.macos_titlebar_style = MacosTitlebarStyle::Hidden;
        assert!(new_session_only_config_changed(&previous, &next));
    }
}
