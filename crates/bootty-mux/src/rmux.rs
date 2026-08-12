use anyhow::{Context, Result};
use rmux_proto::{ListPanesRequest, ListWindowsRequest, Request, Response, RmuxError};
use rmux_sdk::{Rmux, SessionName};

use crate::operation::{
    MuxAllocatedResources, MuxBackendCommandCompletion, MuxBackendOperationError, MuxEventTarget,
};

#[cfg(feature = "app")]
use crate::rmux_bridge::resize_rmux_window;
use crate::rmux_bridge::{
    rmux_execute, rmux_launch_session, rmux_snapshot, supports_rmux_session_launch_plan,
};

#[cfg(feature = "app")]
use super::{
    backend::{
        MuxBackend, MuxEvent, MuxEventCapability, MuxEventTopic, MuxScopedExecutionPrecondition,
    },
    capability::{
        BindingCapabilityDescriptor, BindingOperation, BindingOperationAvailability,
        BindingOperationOutcome,
    },
    controller::MuxScope,
};
use super::{
    command::{MuxCommand, MuxDirection, MuxPaneResize, MuxSessionLaunchPlan, MuxSplitDirection},
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxPaneSplitDirection, MuxSession, MuxSnapshot, MuxWindow,
    },
    tmux_protocol::{TmuxLayout, TmuxLayoutContent},
};

const RMUX_FIELD_SEPARATOR: char = '\u{1f}';
pub(crate) const RMUX_WINDOW_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_active}\u{1f}#{window_name}\u{1f}#{window_layout}";
pub(crate) const RMUX_PANE_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{pane_id}\u{1f}#{pane_tty}\u{1f}#{pane_index}\u{1f}#{pane_active}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}\u{1f}#{pane_lifecycle_generation}\u{1f}#{pid}";

pub trait RmuxSessionClient {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()>;
    fn rename_session(&self, session_name: &str, name: &str) -> Result<()> {
        anyhow::bail!("rmux client does not support renaming {session_name} to {name}")
    }
    fn kill_session(&self, session_name: &str) -> Result<()>;
    fn activate_window(&self, session_name: &str, window_id: &str) -> Result<()>;
    fn rename_window(&self, session_name: &str, window_id: &str, name: &str) -> Result<()>;
    fn new_window(&self, session_name: &str, cwd: Option<&str>) -> Result<()>;
    fn activate_next_window(&self, session_name: &str) -> Result<()>;
    fn activate_previous_window(&self, session_name: &str) -> Result<()>;
    fn activate_last_window(&self, session_name: &str) -> Result<()>;
    fn activate_window_index(&self, session_name: &str, index: u32) -> Result<()>;
    fn move_window(&self, session_name: &str, window_id: Option<&str>, delta: i32) -> Result<()>;
    fn split_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        direction: MuxSplitDirection,
    ) -> Result<()>;
    fn close_pane(&self, session_name: &str, pane_id: Option<&str>) -> Result<()>;

    fn select_pane(
        &self,
        session_name: &str,
        window_id: Option<&str>,
        direction: MuxDirection,
    ) -> Result<()>;
    fn select_next_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()>;
    fn select_previous_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()>;
    fn select_last_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()>;
    fn resize_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        adjustment: MuxPaneResize,
    ) -> Result<()>;
    fn toggle_pane_zoom(&self, session_name: &str, pane_id: Option<&str>) -> Result<()>;

    fn launch_session(&self, plan: MuxSessionLaunchPlan) -> Result<()> {
        anyhow::bail!(
            "rmux client does not support recursive session launch {:?}",
            plan.session_id
        )
    }

    fn launch_session_with_allocation(
        &self,
        plan: MuxSessionLaunchPlan,
    ) -> Result<Option<MuxAllocatedResources>> {
        self.launch_session(plan)?;
        Ok(None)
    }

    #[cfg(feature = "app")]
    fn session_launch_capability(
        &self,
        _plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        BindingOperationOutcome::Unsupported
    }

    #[cfg(feature = "app")]
    fn launch_session_outcome(
        &self,
        _plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        BindingOperationOutcome::Unsupported
    }
    #[cfg(feature = "app")]
    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        MuxEventTopic::ALL
            .into_iter()
            .map(|topic| {
                MuxEventCapability::unsupported(
                    topic,
                    "rmux client does not expose an embedded SDK event source",
                )
            })
            .collect()
    }

    #[cfg(feature = "app")]
    fn start_event_stream(&self) {}

    #[cfg(feature = "app")]
    fn drain_events(&self, _scope: MuxScope, _maximum: usize) -> Vec<MuxEvent> {
        Vec::new()
    }
    #[cfg(feature = "app")]
    fn release_event_scope(&self, _scope: MuxScope) {}

    #[cfg(feature = "app")]
    fn topology_invalidated(&self) {}
}
pub struct SdkRmuxClient;

pub struct RmuxBackend<C = SdkRmuxClient> {
    client: C,
    authoritative_completion: Option<MuxBackendCommandCompletion>,
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
        Self {
            client,
            authoritative_completion: None,
        }
    }
}

impl<C: RmuxSessionClient> RmuxBackend<C> {
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        self.client.snapshot()
    }

    #[cfg(feature = "app")]
    fn rmux_session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        if plan.validate().is_err() || !supports_rmux_session_launch_plan(plan) {
            return BindingOperationOutcome::Unsupported;
        }
        self.client.session_launch_capability(plan)
    }

    #[cfg(feature = "app")]
    fn require_rmux_session_launch(&self, plan: &MuxSessionLaunchPlan) -> Result<()> {
        match self.rmux_session_launch_capability(plan) {
            BindingOperationOutcome::Supported(()) => Ok(()),
            BindingOperationOutcome::Unsupported => Err(MuxBackendOperationError::unsupported(
                "rmux backend cannot preserve this recursive session launch plan",
            )
            .into()),
            BindingOperationOutcome::Unavailable => Err(MuxBackendOperationError::Unavailable(
                "rmux session launch is unavailable".to_owned(),
            )
            .into()),
            BindingOperationOutcome::Denied => Err(MuxBackendOperationError::Denied(
                "rmux session launch was denied".to_owned(),
            )
            .into()),
            BindingOperationOutcome::Stale => Err(MuxBackendOperationError::stale(
                "rmux session launch capability is stale",
            )
            .into()),
        }
    }
    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.authoritative_completion = None;
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => {
                self.client.activate_window(&session_id, &window_id)?;
            }
            MuxCommand::CreateSession { plan } => {
                #[cfg(feature = "app")]
                self.require_rmux_session_launch(&plan)?;
                #[cfg(not(feature = "app"))]
                {
                    plan.validate()?;
                    if !supports_rmux_session_launch_plan(&plan) {
                        return Err(MuxBackendOperationError::unsupported(
                            "rmux backend cannot preserve this recursive session launch plan",
                        )
                        .into());
                    }
                }
                let requested_session_id = plan.session_id.clone();
                let allocated = self.client.launch_session_with_allocation(plan)?;
                let target = MuxEventTarget::session(
                    allocated
                        .as_ref()
                        .map_or(requested_session_id, |allocated| {
                            allocated.session_id.clone()
                        }),
                );
                self.authoritative_completion = Some(MuxBackendCommandCompletion {
                    allocated,
                    target: Some(target),
                });
            }
            MuxCommand::CreateProjectSession { session_id, cwd }
            | MuxCommand::CreateWorktreeSession { session_id, cwd } => {
                self.client.ensure_session(&session_id, &cwd)?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.client.rename_session(&session_id, &name)?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.client.kill_session(&session_id)?;
            }
            MuxCommand::RenameWindow {
                session_id,
                window_id,
                name,
            } => {
                self.client.rename_window(&session_id, &window_id, &name)?;
            }
            MuxCommand::NewWindow { session_id, cwd } => {
                self.client.new_window(&session_id, cwd.as_deref())?;
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.client.activate_next_window(&session_id)?;
            }
            MuxCommand::ActivatePreviousWindow { session_id } => {
                self.client.activate_previous_window(&session_id)?;
            }
            MuxCommand::ActivateLastWindow { session_id } => {
                self.client.activate_last_window(&session_id)?;
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                self.client.activate_window_index(&session_id, index)?;
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                self.client
                    .move_window(&session_id, window_id.as_deref(), delta)?;
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                delta,
                selected_window_id,
            } => {
                self.client
                    .move_window(&session_id, Some(&window_id), delta)?;
                self.client
                    .activate_window(&session_id, &selected_window_id)?;
            }
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction,
            } => {
                self.client
                    .split_pane(&session_id, pane_id.as_deref(), direction)?;
            }
            MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            } => {
                self.client
                    .select_pane(&session_id, window_id.as_deref(), direction)?;
            }
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => {
                self.client
                    .select_next_pane(&session_id, window_id.as_deref())?;
            }
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => {
                self.client
                    .select_previous_pane(&session_id, window_id.as_deref())?;
            }
            MuxCommand::SelectLastPane {
                session_id,
                window_id,
            } => {
                self.client
                    .select_last_pane(&session_id, window_id.as_deref())?;
            }
            MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => {
                self.client.close_pane(&session_id, pane_id.as_deref())?;
            }
            MuxCommand::ResizePane {
                session_id,
                pane_id,
                adjustment,
            } => {
                if !adjustment.is_valid() {
                    return Err(MuxBackendOperationError::Failed(
                        "rmux pane resize requires every supplied dimension to be positive"
                            .to_owned(),
                    )
                    .into());
                }
                self.client
                    .resize_pane(&session_id, pane_id.as_deref(), adjustment)?;
            }
            MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            } => {
                self.client
                    .toggle_pane_zoom(&session_id, pane_id.as_deref())?;
            }
        }
        #[cfg(feature = "app")]
        self.client.topology_invalidated();
        Ok(())
    }

    pub fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        self.authoritative_completion.take()
    }
}

