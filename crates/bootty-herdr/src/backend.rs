use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use bootty_mux::{
    backend::MuxBackend,
    command::{MuxCommand, MuxDirection, MuxSplitDirection},
    snapshot::{
        MuxPaneAnchor, MuxPaneLayout, MuxPaneSplitDirection, MuxSession, MuxSessionTag,
        MuxSnapshot, MuxWindow, MuxWindowProgress,
    },
};
#[cfg(feature = "app")]
use bootty_mux::{
    capability::{BindingCapabilityDescriptor, BindingOperation},
    controller::SpaceId,
};
use serde_json::{Value, json};

use crate::{
    control::{CliHerdrApi, HerdrApi},
    model::{
        HerdrLayout, HerdrLayoutPane, HerdrLayoutSplit, HerdrPane, HerdrSessionSnapshot, HerdrTab,
        HerdrWorkspace,
    },
};

const BOOTTY_ID_TOKEN: &str = "bootty_id";
const BOOTTY_SPACE_TOKEN: &str = "bootty_space";

pub struct HerdrBackend<A = CliHerdrApi> {
    api: A,
}

impl HerdrBackend<CliHerdrApi> {
    pub fn new(session: impl Into<String>) -> Self {
        Self::with_api(CliHerdrApi::new(session))
    }
}

impl<A> HerdrBackend<A> {
    pub fn with_api(api: A) -> Self {
        Self { api }
    }

    pub fn api(&self) -> &A {
        &self.api
    }
}

impl<A: HerdrApi> HerdrBackend<A> {
    pub fn herdr_snapshot(&self) -> Result<HerdrSessionSnapshot> {
        self.api.snapshot()
    }

    pub fn snapshot(&self) -> Result<MuxSnapshot> {
        project_snapshot(&self.herdr_snapshot()?)
    }

