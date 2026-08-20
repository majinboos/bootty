use std::path::PathBuf;

use anyhow::{Context, Result};

#[cfg(feature = "app")]
use super::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::MuxScope,
};
use super::{
    command::MuxCommand,
    process::{CommandRunner, SystemCommandRunner, require_success},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot},
};

#[derive(Clone, Debug)]
pub struct ZellijBackend<R = SystemCommandRunner> {
    runner: R,
    socket_dir: Option<PathBuf>,
}

impl ZellijBackend<SystemCommandRunner> {
    pub fn new() -> Self {
        Self::with_runner(SystemCommandRunner)
    }

    pub fn for_identity(identity: bootty_identity::ApplicationIdentity) -> Result<Self> {
        Ok(Self {
            runner: SystemCommandRunner,
            socket_dir: prepare_socket_dir(identity)?,
        })
    }
}

impl Default for ZellijBackend<SystemCommandRunner> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R> ZellijBackend<R> {
    pub fn with_runner(runner: R) -> Self {
        Self {
            runner,
            socket_dir: None,
        }
    }
}

impl<R: CommandRunner> ZellijBackend<R> {
    fn command(&self, args: &[String]) -> Result<crate::process::CommandOutput> {
        let Some(socket_dir) = &self.socket_dir else {
            return self.runner.run("zellij", args);
        };
        let output = std::process::Command::new("zellij")
            .args(args)
            .env("ZELLIJ_SOCKET_DIR", socket_dir)
            .output()
            .context("run zellij")?;
        Ok(crate::process::CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
        let output = self.command(&args)?;
        require_success("zellij", &args, output)
    }

    fn run_owned(&self, args: Vec<String>) -> Result<String> {
        let output = self.command(&args)?;
        require_success("zellij", &args, output)
    }
}

pub(crate) fn prepare_socket_dir(
    identity: bootty_identity::ApplicationIdentity,
) -> Result<Option<PathBuf>> {
    if identity == bootty_identity::ApplicationIdentity::Production {
        return Ok(None);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut path = rmux_ipc::endpoint_for_label(identity.namespace())?.into_path();
        path.pop();
        path.push("zellij");
        std::fs::create_dir_all(&path)
            .with_context(|| format!("create zellij socket directory {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("secure zellij socket directory {}", path.display()))?;
        Ok(Some(path))
    }

    #[cfg(not(unix))]
    anyhow::bail!("BoottyDev local zellij is unavailable on this platform")
}

impl<R: CommandRunner> ZellijBackend<R> {
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        let output = self.run(&["list-sessions", "--short", "--no-formatting"])?;
        Ok(parse_zellij_snapshot(&output))
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow { .. } => {
                anyhow::bail!("zellij native window activation is not implemented");
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.run_owned(vec![
                    "--layout-string".into(),
                    "layout {\n  pane\n}".into(),
                    "attach".into(),
                    "--create-background".into(),
                    session_id,
                    "options".into(),
                    "--pane-frames".into(),
                    "false".into(),
                    "--simplified-ui".into(),
                    "true".into(),
                    "--show-startup-tips".into(),
                    "false".into(),
                    "--default-cwd".into(),
                    cwd,
                ])?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.run_owned(vec!["action".into(), "switch-session".into(), session_id])?;
                self.run_owned(vec!["action".into(), "rename-session".into(), name])?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.run_owned(vec!["kill-session".into(), session_id])?;
            }
            MuxCommand::RenameWindow { .. } => {
                anyhow::bail!("zellij backend does not support window rename");
            }
            MuxCommand::NewWindow { .. }
            | MuxCommand::ActivateNextWindow { .. }
            | MuxCommand::ActivatePreviousWindow { .. }
            | MuxCommand::ActivateLastWindow { .. }
            | MuxCommand::ActivateWindowIndex { .. }
            | MuxCommand::MoveWindow { .. }
            | MuxCommand::MoveWindowPreservingSelection { .. }
            | MuxCommand::SplitPane { .. }
            | MuxCommand::SelectPane { .. }
            | MuxCommand::SelectNextPane { .. }
            | MuxCommand::SelectPreviousPane { .. }
            | MuxCommand::KillPane { .. }
            | MuxCommand::ClosePane { .. }
            | MuxCommand::TogglePaneZoom { .. } => {
                anyhow::bail!("zellij backend does not support mux command {command:?}");
            }
        }
        Ok(())
    }
}

#[cfg(feature = "app")]
impl<R: CommandRunner> MuxBackend for ZellijBackend<R> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        ZellijBackend::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        ZellijBackend::execute(self, command)
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        zellij_capabilities(scope)
    }
}

#[cfg(feature = "app")]
pub(crate) fn zellij_capabilities(scope: MuxScope) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(
        scope,
        [
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
}

fn parse_zellij_snapshot(output: &str) -> MuxSnapshot {
    let sessions = output
        .lines()
        .filter_map(|line| {
            let name = line.trim();
            if name.is_empty() || name.starts_with("No active zellij sessions") {
                return None;
            }
            Some(MuxSession {
                id: name.to_owned(),
                name: name.to_owned(),
                active: false,
                anchor: MuxPaneAnchor {
                    session_id: name.to_owned(),
                    pane_id: None,
                    pane_pid: None,
                    cwd: None,
                    process: None,
                },
                active_window_id: None,
                windows: Vec::new(),
            })
        })
        .collect();
    MuxSnapshot {
        sessions,
        active_session_id: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    use crate::{
        command::MuxCommand,
        process::{CommandOutput, CommandRunner},
    };

    #[derive(Clone, Default)]
    struct RecordingRunner {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
        stdout: String,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[String]) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_owned()];
            call.extend(args.iter().cloned());
            self.calls.borrow_mut().push(call);
            Ok(CommandOutput {
                success: true,
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    #[test]
    fn zellij_adapter_translates_lifecycle_without_tmux_fallback() {
        let runner = RecordingRunner::default();
        let calls = runner.calls.clone();
        let mut backend = ZellijBackend::with_runner(runner);

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
        backend
            .execute(MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "renamed".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            vec![
                vec![
                    "zellij",
                    "--layout-string",
                    "layout {\n  pane\n}",
                    "attach",
                    "--create-background",
                    "next",
                    "options",
                    "--pane-frames",
                    "false",
                    "--simplified-ui",
                    "true",
                    "--show-startup-tips",
                    "false",
                    "--default-cwd",
                    "/next"
                ],
                vec!["zellij", "kill-session", "next"],
                vec!["zellij", "action", "switch-session", "project"],
                vec!["zellij", "action", "rename-session", "renamed"],
            ]
            .into_iter()
            .map(|call| call.into_iter().map(str::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>()
            .as_slice()
        );
    }

    #[test]
    fn zellij_snapshot_maps_list_sessions_without_active_fallback() {
        let runner = RecordingRunner {
            calls: Rc::default(),
            stdout: "alpha\nbeta\n".to_owned(),
        };
        let backend = ZellijBackend::with_runner(runner);

        let snapshot = backend.snapshot().unwrap();

        assert_eq!(snapshot.active_session_id, None);
        assert_eq!(snapshot.sessions[0].id, "alpha");
        assert_eq!(snapshot.sessions[0].anchor.session_id, "alpha");
    }
}
