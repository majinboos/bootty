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

#[allow(clippy::float_cmp)]
pub fn new_session_only_config_changed(previous: &BoottyConfig, next: &BoottyConfig) -> bool {
    previous.session != next.session
        || previous.window.width != next.window.width
        || previous.window.height != next.window.height
        || previous.window.fullscreen != next.window.fullscreen
        || previous.window.window_decoration != next.window.window_decoration
        || previous.window.macos_titlebar_style != next.window.macos_titlebar_style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ChromeConfig, MacosTitlebarStyle, WindowDecoration, WindowFullscreen};

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
        next.window.fullscreen = WindowFullscreen::NonNative;
        assert!(new_session_only_config_changed(&previous, &next));

        let mut next = previous.clone();
        next.window.window_decoration = WindowDecoration::None;
        assert!(new_session_only_config_changed(&previous, &next));
        let mut next = previous.clone();
        next.window.macos_titlebar_style = MacosTitlebarStyle::Hidden;
        assert!(new_session_only_config_changed(&previous, &next));
    }
}
