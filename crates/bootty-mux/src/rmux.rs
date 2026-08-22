use anyhow::{Context, Result};
use rmux_proto::{ListPanesRequest, ListWindowsRequest, Request, Response};
use rmux_sdk::{Rmux, SessionName};

#[cfg(feature = "app")]
use crate::rmux_bridge::resize_rmux_window;
use crate::rmux_bridge::{rmux_execute, rmux_snapshot};

#[cfg(feature = "app")]
use super::{
    backend::MuxBackend,
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::MuxScope,
};
use super::{
    command::{MuxCommand, MuxSplitDirection},
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxPaneSplitDirection, MuxSession, MuxSnapshot, MuxWindow,
    },
    tmux_protocol::{TmuxLayout, TmuxLayoutContent},
};

const RMUX_FIELD_SEPARATOR: char = '\u{1f}';
pub(crate) const RMUX_WINDOW_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_active}\u{1f}#{window_name}\u{1f}#{window_layout}";
pub(crate) const RMUX_PANE_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{pane_id}\u{1f}#{pane_index}\u{1f}#{pane_active}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}";

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

impl<C: RmuxSessionClient> RmuxBackend<C> {
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        self.client.snapshot()
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        match command {
            MuxCommand::ActivateWindow {
                session_id,
                window_id,
            } => {
                self.client.activate_window(&session_id, &window_id)?;
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
            MuxCommand::SelectPane { .. }
            | MuxCommand::SelectNextPane { .. }
            | MuxCommand::SelectPreviousPane { .. }
            | MuxCommand::TogglePaneZoom { .. } => {
                anyhow::bail!("rmux backend does not support mux command {command:?}");
            }
        }
        Ok(())
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

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        rmux_capabilities(scope)
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
            BindingOperation::ClosePane,
            BindingOperation::CreateProjectSession,
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
        ],
    )
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
        rmux_snapshot()
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
    pub(crate) index: u32,
    pub(crate) active: bool,
    pub(crate) cwd: Option<String>,
    pub(crate) process: Option<String>,
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

pub(crate) async fn rmux_request(request: Request) -> Result<Response> {
    let endpoint = crate::local_rmux::endpoint_path().context("resolve Bootty rmux endpoint")?;
    let response =
        tokio::task::spawn_blocking(move || rmux_client::connect(&endpoint)?.roundtrip(&request))
            .await
            .context("join rmux request")??;
    if let Response::Error(error) = response {
        anyhow::bail!("rmux request failed: {}", error.error);
    }
    Ok(response)
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
    let mut fields = line.splitn(7, RMUX_FIELD_SEPARATOR);
    let session_name = next_rmux_field(&mut fields, "pane session")?.to_owned();
    let window_id = next_rmux_field(&mut fields, "pane window id")?.to_owned();
    let pane_id = next_rmux_field(&mut fields, "pane id")?.to_owned();
    let index = next_rmux_field(&mut fields, "pane index")?
        .parse::<u32>()
        .with_context(|| format!("invalid rmux pane index in {line:?}"))?;
    let active = parse_rmux_bool(next_rmux_field(&mut fields, "pane active")?);
    let cwd = non_empty_rmux_field(next_rmux_field(&mut fields, "pane cwd")?);
    let process = non_empty_rmux_field(next_rmux_field(&mut fields, "pane process")?);
    Ok(RmuxPaneRow {
        session_name,
        window_id,
        pane_id,
        index,
        active,
        cwd,
        process,
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
                    pane_pid: None,
                    cwd: None,
                    process: None,
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
            pane_pid: None,
            cwd: None,
            process: None,
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
        pane_pid: None,
        cwd: pane.cwd.clone(),
        process: pane.process.clone(),
    }
}
