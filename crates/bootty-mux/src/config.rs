use std::{path::Path, sync::Arc};

use bootty_mux_model::{MuxBackendKind, MuxBindingConfig};

use super::{
    backend::MuxBackend,
    native::NativeBackend,
    process::SystemCommandRunner,
    remote_space::RemoteSpaceBackend,
    rmux::RmuxBackend,
    rmux_remote::RemoteRmuxBackend,
    ssh::{SshCommandRunner, SshRemote},
    tmux::TmuxBackend,
    tmux_control::TmuxControlRunner,
    zellij::ZellijBackend,
};

/// Builds the backend a controller talks to. Held per controller rather than in a global so a test
/// can drive one app against a scripted backend without leaking into every other test in the
/// process. Shared because the refresh and command workers build their own backend off-thread.
pub type BackendFactory = Arc<dyn Fn(&MuxBindingConfig) -> Box<dyn MuxBackend> + Send + Sync>;

struct UnavailableBackend {
    message: String,
}

impl MuxBackend for UnavailableBackend {
    fn snapshot(&self) -> anyhow::Result<crate::snapshot::MuxSnapshot> {
        anyhow::bail!("{}", self.message)
    }

    fn execute(&mut self, _command: crate::command::MuxCommand) -> anyhow::Result<()> {
        anyhow::bail!("{}", self.message)
    }
}

pub fn unavailable_backend(message: impl Into<String>) -> Box<dyn MuxBackend> {
    Box::new(UnavailableBackend {
        message: message.into(),
    })
}

pub fn selected_backend(config: &MuxBindingConfig) -> MuxBackendKind {
    resolve_backend(config.backend, config.remote.is_some(), cfg!(windows))
}

/// Windows has no tmux to fall back from, so a local tmux binding renders natively there. A remote
/// one still resolves to tmux: its client runs on the other host, and this side only runs `ssh`,
/// which Windows does ship.
fn resolve_backend(backend: MuxBackendKind, remote: bool, windows: bool) -> MuxBackendKind {
    if windows && backend == MuxBackendKind::Tmux && !remote {
        return MuxBackendKind::Native;
    }
    backend
}

/// The SSH transport a binding's backend client runs over, or `None` when the multiplexer is this
/// machine's. Remote configs that name a backend without a client are rejected when the config is
/// loaded, so anything reaching here is a backend that can be driven from another host.
pub fn remote_transport(config: &MuxBindingConfig) -> Option<SshRemote> {
    config.remote.clone().map(SshRemote::new)
}

pub fn build_backend(config: &MuxBindingConfig) -> Box<dyn MuxBackend> {
    build_backend_for_workspace(config, None)
}

/// Build the backend for `config`, giving the native backend the mux state belonging to
/// `workspace`. Only the native backend keeps its sessions in this process, so it is the only one a
/// workspace can scope; the others reach a server that is already shared.
pub fn build_backend_for_workspace(
    config: &MuxBindingConfig,
    workspace: Option<&Path>,
) -> Box<dyn MuxBackend> {
    let remote = remote_transport(config);
    if let (Some(remote), Some(space_id)) = (remote.clone(), config.remote_space_id.clone()) {
        return Box::new(RemoteSpaceBackend::new(
            remote,
            space_id,
            selected_backend(config),
        ));
    }
    match selected_backend(config) {
        MuxBackendKind::Rmux => match remote {
            Some(remote) => Box::new(RemoteRmuxBackend::new(remote)),
            None => Box::new(RmuxBackend::new()),
        },
        MuxBackendKind::Native => Box::new(match workspace {
            Some(workspace) => NativeBackend::for_workspace(workspace),
            None => NativeBackend::new(),
        }),
        MuxBackendKind::Tmux => Box::new(match remote {
            Some(remote) => TmuxBackend::with_runner("tmux", TmuxControlRunner::for_remote(remote)),
            None => TmuxBackend::for_identity(bootty_identity::ApplicationIdentity::for_process()),
        }),
        MuxBackendKind::Zellij => match remote {
            Some(remote) => Box::new(ZellijBackend::with_runner(SshCommandRunner::new(
                remote,
                SystemCommandRunner,
            ))),
            None => match ZellijBackend::for_identity(
                bootty_identity::ApplicationIdentity::for_process(),
            ) {
                Ok(backend) => Box::new(backend),
                Err(error) => unavailable_backend(error.to_string()),
            },
        },
    }
}

pub fn build_backend_with(
    factory: Option<&BackendFactory>,
    config: &MuxBindingConfig,
) -> Box<dyn MuxBackend> {
    match factory {
        Some(factory) => factory(config),
        None => build_backend(config),
    }
}
