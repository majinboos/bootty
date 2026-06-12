use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use rmux_sdk::{
    EnsureSession, EnsureSessionPolicy, InfoSnapshot, PaneInfo, PaneProcessState, ProcessSpec,
    Rmux, RmuxEndpoint, SessionName, TerminalSizeSpec,
};
use tokio::runtime::{Builder, Runtime};

use super::{
    backend::MuxBackend,
    command::MuxCommand,
    config::MuxBackendKind,
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, MuxWindow},
};

const DEFAULT_RMUX_SESSION_NAME: &str = "local";

pub trait RmuxSessionClient {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()>;
    fn kill_session(&self, session_name: &str) -> Result<()>;
}

pub struct RmuxBackend<C = SdkRmuxClient> {
    client: C,
}

impl RmuxBackend<SdkRmuxClient> {
    pub fn new() -> Self {
        Self::with_client(SdkRmuxClient::new())
    }
}

impl Default for RmuxBackend<SdkRmuxClient> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> RmuxBackend<C> {
    pub fn with_client(client: C) -> Self {
        Self { client }
    }
}

impl<C: RmuxSessionClient> MuxBackend for RmuxBackend<C> {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Rmux
    }

    fn snapshot(&self) -> Result<MuxSnapshot> {
        let snapshot = self.client.snapshot()?;
        if !snapshot.sessions.is_empty() {
            return Ok(snapshot);
        }

        let cwd = std::env::current_dir()
            .context("resolve cwd for default rmux session")?
            .to_string_lossy()
            .into_owned();
        self.client
            .ensure_session(DEFAULT_RMUX_SESSION_NAME, &cwd)
            .context("bootstrap default rmux session")?;
        self.client.snapshot()
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow { .. } => {
                // Bootty owns rmux selection and pane rendering natively; there is no
                // attached rmux client to switch.
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.client.ensure_session(&session_id, &cwd)?;
            }
            MuxCommand::RenameSession { .. } => {
                anyhow::bail!("rmux-sdk does not expose session rename yet");
            }
            MuxCommand::DitchSession { session_id } => {
                self.client.kill_session(&session_id)?;
            }
            MuxCommand::NewWindow { .. }
            | MuxCommand::ActivateNextWindow { .. }
            | MuxCommand::ActivatePreviousWindow { .. }
            | MuxCommand::ActivateLastWindow { .. }
            | MuxCommand::ActivateWindowIndex { .. }
            | MuxCommand::MoveWindow { .. }
            | MuxCommand::SplitPane { .. }
            | MuxCommand::SelectPane { .. }
            | MuxCommand::SelectNextPane { .. }
            | MuxCommand::KillPane { .. }
            | MuxCommand::TogglePaneZoom { .. } => {
                anyhow::bail!("rmux backend does not support mux command {command:?}");
            }
        }
        Ok(())
    }
}

pub struct SdkRmuxClient;

impl SdkRmuxClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SdkRmuxClient {
    fn default() -> Self {
        Self::new()
    }
}

impl RmuxSessionClient for SdkRmuxClient {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        shared_rmux_runtime()
            .lock()
            .expect("rmux runtime lock")
            .block_on(async {
                let rmux = connect_bootty_rmux().await?;
                let names = rmux.list_sessions().await?;
                let mut sessions = Vec::with_capacity(names.len());
                for name in names {
                    let handle = rmux.session(name.clone()).await?;
                    let info = handle.pane(0, 0).info().await?;
                    sessions.push(session_from_info(&name, &info));
                }
                Ok(MuxSnapshot {
                    active_session_id: sessions
                        .iter()
                        .find(|session| session.active)
                        .map(|session| session.id.clone()),
                    sessions,
                })
            })
    }

    fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()> {
        shared_rmux_runtime()
            .lock()
            .expect("rmux runtime lock")
            .block_on(async {
                let rmux = connect_bootty_rmux().await?;
                let name = SessionName::new(session_name).context("invalid rmux session name")?;
                rmux.ensure_session(
                    EnsureSession::named(name)
                        .policy(EnsureSessionPolicy::CreateOrReuse)
                        .detached(true)
                        .working_directory(cwd)
                        .size(TerminalSizeSpec::new(80, 24))
                        .process(ProcessSpec::default()),
                )
                .await?;
                Ok(())
            })
    }

    fn kill_session(&self, session_name: &str) -> Result<()> {
        shared_rmux_runtime()
            .lock()
            .expect("rmux runtime lock")
            .block_on(async {
                let rmux = connect_bootty_rmux().await?;
                let name = SessionName::new(session_name).context("invalid rmux session name")?;
                let session = rmux.session(name).await?;
                session.kill().await?;
                Ok(())
            })
    }
}

fn shared_rmux_runtime() -> &'static Mutex<Runtime> {
    static RUNTIME: OnceLock<Mutex<Runtime>> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Mutex::new(
            Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rmux sdk runtime should initialize"),
        )
    })
}

async fn connect_bootty_rmux() -> Result<Rmux> {
    let endpoint = rmux_ipc::default_endpoint()
        .context("resolve default rmux endpoint")?
        .into_path();
    Rmux::connect_or_start_at(RmuxEndpoint::UnixSocket(endpoint))
        .await
        .map_err(Into::into)
}

