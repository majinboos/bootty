use std::{path::Path, sync::Arc};

use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};

use super::{
    backend::MuxBackend,
    native::NativeBackend,
    process::SystemCommandRunner,
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
pub type BackendFactory = Arc<dyn Fn(&MultiplexerConfig) -> Box<dyn MuxBackend> + Send + Sync>;

pub fn selected_backend(config: &MultiplexerConfig) -> MultiplexerBackendConfig {
    resolve_backend(config.backend, config.remote.is_some(), cfg!(windows))
}

/// Windows has no tmux to fall back from, so a local tmux binding renders natively there. A remote
/// one still resolves to tmux: its client runs on the other host, and this side only runs `ssh`,
/// which Windows does ship.
fn resolve_backend(
    backend: MultiplexerBackendConfig,
    remote: bool,
    windows: bool,
) -> MultiplexerBackendConfig {
    if windows && backend == MultiplexerBackendConfig::Tmux && !remote {
        return MultiplexerBackendConfig::Native;
    }
    backend
}

/// The SSH transport a binding's backend client runs over, or `None` when the multiplexer is this
/// machine's. Remote configs that name a backend without a client are rejected when the config is
/// loaded, so anything reaching here is a backend that can be driven from another host.
pub fn remote_transport(config: &MultiplexerConfig) -> Option<SshRemote> {
    config.remote.clone().map(SshRemote::new)
}

pub fn build_backend(config: &MultiplexerConfig) -> Box<dyn MuxBackend> {
    build_backend_for_workspace(config, None)
}

/// Build the backend for `config`, giving the native backend the mux state belonging to
/// `workspace`. Only the native backend keeps its sessions in this process, so it is the only one a
/// workspace can scope; the others reach a server that is already shared.
pub fn build_backend_for_workspace(
    config: &MultiplexerConfig,
    workspace: Option<&Path>,
) -> Box<dyn MuxBackend> {
    let remote = remote_transport(config);
    match selected_backend(config) {
        MultiplexerBackendConfig::Rmux => match remote {
            Some(remote) => Box::new(RemoteRmuxBackend::new(remote)),
            None => Box::new(RmuxBackend::new()),
        },
        MultiplexerBackendConfig::Native => Box::new(match workspace {
            Some(workspace) => NativeBackend::for_workspace(workspace),
            None => NativeBackend::new(),
        }),
        MultiplexerBackendConfig::Tmux => Box::new(match remote {
            Some(remote) => TmuxBackend::with_runner("tmux", TmuxControlRunner::for_remote(remote)),
            None => TmuxBackend::new(),
        }),
        MultiplexerBackendConfig::Zellij => match remote {
            Some(remote) => Box::new(ZellijBackend::with_runner(SshCommandRunner::new(
                remote,
                SystemCommandRunner,
            ))),
            None => Box::new(ZellijBackend::new()),
        },
    }
}

pub fn build_backend_with(
    factory: Option<&BackendFactory>,
    config: &MultiplexerConfig,
) -> Box<dyn MuxBackend> {
    match factory {
        Some(factory) => factory(config),
        None => build_backend(config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        capability::BindingOperation,
        controller::{BindingId, MuxScope, SpaceId},
    };
    use bootty_config::config::MultiplexerConfig;

    #[test]
    fn selected_backend_resolves_configured_backend() {
        for (backend, expected) in [
            (
                MultiplexerBackendConfig::Rmux,
                MultiplexerBackendConfig::Rmux,
            ),
            (
                MultiplexerBackendConfig::Native,
                MultiplexerBackendConfig::Native,
            ),
            (
                MultiplexerBackendConfig::Tmux,
                if cfg!(windows) {
                    MultiplexerBackendConfig::Native
                } else {
                    MultiplexerBackendConfig::Tmux
                },
            ),
            (
                MultiplexerBackendConfig::Zellij,
                MultiplexerBackendConfig::Zellij,
            ),
        ] {
            let config = MultiplexerConfig {
                backend,
                ..Default::default()
            };

            assert_eq!(selected_backend(&config), expected);
        }
    }

    /// Windows ships an SSH client but no tmux, so the fallback to the native backend has to look
    /// at where the tmux server is: substituting it for a remote binding would render this
    /// machine's own shells instead of the ones the user asked to attach to.
    #[test]
    fn windows_keeps_a_remote_tmux_binding_and_replaces_only_a_local_one() {
        for windows in [true, false] {
            assert_eq!(
                resolve_backend(MultiplexerBackendConfig::Tmux, true, windows),
                MultiplexerBackendConfig::Tmux
            );
        }

        assert_eq!(
            resolve_backend(MultiplexerBackendConfig::Tmux, false, true),
            MultiplexerBackendConfig::Native
        );
        assert_eq!(
            resolve_backend(MultiplexerBackendConfig::Tmux, false, false),
            MultiplexerBackendConfig::Tmux
        );
    }

    #[test]
    fn adapters_publish_exact_backend_neutral_capability_matrix() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(2));
        for backend in [
            MultiplexerBackendConfig::Native,
            MultiplexerBackendConfig::Rmux,
            MultiplexerBackendConfig::Tmux,
            MultiplexerBackendConfig::Zellij,
        ] {
            let expected = match backend {
                MultiplexerBackendConfig::Native => vec![
                    BindingOperation::ActivateWindow,
                    BindingOperation::CreateWindow,
                    BindingOperation::RenameWindow,
                    BindingOperation::NavigateWindow,
                    BindingOperation::MoveWindow,
                    BindingOperation::SplitPane,
                    BindingOperation::NavigatePane,
                    BindingOperation::ClosePane,
                    BindingOperation::CreateProjectSession,
                    BindingOperation::CreateWorktreeSession,
                    BindingOperation::RenameSession,
                    BindingOperation::DitchSession,
                ],
                MultiplexerBackendConfig::Rmux => vec![
                    BindingOperation::ActivateWindow,
                    BindingOperation::CreateWindow,
                    BindingOperation::RenameWindow,
                    BindingOperation::NavigateWindow,
                    BindingOperation::MoveWindow,
                    BindingOperation::SplitPane,
                    BindingOperation::ClosePane,
                    BindingOperation::CreateProjectSession,
                    BindingOperation::CreateWorktreeSession,
                    BindingOperation::RenameSession,
                    BindingOperation::DitchSession,
                ],
                MultiplexerBackendConfig::Tmux => vec![
                    BindingOperation::ActivateWindow,
                    BindingOperation::CreateWindow,
                    BindingOperation::RenameWindow,
                    BindingOperation::NavigateWindow,
                    BindingOperation::MoveWindow,
                    BindingOperation::SplitPane,
                    BindingOperation::NavigatePane,
                    BindingOperation::ClosePane,
                    BindingOperation::TogglePaneZoom,
                    BindingOperation::CreateProjectSession,
                    BindingOperation::CreateWorktreeSession,
                    BindingOperation::RenameSession,
                    BindingOperation::DitchSession,
                ],
                MultiplexerBackendConfig::Zellij => vec![
                    BindingOperation::CreateProjectSession,
                    BindingOperation::CreateWorktreeSession,
                    BindingOperation::RenameSession,
                    BindingOperation::DitchSession,
                ],
            };
            let config = MultiplexerConfig {
                backend,
                ..Default::default()
            };
            let descriptor = build_backend(&config).capabilities(scope);

            assert_eq!(descriptor.scope(), scope);
            assert_eq!(descriptor.operations().collect::<Vec<_>>(), expected);
        }
    }
}
