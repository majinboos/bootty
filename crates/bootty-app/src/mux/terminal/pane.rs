use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    hash::{Hash, Hasher},
    path::Path,
    sync::Arc,
};

use anyhow::Result;
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::terminal_frame::RenderFrame;
use derive_more::{Deref, DerefMut};

use crate::{
    config::MultiplexerConfig,
    mux::{
        config::{MuxBackendKind, selected_backend},
        snapshot::MuxPaneAnchor,
    },
    renderer::TerminalRenderSource,
    terminal::{DrainStats, KeyInput, MouseInput, TerminalSession, TerminalSessionConfig},
};

use super::{rmux_native::RmuxNativeTerminal, tmux_control::TmuxControlTerminal};

#[derive(Deref, DerefMut)]
pub struct BackendPaneTerminal {
    backend: MuxBackendKind,
    pub(super) active_target: Option<MuxPaneTarget>,
    geometry: TerminalGeometry,
    terminal_config: TerminalSessionConfig,
    repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    native_terminals: HashMap<MuxPaneTarget, ActiveTerminalRuntime>,
    #[deref]
    #[deref_mut]
    terminal: ActiveTerminalRuntime,
}

#[derive(Deref, DerefMut)]
#[deref(forward)]
#[deref_mut(forward)]
pub struct ActiveTerminalRuntime(Box<dyn TerminalRuntime>);

impl ActiveTerminalRuntime {
    fn idle() -> Self {
        Self(Box::new(IdleRenderSource))
    }
}

pub trait TerminalRuntime: TerminalRenderSource {
    fn drain_pty(&mut self) -> DrainStats;
    fn pending_pty_len(&self) -> usize;
    fn child_exited(&mut self) -> Result<bool>;
    fn set_colors(
        &mut self,
        colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()>;
    fn write_input(&mut self, bytes: &[u8]) -> Result<()>;
    fn write_paste(&mut self, text: &str) -> Result<()>;
    fn encode_key(&mut self, input: KeyInput) -> Result<()>;
    fn encode_focus(&mut self, gained: bool) -> Result<()>;
    fn encode_mouse(&mut self, input: MouseInput) -> Result<()>;
    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()>;
}
struct IdleRenderSource;

impl TerminalRenderSource for IdleRenderSource {
    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(RenderFrame::default()))
    }
}

impl TerminalRuntime for IdleRenderSource {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn set_colors(
        &mut self,
        _colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()> {
        Ok(())
    }

    fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn write_paste(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }
    fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
        Ok(())
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
        Ok(())
    }
}

impl TerminalRuntime for TerminalSession {
    fn drain_pty(&mut self) -> DrainStats {
        Self::drain_pty(self)
    }

    fn pending_pty_len(&self) -> usize {
        Self::pending_pty_len(self)
    }

    fn child_exited(&mut self) -> Result<bool> {
        Self::child_exited(self)
    }

    fn set_colors(
        &mut self,
        colors: bootty_terminal::terminal_engine::TerminalColorConfig,
    ) -> Result<()> {
        Self::set_colors(self, colors)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        Self::write_input(self, bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        Self::write_paste(self, text)
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        Self::encode_key(self, input)
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        Self::encode_focus(self, gained)
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        Self::encode_mouse(self, input)
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        Self::handle_mouse_wheel(self, input, scroll_delta)
    }
}

impl BackendPaneTerminal {
    pub fn new(
        geometry: TerminalGeometry,
        config: &MultiplexerConfig,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self::new_with_backend(
            geometry,
            selected_backend(config),
            terminal_config,
            repaint_wakeup,
        )
    }

    pub(super) fn new_with_backend(
        geometry: TerminalGeometry,
        backend: MuxBackendKind,
        terminal_config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        Self {
            backend,
            active_target: None,
            geometry,
            terminal_config,
            repaint_wakeup,
            native_terminals: HashMap::new(),
            terminal: ActiveTerminalRuntime::idle(),
        }
    }

