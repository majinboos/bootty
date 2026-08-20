use std::collections::{HashMap, HashSet};

use crate::{
    layout::PaneLayout,
    mux::{
        RepaintHandle,
        provider::{PaneTopology, TerminalProgressPolicy},
        snapshot::{MuxSession, MuxWindow, MuxWindowProgress},
    },
};

use super::workspace_runtime::{BindingRuntime, ScopedPaneId, ScopedWindowId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalProgress {
    pub state: TerminalProgressState,
    pub value: Option<u8>,
}

impl TerminalProgress {
    pub(super) fn from_conemu(state: &str, value: Option<u8>) -> Option<Self> {
        let state = match state {
            "normal" => TerminalProgressState::Normal,
            "error" => TerminalProgressState::Error,
            "indeterminate" => TerminalProgressState::Indeterminate,
            "warning" => TerminalProgressState::Warning,
            "inactive" => return None,
            _ => return None,
        };
        Some(Self { state, value })
    }

    pub(super) fn from_mux(progress: &MuxWindowProgress) -> Option<Self> {
        Self::from_conemu(&progress.state, progress.percent)
    }

    pub(crate) fn fraction(self) -> Option<f32> {
        self.value
            .map(|value| f32::from(value) / 100.0)
            .or((self.state == TerminalProgressState::Indeterminate).then_some(0.5))
    }

    pub(super) fn percent(self) -> Option<u8> {
        self.value
            .or((self.state == TerminalProgressState::Indeterminate).then_some(50))
    }
}

#[derive(Default)]
pub(super) struct BindingTerminalFacts {
    custom_window_names: HashSet<ScopedWindowId>,
    window_titles: HashMap<ScopedWindowId, String>,
    pane_progress: HashMap<ScopedPaneId, TerminalProgress>,
    unscoped_progress: Option<TerminalProgress>,
    pane_ports: HashMap<ScopedPaneId, Vec<u16>>,
    unscoped_ports: Vec<u16>,
}

impl BindingRuntime {
    pub(super) fn current_window_id(&self) -> ScopedWindowId {
        let session = self.mux.selected_session().unwrap_or("local").to_owned();
        let window = self
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session || candidate.name == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            })
            .unwrap_or_default();
        self.window_id(session, window)
    }

    fn window_id_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.mux.sessions().iter().find_map(|session| {
            session.windows.iter().find_map(|window| {
                let anchor_matches = window.anchor.pane_id.as_deref() == Some(pane_id);
                let pane_matches = window
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id.as_deref() == Some(pane_id));
                (anchor_matches || pane_matches)
                    .then(|| self.window_id(session.id.clone(), window.id.clone()))
            })
        })
    }

    fn terminal_pane_id(&self, pane_id: &str) -> ScopedPaneId {
        let window = self
            .window_id_for_pane(pane_id)
            .unwrap_or_else(|| self.current_window_id());
        self.pane_id(window, pane_id)
    }

    fn mark_custom_window_name(&mut self, key: ScopedWindowId) {
        self.terminal_facts.custom_window_names.insert(key);
    }

    fn clear_custom_window_name(&mut self, key: &ScopedWindowId) -> Option<String> {
        self.terminal_facts.custom_window_names.remove(key);
        self.terminal_facts.window_titles.get(key).cloned()
    }

    pub(super) fn apply_window_title(
        &mut self,
        source_pane_id: Option<&str>,
        title: String,
        repaint: &RepaintHandle,
    ) {
        let key = source_pane_id
            .and_then(|pane_id| self.window_id_for_pane(pane_id))
            .or_else(|| source_pane_id.is_none().then(|| self.current_window_id()))
            .filter(|key| !key.window_id.is_empty());
        let Some(key) = key else {
            return;
        };
        self.terminal_facts.window_titles.insert(key.clone(), title);
        if !self.terminal_facts.custom_window_names.contains(&key) {
            let title = self.terminal_facts.window_titles[&key].clone();
            self.rename_window_if_changed(&key.session_id, &key.window_id, &title, repaint);
        }
    }

    pub(super) fn set_custom_window_name(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: &str,
        repaint: &RepaintHandle,
    ) {
        let key = self.window_id(session_id.to_owned(), window_id.to_owned());
        if name.is_empty() {
            if let Some(title) = self.clear_custom_window_name(&key) {
                self.rename_window_if_changed(session_id, window_id, &title, repaint);
            }
        } else {
            self.mark_custom_window_name(key);
            self.rename_window_if_changed(session_id, window_id, name, repaint);
        }
    }

    fn rename_window_if_changed(
        &mut self,
        session_id: &str,
        window_id: &str,
        name: &str,
        repaint: &RepaintHandle,
    ) {
        let current = self
            .mux
            .session_by_id_or_name(session_id)
            .and_then(|session| session.windows.iter().find(|window| window.id == window_id))
            .map(|window| window.name.as_str());
        if current == Some(name) {
            return;
        }
        let config = self.multiplexer.clone();
        self.mux
            .rename_window(session_id, window_id, name.to_owned(), repaint, &config);
    }

    pub(super) fn record_terminal_progress(
        &mut self,
        source_pane_id: Option<&str>,
        state: &str,
        value: Option<u8>,
    ) {
        // A tmux client reports progress for every window through its own bookkeeping. It forwards
        // OSC 9;4 only for the pane it currently shows. Recording that copy would credit it to the
        // attach pane and leave a stale bar on the wrong window.
        if state == "unknown"
            || self.backend_policy.progress == TerminalProgressPolicy::BackendSnapshot
        {
            return;
        }
        let progress = TerminalProgress::from_conemu(state, value);
        match source_pane_id.map(|pane_id| self.terminal_pane_id(pane_id)) {
            Some(pane) => match progress {
                Some(progress) => {
                    self.terminal_facts.pane_progress.insert(pane, progress);
                }
                None => {
                    self.terminal_facts.pane_progress.remove(&pane);
                }
            },
            None => self.terminal_facts.unscoped_progress = progress,
        }
    }

    pub(super) fn record_terminal_ports(&mut self, source_pane_id: Option<&str>, ports: Vec<u16>) {
        match source_pane_id.map(|pane_id| self.terminal_pane_id(pane_id)) {
            Some(pane) => {
                self.terminal_facts.pane_ports.insert(pane, ports);
            }
            None => self.terminal_facts.unscoped_ports = ports,
        }
    }

    pub(super) fn current_terminal_progress(&self) -> Option<TerminalProgress> {
        let selected = self.mux.selected_window();
        self.mux
            .selected_session_windows()
            .iter()
            .find(|window| match selected {
                Some(selected) => window.id == selected,
                None => window.active,
            })
            .and_then(Self::backend_window_progress)
            .or_else(|| {
                self.current_pane_layout()
                    .map(PaneLayout::focused)
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or_else(|| {
                self.mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.as_deref())
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or(self.terminal_facts.unscoped_progress)
    }

    pub(super) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.terminal_facts
            .pane_progress
            .get(&self.terminal_pane_id(pane_id))
            .copied()
    }

    fn pane_ports(&self, pane_id: &str) -> Option<&[u16]> {
        self.terminal_facts
            .pane_ports
            .get(&self.terminal_pane_id(pane_id))
            .map(Vec::as_slice)
    }

    pub(super) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        let selected = self.mux.selected_session();
        let mut ports =
            if selected == Some(session.id.as_str()) || selected == Some(session.name.as_str()) {
                self.terminal_facts.unscoped_ports.clone()
            } else {
                Vec::new()
            };
        for pane in session
            .windows
            .iter()
            .flat_map(|window| window.panes.iter().chain(std::iter::once(&window.anchor)))
            .filter_map(|pane| pane.pane_id.as_deref())
        {
            if let Some(reported) = self.pane_ports(pane) {
                for port in reported {
                    if !ports.contains(port) {
                        ports.push(*port);
                    }
                }
            }
        }
        ports
    }

    pub(super) fn has_indeterminate_terminal_progress(&self) -> bool {
        self.terminal_facts
            .pane_progress
            .values()
            .chain(self.terminal_facts.unscoped_progress.iter())
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
            || self.mux.sessions().iter().any(|session| {
                session
                    .windows
                    .iter()
                    .any(|window| self.window_has_indeterminate_progress(window))
            })
    }

    pub(super) fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        if let Some(progress) = Self::backend_window_progress(window) {
            return progress.state == TerminalProgressState::Indeterminate;
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
    }

    pub(super) fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        if let Some(progress) = Self::backend_window_progress(window) {
            return progress.percent();
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .filter_map(TerminalProgress::percent)
            .max()
    }

    fn backend_window_progress(window: &MuxWindow) -> Option<TerminalProgress> {
        window
            .progress
            .as_ref()
            .and_then(TerminalProgress::from_mux)
    }

    pub(super) fn current_pane_layout(&self) -> Option<&PaneLayout> {
        (self.backend_policy.panes.topology != PaneTopology::Attach)
            .then(|| self.pane_layouts.get(&self.current_window_id()))
            .flatten()
    }
}
