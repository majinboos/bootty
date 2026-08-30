use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrSessionSnapshot {
    pub version: String,
    pub protocol: u32,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_tab_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<HerdrWorkspace>,
    #[serde(default)]
    pub tabs: Vec<HerdrTab>,
    #[serde(default)]
    pub panes: Vec<HerdrPane>,
    #[serde(default)]
    pub layouts: Vec<HerdrLayout>,
    #[serde(default)]
    pub agents: Vec<serde_json::Value>,
}

impl HerdrSessionSnapshot {
    pub fn terminal_id_for_pane(&self, pane_id: &str) -> Option<&str> {
        self.panes
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .map(|pane| pane.terminal_id.as_str())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrWorkspace {
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub active_tab_id: String,
    pub agent_status: String,
    #[serde(default)]
    pub tokens: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrTab {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub agent_status: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrPane {
    pub pane_id: String,
    pub terminal_id: String,
    pub workspace_id: String,
    pub tab_id: String,
    pub focused: bool,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub foreground_cwd: Option<String>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub display_agent: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub agent_status: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrLayout {
    pub workspace_id: String,
    pub tab_id: String,
    pub zoomed: bool,
    pub focused_pane_id: String,
    pub panes: Vec<HerdrLayoutPane>,
    pub splits: Vec<HerdrLayoutSplit>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrLayoutPane {
    pub pane_id: String,
    pub focused: bool,
    pub rect: HerdrRect,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct HerdrLayoutSplit {
    pub id: String,
    pub direction: String,
    pub ratio: f64,
    pub rect: HerdrRect,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HerdrRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}
