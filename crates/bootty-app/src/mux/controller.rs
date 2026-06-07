use std::{
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use eframe::egui;

use crate::{
    config::MultiplexerConfig,
    mux::{
        command::MuxCommand,
        config::{MuxBackendKind, build_backend, selected_backend},
        snapshot::{MuxSession, MuxSnapshot},
    },
    ui::{chrome, new_session_picker::NewMuxSessionRequest},
};

const MUX_SESSION_REFRESH_INTERVAL: Duration = Duration::from_millis(900);

type SessionRefreshResult = std::result::Result<(MuxBackendKind, MuxSnapshot), String>;
type MuxCommandResult = std::result::Result<Option<String>, String>;

fn selected_window_after_refresh(
    selected_session: Option<&str>,
    current: Option<String>,
    snapshot: &MuxSnapshot,
) -> Option<String> {
    let selected_session = selected_session?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == selected_session || session.name == selected_session)?;
    current
        .filter(|window_id| session.windows.iter().any(|window| &window.id == window_id))
        .or_else(|| session.active_window_id.clone())
}

fn stable_session_order(
    previous: &[MuxSession],
    mut refreshed: Vec<MuxSession>,
) -> Vec<MuxSession> {
    let mut ordered = Vec::with_capacity(refreshed.len());
    for old in previous {
        if let Some(index) = refreshed
            .iter()
            .position(|session| session.id == old.id || session.name == old.name)
        {
            ordered.push(refreshed.remove(index));
        }
    }
    ordered.extend(refreshed);
    ordered
}

#[derive(Default)]
pub struct MuxController {
    sessions: Vec<MuxSession>,
    selected_session: Option<String>,
    selected_window: Option<String>,
    current_backend: Option<MuxBackendKind>,
    last_session_refresh: Option<Instant>,
    session_refresh_rx: Option<mpsc::Receiver<SessionRefreshResult>>,
    mux_command_rx: Option<mpsc::Receiver<MuxCommandResult>>,
}

impl MuxController {
    pub fn new() -> Self {
        Self {
            last_session_refresh: Some(Instant::now() - Duration::from_secs(2)),
            ..Default::default()
        }
    }

    pub fn sessions(&self) -> &[MuxSession] {
        &self.sessions
    }

    pub fn selected_session(&self) -> Option<&str> {
        self.selected_session.as_deref()
    }

    pub fn selected_session_anchor(&self) -> Option<&crate::mux::snapshot::MuxPaneAnchor> {
        let selected = self.selected_session.as_deref()?;
        let session = self
            .sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        if let Some(selected_window) = self.selected_window.as_deref()
            && let Some(window) = session
                .windows
                .iter()
                .find(|window| window.id == selected_window)
        {
            return Some(&window.anchor);
        }
        Some(&session.anchor)
    }

    pub fn selected_session_windows(&self) -> &[crate::mux::snapshot::MuxWindow] {
        let Some(selected) = self.selected_session.as_deref() else {
            return &[];
        };
        self.sessions
            .iter()
            .find(|session| session.id == selected || session.name == selected)
            .map(|session| session.windows.as_slice())
            .unwrap_or_default()
    }

    pub fn selected_window(&self) -> Option<&str> {
        self.selected_window.as_deref()
    }

    pub fn refresh_sessions(
        &mut self,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
    ) -> Option<String> {
        if let Some(result) = self.poll_session_refresh() {
            match result {
                Ok((backend, snapshot)) => {
                    let same_backend = self.current_backend == Some(backend);
                    let current_session =
                        same_backend.then(|| self.selected_session.take()).flatten();
                    let current_window =
                        same_backend.then(|| self.selected_window.take()).flatten();
                    self.apply_snapshot(backend, snapshot, current_session, current_window);
                }
                Err(error) => return Some(error),
            }
        }

        if self
            .last_session_refresh
            .is_some_and(|last| last.elapsed() < MUX_SESSION_REFRESH_INTERVAL)
            || self.session_refresh_rx.is_some()
        {
            return None;
        }

        self.last_session_refresh = Some(Instant::now());
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let mux_config = config.clone();
        thread::spawn(move || {
            let backend_kind = selected_backend(&mux_config);
            let result = build_backend(&mux_config)
                .snapshot()
                .map(|snapshot| (backend_kind, snapshot))
                .map_err(|error| error.to_string());
            if tx.send(result).is_ok() {
                repaint.request_repaint();
            }
        });
        self.session_refresh_rx = Some(rx);
        None
    }

