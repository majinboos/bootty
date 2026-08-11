use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig, SshRemoteConfig},
    session_order::SessionOrderStore,
    workspace::{
        DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceBinding, WorkspaceStore,
    },
};
use bootty_mux::project::{ProjectPickerEntry, WorktreePickerEntry};
use bootty_mux::{
    backend::{MuxBackend, MuxBackendOperationError},
    capability::BindingOperationOutcome,
    command::MuxCommand,
    process::{CommandRunner, SystemCommandRunner},
    snapshot::{MuxSnapshot, session_matches},
    ssh::{SshRemote, remote_daemon_failure},
};

pub const REMOTE_SPACE_CATALOG_VERSION: u32 = 3;

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
    filter_snapshot_for_space(backend.snapshot()?, &mut sessions)
}

fn filter_snapshot_for_space(
    mut snapshot: MuxSnapshot,
    sessions: &mut SessionOrderStore,
) -> Result<MuxSnapshot> {
    let alive = snapshot
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect::<Vec<_>>();
    let allowed = sessions
        .sync_sessions(alive)?
        .into_iter()
        .collect::<HashSet<_>>();
    snapshot
        .sessions
        .retain(|session| allowed.iter().any(|id| session_matches(session, id)));
    snapshot.active_session_id = snapshot
        .active_session_id
        .filter(|id| snapshot.sessions.iter().any(|session| &session.id == id));
    Ok(snapshot)
}

pub fn execute(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
    payload: &str,
) -> Result<()> {
    let command = bootty_mux::remote_space::decode_command(payload)?;
    validate_remote_session_launch(&command)?;
    let (mut backend, mut sessions) = remote_space_runtime(config, space_id, expected_backend)?;
    execute_with_runtime(backend.as_mut(), &mut sessions, command, space_id)
}