    pub fn sync_mux_anchor(
        &mut self,
        config: &MultiplexerConfig,
        anchor: Option<&MuxPaneAnchor>,
    ) -> Result<()> {
        let backend = selected_backend(config);
        if self.backend == backend && target_matches_anchor(self.active_target.as_ref(), anchor) {
            return Ok(());
        }

        let target = anchor.cloned().map(MuxPaneTarget::from);
        self.park_native_terminal();
        let terminal = self
            .start_terminal(backend, target.as_ref())
            .inspect_err(|_| {
                self.backend = backend;
                self.active_target = None;
                self.clear_terminal();
            })?;

        self.backend = backend;
        self.active_target = target;
        self.terminal = terminal;
        Ok(())
    }

    pub fn set_terminal_config(&mut self, terminal_config: TerminalSessionConfig) {
        self.terminal_config = terminal_config;
    }

    fn start_terminal(
        &mut self,
        backend: MuxBackendKind,
        target: Option<&MuxPaneTarget>,
    ) -> Result<ActiveTerminalRuntime> {
        let Some(target) = target else {
            return Ok(ActiveTerminalRuntime::idle());
        };

        if backend == MuxBackendKind::Native {
            if let Some(mut terminal) = self.native_terminals.remove(target) {
                terminal.resize(self.geometry)?;
                return Ok(terminal);
            }
            let mut config = self.terminal_config.clone();
            config.launch.working_directory = target.cwd().map(Path::new).map(Path::to_path_buf);
            config.max_scrollback = bootty_terminal::terminal_engine::NATIVE_MAX_SCROLLBACK;
            Ok(ActiveTerminalRuntime(Box::new(
                TerminalSession::new_with_config(
                    self.geometry,
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?,
            )))
        } else if matches!(backend, MuxBackendKind::Tmux | MuxBackendKind::Zellij) {
            let config =
                backend_attach_session_config(self.terminal_config.clone(), backend, target)?;
            Ok(ActiveTerminalRuntime(Box::new(
                TerminalSession::new_with_config(
                    self.geometry,
                    config,
                    Arc::clone(&self.repaint_wakeup),
                )?,
            )))
        } else if backend == MuxBackendKind::Rmux {
            Ok(ActiveTerminalRuntime(Box::new(RmuxNativeTerminal::new(
                target.clone(),
                self.geometry,
                self.terminal_config.colors.clone(),
            )?)))
        } else {
            Ok(ActiveTerminalRuntime(Box::new(TmuxControlTerminal::new(
                backend,
                target.clone(),
                self.geometry,
                self.terminal_config.colors.clone(),
                self.terminal_config.macos_option_as_alt,
                Arc::clone(&self.repaint_wakeup),
            )?)))
        }
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn child_exited(&mut self) -> Result<bool> {
        if !self.terminal.child_exited()? {
            return Ok(false);
        }
        Ok(false)
    }

    fn clear_terminal(&mut self) {
        self.terminal = ActiveTerminalRuntime::idle();
    }

    fn park_native_terminal(&mut self) {
        if self.backend != MuxBackendKind::Native {
            return;
        }
        let Some(target) = self.active_target.clone() else {
            return;
        };
        let terminal = std::mem::replace(&mut self.terminal, ActiveTerminalRuntime::idle());
        self.native_terminals.insert(target, terminal);
    }
}

impl TerminalRenderSource for BackendPaneTerminal {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.geometry = geometry;
        self.terminal.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.terminal.extract_frame()
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.terminal.scroll_viewport_delta(delta)
    }
}

#[derive(Clone, Debug, Eq)]
pub(super) enum MuxPaneTarget {
    Session {
        session_id: String,
        cwd: Option<String>,
    },
    Pane {
        session_id: String,
        pane_id: String,
        cwd: Option<String>,
    },
}

impl PartialEq for MuxPaneTarget {
    fn eq(&self, other: &Self) -> bool {
        self.session_id() == other.session_id() && self.input_selector() == other.input_selector()
    }
}

impl Hash for MuxPaneTarget {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.session_id().hash(state);
        self.input_selector().hash(state);
    }
}

impl MuxPaneTarget {
    pub(super) fn session_id(&self) -> &str {
        match self {
            Self::Session { session_id, .. } | Self::Pane { session_id, .. } => session_id,
        }
    }

    pub(super) fn input_selector(&self) -> &str {
        match self {
            Self::Pane { pane_id, .. } => pane_id,
            target => target.session_id(),
        }
    }