    pub fn execute(&mut self, command: MuxCommand) -> Result<()> {
        let snapshot = needs_snapshot(&command)
            .then(|| self.herdr_snapshot())
            .transpose()?;
        match command {
            MuxCommand::ActivateWindow { window_id, .. } => {
                self.call("tab.focus", json!({"tab_id": window_id}))?;
            }
            MuxCommand::NewWindow { session_id, cwd } => {
                self.call(
                    "tab.create",
                    compact(json!({"workspace_id": session_id, "cwd": cwd, "focus": true})),
                )?;
            }
            MuxCommand::RenameWindow {
                window_id, name, ..
            } => {
                self.call("tab.rename", json!({"tab_id": window_id, "label": name}))?;
            }
            MuxCommand::ActivateNextWindow { session_id } => {
                self.focus_relative_tab(snapshot.as_ref(), &session_id, 1)?;
            }
            MuxCommand::ActivatePreviousWindow { session_id }
            | MuxCommand::ActivateLastWindow { session_id } => {
                self.focus_relative_tab(snapshot.as_ref(), &session_id, -1)?;
            }
            MuxCommand::ActivateWindowIndex { session_id, index } => {
                let tab = tabs(require_snapshot(snapshot.as_ref())?, &session_id)
                    .into_iter()
                    .find(|tab| tab.number == index || tab.number.saturating_add(1) == index)
                    .with_context(|| format!("Herdr workspace {session_id} has no tab {index}"))?;
                self.call("tab.focus", json!({"tab_id": tab.tab_id}))?;
            }
            MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            } => {
                let snapshot = require_snapshot(snapshot.as_ref())?;
                let id = window_id
                    .or_else(|| active_tab(snapshot, &session_id))
                    .context("Herdr workspace has no active tab")?;
                self.move_tab(snapshot, &session_id, &id, delta)?;
            }
            MuxCommand::MoveWindowPreservingSelection {
                session_id,
                window_id,
                delta,
                selected_window_id,
            } => {
                self.move_tab(
                    require_snapshot(snapshot.as_ref())?,
                    &session_id,
                    &window_id,
                    delta,
                )?;
                self.call("tab.focus", json!({"tab_id": selected_window_id}))?;
            }
            MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction,
            } => {
                let pane = resolve_pane(
                    require_snapshot(snapshot.as_ref())?,
                    &session_id,
                    None,
                    pane_id.as_deref(),
                )?;
                self.call("pane.split", json!({"target_pane_id": pane, "direction": split_direction(direction), "focus": true}))?;
            }
            MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            } => {
                let pane = focused_pane(
                    require_snapshot(snapshot.as_ref())?,
                    &session_id,
                    window_id.as_deref(),
                );
                self.call(
                    "pane.focus_direction",
                    compact(json!({"pane_id": pane, "direction": pane_direction(direction)})),
                )?;
            }
            MuxCommand::SelectNextPane {
                session_id,
                window_id,
            } => {
                self.focus_relative_pane(snapshot.as_ref(), &session_id, window_id.as_deref(), 1)?;
            }
            MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            } => {
                self.focus_relative_pane(snapshot.as_ref(), &session_id, window_id.as_deref(), -1)?;
            }
            MuxCommand::KillPane {
                session_id,
                pane_id,
            }
            | MuxCommand::ClosePane {
                session_id,
                pane_id,
            } => {
                let pane = resolve_pane(
                    require_snapshot(snapshot.as_ref())?,
                    &session_id,
                    None,
                    pane_id.as_deref(),
                )?;
                self.call("pane.close", json!({"pane_id": pane}))?;
            }
            MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            } => {
                let pane = resolve_pane(
                    require_snapshot(snapshot.as_ref())?,
                    &session_id,
                    None,
                    pane_id.as_deref(),
                )?;
                self.call("pane.zoom", json!({"pane_id": pane, "mode": "toggle"}))?;
            }
            MuxCommand::CreateProjectSession {
                session_id,
                cwd,
                tag,
            }
            | MuxCommand::CreateWorktreeSession {
                session_id,
                cwd,
                tag,
            } => {
                let result = self.call(
                    "workspace.create",
                    json!({"cwd": cwd, "label": session_id, "focus": true}),
                )?;
                let id = result
                    .pointer("/workspace/workspace_id")
                    .and_then(Value::as_str)
                    .context("Herdr workspace.create omitted workspace.workspace_id")?;
                self.stamp(id, &tag)?;
            }
            MuxCommand::RenameSession { session_id, name } => {
                self.call(
                    "workspace.rename",
                    json!({"workspace_id": session_id, "label": name}),
                )?;
            }
            MuxCommand::DitchSession { session_id } => {
                self.call("workspace.close", json!({"workspace_id": session_id}))?;
            }
            MuxCommand::StampSession { session_id, tag } => {
                self.stamp(&session_id, &tag)?;
            }
        }
        Ok(())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.api
            .request(method, params)
            .with_context(|| format!("Herdr {method}"))
    }

    fn stamp(&self, id: &str, tag: &MuxSessionTag) -> Result<Value> {
        self.call(
            "workspace.report_metadata",
            json!({
                "workspace_id": id, "source": "bootty",
                "tokens": {"bootty_id": tag.identity, "bootty_space": tag.space}
            }),
        )
    }

    fn focus_relative_tab(
        &self,
        snapshot: Option<&HerdrSessionSnapshot>,
        workspace: &str,
        delta: i32,
    ) -> Result<()> {
        let snapshot = require_snapshot(snapshot)?;
        let tabs = tabs(snapshot, workspace);
        let current = active_tab(snapshot, workspace)
            .and_then(|id| tabs.iter().position(|tab| tab.tab_id == id))
            .unwrap_or(0);
        let index = relative_index(current, tabs.len(), delta)
            .context("cannot navigate an empty Herdr workspace")?;
        self.call("tab.focus", json!({"tab_id": tabs[index].tab_id}))?;
        Ok(())
    }

    fn focus_relative_pane(
        &self,
        snapshot: Option<&HerdrSessionSnapshot>,
        workspace: &str,
        window: Option<&str>,
        delta: i32,
    ) -> Result<()> {
        let snapshot = require_snapshot(snapshot)?;
        let tab = window
            .map(str::to_owned)
            .or_else(|| active_tab(snapshot, workspace))
            .context("Herdr workspace has no active tab")?;
        let panes = panes(snapshot, &tab);
        let current = panes
            .iter()
            .position(|pane| {
                pane.focused || snapshot.focused_pane_id.as_deref() == Some(&pane.pane_id)
            })
            .unwrap_or(0);
        let index = relative_index(current, panes.len(), delta)
            .context("cannot navigate an empty Herdr tab")?;
        self.call("pane.focus", json!({"pane_id": panes[index].pane_id}))?;
        Ok(())
    }

    fn move_tab(
        &self,
        snapshot: &HerdrSessionSnapshot,
        workspace: &str,
        window: &str,
        delta: i32,
    ) -> Result<()> {
        let tabs = tabs(snapshot, workspace);
        let current = tabs
            .iter()
            .position(|tab| tab.tab_id == window)
            .with_context(|| format!("unknown Herdr tab {window}"))?;
        let index = relative_index(current, tabs.len(), delta)
            .context("cannot move an empty Herdr tab list")?;
        self.call("tab.move", json!({"tab_id": window, "insert_index": index}))?;
        Ok(())
    }
}

