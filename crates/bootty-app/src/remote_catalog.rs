use std::collections::HashSet;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    config::{BoottyConfig, MultiplexerBackendConfig, SshProfileConfig, SshRemoteConfig},
    workspace::{
        BackendSessionMembership, BindingMembershipMutation, DEFAULT_SPACE_COLOR,
        DEFAULT_SPACE_ICON, SessionNameStore, SessionOrderStore, SpaceMuxOverride,
        SpaceRemoteOverride, WorkspaceBinding, WorkspaceRepository,
    },
};
use bootty_mux::project::{ProjectPickerEntry, WorktreePickerEntry};
use bootty_mux::{
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
    let (_, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    Ok(snapshot
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
    let (mut repository, _) = WorkspaceRepository::open(&config.config_path)?;
    let space = repository
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
    let mut runtime = remote_space_runtime(config, space_id, expected_backend)?;
    let snapshot = runtime.backend.snapshot()?;
    runtime.reconcile_pending_membership(&snapshot)?;
    let snapshot = filter_snapshot_for_space(snapshot, &mut runtime.session_order);
    runtime.repository.commit_binding_state(
        runtime.scope,
        &runtime.session_order,
        &runtime.session_names,
    )?;
    Ok(snapshot)
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
    let mut runtime = remote_space_runtime(config, space_id, expected_backend)?;
    let snapshot = runtime.backend.snapshot()?;
    runtime.reconcile_pending_membership(&snapshot)?;
    let owned_names = runtime.session_order.session_names();
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
    let mutation = binding_membership_mutation(
        &command,
        owned_session_name.as_deref(),
        &runtime.session_names,
    );
    let Some(mutation) = mutation else {
        runtime.backend.execute(command)?;
        return Ok(());
    };

    runtime
        .repository
        .begin_binding_membership_mutation(runtime.scope, &mutation)?;
    if let Err(backend_error) = runtime.backend.execute(command) {
        return Err(anyhow::anyhow!(
            "remote backend result is ambiguous: {backend_error}; binding membership recovery is pending"
        ));
    }
    if let Err(persistence_error) = runtime.repository.commit_binding_membership_mutation(
        runtime.scope,
        &mutation,
        &mut runtime.session_order,
        &mut runtime.session_names,
    ) {
        return Err(anyhow::Error::new(persistence_error).context(format!(
            "remote backend completed {mutation:?}, but workspace persistence failed"
        )));
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

struct RemoteSpaceRuntime {
    backend: Box<dyn bootty_mux::backend::MuxBackend>,
    repository: WorkspaceRepository,
    scope: bootty_mux::controller::MuxScope,
    session_order: SessionOrderStore,
    session_names: SessionNameStore,
}

impl RemoteSpaceRuntime {
    fn reconcile_pending_membership(&mut self, snapshot: &MuxSnapshot) -> Result<()> {
        let memberships = snapshot
            .sessions
            .iter()
            .map(|session| BackendSessionMembership {
                id: session.id.clone(),
                name: session.name.clone(),
            })
            .collect::<Vec<_>>();
        self.repository.reconcile_binding_membership_mutation(
            self.scope,
            &memberships,
            &mut self.session_order,
            &mut self.session_names,
        )?;
        Ok(())
    }
}

fn remote_space_runtime(
    config: &BoottyConfig,
    space_id: &str,
    expected_backend: MultiplexerBackendConfig,
) -> Result<RemoteSpaceRuntime> {
    let (repository, snapshot) = WorkspaceRepository::open(&config.config_path)?;
    let space = snapshot
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
    let scope = binding.mux_scope();
    let session_order = binding.session_order().clone();
    let session_names = binding.session_names().clone();
    Ok(RemoteSpaceRuntime {
        backend,
        repository,
        scope,
        session_order,
        session_names,
    })
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
        | MuxCommand::SelectNextPane { session_id, .. }
        | MuxCommand::SelectPreviousPane { session_id, .. }
        | MuxCommand::KillPane { session_id, .. }
        | MuxCommand::ClosePane { session_id, .. }
        | MuxCommand::TogglePaneZoom { session_id, .. }
        | MuxCommand::RenameSession { session_id, .. }
        | MuxCommand::DitchSession { session_id } => Some(session_id),
    }
}

fn binding_membership_mutation(
    command: &MuxCommand,
    owned_session_name: Option<&str>,
    session_names: &SessionNameStore,
) -> Option<BindingMembershipMutation> {
    match command {
        MuxCommand::CreateProjectSession { session_id, cwd }
        | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
            Some(BindingMembershipMutation::Create {
                session_id: session_id.clone(),
                session_name: session_id.clone(),
                display_name: session_id.clone(),
                explicit: true,
                cwd: Some(cwd.clone()),
            })
        }
        MuxCommand::RenameSession { session_id, name } => {
            let old_name = owned_session_name?.to_owned();
            let cwd = session_names
                .record(session_id)
                .map(|record| record.cwd.clone());
            Some(BindingMembershipMutation::Rename {
                session_id: session_id.clone(),
                old_name,
                new_name: name.clone(),
                display_name: name.clone(),
                explicit: true,
                cwd,
            })
        }
        MuxCommand::DitchSession { session_id } => Some(BindingMembershipMutation::Ditch {
            session_id: session_id.clone(),
            old_name: owned_session_name?.to_owned(),
        }),
        _ => None,
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
