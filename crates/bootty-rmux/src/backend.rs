use anyhow::{Context, Result};
use rmux_proto::{ListPanesRequest, ListWindowsRequest, Request, Response};
use rmux_sdk::{Rmux, SessionName};

#[cfg(feature = "app")]
use crate::bridge::resize_rmux_window;
use crate::bridge::{rmux_execute, rmux_snapshot};

use bootty_mux::{
    backend::MuxBackend,
    command::MuxCommand,
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxSession, MuxSnapshot, MuxSnapshotDisposition, MuxWindow,
    },
    tmux_compatible_layout::{parse, parse_with_checksum},
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::MuxScope,
};

const RMUX_FIELD_SEPARATOR: char = '\u{1f}';
pub(crate) const RMUX_WINDOW_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_active}\u{1f}#{window_name}\u{1f}#{window_layout}";
pub(crate) const RMUX_PANE_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{pane_id}\u{1f}#{pane_index}\u{1f}#{pane_active}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}";

pub struct RmuxBackend<C = RmuxControl> {
    control: C,
}

impl RmuxBackend<RmuxControl> {
    pub fn new() -> Self {
        Self::with_control(RmuxControl)
    }
}

impl Default for RmuxBackend<RmuxControl> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> RmuxBackend<C> {
    pub fn with_control(control: C) -> Self {
        Self { control }
    }
}

impl<C: MuxBackend> RmuxBackend<C> {
    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        let mut snapshot = self.control.snapshot()?;
        snapshot.disposition = rmux_snapshot_disposition(&snapshot.sessions);
        Ok(snapshot)
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.control.execute(command)
    }
}

fn rmux_snapshot_disposition(sessions: &[MuxSession]) -> MuxSnapshotDisposition {
    if sessions.is_empty()
        || sessions.iter().all(|session| {
            session
                .windows
                .iter()
                .any(|window| !window.panes.is_empty())
        })
    {
        MuxSnapshotDisposition::Authoritative
    } else {
        MuxSnapshotDisposition::Transient
    }
}

impl<C: MuxBackend> MuxBackend for RmuxBackend<C> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        RmuxBackend::snapshot(self)
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        RmuxBackend::execute(self, command)
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

pub struct RmuxControl;

impl MuxBackend for RmuxControl {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        rmux_snapshot()
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        rmux_execute(command)
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
    let endpoint = crate::local::endpoint_path().context("resolve Bootty rmux endpoint")?;
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
    parse_with_checksum(raw).or_else(|_| parse(raw)).ok()
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