#[cfg(feature = "app")]
impl<C: RmuxSessionClient> MuxBackend for RmuxBackend<C> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        RmuxBackend::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        RmuxBackend::execute(self, command)
    }

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
        precondition: Option<&MuxScopedExecutionPrecondition>,
    ) -> BindingOperationOutcome<Result<()>> {
        self.authoritative_completion = None;
        let descriptor = self.capabilities(scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || {
                if let Some(precondition) = precondition {
                    if precondition.scope != scope {
                        return Err(MuxBackendOperationError::stale(
                            "rmux mux binding scope changed",
                        )
                        .into());
                    }
                    let snapshot = self.client.snapshot()?;
                    if !precondition.matches_snapshot(&snapshot) {
                        return Err(MuxBackendOperationError::stale(
                            "rmux command target changed before mutation",
                        )
                        .into());
                    }
                }
                self.execute(command)
            },
        )
    }

    fn execute_session_launch(
        &mut self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        self.authoritative_completion = None;
        match self.rmux_session_launch_capability(&plan) {
            BindingOperationOutcome::Supported(()) => {}
            BindingOperationOutcome::Unsupported => return BindingOperationOutcome::Unsupported,
            BindingOperationOutcome::Unavailable => return BindingOperationOutcome::Unavailable,
            BindingOperationOutcome::Denied => return BindingOperationOutcome::Denied,
            BindingOperationOutcome::Stale => return BindingOperationOutcome::Stale,
        }
        let requested_session_id = plan.session_id.clone();
        match self.client.launch_session_with_allocation(plan) {
            Ok(allocated) => {
                let target = MuxEventTarget::session(
                    allocated
                        .as_ref()
                        .map_or(requested_session_id, |allocated| {
                            allocated.session_id.clone()
                        }),
                );
                self.authoritative_completion = Some(MuxBackendCommandCompletion {
                    allocated,
                    target: Some(target),
                });
                self.client.topology_invalidated();
                BindingOperationOutcome::Supported(Ok(()))
            }
            Err(error) => BindingOperationOutcome::Supported(Err(error)),
        }
    }

    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        self.rmux_session_launch_capability(plan)
    }

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
    }

    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        self.client.event_capabilities()
    }

    fn start_event_stream(&mut self) {
        self.client.start_event_stream();
    }

    fn drain_events(&mut self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        self.client.drain_events(scope, maximum)
    }
    fn release_event_scope(&mut self, scope: MuxScope) {
        self.client.release_event_scope(scope);
    }

    fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
        RmuxBackend::take_authoritative_completion(self)
    }
}

#[cfg(feature = "app")]
/// What an rmux binding can do, wherever its daemon runs. A remote binding drives the same rmux
/// through its command line rather than the socket, so it has to claim the same operations and not
/// the ones tmux happens to add.
pub fn rmux_capabilities(scope: MuxScope) -> BindingCapabilityDescriptor {
    BindingCapabilityDescriptor::new(
        scope,
        [
            BindingOperation::ActivateWindow,
            BindingOperation::CreateWindow,
            BindingOperation::RenameWindow,
            BindingOperation::NavigateWindow,
            BindingOperation::MoveWindow,
            BindingOperation::SplitPane,
            BindingOperation::NavigatePane,
            BindingOperation::LastPane,
            BindingOperation::ResizePane,
            BindingOperation::ClosePane,
            BindingOperation::TogglePaneZoom,
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
}

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
        rmux_snapshot()
    }

    fn launch_session(&self, plan: MuxSessionLaunchPlan) -> Result<()> {
        rmux_execute(MuxCommand::CreateSession { plan })
    }

    fn launch_session_with_allocation(
        &self,
        plan: MuxSessionLaunchPlan,
    ) -> Result<Option<MuxAllocatedResources>> {
        rmux_launch_session(plan).map(Some)
    }

    #[cfg(feature = "app")]
    fn session_launch_capability(
        &self,
        plan: &MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<()> {
        (plan.validate().is_ok() && supports_rmux_session_launch_plan(plan))
            .then_some(())
            .map_or(
                BindingOperationOutcome::Unsupported,
                BindingOperationOutcome::Supported,
            )
    }

    #[cfg(feature = "app")]
    fn launch_session_outcome(
        &self,
        plan: MuxSessionLaunchPlan,
    ) -> BindingOperationOutcome<Result<()>> {
        match self.session_launch_capability(&plan) {
            BindingOperationOutcome::Supported(()) => {
                BindingOperationOutcome::Supported(self.launch_session(plan))
            }
            BindingOperationOutcome::Unsupported => BindingOperationOutcome::Unsupported,
            BindingOperationOutcome::Unavailable => BindingOperationOutcome::Unavailable,
            BindingOperationOutcome::Denied => BindingOperationOutcome::Denied,
            BindingOperationOutcome::Stale => BindingOperationOutcome::Stale,
        }
    }

    fn ensure_session(&self, session_name: &str, cwd: &str) -> Result<()> {
        rmux_execute(MuxCommand::CreateProjectSession {
            session_id: session_name.to_owned(),
            cwd: cwd.to_owned(),
        })
    }

    fn rename_session(&self, session_name: &str, name: &str) -> Result<()> {
        rmux_execute(MuxCommand::RenameSession {
            session_id: session_name.to_owned(),
            name: name.to_owned(),
        })
    }

    fn kill_session(&self, session_name: &str) -> Result<()> {
        rmux_execute(MuxCommand::DitchSession {
            session_id: session_name.to_owned(),
        })
    }

    fn activate_window(&self, session_name: &str, window_id: &str) -> Result<()> {
        rmux_execute(MuxCommand::ActivateWindow {
            session_id: session_name.to_owned(),
            window_id: window_id.to_owned(),
        })
    }

    fn rename_window(&self, session_name: &str, window_id: &str, name: &str) -> Result<()> {
        rmux_execute(MuxCommand::RenameWindow {
            session_id: session_name.to_owned(),
            window_id: window_id.to_owned(),
            name: name.to_owned(),
        })
    }

    fn new_window(&self, session_name: &str, cwd: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::NewWindow {
            session_id: session_name.to_owned(),
            cwd: cwd.map(str::to_owned),
        })
    }

    fn activate_next_window(&self, session_name: &str) -> Result<()> {
        rmux_execute(MuxCommand::ActivateNextWindow {
            session_id: session_name.to_owned(),
        })
    }

    fn activate_previous_window(&self, session_name: &str) -> Result<()> {
        rmux_execute(MuxCommand::ActivatePreviousWindow {
            session_id: session_name.to_owned(),
        })
    }

    fn activate_last_window(&self, session_name: &str) -> Result<()> {
        rmux_execute(MuxCommand::ActivateLastWindow {
            session_id: session_name.to_owned(),
        })
    }

    fn activate_window_index(&self, session_name: &str, index: u32) -> Result<()> {
        rmux_execute(MuxCommand::ActivateWindowIndex {
            session_id: session_name.to_owned(),
            index,
        })
    }

    fn move_window(&self, session_name: &str, window_id: Option<&str>, delta: i32) -> Result<()> {
        rmux_execute(MuxCommand::MoveWindow {
            session_id: session_name.to_owned(),
            window_id: window_id.map(str::to_owned),
            delta,
        })
    }

    fn split_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        direction: MuxSplitDirection,
    ) -> Result<()> {
        rmux_execute(MuxCommand::SplitPane {
            session_id: session_name.to_owned(),
            pane_id: pane_id.map(str::to_owned),
            direction,
        })
    }

    fn close_pane(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::ClosePane {
            session_id: session_name.to_owned(),
            pane_id: pane_id.map(str::to_owned),
        })
    }

    fn select_pane(
        &self,
        session_name: &str,
        window_id: Option<&str>,
        direction: MuxDirection,
    ) -> Result<()> {
        rmux_execute(MuxCommand::SelectPane {
            session_id: session_name.to_owned(),
            window_id: window_id.map(str::to_owned),
            direction,
        })
    }

    fn select_next_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::SelectNextPane {
            session_id: session_name.to_owned(),
            window_id: window_id.map(str::to_owned),
        })
    }

    fn select_previous_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::SelectPreviousPane {
            session_id: session_name.to_owned(),
            window_id: window_id.map(str::to_owned),
        })
    }

    fn select_last_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::SelectLastPane {
            session_id: session_name.to_owned(),
            window_id: window_id.map(str::to_owned),
        })
    }

    fn resize_pane(
        &self,
        session_name: &str,
        pane_id: Option<&str>,
        adjustment: MuxPaneResize,
    ) -> Result<()> {
        rmux_execute(MuxCommand::ResizePane {
            session_id: session_name.to_owned(),
            pane_id: pane_id.map(str::to_owned),
            adjustment,
        })
    }

    fn toggle_pane_zoom(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
        rmux_execute(MuxCommand::TogglePaneZoom {
            session_id: session_name.to_owned(),
            pane_id: pane_id.map(str::to_owned),
        })
    }

    #[cfg(feature = "app")]
    fn event_capabilities(&self) -> Vec<MuxEventCapability> {
        crate::rmux_events::event_capabilities()
    }

    #[cfg(feature = "app")]
    fn start_event_stream(&self) {
        crate::rmux_events::start();
    }

    #[cfg(feature = "app")]
    fn drain_events(&self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
        crate::rmux_events::drain_events(scope, maximum)
    }
    #[cfg(feature = "app")]
    fn release_event_scope(&self, scope: MuxScope) {
        crate::rmux_events::release_event_scope(scope);
    }

    #[cfg(feature = "app")]
    fn topology_invalidated(&self) {
        crate::rmux_events::topology_invalidated();
    }
}

