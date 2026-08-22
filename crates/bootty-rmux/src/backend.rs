use std::collections::HashMap;

use anyhow::{Context, Result};
use rmux_proto::{
    ListPanesRequest, ListSessionsRequest, ListWindowsRequest, OptionScopeSelector, Request,
    Response, SetOptionByNameRequest, SetOptionMode, ShowOptionsRequest,
};
use rmux_sdk::{Rmux, SessionName};

#[cfg(feature = "app")]
use crate::bridge::resize_rmux_window;
use crate::bridge::{rmux_execute, rmux_snapshot};

use bootty_mux::{
    backend::MuxBackend,
    command::MuxCommand,
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxSession, MuxSessionTag, MuxSnapshot,
        MuxSnapshotDisposition, MuxWindow, SESSION_IDENTITY_OPTION, SESSION_SPACE_OPTION,
    },
    tmux_compatible_layout::{parse, parse_with_checksum},
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::SpaceId,
};

const RMUX_FIELD_SEPARATOR: char = '\u{1f}';
pub(crate) const RMUX_WINDOW_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{window_index}\u{1f}#{window_active}\u{1f}#{window_name}\u{1f}#{window_layout}";
pub(crate) const RMUX_PANE_FORMAT: &str = "#{session_name}\u{1f}#{window_id}\u{1f}#{pane_id}\u{1f}#{pane_index}\u{1f}#{pane_active}\u{1f}#{pane_current_path}\u{1f}#{pane_current_command}";
/// Names and rename-stable ids, which is what the Bootty tag is keyed by.
pub(crate) const RMUX_SESSION_ID_FORMAT: &str = "#{session_name}\u{1f}#{session_id}";

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
pub fn rmux_capabilities(scope: SpaceId) -> BindingCapabilityDescriptor {
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
            BindingOperation::ClosePane,
            BindingOperation::TogglePaneZoom,
            BindingOperation::CreateProjectSession,
            BindingOperation::StampSession,
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
    tag: MuxSessionTag,
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
        tag,
    }
}

/// The Bootty tag for one rmux session, at server scope, keyed by the session's stable id.
///
/// tmux hangs options off the session itself, so a tag written there survives a rename from
/// anywhere. rmux keys its option store by session *name* and does not migrate it on rename
/// (`rmux-core::session::store::rename_session` rekeys leases, subscriptions and attaches, but not
/// options), so a session-scoped tag would be orphaned by a rename bootty did not issue. Keying on
/// `session_id` -- which rmux documents as its stable identity -- gets the same guarantee.
pub fn session_tag_option(session_id: &str, option: &str) -> String {
    // rmux renders session ids as `$3`; the sigil buys nothing inside an option name.
    format!("{option}_{}", session_id.trim_start_matches('$'))
}

/// Every session's name and stable id, in rmux's own order.
async fn list_session_ids() -> Result<Vec<(String, String)>> {
    let response = rmux_request(Request::ListSessions(ListSessionsRequest {
        format: Some(RMUX_SESSION_ID_FORMAT.to_owned()),
        filter: None,
        sort_order: None,
        reversed: false,
    }))
    .await?;
    let Response::ListSessions(response) = response else {
        anyhow::bail!("rmux returned an unexpected list-sessions response");
    };
    Ok(String::from_utf8_lossy(&response.output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(RMUX_FIELD_SEPARATOR);
            let name = fields.next().and_then(non_empty_rmux_field)?;
            let id = fields.next().and_then(non_empty_rmux_field)?;
            Some((name, id))
        })
        .collect())
}

/// Every `@`-prefixed server option, which is where the tags live.
async fn server_user_options() -> Result<HashMap<String, String>> {
    let response = rmux_request(Request::ShowOptions(ShowOptionsRequest {
        scope: OptionScopeSelector::ServerGlobal,
        name: None,
        value_only: false,
        include_inherited: false,
        quiet: true,
        include_hooks: false,
    }))
    .await?;
    let Response::ShowOptions(response) = response else {
        anyhow::bail!("rmux returned an unexpected show-options response");
    };
    Ok(String::from_utf8_lossy(&response.output.stdout)
        .lines()
        .filter(|line| line.starts_with('@'))
        .filter_map(|line| {
            let (name, value) = line.split_once(' ')?;
            Some((name.to_owned(), value.to_owned()))
        })
        .collect())
}

/// The tag each live session carries, keyed by session name.
///
/// Also clears tags left behind by sessions that are gone, since rmux reuses a session id once
/// nothing holds it and a stale option would be inherited by whatever takes that id next.
pub(crate) async fn list_session_tags(_rmux: &Rmux) -> Result<HashMap<String, MuxSessionTag>> {
    let sessions = list_session_ids().await?;
    let mut options = server_user_options().await?;
    let mut tags = HashMap::with_capacity(sessions.len());
    let mut highest = 0;
    for (name, id) in &sessions {
        highest = highest.max(numeric_session_id(id).unwrap_or(0));
        let tag = MuxSessionTag {
            identity: options.remove(&session_tag_option(id, SESSION_IDENTITY_OPTION)),
            space: options.remove(&session_tag_option(id, SESSION_SPACE_OPTION)),
        };
        if !tag.is_empty() {
            tags.insert(name.clone(), tag);
        }
    }

    // Only ids below the highest live one are safely dead. rmux hands out increasing ids, so an
    // id above every live session belongs to one that was created but is not in this listing yet
    // -- pruning it would throw away the tag of a session mid-creation.
    let stale = options
        .keys()
        .filter(|name| tag_option_id(name).is_some_and(|id| id < highest))
        .cloned()
        .collect::<Vec<_>>();
    for option in stale {
        set_server_option(&option, None).await?;
    }
    Ok(tags)
}

pub fn numeric_session_id(session_id: &str) -> Option<u32> {
    session_id.trim_start_matches('$').parse().ok()
}

/// The session id a tag option is keyed by, for the options bootty owns.
pub fn tag_option_id(option: &str) -> Option<u32> {
    [SESSION_IDENTITY_OPTION, SESSION_SPACE_OPTION]
        .into_iter()
        .find_map(|owned| option.strip_prefix(owned)?.strip_prefix('_'))
        .and_then(|id| id.parse().ok())
}

/// Writes one server option, or clears it when `value` is `None`.
async fn set_server_option(name: &str, value: Option<&str>) -> Result<()> {
    rmux_request_checked(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
        scope: OptionScopeSelector::ServerGlobal,
        name: name.to_owned(),
        value: value.map(str::to_owned),
        mode: SetOptionMode::Replace,
        only_if_unset: false,
        unset: value.is_none(),
        unset_pane_overrides: false,
        format: false,
        format_target: None,
    })))
    .await
}

/// Writes `tag` onto the named session. A half that is `None` is a claim being dropped, so it
/// clears its option rather than writing an empty value.
pub(crate) async fn stamp_session_tag(name: &SessionName, tag: &MuxSessionTag) -> Result<()> {
    let name = name.to_string();
    let Some((_, id)) = list_session_ids()
        .await?
        .into_iter()
        .find(|(session, _)| *session == name)
    else {
        anyhow::bail!("rmux session {name} is unavailable")
    };
    for (option, value) in [
        (SESSION_IDENTITY_OPTION, tag.identity.as_deref()),
        (SESSION_SPACE_OPTION, tag.space.as_deref()),
    ] {
        set_server_option(&session_tag_option(&id, option), value).await?;
    }
    Ok(())
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