    fn cwd(&self) -> Option<&str> {
        match self {
            Self::Session { cwd, .. } | Self::Pane { cwd, .. } => cwd.as_deref(),
        }
    }

    pub(super) fn tmux_pane_number(&self) -> Option<usize> {
        let Self::Pane { pane_id, .. } = self else {
            return None;
        };
        pane_id.strip_prefix('%')?.parse().ok()
    }
}

impl From<MuxPaneAnchor> for MuxPaneTarget {
    fn from(anchor: MuxPaneAnchor) -> Self {
        match anchor.pane_id {
            Some(pane_id) => Self::Pane {
                session_id: anchor.session_id,
                pane_id,
                cwd: anchor.cwd,
            },
            None => Self::Session {
                session_id: anchor.session_id,
                cwd: anchor.cwd,
            },
        }
    }
}

fn target_matches_anchor(target: Option<&MuxPaneTarget>, anchor: Option<&MuxPaneAnchor>) -> bool {
    match (target, anchor) {
        (None, None) => true,
        (Some(target), Some(anchor)) => {
            let anchor_selector = anchor.pane_id.as_deref().unwrap_or(&anchor.session_id);
            target.session_id() == anchor.session_id && target.input_selector() == anchor_selector
        }
        _ => false,
    }
}

pub(super) fn backend_attach_launch(
    backend: MuxBackendKind,
    target: &MuxPaneTarget,
) -> (String, Vec<String>) {
    let session = target.session_id().to_owned();
    match backend {
        MuxBackendKind::Tmux => (
            "tmux".to_owned(),
            vec!["attach-session".to_owned(), "-t".to_owned(), session],
        ),
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => unreachable!("native panes are rendered directly by Bootty"),
        MuxBackendKind::Zellij => (
            "zellij".to_owned(),
            vec!["attach".to_owned(), "--create".to_owned(), session],
        ),
    }
}

fn backend_attach_env_remove(backend: MuxBackendKind) -> Vec<String> {
    match backend {
        MuxBackendKind::Tmux => vec!["TMUX".to_owned()],
        MuxBackendKind::Rmux => unreachable!("rmux is rendered natively via rmux-sdk"),
        MuxBackendKind::Native => unreachable!("native panes are rendered directly by Bootty"),
        MuxBackendKind::Zellij => vec!["ZELLIJ".to_owned()],
    }
}

fn backend_attach_session_config(
    mut config: TerminalSessionConfig,
    backend: MuxBackendKind,
    target: &MuxPaneTarget,
) -> Result<TerminalSessionConfig> {
    let (program, args) = backend_attach_launch(backend, target);
    config.launch.shell = Some(resolve_launch_program(&program)?);
    config.launch.args = args;
    config.launch.env_remove = backend_attach_env_remove(backend);
    config.launch.term = "xterm-256color".to_owned();
    Ok(config)
}

fn resolve_launch_program(program: &str) -> Result<String> {
    resolve_launch_program_with_path(program, env::var_os("PATH").as_deref())
}

fn resolve_launch_program_with_path(program: &str, path: Option<&OsStr>) -> Result<String> {
    if Path::new(program).is_absolute() {
        return Ok(program.to_owned());
    }
    if let Some(found) = path
        .into_iter()
        .flat_map(env::split_paths)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
    {
        return Ok(found.to_string_lossy().into_owned());
    }
    anyhow::bail!("backend attach program {program:?} not found in PATH")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootty_terminal::terminal_engine::TerminalColorConfig;
    use tempfile::TempDir;

    use crate::config::{MultiplexerBackendConfig, MultiplexerConfig};

    fn terminal_config() -> TerminalSessionConfig {
        TerminalSessionConfig {
            launch: Default::default(),
            colors: TerminalColorConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
        }
    }

    fn target(session_id: &str) -> MuxPaneTarget {
        MuxPaneTarget::Session {
            session_id: session_id.to_owned(),
            cwd: None,
        }
    }

    #[test]
    fn attach_target_uses_session_and_pane_identity_not_process_metadata() {
        let before = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo".to_owned()),
            process: Some("nvim".to_owned()),
        };
        let after = MuxPaneAnchor {
            process: Some("zsh".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            ..before.clone()
        };

        assert_eq!(MuxPaneTarget::from(before), MuxPaneTarget::from(after));
    }

    #[test]
    fn sync_mux_anchor_does_not_commit_target_after_restart_failure() {
        let geometry = TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        };
        let mut terminal = BackendPaneTerminal::new_with_backend(
            geometry,
            MuxBackendKind::Tmux,
            terminal_config(),
            Arc::new(|| {}),
        );

        let anchor = MuxPaneAnchor {
            session_id: String::new(),
            pane_id: Some("%11".to_owned()),
            cwd: None,
            process: None,
        };
        let result = terminal.sync_mux_anchor(
            &MultiplexerConfig {
                backend: MultiplexerBackendConfig::Rmux,
            },
            Some(&anchor),
        );

        assert!(result.is_err());
        assert_eq!(terminal.active_target, None);
    }