impl<A: HerdrApi> MuxBackend for HerdrBackend<A> {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        HerdrBackend::snapshot(self)
    }
    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        HerdrBackend::execute(self, command)
    }
}

fn needs_snapshot(command: &MuxCommand) -> bool {
    !matches!(
        command,
        MuxCommand::ActivateWindow { .. }
            | MuxCommand::NewWindow { .. }
            | MuxCommand::RenameWindow { .. }
            | MuxCommand::CreateProjectSession { .. }
            | MuxCommand::CreateWorktreeSession { .. }
            | MuxCommand::RenameSession { .. }
            | MuxCommand::DitchSession { .. }
            | MuxCommand::StampSession { .. }
    )
}

fn require_snapshot(snapshot: Option<&HerdrSessionSnapshot>) -> Result<&HerdrSessionSnapshot> {
    snapshot.context("Herdr command requires a snapshot")
}
fn compact(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    value
}
fn split_direction(direction: MuxSplitDirection) -> &'static str {
    match direction {
        MuxSplitDirection::Right => "right",
        MuxSplitDirection::Down => "down",
    }
}
fn pane_direction(direction: MuxDirection) -> &'static str {
    match direction {
        MuxDirection::Left => "left",
        MuxDirection::Down => "down",
        MuxDirection::Up => "up",
        MuxDirection::Right => "right",
    }
}
fn relative_index(index: usize, len: usize, delta: i32) -> Option<usize> {
    (len != 0).then(|| (index as i64 + i64::from(delta)).rem_euclid(len as i64) as usize)
}