#[cfg(feature = "app")]
pub(crate) fn resize_bootty_rmux_window(window_id: &str, cols: u16, rows: u16) -> Result<()> {
    resize_rmux_window(window_id, cols, rows)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RmuxWindowRow {
    pub(crate) session_name: String,
    pub(crate) id: String,
    pub(crate) index: u32,
    pub(crate) active: bool,
    pub(crate) name: String,
    pub(crate) layout: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RmuxPaneRow {
    pub(crate) session_name: String,
    pub(crate) window_id: String,
    pub(crate) pane_id: String,
    /// Backend-reported terminal identity hosted by this pane. It is absent when the backend
    /// cannot report a terminal identity; callers must not infer it from `pane_id`.
    pub(crate) terminal_id: Option<String>,
    pub(crate) index: u32,
    pub(crate) active: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) process: Option<String>,
    /// Rmux's lifecycle generation is monotonic for a pane within one daemon connection. It is
    /// embedded in the opaque occupant handle so a queued command cannot cross a replacement.
    pub(crate) occupant_id: Option<String>,
}

pub(crate) async fn list_window_rows(
    _rmux: &Rmux,
    name: &SessionName,
) -> Result<Vec<RmuxWindowRow>> {
    let response = rmux_request(Request::ListWindows(Box::new(ListWindowsRequest {
        target: name.clone(),
        format: Some(RMUX_WINDOW_FORMAT.to_owned()),
        filter: None,
        sort_order: None,
        reversed: false,
    })))
    .await?;
    let Response::ListWindows(response) = response else {
        anyhow::bail!("rmux returned an unexpected list-windows response");
    };
    String::from_utf8_lossy(&response.output.stdout)
        .lines()
        .map(parse_window_row)
        .collect()
}

pub(crate) async fn list_pane_rows(_rmux: &Rmux, name: &SessionName) -> Result<Vec<RmuxPaneRow>> {
    let response = rmux_request(Request::ListPanes(Box::new(ListPanesRequest {
        target: name.clone(),
        target_window_index: None,
        format: Some(RMUX_PANE_FORMAT.to_owned()),
        filter: None,
        sort_order: None,
        reversed: false,
    })))
    .await?;
    let Response::ListPanes(response) = response else {
        anyhow::bail!("rmux returned an unexpected list-panes response");
    };
    String::from_utf8_lossy(&response.output.stdout)
        .lines()
        .map(parse_pane_row)
        .collect()
}

#[derive(Clone, Copy)]
enum RmuxResponseOutcome {
    Unsupported,
    Unavailable,
    Denied,
    Stale,
    Failed,
}

fn rmux_daemon_message_outcome(message: &str) -> RmuxResponseOutcome {
    if message == "no current client" || message.starts_with("server is shutting down") {
        return RmuxResponseOutcome::Unavailable;
    }
    if message == "client is read-only" {
        return RmuxResponseOutcome::Denied;
    }
    if message.contains("target was replaced")
        || message.contains("target retired")
        || message.contains("target changed")
        || message.contains("identity changed")
        || message.contains("no longer exists")
    {
        return RmuxResponseOutcome::Stale;
    }

    let kind = message
        .split_once(':')
        .map_or(message, |(kind, _)| kind)
        .trim();
    if kind.eq_ignore_ascii_case("unsupported") {
        RmuxResponseOutcome::Unsupported
    } else if kind.eq_ignore_ascii_case("unavailable") {
        RmuxResponseOutcome::Unavailable
    } else if kind.eq_ignore_ascii_case("denied") {
        RmuxResponseOutcome::Denied
    } else if kind.eq_ignore_ascii_case("stale") {
        RmuxResponseOutcome::Stale
    } else {
        RmuxResponseOutcome::Failed
    }
}

fn rmux_response_outcome(error: &RmuxError) -> Option<RmuxResponseOutcome> {
    match error {
        RmuxError::UnknownCommand(_) | RmuxError::UnsupportedCapability { .. } => {
            Some(RmuxResponseOutcome::Unsupported)
        }
        RmuxError::SessionNotFound(_)
        | RmuxError::PaneNotFound { .. }
        | RmuxError::OwnedSessionLeaseLost { .. } => Some(RmuxResponseOutcome::Stale),
        RmuxError::ProcessStillRunning => Some(RmuxResponseOutcome::Denied),
        RmuxError::Server(message) | RmuxError::Message(message) => {
            Some(rmux_daemon_message_outcome(message))
        }
        RmuxError::UnsupportedWireVersion { .. }
        | RmuxError::FrameTooLarge { .. }
        | RmuxError::EmptyFrame
        | RmuxError::BadFrameMagic(_)
        | RmuxError::IncompleteFrame { .. }
        | RmuxError::Encode(_)
        | RmuxError::Decode(_) => None,
        RmuxError::EmptySessionName
        | RmuxError::InvalidSessionNameCharacter
        | RmuxError::InvalidTarget { .. }
        | RmuxError::DuplicateSession(_)
        | RmuxError::InvalidSetOption(_)
        | RmuxError::SpawnFailed { .. } => Some(RmuxResponseOutcome::Failed),
    }
}

pub(crate) fn rmux_response_checked(response: Response) -> Result<Response> {
    let Response::Error(error) = response else {
        return Ok(response);
    };
    let message = error.error.to_string();
    let Some(outcome) = rmux_response_outcome(&error.error) else {
        return Err(anyhow::Error::msg(format!(
            "rmux request failed: {message}"
        )));
    };
    Err(match outcome {
        RmuxResponseOutcome::Unsupported => MuxBackendOperationError::Unsupported(message).into(),
        RmuxResponseOutcome::Unavailable => MuxBackendOperationError::Unavailable(message).into(),
        RmuxResponseOutcome::Denied => MuxBackendOperationError::Denied(message).into(),
        RmuxResponseOutcome::Stale => MuxBackendOperationError::Stale(message).into(),
        RmuxResponseOutcome::Failed => MuxBackendOperationError::Failed(message).into(),
    })
}

pub(crate) async fn rmux_request(request: Request) -> Result<Response> {
    let endpoint = crate::bootty_rmux_endpoint_path().context("resolve Bootty rmux endpoint")?;
    let response =
        tokio::task::spawn_blocking(move || rmux_client::connect(&endpoint)?.roundtrip(&request))
            .await
            .context("join rmux request")??;
    rmux_response_checked(response)
}

pub(crate) async fn rmux_request_checked(request: Request) -> Result<()> {
    rmux_request(request).await.map(|_| ())
}

fn parse_window_row(line: &str) -> Result<RmuxWindowRow> {
    let mut fields = line.splitn(6, RMUX_FIELD_SEPARATOR);
    let session_name = next_rmux_field(&mut fields, "window session")?.to_owned();
    let id = next_rmux_field(&mut fields, "window id")?.to_owned();
    let index = next_rmux_field(&mut fields, "window index")?
        .parse::<u32>()
        .with_context(|| format!("invalid rmux window index in {line:?}"))?;
    let active = parse_rmux_bool(next_rmux_field(&mut fields, "window active")?);
    let name = next_rmux_field(&mut fields, "window name")?.to_owned();
    let layout = non_empty_rmux_field(next_rmux_field(&mut fields, "window layout")?);
    Ok(RmuxWindowRow {
        session_name,
        id,
        index,
        active,
        name,
        layout,
    })
}

fn parse_pane_row(line: &str) -> Result<RmuxPaneRow> {
    let mut fields = line.splitn(10, RMUX_FIELD_SEPARATOR);
    let session_name = next_rmux_field(&mut fields, "pane session")?.to_owned();
    let window_id = next_rmux_field(&mut fields, "pane window id")?.to_owned();
    let pane_id = next_rmux_field(&mut fields, "pane id")?.to_owned();
    let terminal_id = non_empty_rmux_field(next_rmux_field(&mut fields, "pane terminal id")?);
    let index = next_rmux_field(&mut fields, "pane index")?
        .parse::<u32>()
        .with_context(|| format!("invalid rmux pane index in line {line:?}"))?;
    let active = parse_rmux_bool(next_rmux_field(&mut fields, "pane active")?);
    let cwd = non_empty_rmux_field(next_rmux_field(&mut fields, "pane cwd")?);
    let process = non_empty_rmux_field(next_rmux_field(&mut fields, "pane process")?);
    let generation = fields.next().and_then(non_empty_rmux_field);
    let server_pid = fields.next().and_then(|value| value.parse::<u32>().ok());
    let occupant_id = generation.map(|generation| {
        server_pid.map_or_else(
            || format!("rmux:{pane_id}:generation:{generation}"),
            |server_pid| format!("rmux:{pane_id}:server_pid={server_pid}:generation:{generation}"),
        )
    });
    Ok(RmuxPaneRow {
        session_name,
        window_id,
        terminal_id,
        pane_id,
        index,
        active,
        cwd,
        process,
        occupant_id,
    })
}

fn next_rmux_field<'a>(fields: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<&'a str> {
    fields
        .next()
        .with_context(|| format!("rmux row omitted {name}"))
}

fn parse_rmux_bool(value: &str) -> bool {
    value == "1"
}

fn non_empty_rmux_field(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn rmux_window_layout(raw: &str) -> Option<MuxPaneLayout> {
    TmuxLayout::parse_with_checksum(raw)
        .or_else(|_| TmuxLayout::parse(raw))
        .ok()
        .and_then(|layout| mux_layout_from_tmux_layout(&layout))
}

fn mux_layout_from_tmux_layout(layout: &TmuxLayout) -> Option<MuxPaneLayout> {
    match &layout.content {
        TmuxLayoutContent::Pane(pane_id) => Some(MuxPaneLayout::Pane(format!("%{pane_id}"))),
        TmuxLayoutContent::Horizontal(children) => {
            mux_layout_from_tmux_children(MuxPaneSplitDirection::Right, children, |layout| {
                layout.width
            })
        }
        TmuxLayoutContent::Vertical(children) => {
            mux_layout_from_tmux_children(MuxPaneSplitDirection::Down, children, |layout| {
                layout.height
            })
        }
    }
}

fn mux_layout_from_tmux_children(
    direction: MuxPaneSplitDirection,
    children: &[TmuxLayout],
    extent: fn(&TmuxLayout) -> usize,
) -> Option<MuxPaneLayout> {
    let (first, rest) = children.split_first()?;
    if rest.is_empty() {
        return mux_layout_from_tmux_layout(first);
    }
    let first_layout = mux_layout_from_tmux_layout(first)?;
    let second_layout = mux_layout_from_tmux_children(direction.clone(), rest, extent)?;
    let first_extent = extent(first);
    let total_extent = children.iter().map(extent).sum::<usize>().max(1);
    let ratio_millis = ((first_extent.saturating_mul(1000) + total_extent / 2) / total_extent)
        .clamp(1, 999) as u16;

    Some(MuxPaneLayout::Split {
        direction,
        ratio_millis,
        first: Box::new(first_layout),
        second: Box::new(second_layout),
    })
}

pub(crate) fn session_from_rows(
    name: &str,
    window_rows: &[RmuxWindowRow],
    pane_rows: &[RmuxPaneRow],
) -> MuxSession {
    let mut session_window_rows = window_rows
        .iter()
        .filter(|window| window.session_name == name)
        .collect::<Vec<_>>();
    session_window_rows.sort_by(|left, right| {
        left.index
            .cmp(&right.index)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut windows = session_window_rows
        .iter()
        .enumerate()
        .map(|(position, window)| {
            let mut window_pane_rows = pane_rows
                .iter()
                .filter(|pane| pane.session_name == name && pane.window_id == window.id)
                .collect::<Vec<_>>();
            window_pane_rows.sort_by_key(|pane| pane.index);
            let window_panes = window_pane_rows
                .iter()
                .map(|pane| anchor_for_pane_row(name, pane))
                .collect::<Vec<_>>();
            let anchor = window_pane_rows
                .iter()
                .find(|pane| pane.active)
                .map(|pane| anchor_for_pane_row(name, pane))
                .or_else(|| window_panes.first().cloned())
                .unwrap_or_else(|| MuxPaneAnchor {
                    session_id: name.to_owned(),
                    pane_id: None,
                    terminal_id: None,
                    pane_pid: None,
                    cwd: None,
                    process: None,
                    occupant_id: None,
                });
            MuxWindow {
                id: window.id.clone(),
                index: position as u32 + 1,
                name: window.name.clone(),
                active: window.active,
                panes: window_panes,
                layout: window.layout.as_deref().and_then(rmux_window_layout),
                anchor,
                // Rmux panes each own a PTY, so their progress arrives as OSC 9;4.
                progress: None,
            }
        })
        .collect::<Vec<_>>();
    let active_window_id = windows
        .iter()
        .find(|window| window.active)
        .or_else(|| windows.last())
        .map(|window| window.id.clone());
    if !windows.iter().any(|window| window.active)
        && let Some(active_window_id) = active_window_id.as_deref()
        && let Some(window) = windows
            .iter_mut()
            .find(|window| window.id == active_window_id)
    {
        window.active = true;
    }
    let anchor = active_window_id
        .as_deref()
        .and_then(|id| windows.iter().find(|window| window.id == id))
        .map(|window| window.anchor.clone())
        .or_else(|| windows.first().map(|window| window.anchor.clone()))
        .unwrap_or_else(|| MuxPaneAnchor {
            session_id: name.to_owned(),
            pane_id: None,
            terminal_id: None,
            pane_pid: None,
            cwd: None,
            process: None,
            occupant_id: None,
        });

    MuxSession {
        id: name.to_owned(),
        name: name.to_owned(),
        active: false,
        anchor,
        active_window_id,
        windows,
    }
}

fn anchor_for_pane_row(session_name: &str, pane: &RmuxPaneRow) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session_name.to_owned(),
        pane_id: Some(pane.pane_id.clone()),
        terminal_id: pane.terminal_id.clone(),
        pane_pid: None,
        cwd: pane.cwd.clone(),
        process: pane.process.clone(),
        occupant_id: pane.occupant_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use super::*;
    #[cfg(feature = "app")]
    use crate::backend::{MuxBackend, MuxEventPayload, MuxRebaseReason};
    use crate::command::{MuxCommand, MuxDirection, MuxPaneResize};
    #[cfg(feature = "app")]
    use crate::controller::{BindingId, MuxScope, SpaceId};
    #[cfg(feature = "app")]
    use rmux_sdk::{PaneId, TerminalSizeSpec};

    fn rmux_test_request(request: Request) -> Result<Response> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(rmux_request(request))
    }

    fn rmux_response_error(error: rmux_proto::RmuxError) -> anyhow::Error {
        rmux_response_checked(Response::Error(rmux_proto::ErrorResponse { error }))
            .expect_err("rmux error response must fail")
    }

    #[test]
    fn rmux_protocol_response_errors_keep_backend_outcome_types() {
        let cases = [
            (
                rmux_proto::RmuxError::UnknownCommand("future-command".to_owned()),
                MuxBackendOperationError::Unsupported("unknown command: future-command".to_owned()),
            ),
            (
                rmux_proto::RmuxError::Server(
                    "server is shutting down; mutation was not started".to_owned(),
                ),
                MuxBackendOperationError::Unavailable(
                    "server error: server is shutting down; mutation was not started".to_owned(),
                ),
            ),
            (
                rmux_proto::RmuxError::Server("client is read-only".to_owned()),
                MuxBackendOperationError::Denied("server error: client is read-only".to_owned()),
            ),
            (
                rmux_proto::RmuxError::SessionNotFound("project".to_owned()),
                MuxBackendOperationError::Stale("session not found: project".to_owned()),
            ),
            (
                rmux_proto::RmuxError::InvalidSetOption(
                    "resize dimensions must be positive".to_owned(),
                ),
                MuxBackendOperationError::Failed(
                    "invalid set-option request: resize dimensions must be positive".to_owned(),
                ),
            ),
        ];

        for (protocol_error, expected) in cases {
            let error = rmux_response_error(protocol_error);
            assert_eq!(
                error.downcast_ref::<MuxBackendOperationError>(),
                Some(&expected),
                "rmux protocol error should retain its backend outcome"
            );
        }
    }

    #[test]
    fn rmux_pane_rows_preserve_terminal_identity_independently() {
        let row = parse_pane_row("$1\x1f@1\x1f%p\x1ft1\x1f0\x1f1\x1f/repo\x1fzsh\x1f7")
            .expect("parse pane row");
        assert_eq!(row.pane_id, "%p");
        assert_eq!(row.terminal_id.as_deref(), Some("t1"));

        let anchor = anchor_for_pane_row("$1", &row);
        assert_eq!(anchor.pane_id.as_deref(), Some("%p"));
        assert_eq!(anchor.terminal_id.as_deref(), Some("t1"));
    }

    #[test]
    fn rmux_protocol_stale_errors_preserve_the_exact_target() {
        let error = rmux_response_error(rmux_proto::RmuxError::PaneNotFound {
            session_name: rmux_proto::SessionName::new("project").expect("valid session"),
            pane_id: rmux_proto::PaneId::new(7),
        });

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Stale(
                "invalid target 'project:%7': pane id does not exist in session".to_owned(),
            ))
        );
    }

    #[test]
    fn rmux_protocol_invalid_targets_are_failed_and_preserve_the_exact_target() {
        let error = rmux_response_error(rmux_proto::RmuxError::InvalidTarget {
            value: "project:not-a-pane".to_owned(),
            reason: "pane index must be numeric".to_owned(),
        });

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Failed(
                "invalid target 'project:not-a-pane': pane index must be numeric".to_owned(),
            ))
        );
    }

    #[test]
    fn rmux_protocol_unknown_response_errors_fall_back_to_typed_failure() {
        let error = rmux_response_error(rmux_proto::RmuxError::Server(
            "unrecognized daemon rejection".to_owned(),
        ));

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Failed(
                "server error: unrecognized daemon rejection".to_owned(),
            ))
        );
    }

    #[test]
    fn rmux_protocol_serialization_response_errors_remain_generic() {
        let error = rmux_response_error(rmux_proto::RmuxError::Decode(
            "truncated payload".to_owned(),
        ));

        assert!(
            error.downcast_ref::<MuxBackendOperationError>().is_none(),
            "serialization failures must remain generic"
        );
        assert_eq!(
            error.to_string(),
            "rmux request failed: failed to decode frame payload: truncated payload"
        );

        let error = rmux_response_error(rmux_proto::RmuxError::UnsupportedWireVersion {
            got: 2,
            minimum: 1,
            maximum: 1,
        });
        assert!(
            error.downcast_ref::<MuxBackendOperationError>().is_none(),
            "wire compatibility failures must remain generic"
        );
    }

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

        fn rename_session(&self, session_name: &str, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "rename_session".to_owned(),
                session_name.to_owned(),
                name.to_owned(),
            ]);
            Ok(())
        }

        fn kill_session(&self, session_name: &str) -> Result<()> {
            self.calls
                .borrow_mut()
                .push(vec!["kill_session".to_owned(), session_name.to_owned()]);
            Ok(())
        }

        fn activate_window(&self, session_name: &str, window_id: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_window".to_owned(),
                session_name.to_owned(),
                window_id.to_owned(),
            ]);
            Ok(())
        }

        fn rename_window(&self, session_name: &str, window_id: &str, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "rename_window".to_owned(),
                session_name.to_owned(),
                window_id.to_owned(),
                name.to_owned(),
            ]);
            Ok(())
        }

        fn new_window(&self, session_name: &str, cwd: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "new_window".to_owned(),
                session_name.to_owned(),
                cwd.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn activate_next_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_next_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_previous_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_previous_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_last_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_last_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_window_index(&self, session_name: &str, index: u32) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_window_index".to_owned(),
                session_name.to_owned(),
                index.to_string(),
            ]);
            Ok(())
        }

        fn move_window(
            &self,
            session_name: &str,
            window_id: Option<&str>,
            delta: i32,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "move_window".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
                delta.to_string(),
            ]);
            Ok(())
        }

        fn split_pane(
            &self,
            session_name: &str,
            pane_id: Option<&str>,
            direction: MuxSplitDirection,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "split_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
                format!("{direction:?}"),
            ]);
            Ok(())
        }

        fn close_pane(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "close_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_pane(
            &self,
            session_name: &str,
            window_id: Option<&str>,
            direction: MuxDirection,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
                format!("{direction:?}"),
            ]);
            Ok(())
        }

        fn select_next_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_next_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_previous_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_previous_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_last_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_last_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn resize_pane(
            &self,
            session_name: &str,
            pane_id: Option<&str>,
            adjustment: MuxPaneResize,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "resize_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
                format!("{adjustment:?}"),
            ]);
            Ok(())
        }

        fn toggle_pane_zoom(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "toggle_pane_zoom".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct EmptyClient {
        calls: Rc<RefCell<Vec<Vec<String>>>>,
    }

    impl RmuxSessionClient for EmptyClient {
        fn snapshot(&self) -> Result<MuxSnapshot> {
            self.calls.borrow_mut().push(vec!["snapshot".to_owned()]);
            Ok(MuxSnapshot::default())
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

        fn activate_window(&self, session_name: &str, window_id: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_window".to_owned(),
                session_name.to_owned(),
                window_id.to_owned(),
            ]);
            Ok(())
        }

        fn rename_window(&self, session_name: &str, window_id: &str, name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "rename_window".to_owned(),
                session_name.to_owned(),
                window_id.to_owned(),
                name.to_owned(),
            ]);
            Ok(())
        }

        fn new_window(&self, session_name: &str, cwd: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "new_window".to_owned(),
                session_name.to_owned(),
                cwd.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn activate_next_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_next_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_previous_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_previous_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_last_window(&self, session_name: &str) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_last_window".to_owned(),
                session_name.to_owned(),
            ]);
            Ok(())
        }

        fn activate_window_index(&self, session_name: &str, index: u32) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "activate_window_index".to_owned(),
                session_name.to_owned(),
                index.to_string(),
            ]);
            Ok(())
        }

        fn move_window(
            &self,
            session_name: &str,
            window_id: Option<&str>,
            delta: i32,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "move_window".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
                delta.to_string(),
            ]);
            Ok(())
        }

        fn split_pane(
            &self,
            session_name: &str,
            pane_id: Option<&str>,
            direction: MuxSplitDirection,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "split_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
                format!("{direction:?}"),
            ]);
            Ok(())
        }

        fn close_pane(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "close_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_pane(
            &self,
            session_name: &str,
            window_id: Option<&str>,
            direction: MuxDirection,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
                format!("{direction:?}"),
            ]);
            Ok(())
        }

        fn select_next_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_next_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_previous_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_previous_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn select_last_pane(&self, session_name: &str, window_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "select_last_pane".to_owned(),
                session_name.to_owned(),
                window_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }

        fn resize_pane(
            &self,
            session_name: &str,
            pane_id: Option<&str>,
            adjustment: MuxPaneResize,
        ) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "resize_pane".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
                format!("{adjustment:?}"),
            ]);
            Ok(())
        }

        fn toggle_pane_zoom(&self, session_name: &str, pane_id: Option<&str>) -> Result<()> {
            self.calls.borrow_mut().push(vec![
                "toggle_pane_zoom".to_owned(),
                session_name.to_owned(),
                pane_id.unwrap_or_default().to_owned(),
            ]);
            Ok(())
        }
    }
    #[cfg(feature = "app")]
    #[test]
    fn local_rmux_scope_release_allows_a_recreated_binding_to_bootstrap() {
        let scope = MuxScope::new(
            SpaceId::from_persistence(61_001),
            BindingId::from_persistence(62_001),
        );
        let mut backend = RmuxBackend::new();

        let first = backend.drain_events(scope, 8);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].revision, 1);
        assert!(matches!(
            &first[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));

        backend.release_event_scope(scope);

        let recreated = backend.drain_events(scope, 8);
        assert_eq!(recreated.len(), 1);
        assert_eq!(recreated[0].revision, 1);
        assert!(matches!(
            &recreated[0].payload,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap
            }
        ));
    }

    #[cfg(feature = "app")]
    #[test]
    fn rmux_checked_mutation_rejects_stale_snapshot_before_side_effect() {
        let client = RecordingClient::default();
        let calls = client.calls.clone();
        let mut backend = RmuxBackend::with_client(client);
        let scope = MuxScope::new(
            SpaceId::from_persistence(71),
            BindingId::from_persistence(72),
        );
        let precondition = MuxScopedExecutionPrecondition {
            scope,
            target: MuxEventTarget::session("project"),
            occupant_fingerprint: None,
            binding_generation: None,
            occupant_generation: None,
        };
        let outcome = backend.execute_checked(
            scope,
            MuxCommand::RenameSession {
                session_id: "project".to_owned(),
                name: "replacement".to_owned(),
            },
            Some(&precondition),
        );
        assert!(matches!(
            outcome,
            BindingOperationOutcome::Supported(Err(error))
                if matches!(
                    error.downcast_ref::<MuxBackendOperationError>(),
                    Some(MuxBackendOperationError::Stale(_))
                )
        ));
        assert_eq!(
            calls.borrow().as_slice(),
            &[vec!["snapshot".to_owned()]],
            "rmux must verify the authoritative snapshot before invoking the client"
        );
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
                    "activate_window".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned()
                ],
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
    fn rmux_adapter_maps_native_tab_and_pane_commands_to_sdk_client() {
        let client = RecordingClient::default();
        let calls = client.calls.clone();
        let mut backend = RmuxBackend::with_client(client);

        backend
            .execute(MuxCommand::NewWindow {
                session_id: "project".to_owned(),
                cwd: Some("/repo".to_owned()),
            })
            .unwrap();
        backend
            .execute(MuxCommand::SplitPane {
                session_id: "project".to_owned(),
                pane_id: Some("%3".to_owned()),
                direction: MuxSplitDirection::Down,
            })
            .unwrap();
        backend
            .execute(MuxCommand::ClosePane {
                session_id: "project".to_owned(),
                pane_id: Some("%4".to_owned()),
            })
            .unwrap();
        backend
            .execute(MuxCommand::RenameWindow {
                session_id: "project".to_owned(),
                window_id: "@2".to_owned(),
                name: "build".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivateNextWindow {
                session_id: "project".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivatePreviousWindow {
                session_id: "project".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivateLastWindow {
                session_id: "project".to_owned(),
            })
            .unwrap();
        backend
            .execute(MuxCommand::ActivateWindowIndex {
                session_id: "project".to_owned(),
                index: 2,
            })
            .unwrap();
        backend
            .execute(MuxCommand::MoveWindow {
                session_id: "project".to_owned(),
                window_id: Some("@2".to_owned()),
                delta: -1,
            })
            .unwrap();
        for command in [
            MuxCommand::SelectPane {
                session_id: "project".to_owned(),
                window_id: Some("@2".to_owned()),
                direction: MuxDirection::Right,
            },
            MuxCommand::SelectNextPane {
                session_id: "project".to_owned(),
                window_id: Some("@2".to_owned()),
            },
            MuxCommand::SelectPreviousPane {
                session_id: "project".to_owned(),
                window_id: Some("@2".to_owned()),
            },
            MuxCommand::SelectLastPane {
                session_id: "project".to_owned(),
                window_id: Some("@2".to_owned()),
            },
            MuxCommand::ResizePane {
                session_id: "project".to_owned(),
                pane_id: Some("%4".to_owned()),
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Down,
                    cells: 3,
                },
            },
            MuxCommand::TogglePaneZoom {
                session_id: "project".to_owned(),
                pane_id: Some("%4".to_owned()),
            },
        ] {
            backend.execute(command).unwrap();
        }

        assert_eq!(
            calls.borrow().as_slice(),
            &[
                vec![
                    "new_window".to_owned(),
                    "project".to_owned(),
                    "/repo".to_owned()
                ],
                vec![
                    "split_pane".to_owned(),
                    "project".to_owned(),
                    "%3".to_owned(),
                    "Down".to_owned()
                ],
                vec![
                    "close_pane".to_owned(),
                    "project".to_owned(),
                    "%4".to_owned()
                ],
                vec![
                    "rename_window".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                    "build".to_owned()
                ],
                vec!["activate_next_window".to_owned(), "project".to_owned()],
                vec!["activate_previous_window".to_owned(), "project".to_owned()],
                vec!["activate_last_window".to_owned(), "project".to_owned()],
                vec![
                    "activate_window_index".to_owned(),
                    "project".to_owned(),
                    "2".to_owned()
                ],
                vec![
                    "move_window".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                    "-1".to_owned()
                ],
                vec![
                    "select_pane".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                    "Right".to_owned(),
                ],
                vec![
                    "select_next_pane".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                ],
                vec![
                    "select_previous_pane".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                ],
                vec![
                    "select_last_pane".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                ],
                vec![
                    "resize_pane".to_owned(),
                    "project".to_owned(),
                    "%4".to_owned(),
                    "Directional { direction: Down, cells: 3 }".to_owned(),
                ],
                vec![
                    "toggle_pane_zoom".to_owned(),
                    "project".to_owned(),
                    "%4".to_owned(),
                ],
            ]
        );
    }

    #[test]
    fn rmux_adapter_rejects_invalid_resize_as_a_typed_failure() {
        let client = RecordingClient::default();
        let calls = client.calls.clone();
        let mut backend = RmuxBackend::with_client(client);

        let error = backend
            .execute(MuxCommand::ResizePane {
                session_id: "project".to_owned(),
                pane_id: Some("%4".to_owned()),
                adjustment: MuxPaneResize::Directional {
                    direction: MuxDirection::Right,
                    cells: 0,
                },
            })
            .expect_err("invalid resize must fail");

        assert_eq!(
            error.downcast_ref::<MuxBackendOperationError>(),
            Some(&MuxBackendOperationError::Failed(
                "rmux pane resize requires every supplied dimension to be positive".to_owned(),
            ))
        );
        assert!(
            calls.borrow().is_empty(),
            "invalid resize must not reach the rmux client"
        );
    }

    #[test]
    fn rmux_context_move_restores_the_previously_active_window() {
        let client = RecordingClient::default();
        let calls = client.calls.clone();
        let mut backend = RmuxBackend::with_client(client);

        backend
            .execute(MuxCommand::MoveWindowPreservingSelection {
                session_id: "project".to_owned(),
                window_id: "@2".to_owned(),
                delta: 1,
                selected_window_id: "@3".to_owned(),
            })
            .unwrap();

        assert_eq!(
            calls.borrow().as_slice(),
            [
                vec![
                    "move_window".to_owned(),
                    "project".to_owned(),
                    "@2".to_owned(),
                    "1".to_owned(),
                ],
                vec![
                    "activate_window".to_owned(),
                    "project".to_owned(),
                    "@3".to_owned(),
                ],
            ]
            .as_slice()
        );
    }

    #[cfg(feature = "app")]
    fn wait_for_controller(
        label: &str,
        controller: &mut crate::controller::MuxController,
        repaint: &crate::RepaintHandle,
        config: &bootty_config::config::MultiplexerConfig,
        mut done: impl FnMut(&crate::controller::MuxController) -> bool,
    ) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if let Some(result) = controller.poll_command() {
                result.map_err(anyhow::Error::msg)?;
            }
            if let Some(error) = controller.refresh_sessions(repaint, config) {
                anyhow::bail!(error);
            }
            if done(controller) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                let sessions = controller
                    .sessions()
                    .iter()
                    .map(|session| format!("{}:{}", session.id, session.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "timed out waiting for rmux controller state: {label}; selected={:?}; sessions=[{sessions}]",
                    controller.selected_session()
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn rmux_pane_sizes(session: &str) -> Result<Vec<(u16, u16)>> {
        let response = rmux_test_request(Request::ListPanes(Box::new(ListPanesRequest {
            target: SessionName::new(session)?,
            target_window_index: None,
            format: Some("#{pane_width} #{pane_height}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })))?;
        let Response::ListPanes(response) = response else {
            anyhow::bail!("rmux returned an unexpected list-panes response");
        };
        String::from_utf8_lossy(&response.output.stdout)
            .lines()
            .map(|line| {
                let mut fields = line.split_whitespace();
                let width = fields
                    .next()
                    .context("missing pane width")?
                    .parse::<u16>()?;
                let height = fields
                    .next()
                    .context("missing pane height")?
                    .parse::<u16>()?;
                Ok((width, height))
            })
            .collect()
    }

    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_split_down_stacks_panes() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        crate::start_embedded_rmux_daemon_for_tests()?;
        let client = SdkRmuxClient::new();
        let session = format!("bootty-split-down-{}", std::process::id());
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

        client.ensure_session(&session, &cwd)?;
        client.split_pane(&session, None, MuxSplitDirection::Down)?;
        let sizes = rmux_pane_sizes(&session)?;
        let snapshot = client.snapshot()?;
        let restored_layout = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .and_then(|session| session.windows.first())
            .and_then(|window| window.layout.as_ref())
            .context("split-down rmux snapshot should expose window layout")?;
        assert!(
            matches!(
                restored_layout,
                MuxPaneLayout::Split {
                    direction: MuxPaneSplitDirection::Down,
                    ..
                }
            ),
            "split down snapshot should preserve vertical layout, got {restored_layout:?}"
        );

        client.kill_session(&session)?;

        assert_eq!(sizes.len(), 2, "expected two panes, got {sizes:?}");
        assert!(
            sizes.iter().all(|(width, _)| *width >= 78),
            "split down should stack panes at full width, got {sizes:?}"
        );
        assert!(
            sizes.iter().all(|(_, height)| *height < 24),
            "split down should divide pane height, got {sizes:?}"
        );
        Ok(())
    }

    #[cfg(feature = "app")]
    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_backend_smoke_covers_tabs_splits_switching_and_persistence() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        crate::start_embedded_rmux_daemon_for_tests()?;
        let client = SdkRmuxClient::new();
        let session = format!("bootty-smoke-{}", std::process::id());
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        let other_session = format!("bootty-smoke-other-{}", std::process::id());

        client.ensure_session(&session, &cwd)?;
        client.new_window(&session, Some(&cwd))?;
        client.activate_window_index(&session, 1)?;
        let snapshot = client.snapshot()?;
        let smoke = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("smoke rmux session should exist after creation")?;
        assert_eq!(smoke.windows.len(), 2);
        assert_eq!(
            smoke.active_window_id.as_deref(),
            Some(smoke.windows[0].id.as_str())
        );

        client.split_pane(&session, None, MuxSplitDirection::Down)?;
        let snapshot = client.snapshot()?;
        let smoke = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("smoke rmux session should exist after active-pane split")?;
        assert_eq!(smoke.windows[0].panes.len(), 2);

        let pane_id = smoke.windows[0]
            .anchor
            .pane_id
            .as_deref()
            .context("smoke window should expose its active pane id")?;
        client.split_pane(&session, Some(pane_id), MuxSplitDirection::Right)?;
        let snapshot = client.snapshot()?;
        let smoke = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("smoke rmux session should exist after targeted split")?;
        assert_eq!(smoke.windows[0].panes.len(), 3);

        client.activate_next_window(&session)?;
        let snapshot = client.snapshot()?;
        let smoke = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("smoke rmux session should exist after tab switch")?;
        assert_eq!(
            smoke.active_window_id.as_deref(),
            Some(smoke.windows[1].id.as_str())
        );

        let repaint: crate::RepaintHandle = std::sync::Arc::new(|| {});
        let config = bootty_config::config::MultiplexerConfig {
            backend: bootty_config::config::MultiplexerBackendConfig::Rmux,
            ..Default::default()
        };
        let mut controller = crate::controller::MuxController::new();
        controller.create_project_session(
            crate::controller::NewMuxSessionRequest {
                session_id: session.clone(),
                cwd: cwd.clone(),
            },
            &repaint,
            &config,
        );
        wait_for_controller(
            "initial controller session",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(session.as_str())
                    && controller.selected_session_anchor().is_some()
            },
        )?;
        controller.create_project_session(
            crate::controller::NewMuxSessionRequest {
                session_id: other_session.clone(),
                cwd: cwd.clone(),
            },
            &repaint,
            &config,
        );
        wait_for_controller(
            "other controller session",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(other_session.as_str())
                    && controller.selected_session_anchor().is_some()
            },
        )?;
        let pane_id = controller
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone())
            .context("controller should expose the selected rmux pane id")?;

        controller.execute_command(
            &repaint,
            &config,
            MuxCommand::SplitPane {
                session_id: other_session.clone(),
                pane_id: Some(pane_id),
                direction: MuxSplitDirection::Right,
            },
        );
        wait_for_controller(
            "controller split",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(other_session.as_str())
                    && controller.selected_window_panes().len() == 2
            },
        )?;

        controller.execute_command(
            &repaint,
            &config,
            MuxCommand::NewWindow {
                session_id: other_session.clone(),
                cwd: Some(cwd.clone()),
            },
        );
        wait_for_controller(
            "controller new window",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(other_session.as_str())
                    && controller.selected_session_windows().len() >= 2
            },
        )?;
        let active_before_switch = controller.selected_window().map(str::to_owned);
        controller.execute_command(
            &repaint,
            &config,
            MuxCommand::ActivateNextWindow {
                session_id: other_session.clone(),
            },
        );
        wait_for_controller(
            "controller next window",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(other_session.as_str())
                    && controller.selected_window().map(str::to_owned) != active_before_switch
            },
        )?;
        let moved_window = controller
            .selected_session_windows()
            .get(1)
            .map(|window| window.id.clone())
            .context("controller should have a second rmux window to move")?;
        let before_move_index = 1;
        controller.execute_command(
            &repaint,
            &config,
            MuxCommand::MoveWindow {
                session_id: other_session.clone(),
                window_id: Some(moved_window.clone()),
                delta: -1,
            },
        );
        wait_for_controller(
            "controller move window",
            &mut controller,
            &repaint,
            &config,
            |controller| {
                controller.selected_session() == Some(other_session.as_str())
                    && controller.selected_window() == Some(moved_window.as_str())
                    && controller
                        .selected_session_windows()
                        .iter()
                        .position(|window| window.id == moved_window)
                        .is_some_and(|index| index + 1 == before_move_index)
            },
        )?;
        client.kill_session(&session)?;
        client.kill_session(&other_session)?;

        Ok(())
    }

    #[cfg(feature = "app")]
    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_window_resize_makes_bootty_split_pane_sizes_real() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        crate::start_embedded_rmux_daemon_for_tests()?;
        let client = SdkRmuxClient::new();
        let session = format!("bootty-resize-{}", std::process::id());
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

        client.ensure_session(&session, &cwd)?;
        client.split_pane(&session, None, MuxSplitDirection::Right)?;
        let snapshot = client.snapshot()?;
        let smoke = snapshot
            .sessions
            .iter()
            .find(|candidate| candidate.id == session)
            .context("resize rmux session should exist after split")?;
        let window = smoke
            .windows
            .first()
            .context("resize window should exist")?;
        let window_id = window.id.clone();
        let pane_ids = window
            .panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect::<Vec<_>>();
        assert_eq!(pane_ids.len(), 2);

        resize_bootty_rmux_window(&window_id, 117, 40)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let rmux = crate::rmux_bridge::connect_bootty_rmux().await?;
            let session_name = SessionName::new(&session)?;
            for pane_id in &pane_ids {
                let pane_id = pane_id
                    .strip_prefix('%')
                    .context("rmux pane id should use tmux-style prefix")?
                    .parse::<u32>()?;
                rmux.pane_by_id(session_name.clone(), PaneId::from(pane_id))
                    .await?
                    .resize(TerminalSizeSpec::new(58, 40))
                    .await?;
            }
            Result::<()>::Ok(())
        })?;
        let sizes = rmux_pane_sizes(&session)?;

        client.kill_session(&session)?;

        assert_eq!(sizes, vec![(58, 40), (58, 40)]);
        Ok(())
    }

    #[test]
    fn rmux_window_layout_restores_vertical_split_tree() {
        let layout = rmux_window_layout("80x24,0,0[80x12,0,0,1,80x11,0,13,2]")
            .expect("tmux-compatible layout should parse");

        assert_eq!(
            layout,
            MuxPaneLayout::Split {
                direction: MuxPaneSplitDirection::Down,
                ratio_millis: 522,
                first: Box::new(MuxPaneLayout::Pane("%1".to_owned())),
                second: Box::new(MuxPaneLayout::Pane("%2".to_owned())),
            }
        );
    }

    #[test]
    fn rmux_snapshot_preserves_window_layout_metadata() {
        let windows = vec![RmuxWindowRow {
            session_name: "alpha".to_owned(),
            id: "@10".to_owned(),
            index: 0,
            active: true,
            name: "one".to_owned(),
            layout: Some("80x24,0,0[80x12,0,0,1,80x11,0,13,2]".to_owned()),
        }];
        let panes = vec![
            RmuxPaneRow {
                session_name: "alpha".to_owned(),
                window_id: "@10".to_owned(),
                pane_id: "%1".to_owned(),
                terminal_id: Some("t1".to_owned()),
                index: 0,
                active: true,
                cwd: None,
                process: None,
                occupant_id: Some("rmux:%1:generation:1".to_owned()),
            },
            RmuxPaneRow {
                session_name: "alpha".to_owned(),
                window_id: "@10".to_owned(),
                pane_id: "%2".to_owned(),
                terminal_id: Some("t2".to_owned()),
                index: 1,
                active: false,
                cwd: None,
                process: None,
                occupant_id: Some("rmux:%2:generation:1".to_owned()),
            },
        ];

        let session = session_from_rows("alpha", &windows, &panes);

        assert!(matches!(
            session.windows[0].layout,
            Some(MuxPaneLayout::Split {
                direction: MuxPaneSplitDirection::Down,
                ..
            })
        ));
    }
    #[test]
    fn rmux_lifecycle_generation_is_part_of_the_opaque_occupant_handle() {
        let first = parse_pane_row("alpha\x1f@10\x1f%1\x1ft1\x1f0\x1f1\x1f/repo\x1fzsh\x1f1")
            .expect("initial pane row");
        let replacement = parse_pane_row("alpha\x1f@10\x1f%1\x1ft1\x1f0\x1f1\x1f/repo\x1fzsh\x1f2")
            .expect("replacement pane row");

        assert_ne!(first.occupant_id, replacement.occupant_id);
        assert_eq!(
            replacement.occupant_id.as_deref(),
            Some("rmux:%1:generation:2")
        );
    }

    #[test]
    fn rmux_snapshot_presents_full_session_windows_and_panes() {
        let windows = vec![
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@10".to_owned(),
                index: 0,
                active: false,
                name: "one".to_owned(),
                layout: None,
            },
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@11".to_owned(),
                index: 1,
                active: true,
                name: "two".to_owned(),
                layout: None,
            },
        ];
        let panes = vec![
            RmuxPaneRow {
                session_name: "alpha".to_owned(),
                window_id: "@10".to_owned(),
                pane_id: "%1".to_owned(),
                terminal_id: Some("t1".to_owned()),
                index: 1,
                active: false,
                cwd: Some("/repo".to_owned()),
                process: Some("fish".to_owned()),
                occupant_id: Some("rmux:%1:generation:1".to_owned()),
            },
            RmuxPaneRow {
                session_name: "alpha".to_owned(),
                window_id: "@10".to_owned(),
                pane_id: "%2".to_owned(),
                terminal_id: Some("t2".to_owned()),
                index: 0,
                active: true,
                cwd: Some("/repo".to_owned()),
                process: Some("vim".to_owned()),
                occupant_id: Some("rmux:%2:generation:1".to_owned()),
            },
            RmuxPaneRow {
                session_name: "alpha".to_owned(),
                window_id: "@11".to_owned(),
                pane_id: "%3".to_owned(),
                terminal_id: Some("t3".to_owned()),
                index: 0,
                active: true,
                cwd: Some("/build".to_owned()),
                process: Some("cargo".to_owned()),
                occupant_id: Some("rmux:%3:generation:1".to_owned()),
            },
        ];

        let snapshot = session_from_rows("alpha", &windows, &panes);

        assert_eq!(snapshot.active_window_id.as_deref(), Some("@11"));
        assert_eq!(snapshot.anchor.pane_id.as_deref(), Some("%3"));
        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|window| (window.id.as_str(), window.index, window.active))
                .collect::<Vec<_>>(),
            vec![("@10", 1, false), ("@11", 2, true)]
        );
        assert_eq!(
            snapshot.windows[0]
                .panes
                .iter()
                .filter_map(|pane| pane.pane_id.as_deref())
                .collect::<Vec<_>>(),
            vec!["%2", "%1"]
        );
        assert_eq!(snapshot.windows[0].anchor.pane_id.as_deref(), Some("%2"));
    }
    #[test]
    fn rmux_snapshot_compacts_skipped_window_indexes_for_display() {
        let windows = vec![
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@10".to_owned(),
                index: 0,
                active: false,
                name: "one".to_owned(),
                layout: None,
            },
            RmuxWindowRow {
                session_name: "alpha".to_owned(),
                id: "@12".to_owned(),
                index: 2,
                active: true,
                name: "three".to_owned(),
                layout: None,
            },
        ];

        let snapshot = session_from_rows("alpha", &windows, &[]);

        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|window| (window.id.as_str(), window.index))
                .collect::<Vec<_>>(),
            vec![("@10", 1), ("@12", 2)]
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
                        terminal_id: Some("t1".to_owned()),
                        pane_pid: None,
                        cwd: Some("/repo".to_owned()),
                        process: Some("vim".to_owned()),
                        occupant_id: None,
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
    fn rmux_snapshot_leaves_empty_server_empty() {
        let client = EmptyClient::default();
        let calls = client.calls.clone();
        let backend = RmuxBackend::with_client(client);

        let snapshot = backend.snapshot().unwrap();

        assert!(snapshot.sessions.is_empty());
        assert_eq!(calls.borrow().as_slice(), &[vec!["snapshot".to_owned()]]);
    }
}