    #[test]
    fn target_match_uses_session_and_pane_without_cloning_metadata() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: Some("/repo".to_owned()),
        };
        let anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: Some("%3".to_owned()),
            cwd: Some("/repo/subdir".to_owned()),
            process: Some("zsh".to_owned()),
        };

        assert!(target_matches_anchor(Some(&target), Some(&anchor)));
    }

    #[test]
    fn target_match_distinguishes_missing_and_changed_panes() {
        let target = MuxPaneTarget::Pane {
            session_id: "agents".to_owned(),
            pane_id: "%3".to_owned(),
            cwd: None,
        };
        let session_anchor = MuxPaneAnchor {
            session_id: "agents".to_owned(),
            pane_id: None,
            cwd: None,
            process: None,
        };
        let other_pane = MuxPaneAnchor {
            pane_id: Some("%4".to_owned()),
            ..session_anchor.clone()
        };

        assert!(!target_matches_anchor(Some(&target), Some(&session_anchor)));
        assert!(!target_matches_anchor(Some(&target), Some(&other_pane)));
        assert!(target_matches_anchor(None, None));
    }

    #[test]
    fn backend_owned_ui_launches_normal_backend_attach() {
        assert_eq!(
            backend_attach_launch(MuxBackendKind::Tmux, &target("agents")),
            (
                "tmux".to_owned(),
                vec![
                    "attach-session".to_owned(),
                    "-t".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
        assert_eq!(
            backend_attach_launch(MuxBackendKind::Zellij, &target("agents")),
            (
                "zellij".to_owned(),
                vec![
                    "attach".to_owned(),
                    "--create".to_owned(),
                    "agents".to_owned()
                ]
            )
        );
    }

    #[test]
    fn backend_owned_ui_removes_nested_backend_environment() {
        assert_eq!(
            backend_attach_env_remove(MuxBackendKind::Tmux),
            vec!["TMUX".to_owned()]
        );
        assert_eq!(
            backend_attach_env_remove(MuxBackendKind::Zellij),
            vec!["ZELLIJ".to_owned()]
        );
    }

    #[test]
    fn backend_owned_ui_uses_tmux_compatible_term() {
        let mut config = TerminalSessionConfig {
            launch: crate::terminal::SessionLaunchConfig {
                term: "xterm-ghostty".to_owned(),
                ..Default::default()
            },
            colors: TerminalColorConfig::default(),
            max_scrollback: 0,
            macos_option_as_alt: Default::default(),
        };
        let (program, args) = backend_attach_launch(MuxBackendKind::Tmux, &target("agents"));
        config.launch.shell = Some(program);
        config.launch.args = args;
        config.launch.env_remove = backend_attach_env_remove(MuxBackendKind::Tmux);
        config.launch.term = "xterm-256color".to_owned();

        assert_eq!(config.launch.term, "xterm-256color");
        assert_eq!(config.launch.env_remove, vec!["TMUX".to_owned()]);
    }

    #[test]
    fn backend_attach_program_is_resolved_to_absolute_path() {
        let temp = TempDir::new().unwrap();
        let program = temp.path().join("tmux");
        std::fs::write(&program, "").unwrap();

        let resolved = resolve_launch_program_with_path("tmux", Some(temp.path().as_os_str()))
            .expect("program should resolve from supplied PATH");

        assert_eq!(resolved, program.to_string_lossy());
    }
}