fn session_from_info(name: &SessionName, info: &InfoSnapshot) -> MuxSession {
    let session_info = info.sessions.first();
    let name = name.to_string();
    let active = session_info.is_some_and(|session| session.attached_clients > 0);
    let panes = &info.panes;
    let windows = info
        .windows
        .iter()
        .map(|window| {
            let pane = panes
                .iter()
                .find(|pane| pane.window_id == window.id && pane.index == 0)
                .or_else(|| panes.iter().find(|pane| pane.window_id == window.id));
            MuxWindow {
                id: window.id.to_string(),
                index: window.index,
                name: window.name.clone().unwrap_or_default(),
                active: window.index == 0,
                anchor: anchor_for_pane(&name, pane),
            }
        })
        .collect::<Vec<_>>();
    let anchor = panes
        .iter()
        .find(|pane| pane.index == 0)
        .or_else(|| panes.first())
        .map_or_else(
            || MuxPaneAnchor {
                session_id: name.clone(),
                pane_id: None,
                cwd: session_info.and_then(|session| session.working_directory.clone()),
                process: None,
            },
            |pane| anchor_for_pane(&name, Some(pane)),
        );

    MuxSession {
        id: name,
        name: session_info
            .map(|session| session.name.to_string())
            .unwrap_or_else(|| anchor.session_id.clone()),
        active,
        anchor,
        active_window_id: windows.first().map(|window| window.id.clone()),
        windows,
    }
}

fn anchor_for_pane(session_name: &str, pane: Option<&PaneInfo>) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_name.to_owned(),
        pane_id: pane.map(|pane| pane.id.to_string()),
        cwd: pane.and_then(|pane| pane.working_directory.clone()),
        process: pane.and_then(|pane| match &pane.process {
            PaneProcessState::Running { .. } => pane
                .command
                .as_ref()
                .and_then(|command| command.first())
                .cloned(),
            PaneProcessState::Exited | PaneProcessState::Unknown => None,
            _ => None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    use crate::command::MuxCommand;

    #[derive(Clone, Default)]
    struct RecordingClient {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        snapshot: MuxSnapshot,
    }

    impl RmuxSessionClient for RecordingClient {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            self.calls.borrow_mut().push(vec!["snapshot".to_owned()]);
            Ok(self.snapshot.clone())
        }

        fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "ensure_session".to_owned(),
                session_name.to_owned(),
                cwd.to_owned(),
            ]);
            Ok(())
        }

        fn kill_session(&self, session_name: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(vec!["kill_session".to_owned(), session_name.to_owned()]);
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct EmptyThenLocalClient {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        ensured: Rc<RefCell<bool>>,
    }

    impl RmuxSessionClient for EmptyThenLocalClient {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            self.calls.borrow_mut().push(vec!["snapshot".to_owned()]);
            if !*self.ensured.borrow() {
                return Ok(MuxSnapshot::default());
            }
            Ok(MuxSnapshot {
                active_session_id: None,
                sessions: vec![MuxSession {
                    id: "local".to_owned(),
                    name: "local".to_owned(),
                    active: false,
                    anchor: MuxPaneAnchor {
                        session_id: "local".to_owned(),
                        pane_id: Some("%1".to_owned()),
                        cwd: None,
                        process: None,
                    },
                    active_window_id: Some("@1".to_owned()),
                    windows: vec![MuxWindow {
                        id: "@1".to_owned(),
                        index: 1,
                        name: "shell".to_owned(),
                        active: true,
                        anchor: MuxPaneAnchor {
                            session_id: "local".to_owned(),
                            pane_id: Some("%1".to_owned()),
                            cwd: None,
                            process: None,
                        },
                    }],
                }],
            })
        }

        fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "ensure_session".to_owned(),
                session_name.to_owned(),
                cwd.to_owned(),
            ]);
            *self.ensured.borrow_mut() = true;
            Ok(())
        }

        fn kill_session(&self, session_name: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(vec!["kill_session".to_owned(), session_name.to_owned()]);
            Ok(())
        }
    }

    #[test]
    fn rmux_adapter_uses_sdk_client_not_rmux_cli() {
        let client = RecordingClient::default();
        let calls = client.calls.clone();
        let mut backend = RmuxBackend::with_client(client);

        backend
            .execute(MuxCommand::ActivateWindow {
                session_id: "project".to_owned(),
                window_id: "@2".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::CreateProjectSession {
                session_id: "next".to_owned(),
                cwd: "/next".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::DitchSession {
                session_id: "next".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            &[
                vec![
                    "ensure_session".to_owned(),
                    "next".to_owned(),
                    "/next".to_owned()
                ],
                vec!["kill_session".to_owned(), "next".to_owned()],
            ]
        );
    }

    #[test]
    fn rmux_snapshot_keeps_session_name_as_native_render_target() {
        let client = RecordingClient {
            calls: Rc::default(),
            snapshot: MuxSnapshot {
                active_session_id: Some("alpha".to_owned()),
                sessions: vec![MuxSession {
                    id: "alpha".to_owned(),
                    name: "alpha".to_owned(),
                    active: true,
                    anchor: MuxPaneAnchor {
                        session_id: "alpha".to_owned(),
                        pane_id: Some("%1".to_owned()),
                        cwd: Some("/repo".to_owned()),
                        process: Some("vim".to_owned()),
                    },
                    active_window_id: None,
                    windows: Vec::new(),
                }],
            },
        };
        let backend = RmuxBackend::with_client(client);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id.as_deref(), Some("alpha"));
        assert_eq!(snapshot.sessions[0].id, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.session_id, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn rmux_snapshot_bootstraps_local_session_when_server_is_empty() {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let client = EmptyThenLocalClient::default();
        let calls = client.calls.clone();
        let backend = RmuxBackend::with_client(client);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.sessions[0].id, "local");
        assert_eq!(
            calls.borrow().as_slice(),
            &[
                vec!["snapshot".to_owned()],
                vec!["ensure_session".to_owned(), "local".to_owned(), cwd],
                vec!["snapshot".to_owned()],
            ]
        );
    }
}
