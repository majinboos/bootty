use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig},
    session_order::SessionOrderStore,
    workspace::{
        DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceBinding, WorkspaceStore,
    },
};
use bootty_mux::{
    command::MuxCommand,
    process::{CommandRunner, SystemCommandRunner},
    snapshot::{MuxSnapshot, session_matches},
    ssh::{SshRemote, remote_bootty_failure},
};

pub const REMOTE_SPACE_CATALOG_VERSION: u32 = 2;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct RemoteSpaceSummary {
    pub catalog_version: u32,
    pub id: String,
    pub name: String,
    pub backend: MultiplexerBackendConfig,
}

pub fn list(config: &BoottyConfig) -> Result<Vec<RemoteSpaceSummary>> {
    let workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    Ok(workspace
        .spaces()
        .iter()
        .filter_map(|space| {
            let binding = space.bindings().first()?;
            if !binding_is_local(binding, config) {
                return None;
            }
            let backend = binding
                .backend_override()
                .unwrap_or(config.multiplexer.backend);
            backend.supports_remote().then(|| RemoteSpaceSummary {
                catalog_version: REMOTE_SPACE_CATALOG_VERSION,
                id: space.remote_id().to_owned(),
                name: space.name().to_owned(),
                backend,
            })
        })
        .collect())
}

pub fn create(
    config: &BoottyConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceSummary> {
    if !backend.supports_remote() {
        bail!("remote Spaces need tmux, zellij, or rmux")
    }
    let mut workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    let space = workspace
        .create_space(
            name,
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(backend),
                remote: crate::workspace::SpaceRemoteOverride::Local,
            },
            &config.multiplexer,
        )?
        .ok_or_else(|| anyhow::anyhow!("remote Space name cannot be empty"))?;
    Ok(RemoteSpaceSummary {
        catalog_version: REMOTE_SPACE_CATALOG_VERSION,
        id: space.remote_id().to_owned(),
        name: space.name().to_owned(),
        backend,
    })
}