    pub fn poll_command(&mut self) -> Option<Result<(), String>> {
        let result = match self.mux_command_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Some(Err("mux command worker stopped".to_owned()))
            }
        }?;
        self.mux_command_rx = None;

        Some(match result {
            Ok(selected_session) => {
                if let Some(session) = selected_session {
                    self.selected_session = Some(session);
                }
                self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
                Ok(())
            }
            Err(error) => Err(error),
        })
    }

    pub fn activate_session(
        &mut self,
        session_id: &str,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
    ) {
        self.selected_session = Some(session_id.to_owned());
        self.selected_window = None;
        let command = MuxCommand::ActivateSession {
            session_id: session_id.to_owned(),
        };
        if self
            .execute_native_command(config, command.clone(), Some(session_id.to_owned()), None)
            .is_ok()
        {
            ctx.request_repaint();
            return;
        }
        if self.mux_command_rx.is_some() {
            return;
        }
        self.spawn_command(ctx, config, command, Some(session_id.to_owned()));
    }

    pub fn activate_window(
        &mut self,
        session_id: &str,
        window_id: &str,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
    ) {
        self.selected_session = Some(session_id.to_owned());
        self.selected_window = Some(window_id.to_owned());
        let command = MuxCommand::ActivateWindow {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(session_id.to_owned()),
                Some(window_id.to_owned()),
            )
            .is_ok()
        {
            ctx.request_repaint();
            return;
        }
        if self.mux_command_rx.is_some() {
            return;
        }
        self.spawn_command(ctx, config, command, Some(session_id.to_owned()));
    }

    pub fn create_project_session(
        &mut self,
        request: NewMuxSessionRequest,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
    ) {
        let command = MuxCommand::CreateProjectSession {
            session_id: request.session_id.clone(),
            cwd: request.cwd,
        };
        if self
            .execute_native_command(
                config,
                command.clone(),
                Some(request.session_id.clone()),
                None,
            )
            .is_ok()
        {
            ctx.request_repaint();
            return;
        }
        if self.mux_command_rx.is_some() {
            return;
        }
        self.selected_session = Some(request.session_id.clone());
        self.selected_window = None;
        self.spawn_command(ctx, config, command, Some(request.session_id));
    }

    fn poll_session_refresh(&mut self) -> Option<SessionRefreshResult> {
        let result = match self.session_refresh_rx.as_ref()?.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err("mux session refresh worker stopped".to_owned()))
            }
        };
        if result.is_some() {
            self.session_refresh_rx = None;
        }
        result
    }

    pub fn execute_command(
        &mut self,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
        command: MuxCommand,
    ) {
        if self
            .execute_native_command(config, command.clone(), None, None)
            .is_ok()
        {
            ctx.request_repaint();
            return;
        }
        if self.mux_command_rx.is_some() {
            return;
        }
        self.spawn_command(ctx, config, command, None);
    }

    fn execute_native_command(
        &mut self,
        config: &MultiplexerConfig,
        command: MuxCommand,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) -> Result<(), String> {
        if selected_backend(config) != MuxBackendKind::Native {
            return Err("not native".to_owned());
        }
        let mut backend = build_backend(config);
        backend
            .execute(command)
            .and_then(|()| backend.snapshot())
            .map(|snapshot| {
                self.apply_snapshot(
                    MuxBackendKind::Native,
                    snapshot,
                    preferred_session,
                    preferred_window,
                );
                self.last_session_refresh = Some(Instant::now() - MUX_SESSION_REFRESH_INTERVAL);
            })
            .map_err(|error| error.to_string())
    }

    fn apply_snapshot(
        &mut self,
        backend: MuxBackendKind,
        mut snapshot: MuxSnapshot,
        preferred_session: Option<String>,
        preferred_window: Option<String>,
    ) {
        let same_backend = self.current_backend == Some(backend);
        if same_backend {
            snapshot.sessions = stable_session_order(&self.sessions, snapshot.sessions);
        }
        self.selected_session = chrome::selection_after_refresh(preferred_session, &snapshot);
        self.selected_window = selected_window_after_refresh(
            self.selected_session.as_deref(),
            preferred_window,
            &snapshot,
        );
        self.current_backend = Some(backend);
        self.sessions = snapshot.sessions;
    }

    fn spawn_command(
        &mut self,
        ctx: &egui::Context,
        config: &MultiplexerConfig,
        command: MuxCommand,
        selected_session: Option<String>,
    ) {
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let mux_config = config.clone();
        thread::spawn(move || {
            let mut backend = build_backend(&mux_config);
            let result = backend
                .execute(command)
                .map(|()| selected_session)
                .map_err(|error| error.to_string());
            if tx.send(result).is_ok() {
                repaint.request_repaint();
            }
        });
        self.mux_command_rx = Some(rx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::snapshot::{MuxPaneAnchor, MuxWindow};

    #[test]
    fn selected_session_anchor_resolves_by_backend_id_or_session_name() {
        let anchor = MuxPaneAnchor {
            session_id: "$7".to_owned(),
            pane_id: Some("%9".to_owned()),
            cwd: None,
            process: None,
        };
        let mut controller = MuxController {
            sessions: vec![MuxSession {
                id: "$7".to_owned(),
                name: "piu".to_owned(),
                active: false,
                anchor: anchor.clone(),
                active_window_id: Some("@2".to_owned()),
                windows: vec![MuxWindow {
                    id: "@2".to_owned(),
                    index: 1,
                    name: "editor".to_owned(),
                    active: true,
                    anchor: MuxPaneAnchor {
                        session_id: "$7".to_owned(),
                        pane_id: Some("%11".to_owned()),
                        cwd: None,
                        process: Some("nvim".to_owned()),
                    },
                }],
            }],
            selected_session: Some("piu".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            controller
                .selected_session_anchor()
                .map(|anchor| anchor.session_id.as_str()),
            Some("$7")
        );

        controller.selected_session = Some("$7".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%9")
        );

        controller.selected_window = Some("@2".to_owned());
        assert_eq!(
            controller
                .selected_session_anchor()
                .and_then(|anchor| anchor.pane_id.as_deref()),
            Some("%11")
        );
    }

    #[test]
    fn stable_session_order_preserves_existing_order_and_appends_new_sessions() {
        let previous = vec![
            session("$2", "work"),
            session("$1", "main"),
            session("$4", "old"),
        ];
        let refreshed = vec![
            session("$1", "main"),
            session("$3", "new"),
            session("$2", "work"),
        ];

        let ordered = stable_session_order(&previous, refreshed);

        assert_eq!(
            ordered
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["$2", "$1", "$3"]
        );
    }

    fn session(id: &str, name: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: None,
                cwd: None,
                process: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }
}