fn tabs<'a>(snapshot: &'a HerdrSessionSnapshot, workspace: &str) -> Vec<&'a HerdrTab> {
    let mut tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.workspace_id == workspace)
        .collect::<Vec<_>>();
    tabs.sort_by_key(|tab| tab.number);
    tabs
}
fn panes<'a>(snapshot: &'a HerdrSessionSnapshot, tab: &str) -> Vec<&'a HerdrPane> {
    let by_id = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab)
        .map(|pane| (pane.pane_id.as_str(), pane))
        .collect::<HashMap<_, _>>();
    snapshot
        .layouts
        .iter()
        .find(|layout| layout.tab_id == tab)
        .map_or_else(
            || by_id.values().copied().collect(),
            |layout| {
                layout
                    .panes
                    .iter()
                    .filter_map(|pane| by_id.get(pane.pane_id.as_str()).copied())
                    .collect()
            },
        )
}
fn active_tab(snapshot: &HerdrSessionSnapshot, workspace: &str) -> Option<String> {
    snapshot
        .workspaces
        .iter()
        .find(|item| item.workspace_id == workspace)
        .map(|item| item.active_tab_id.clone())
}
fn focused_pane(
    snapshot: &HerdrSessionSnapshot,
    workspace: &str,
    window: Option<&str>,
) -> Option<String> {
    let tab = window
        .map(str::to_owned)
        .or_else(|| active_tab(snapshot, workspace));
    snapshot
        .panes
        .iter()
        .find(|pane| {
            pane.workspace_id == workspace
                && tab.as_deref() == Some(&pane.tab_id)
                && (pane.focused || snapshot.focused_pane_id.as_deref() == Some(&pane.pane_id))
        })
        .or_else(|| {
            snapshot
                .panes
                .iter()
                .find(|pane| pane.workspace_id == workspace && tab.as_deref() == Some(&pane.tab_id))
        })
        .map(|pane| pane.pane_id.clone())
}
fn resolve_pane(
    snapshot: &HerdrSessionSnapshot,
    workspace: &str,
    window: Option<&str>,
    explicit: Option<&str>,
) -> Result<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| focused_pane(snapshot, workspace, window))
        .context("Herdr target has no pane")
}

pub fn project_snapshot(snapshot: &HerdrSessionSnapshot) -> Result<MuxSnapshot> {
    if snapshot.protocol < 20 {
        bail!(
            "Herdr protocol {} is unsupported; Bootty requires protocol 20",
            snapshot.protocol
        );
    }
    let sessions = snapshot
        .workspaces
        .iter()
        .map(|workspace| project_workspace(snapshot, workspace))
        .collect::<Result<Vec<_>>>()?;
    let active_session_id = snapshot.focused_workspace_id.clone().or_else(|| {
        snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.focused)
            .map(|workspace| workspace.workspace_id.clone())
    });
    Ok(MuxSnapshot {
        sessions,
        active_session_id,
        ..MuxSnapshot::default()
    })
}

fn project_workspace(
    snapshot: &HerdrSessionSnapshot,
    workspace: &HerdrWorkspace,
) -> Result<MuxSession> {
    let windows = tabs(snapshot, &workspace.workspace_id)
        .into_iter()
        .map(|tab| project_tab(snapshot, workspace, tab))
        .collect::<Result<Vec<_>>>()?;
    let anchor = windows
        .iter()
        .find(|window| window.id == workspace.active_tab_id)
        .or_else(|| windows.first())
        .map_or_else(
            || empty_anchor(&workspace.workspace_id),
            |window| window.anchor.clone(),
        );
    Ok(MuxSession {
        id: workspace.workspace_id.clone(),
        name: workspace.label.clone(),
        active: workspace.focused
            || snapshot.focused_workspace_id.as_deref() == Some(&workspace.workspace_id),
        anchor,
        active_window_id: Some(workspace.active_tab_id.clone()),
        windows,
        tag: MuxSessionTag {
            identity: workspace.tokens.get(BOOTTY_ID_TOKEN).cloned(),
            space: workspace.tokens.get(BOOTTY_SPACE_TOKEN).cloned(),
        },
    })
}
fn project_tab(
    snapshot: &HerdrSessionSnapshot,
    workspace: &HerdrWorkspace,
    tab: &HerdrTab,
) -> Result<MuxWindow> {
    let pane_rows = panes(snapshot, &tab.tab_id);
    let anchors = pane_rows
        .iter()
        .map(|pane| anchor(&workspace.workspace_id, pane))
        .collect::<Vec<_>>();
    let anchor = pane_rows
        .iter()
        .find(|pane| pane.focused || snapshot.focused_pane_id.as_deref() == Some(&pane.pane_id))
        .map(|pane| anchor(&workspace.workspace_id, pane))
        .or_else(|| anchors.first().cloned())
        .unwrap_or_else(|| empty_anchor(&workspace.workspace_id));
    let layout = snapshot
        .layouts
        .iter()
        .find(|layout| layout.tab_id == tab.tab_id)
        .map(project_layout)
        .transpose()?;
    let progress = match tab.agent_status.as_str() {
        "working" => Some(MuxWindowProgress {
            state: "indeterminate".into(),
            percent: None,
        }),
        "blocked" => Some(MuxWindowProgress {
            state: "paused".into(),
            percent: None,
        }),
        _ => None,
    };
    Ok(MuxWindow {
        id: tab.tab_id.clone(),
        index: tab.number,
        name: tab.label.clone(),
        active: tab.focused || workspace.active_tab_id == tab.tab_id,
        anchor,
        panes: anchors,
        layout,
        progress,
    })
}
fn anchor(session: &str, pane: &HerdrPane) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session.into(),
        pane_id: Some(pane.pane_id.clone()),
        pane_pid: None,
        cwd: pane.foreground_cwd.clone().or_else(|| pane.cwd.clone()),
        process: pane.display_agent.clone().or_else(|| pane.agent.clone()),
    }
}
fn empty_anchor(session: &str) -> MuxPaneAnchor {
    MuxPaneAnchor {
        session_id: session.into(),
        pane_id: None,
        pane_pid: None,
        cwd: None,
        process: None,
    }
}