fn execute_with_runtime(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    command: MuxCommand,
    space_id: &str,
) -> Result<()> {
    preflight_remote_session_launch(backend, &command)?;
    let snapshot = backend.snapshot()?;
    let owned_names = sessions.session_names();
    if let Some(session_id) = created_session_id(&command)
        && let Some(existing) = snapshot
            .sessions
            .iter()
            .find(|session| session_matches(session, session_id))
    {
        if owned_names.iter().any(|name| name == &existing.name) {
            return Ok(());
        }
        bail!("session already belongs to another remote Space");
    }
    let owned_session_name =
        resolve_owned_session_name(&snapshot, &owned_names, &command, space_id)?;
    backend.execute(command.clone())?;
    match command {
        MuxCommand::CreateSession { plan } => {
            persist_created_remote_session(backend, sessions, &plan.session_id)?;
        }
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => {
            persist_created_remote_session(backend, sessions, &session_id)?;
        }
        MuxCommand::RenameSession { name, .. } => {
            if let Some(old_name) = owned_session_name {
                sessions.rename_session(&old_name, &name).map_err(|error| {
                    remote_membership_persistence_failure("session rename", error)
                })?;
            }
        }
        MuxCommand::DitchSession { .. } => {
            if let Some(name) = owned_session_name {
                sessions.remove_session(&name).map_err(|error| {
                    remote_membership_persistence_failure("session removal", error)
                })?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn persist_created_remote_session(
    backend: &mut dyn MuxBackend,
    sessions: &mut SessionOrderStore,
    session_id: &str,
) -> Result<()> {
    if let Err(error) = sessions.add_session(session_id) {
        return Err(remote_create_persistence_failure(backend, error));
    }
    Ok(())
}

fn remote_membership_persistence_failure(operation: &str, error: rusqlite::Error) -> anyhow::Error {
    MuxBackendOperationError::Failed(format!(
        "remote Space {operation} completed in the backend, but membership persistence failed: \
         {error}; authoritative reconciliation is required"
    ))
    .into()
}

fn remote_create_persistence_failure(
    backend: &mut dyn MuxBackend,
    persistence_error: rusqlite::Error,
) -> anyhow::Error {
    let detail = format!(
        "remote Space session creation completed in the backend, but membership persistence \
         failed: {persistence_error}"
    );
    let Some(session_id) = backend
        .take_authoritative_completion()
        .and_then(|completion| completion.allocated)
        .map(|allocated| allocated.session_id)
    else {
        return MuxBackendOperationError::Failed(format!(
            "{detail}; the backend did not report an exact newly allocated session, so cleanup \
             was unsafe; creation is reported as failed and authoritative reconciliation is \
             required"
        ))
        .into();
    };

    match backend.execute(MuxCommand::DitchSession {
        session_id: session_id.clone(),
    }) {
        Ok(()) => MuxBackendOperationError::Failed(format!(
            "{detail}; removed exact newly allocated session {session_id:?}; creation is \
             reported as failed and authoritative reconciliation is required"
        ))
        .into(),
        Err(cleanup_error) => MuxBackendOperationError::Failed(format!(
            "{detail}; cleanup of exact newly allocated session {session_id:?} also failed: \
             {cleanup_error}; creation is reported as failed and authoritative reconciliation \
             is required"
        ))
        .into(),
    }
}

/// Reject an untrusted recursive plan before backend construction, snapshot traversal, or process
/// creation. Every later backend boundary revalidates the same immutable plan before mutation.
fn validate_remote_session_launch(command: &MuxCommand) -> Result<()> {
    if let MuxCommand::CreateSession { plan } = command
        && let Err(error) = plan.validate()
    {
        bail!("invalid recursive session launch: {error}");
    }
    Ok(())
}

/// Check backend fidelity before snapshot traversal or a backend process is started. This is
/// separate from structural validation because a valid recursive plan can still be unsupported by
/// a particular backend.
fn preflight_remote_session_launch(backend: &dyn MuxBackend, command: &MuxCommand) -> Result<()> {
    let MuxCommand::CreateSession { plan } = command else {
        return Ok(());
    };
    match backend.session_launch_capability(plan) {
        BindingOperationOutcome::Supported(()) => Ok(()),
        BindingOperationOutcome::Unsupported => {
            bail!("recursive session launch is unsupported by this remote Space backend")
        }
        BindingOperationOutcome::Unavailable => {
            bail!("remote Space backend is unavailable for recursive session launch")
        }
        BindingOperationOutcome::Denied => {
            bail!("remote Space backend denied recursive session launch")
        }
        BindingOperationOutcome::Stale => {
            bail!("remote Space backend capability is stale for recursive session launch")
        }
    }
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
    )?;
    Ok((backend, sessions))
}

fn command_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateSession { .. }
        | MuxCommand::CreateProjectSession { .. }
        | MuxCommand::CreateWorktreeSession { .. } => None,
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
        | MuxCommand::SelectNextPane { session_id, .. }
        | MuxCommand::SelectPreviousPane { session_id, .. }
        | MuxCommand::SelectLastPane { session_id, .. }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::ResizePane { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => Some(session_id),
    }
}

fn created_session_id(command: &MuxCommand) -> Option<&str> {
    match command {
        MuxCommand::CreateSession { plan } => Some(&plan.session_id),
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

pub fn list_remote_projects_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    runner: &R,
) -> Result<Vec<ProjectPickerEntry>> {
    let output = run_remote_config(remote, &["remote-project", "list"], runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn toggle_remote_project_favorite_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    path: &str,
    runner: &R,
) -> Result<bool> {
    let output = run_remote_config(
        remote,
        &["remote-project", "favorite", "--path", path],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
}

pub fn list_remote_worktrees_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    open_cwds: &[String],
    runner: &R,
) -> Result<Vec<WorktreePickerEntry>> {
    let mut args = vec![
        "remote-worktree".to_owned(),
        "list".to_owned(),
        "--project".to_owned(),
        project.to_owned(),
    ];
    for cwd in open_cwds {
        args.extend(["--open-cwd".to_owned(), cwd.clone()]);
    }
    let output = run_remote_config_owned(remote, &args, runner)?;
    Ok(serde_json::from_str(&output)?)
}

pub fn create_remote_worktree_with_runner<R: CommandRunner>(
    remote: &SshRemoteConfig,
    project: &str,
    branch: &str,
    runner: &R,
) -> Result<String> {
    let output = run_remote_config(
        remote,
        &[
            "remote-worktree",
            "create",
            "--project",
            project,
            "--branch",
            branch,
        ],
        runner,
    )?;
    Ok(serde_json::from_str(&output)?)
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
    run_remote_config(&profile.to_remote(), args, runner)
}

fn run_remote_config<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[&str],
    runner: &R,
) -> Result<String> {
    let args = args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    run_remote_config_owned(remote, &args, runner)
}

fn run_remote_config_owned<R: CommandRunner>(
    remote: &SshRemoteConfig,
    args: &[String],
    runner: &R,
) -> Result<String> {
    let remote = SshRemote::new(remote.clone());
    remote.ensure_daemon_with(runner)?;
    let host = remote.host().to_owned();
    let (program, args) = remote.proxy_command(bootty_mux::ssh::REMOTE_DAEMON_PROGRAM, args)?;
    let output = runner.run(&program, &args)?;
    if output.success {
        return Ok(output.stdout);
    }
    bail!("{}", remote_daemon_failure(&host, &output.stderr))
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
    use bootty_mux::{
        backend::{MuxAllocatedResources, MuxBackendCommandCompletion},
        capability::BindingOperationOutcome,
        command::{MuxPaneLaunch, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxWindowLaunchPlan},
        process::CommandOutput,
    };
    use std::{cell::RefCell, collections::BTreeMap};

    struct FakeRunner {
        output: CommandOutput,
        command: RefCell<Option<(String, Vec<String>)>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<CommandOutput> {
            self.command
                .replace(Some((program.to_owned(), args.to_vec())));
            if args.last().is_some_and(|arg| arg.ends_with("remote-ping")) {
                return Ok(CommandOutput {
                    success: true,
                    stdout: format!(
                        "{}:{}",
                        bootty_mux::ssh::REMOTE_DAEMON_PROTOCOL_VERSION,
                        env!("CARGO_PKG_VERSION")
                    ),
                    stderr: String::new(),
                });
            }
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

    fn launch_plan(cwd: &str) -> MuxSessionLaunchPlan {
        MuxSessionLaunchPlan {
            session_id: "remote-launch".to_owned(),
            focus: true,
            default_cwd: "/remote".to_owned(),
            environment: BTreeMap::new(),
            windows: vec![MuxWindowLaunchPlan {
                name: None,
                focus: true,
                layout: MuxPaneLaunchPlan::Pane(MuxPaneLaunch {
                    cwd: cwd.to_owned(),
                    command: None,
                    argv: None,
                    environment: BTreeMap::new(),
                    title: None,
                }),
            }],
            focused_window: 0,
        }
    }

    struct UnsupportedLaunchBackend;

    impl MuxBackend for UnsupportedLaunchBackend {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            unreachable!("preflight must not snapshot")
        }

        fn execute(&mut self, _command: MuxCommand) -> Result<()> {
            unreachable!("preflight must not execute")
        }

        fn execute_checked(
            &mut self,
            _scope: bootty_mux::controller::MuxScope,
            _command: MuxCommand,
            _precondition: Option<&bootty_mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<Result<()>> {
            BindingOperationOutcome::Unsupported
        }
    }

    fn test_session(id: &str, name: &str) -> bootty_mux::snapshot::MuxSession {
        bootty_mux::snapshot::MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: bootty_mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn session_order(config: &BoottyConfig) -> SessionOrderStore {
        let workspace =
            WorkspaceStore::try_for_config_path(&config.config_path).expect("open workspace");
        let binding_id = workspace.binding_id().expect("default binding");
        SessionOrderStore::for_binding(&config.config_path, binding_id).expect("open session order")
    }

    struct FakeRemoteBackend {
        snapshot: MuxSnapshot,
        completion: Option<MuxBackendCommandCompletion>,
        commands: Vec<MuxCommand>,
        cleanup_error: Option<String>,
    }

    impl FakeRemoteBackend {
        fn with_completion(completion: Option<MuxBackendCommandCompletion>) -> Self {
            Self {
                snapshot: MuxSnapshot::default(),
                completion,
                commands: Vec::new(),
                cleanup_error: None,
            }
        }
    }

    impl MuxBackend for FakeRemoteBackend {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            Ok(self.snapshot.clone())
        }

        fn execute(&mut self, command: MuxCommand) -> Result<()> {
            if matches!(&command, MuxCommand::DitchSession { .. })
                && let Some(error) = self.cleanup_error.take()
            {
                self.commands.push(command);
                anyhow::bail!("{error}");
            }
            match &command {
                MuxCommand::CreateSession { plan } => {
                    let allocated_id = self
                        .completion
                        .as_ref()
                        .and_then(|completion| completion.allocated.as_ref())
                        .map(|allocated| allocated.session_id.clone())
                        .unwrap_or_else(|| plan.session_id.clone());
                    self.snapshot
                        .sessions
                        .push(test_session(&allocated_id, &plan.session_id));
                }
                MuxCommand::CreateProjectSession { session_id, .. }
                | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                    self.snapshot
                        .sessions
                        .push(test_session(session_id, session_id));
                }
                MuxCommand::DitchSession { session_id } => {
                    self.snapshot
                        .sessions
                        .retain(|session| session.id != session_id.as_str());
                }
                _ => {}
            }
            self.commands.push(command);
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: bootty_mux::controller::MuxScope,
            command: MuxCommand,
            precondition: Option<&bootty_mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<Result<()>> {
            if let Some(precondition) = precondition {
                if precondition.scope != scope {
                    return BindingOperationOutcome::Supported(Err(
                        MuxBackendOperationError::stale("remote binding scope changed").into(),
                    ));
                }
                return BindingOperationOutcome::Supported(Err(
                    MuxBackendOperationError::unsupported(
                        "remote backend lacks an atomic checked mutation protocol",
                    )
                    .into(),
                ));
            }
            BindingOperationOutcome::Supported(self.execute(command))
        }
        fn session_launch_capability(
            &self,
            _plan: &MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<()> {
            BindingOperationOutcome::Supported(())
        }

        fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
            self.completion.take()
        }
    }

    #[test]
    fn remote_launch_limits_fail_before_backend_construction() {
        let error = validate_remote_session_launch(&MuxCommand::CreateSession {
            plan: launch_plan(""),
        })
        .expect_err("empty pane cwd must fail");

        assert!(error.to_string().contains("pane cwd"));
    }

    #[test]
    fn remote_launch_fidelity_fails_before_snapshot_or_execution() {
        let error = preflight_remote_session_launch(
            &UnsupportedLaunchBackend,
            &MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
        )
        .expect_err("unsupported backend must fail before work");

        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn remote_create_persistence_failure_cleans_exact_allocation_and_returns_failed() {
        let config = config("remote-create-persistence");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(Some(MuxBackendCommandCompletion {
            allocated: Some(MuxAllocatedResources {
                session_id: "$42".to_owned(),
                windows: Vec::new(),
            }),
            target: None,
        }));

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(_))
        ));
        assert!(
            error
                .to_string()
                .contains("removed exact newly allocated session \"$42\"")
        );
        assert!(backend.snapshot.sessions.is_empty());
        assert!(matches!(
            &backend.commands[..],
            [
                MuxCommand::CreateSession { .. },
                MuxCommand::DitchSession { session_id }
            ] if session_id == "$42"
        ));
        assert!(session_order(&config).session_names().is_empty());
    }

    #[test]
    fn remote_create_persistence_failure_without_allocation_reports_partial_failure() {
        let config = config("remote-create-unknown-allocation");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(None);

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateProjectSession {
                session_id: "project".to_owned(),
                cwd: "/remote/project".to_owned(),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(matches!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(_))
        ));
        assert!(
            error
                .to_string()
                .contains("did not report an exact newly allocated session")
        );
        assert!(
            error
                .to_string()
                .contains("authoritative reconciliation is required")
        );
        assert_eq!(
            backend
                .snapshot
                .sessions
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec!["project"]
        );
        assert!(matches!(
            &backend.commands[..],
            [MuxCommand::CreateProjectSession { .. }]
        ));
        assert!(session_order(&config).session_names().is_empty());
    }

    #[test]
    fn remote_create_persistence_failure_reports_cleanup_failure() {
        let config = config("remote-create-cleanup-failure");
        let mut sessions = session_order(&config);
        sessions.fail_next_save_for_test();
        let mut backend = FakeRemoteBackend::with_completion(Some(MuxBackendCommandCompletion {
            allocated: Some(MuxAllocatedResources {
                session_id: "$42".to_owned(),
                windows: Vec::new(),
            }),
            target: None,
        }));
        backend.cleanup_error = Some("injected cleanup failure".to_owned());

        let error = execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect_err("persistence failure must fail the remote create");

        assert!(
            error
                .to_string()
                .contains("cleanup of exact newly allocated session \"$42\" also failed")
        );
        assert!(
            backend
                .snapshot
                .sessions
                .iter()
                .any(|session| session.id == "$42")
        );
    }

    #[test]
    fn remote_create_for_existing_owned_session_is_idempotent() {
        let config = config("remote-create-idempotent");
        let mut sessions = session_order(&config);
        sessions
            .add_session("remote-launch")
            .expect("persist owned session");
        let mut backend = FakeRemoteBackend::with_completion(None);
        backend
            .snapshot
            .sessions
            .push(test_session("$42", "remote-launch"));

        execute_with_runtime(
            &mut backend,
            &mut sessions,
            MuxCommand::CreateSession {
                plan: launch_plan("/remote"),
            },
            "space-1",
        )
        .expect("existing owned session must be idempotent");

        assert!(backend.commands.is_empty());
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
                stdout: r#"[{"catalog_version":3,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        let spaces = list_remote_with_runner(&profile(), &runner).expect("remote list");

        assert_eq!(spaces[0].id, "remote-7");
        let (_, args) = runner.command.into_inner().expect("command");
        let command = args.last().expect("remote command");
        assert!(command.starts_with(&format!(
            "./.bootty/bin/bootty-daemon-{}-{}.exe remote-exec ",
            bootty_mux::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        )));
        assert!(
            command
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '.' | '/'))
        );
    }

    #[test]
    fn ssh_project_catalog_returns_remote_paths_through_the_daemon_proxy() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"path":"/srv/projects/bootty","favorite":false}]"#.to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        let output =
            run_remote_config(&profile().to_remote(), &["remote-project", "list"], &runner)
                .expect("remote projects");
        let projects =
            serde_json::from_str::<Vec<ProjectPickerEntry>>(&output).expect("project JSON");

        assert_eq!(projects[0].path, "/srv/projects/bootty");
        let (_, args) = runner.command.into_inner().expect("command");
        assert!(
            args.last()
                .expect("remote command")
                .contains(" remote-exec ")
        );
    }

    #[test]
    fn ssh_catalog_rejects_unknown_versions() {
        let runner = FakeRunner {
            output: CommandOutput {
                success: true,
                stdout: r#"[{"catalog_version":4,"id":"remote-7","name":"Lab","backend":"tmux"}]"#
                    .to_owned(),
                stderr: String::new(),
            },
            command: RefCell::new(None),
        };

        assert!(
            list_remote_with_runner(&profile(), &runner)
                .unwrap_err()
                .to_string()
                .contains("version 4")
        );
    }

    #[test]
    fn remote_space_snapshot_only_contains_owned_sessions() {
        let config = config("remote-snapshot");
        let mut order = session_order(&config);
        order.add_session("owned").expect("persist owned session");
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

        let filtered = filter_snapshot_for_space(snapshot, &mut order).expect("filter snapshot");

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