pub fn snapshot(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<MuxSnapshot> {
    let (backend, mut sessions) = remote_space_runtime(config, space_id, expected_backend)?;
    Ok(filter_snapshot_for_space(
        backend.snapshot()?,
        &mut sessions,
    ))
}

fn filter_snapshot_for_space(
    mut snapshot: MuxSnapshot,
    sessions: &mut SessionOrderStore,
) -> MuxSnapshot {
    let alive = snapshot
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect::<Vec<_>>();
    let allowed = sessions
        .sync_sessions(alive)
        .into_iter()
        .collect::<HashSet<_>>();
    snapshot
        .sessions
        .retain(|session| allowed.iter().any(|id| session_matches(session, id)));
    snapshot.active_session_id = snapshot
        .active_session_id
        .filter(|id| snapshot.sessions.iter().any(|session| &session.id == id));
    snapshot
}

pub fn execute(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
    payload: &str,
) -> Result<()> {
    let command = bootty_mux::remote_space::decode_command(payload)?;
    let (mut backend, mut sessions) = remote_space_runtime(config, space_id, expected_backend)?;
    let snapshot = backend.snapshot()?;
    let owned_names = sessions.session_names();
    if let Some(session_id) = created_session_id(&command)
        && !owned_names.iter().any(|name| name == session_id)
        && snapshot
            .sessions
            .iter()
            .any(|session| session_matches(session, session_id))
    {
        bail!("session already belongs to another remote Space")
    }
    let owned_session_name =
        resolve_owned_session_name(&snapshot, &owned_names, &command, space_id)?;
    backend.execute(command.clone())?;
    match command {
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => {
            sessions.add_session(&session_id);
        }
        MuxCommand::RenameSession { name, .. } => {
            if let Some(old_name) = owned_session_name {
                sessions.rename_session(&old_name, &name);
            }
        }
        MuxCommand::DitchSession { .. } => {
            if let Some(name) = owned_session_name {
                sessions.remove_session(&name);
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_owned_session_name(
    snapshot: &MuxSnapshot,
    owned_names: &[String],
    command: &MuxCommand,
    space_id: &str,
) -> Result<Option<String>> {
    let Some(session_id) = command_session_id(command) else {
        return Ok(None);
    };
    let name = snapshot
        .sessions
        .iter()
        .find(|session| session_matches(session, session_id))
        .map(|session| session.name.clone())
        .ok_or_else(|| anyhow::anyhow!("session is unavailable"))?;
    if !owned_names.contains(&name) {
        bail!("session does not belong to remote Space {space_id}")
    }
    Ok(Some(name))
}

fn remote_space_runtime(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<(Box<dyn bootty_mux::backend::MuxBackend>, SessionOrderStore)> {
    let workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
    let space = workspace
        .spaces()
        .iter()
        .find(|space| space.remote_id() == space_id)
        .ok_or_else(|| anyhow::anyhow!("remote Space {space_id} is unavailable"))?;
    let binding = space
        .bindings()
        .first()
        .ok_or_else(|| anyhow::anyhow!("remote Space {space_id} has no backend binding"))?;
    if !binding_is_local(binding, config) {
        bail!("remote Space {space_id} points to another SSH host")
    }
    let mut multiplexer = config.multiplexer.clone();
    multiplexer.backend = binding
        .backend_override()
        .unwrap_or(config.multiplexer.backend);
    if multiplexer.backend != expected_backend {
        bail!(
            "Remote Space now uses {} instead of {}. Edit this Space and select it again.",
            backend_name(multiplexer.backend),
            backend_name(expected_backend)
        )
    }
    multiplexer.remote = None;
    multiplexer.remote_space_id = None;
    let backend =
        bootty_mux::config::build_backend_for_workspace(&multiplexer, Some(&config.config_path));
    let sessions = SessionOrderStore::for_binding(
        &config.config_path,
        binding.mux_scope().binding_id().persistence_value(),
    );
    Ok((backend, sessions))
}

fn command_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateProjectSession { .. } | MuxCommand::CreateWorktreeSession { .. } => None,
        MuxCommand::ActivateWindow { session_id, .. }
        | MuxCommand::NewWindow { session_id, .. }
        | MuxCommand::RenameWindow { session_id, .. }
        | MuxCommand::ActivateNextWindow { session_id }
        | MuxCommand::ActivatePreviousWindow { session_id }
        | MuxCommand::ActivateLastWindow { session_id }
        | MuxCommand::ActivateWindowIndex { session_id, .. }
        | MuxCommand::MoveWindow { session_id, .. }
        | MuxCommand::MoveWindowPreservingSelection { session_id, .. }
        | MuxCommand::SplitPane { session_id, .. }
        | MuxCommand::SelectPane { session_id, .. }
        | MuxCommand::SelectNextPane { session_id }
        | MuxCommand::SelectPreviousPane { session_id }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => Some(session_id),
    }
}

fn created_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id),
        _ => None,
    }
}

fn backend_name(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}

fn binding_is_local(binding: &WorkspaceBinding, config: &BoottyConfig) -> bool {
    match binding.remote_override() {
        SpaceRemoteOverride::Local => true,
        SpaceRemoteOverride::Inherit => config.multiplexer.remote.is_none(),
        SpaceRemoteOverride::Profile(_) | SpaceRemoteOverride::Inline(_) => false,
    }
}

pub fn list_remote(profile: &SshProfileConfig) -> Result<Vec<RemoteSpaceSummary>> {
    list_remote_with_runner(profile, &SystemCommandRunner)
}

pub fn create_remote(
    profile: &SshProfileConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceSummary> {
    create_remote_with_runner(profile, name, backend, &SystemCommandRunner)
}

fn list_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    runner: &R,
) -> Result<Vec<RemoteSpaceSummary>> {
    let output = run_remote(profile, &["remote-space", "list"], runner)?;
    let spaces = serde_json::from_str::<Vec<RemoteSpaceSummary>>(&output)?;
    validate_versions(&spaces)?;
    Ok(spaces)
}

fn create_remote_with_runner<R: CommandRunner>(
    profile: &SshProfileConfig,
    name: &str,
    backend: MultiplexerBackendConfig,
    runner: &R,
) -> Result<RemoteSpaceSummary> {
    let backend = match backend {
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
        MultiplexerBackendConfig::Native => bail!("remote Spaces need tmux, zellij, or rmux"),
    };
    let output = run_remote(
        profile,
        &[
            "remote-space",
            "create",
            "--name",
            name,
            "--backend",
            backend,
        ],
        runner,
    )?;
    let space = serde_json::from_str::<RemoteSpaceSummary>(&output)?;
    validate_versions(std::slice::from_ref(&space))?;
    Ok(space)
}

fn run_remote<R: CommandRunner>(
    profile: &SshProfileConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let (program, args) = SshRemote::new(profile.to_remote()).proxy_command("bootty", &args)?;
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(output.stdout);
    }
    bail!("{}", remote_bootty_failure(&profile.host, &output.stderr))
}