fn project_layout(layout: &HerdrLayout) -> Result<MuxPaneLayout> {
    if layout.zoomed {
        return Ok(MuxPaneLayout::Pane(layout.focused_pane_id.clone()));
    }
    let splits = layout
        .splits
        .iter()
        .map(|split| (split_path(&split.id), split))
        .collect::<HashMap<_, _>>();
    layout_node(Vec::new(), &layout.panes, &splits)
}
fn layout_node(
    path: Vec<bool>,
    candidates: &[HerdrLayoutPane],
    splits: &HashMap<Vec<bool>, &HerdrLayoutSplit>,
) -> Result<MuxPaneLayout> {
    let Some(split) = splits.get(&path) else {
        let [pane] = candidates else {
            bail!("Herdr layout path {path:?} does not resolve to one pane")
        };
        return Ok(MuxPaneLayout::Pane(pane.pane_id.clone()));
    };
    let direction = match split.direction.as_str() {
        "right" => MuxPaneSplitDirection::Right,
        "down" => MuxPaneSplitDirection::Down,
        other => bail!("unknown Herdr split direction {other}"),
    };
    let boundary = if split.direction == "right" {
        f64::from(split.rect.x) + (f64::from(split.rect.width) * split.ratio).round()
    } else {
        f64::from(split.rect.y) + (f64::from(split.rect.height) * split.ratio).round()
    };
    let (first, second): (Vec<_>, Vec<_>) = candidates.iter().cloned().partition(|pane| {
        if split.direction == "right" {
            f64::from(pane.rect.x) + f64::from(pane.rect.width) / 2.0 < boundary
        } else {
            f64::from(pane.rect.y) + f64::from(pane.rect.height) / 2.0 < boundary
        }
    });
    let mut first_path = path.clone();
    first_path.push(false);
    let mut second_path = path;
    second_path.push(true);
    Ok(MuxPaneLayout::Split {
        direction,
        ratio_millis: (split.ratio.clamp(0.0, 1.0) * 1000.0).round() as u16,
        first: Box::new(layout_node(first_path, &first, splits)?),
        second: Box::new(layout_node(second_path, &second, splits)?),
    })
}
fn split_path(id: &str) -> Vec<bool> {
    match id.rsplit('_').next() {
        Some("root") | None => Vec::new(),
        Some(path) => path.chars().map(|digit| digit == '1').collect(),
    }
}

#[cfg(feature = "app")]
pub fn herdr_capabilities(scope: SpaceId) -> BindingCapabilityDescriptor {
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
            BindingOperation::CreateWorktreeSession,
            BindingOperation::RenameSession,
            BindingOperation::DitchSession,
            BindingOperation::StampSession,
        ],
    )
}