fn validate_versions(spaces: &[RemoteSpaceSummary]) -> Result<()> {
    if let Some(space) = spaces
        .iter()
        .find(|space| space.catalog_version != REMOTE_SPACE_CATALOG_VERSION)
    {
        bail!(
            "remote Space catalog version {} is not supported",
            space.catalog_version
        )
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bootty_mux::process::CommandOutput;
    use std::cell::RefCell;

    struct FakeRunner {
        output: CommandOutput,
        command: RefCell<Option<(String, Vec<String>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.command
                .replace(Some((program.to_owned(), args.to_vec())));
            Ok(self.output.clone())
        }
    }

    fn profile() -> SshProfileConfig {
        SshProfileConfig {
            name: "Lab".to_owned(),
            host: "lab".to_owned(),
            user: None,
            port: None,
            authentication: Default::default(),
            host_key_policy: Default::default(),
            identity_file: None,
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        }
    }

    fn config(name: &str) -> BoottyConfig {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        BoottyConfig {
            config_path: dir.join(format!("{name}.toml")),
            ..BoottyConfig::default()
        }
    }

    #[test]
    fn catalog_lists_the_default_space_and_creates_remote_spaces() {
        let config = config("catalog");
        assert!(list(&config).expect("list").is_empty());

        let created = create(&config, "Production", MultiplexerBackendConfig::Tmux)
            .expect("create remote Space");

        assert_eq!(created.name, "Production");
        assert_eq!(created.backend, MultiplexerBackendConfig::Tmux);
        assert!(list(&config).expect("reload").contains(&created));
    }

    #[test]
    fn catalog_rejects_native_remote_space() {
        let error =
            create(&config("native"), "Wrong", MultiplexerBackendConfig::Native).unwrap_err();
        assert!(error.to_string().contains("tmux, zellij, or rmux"));
    }

    #[test]
    fn ssh_catalog_uses_the_cross_platform_bootty_proxy_and_parses_json() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"catalog_version":2,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        let spaces = list_remote_with_runner(&profile(), &runner).expect("remote list");

        assert_eq!(spaces[0].id, "remote-7");
        let (_, args) = runner.command.into_inner().expect("command");
        let command = args.last().expect("remote command");
        assert!(command.starts_with("bootty remote-exec "));
        assert!(
            command
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_'))
        );
    }

    #[test]
    fn ssh_catalog_rejects_unknown_versions() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"catalog_version":3,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        assert!(
            list_remote_with_runner(&profile(), &runner)
                .unwrap_err()
                .to_string()
                .contains("version 3")
        );
    }

    #[test]
    fn ssh_catalog_reports_a_missing_remote_bootty_install() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "fish: Unknown command: bootty\nfish:\n'bootty' 'remote-space' 'list'"
                    .to_owned(),
            },
            command: RefCell::new(None),
        };

        let error = list_remote_with_runner(&profile(), &runner).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Bootty is not installed on lab. Install and open Bootty there, then try again."
        );
    }

    #[test]
    fn remote_space_snapshot_only_contains_owned_sessions() {
        let path = std::env::temp_dir().join(format!(
            "bootty-remote-catalog-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut order = SessionOrderStore::for_binding(&path, 91);
        order.add_session("owned");
        let session = |id: &str| bootty_mux::snapshot::MuxSession {
            id: id.to_owned(),
            name: id.to_owned(),
            active: id == "other",
            anchor: bootty_mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        };
        let snapshot = MuxSnapshot {
            sessions: vec![session("owned"), session("other")],
            active_session_id: Some("other".to_owned()),
        };

        let filtered = filter_snapshot_for_space(snapshot, &mut order);

        assert_eq!(
            filtered
                .sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["owned"]
        );
        assert_eq!(filtered.active_session_id, None);
    }

    #[test]
    fn remote_space_command_resolves_backend_id_to_owned_name() {
        let snapshot = MuxSnapshot {
            sessions: vec![bootty_mux::snapshot::MuxSession {
                id: "$7".to_owned(),
                name: "owned".to_owned(),
                active: true,
                anchor: bootty_mux::snapshot::MuxPaneAnchor {
                    session_id: "$7".to_owned(),
                    ..Default::default()
                },
                active_window_id: None,
                windows: Vec::new(),
            }],
            active_session_id: Some("$7".to_owned()),
        };
        let command = MuxCommand::DitchSession {
            session_id: "$7".to_owned(),
        };

        assert_eq!(
            resolve_owned_session_name(&snapshot, &["owned".to_owned()], &command, "space-3")
                .unwrap(),
            Some("owned".to_owned())
        );
    }

    #[test]
    fn remote_space_rejects_a_stale_cached_backend() {
        let config = config("backend-authority");
        let space = create(&config, "Remote", MultiplexerBackendConfig::Tmux).unwrap();

        assert_eq!(
            snapshot(&config, &space.id, MultiplexerBackendConfig::Zellij)
                .unwrap_err()
                .to_string(),
            "Remote Space now uses tmux instead of zellij. Edit this Space and select it again."
        );
    }

    #[test]
    fn catalog_excludes_spaces_that_point_to_another_ssh_host() {
        let config = config("nested-remote");
        let mut workspace = WorkspaceStore::for_config_path(&config.config_path);
        let nested = workspace
            .create_space(
                "Nested",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Tmux),
                    remote: SpaceRemoteOverride::Inline(crate::config::SshRemoteConfig::for_host(
                        "other-host",
                    )),
                },
                &config.multiplexer,
            )
            .unwrap()
            .unwrap();

        assert!(list(&config).unwrap().is_empty());
        assert_eq!(
            snapshot(&config, nested.remote_id(), MultiplexerBackendConfig::Tmux)
                .unwrap_err()
                .to_string(),
            format!(
                "remote Space {} points to another SSH host",
                nested.remote_id()
            )
        );
    }
}
