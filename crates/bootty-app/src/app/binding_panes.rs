use anyhow::Result;
use eframe::egui::Rect;

use super::workspace_runtime::{BindingRuntime, ScopedWindowId};
use crate::{
    layout::{Direction, Divider, PaneLayout, SplitDirection},
    mux::{
        RepaintHandle,
        command::{MuxCommand, MuxDirection, MuxSplitDirection},
        provider::{PaneTopology, selected_backend},
        snapshot::MuxPaneAnchor,
        terminal::TerminalRuntime,
    },
};

pub(super) fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
    match direction {
        SplitDirection::Right => MuxSplitDirection::Right,
        SplitDirection::Down => MuxSplitDirection::Down,
    }
}

fn layout_direction(direction: MuxDirection) -> Direction {
    match direction {
        MuxDirection::Left => Direction::Left,
        MuxDirection::Right => Direction::Right,
        MuxDirection::Up => Direction::Up,
        MuxDirection::Down => Direction::Down,
    }
}

fn pane_sets_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|pane| b.contains(pane))
}

fn focus_after_reconcile(
    restored_from_server: bool,
    new_panes: &[String],
    selected_pane: Option<&str>,
) -> Option<String> {
    if restored_from_server {
        return selected_pane.map(str::to_owned);
    }
    if let Some(selected_pane) = selected_pane
        && new_panes.iter().any(|pane| pane == selected_pane)
    {
        return Some(selected_pane.to_owned());
    }
    new_panes.first().cloned()
}

impl BindingRuntime {
    pub(super) fn uses_native_terminal_layout(&self) -> bool {
        self.backend_policy.panes.topology != PaneTopology::Attach
    }

    pub(super) fn pane_widget_key(&self, pane_id: &str) -> String {
        let window = self.current_window_id();
        let backend = selected_backend(&self.multiplexer);
        format!(
            "{}:{}:{backend:?}:{}:{}:{pane_id}",
            window.scope.space_id().persistence_value(),
            window.scope.binding_id().persistence_value(),
            window.session_id,
            window.window_id,
        )
    }

    fn take_pending_split_direction(&mut self, key: &ScopedWindowId) -> Option<SplitDirection> {
        self.pending_pane_split_directions.remove(key).or_else(|| {
            if key.window_id.is_empty() {
                None
            } else {
                let fallback = self.window_id(key.session_id.clone(), String::new());
                self.pending_pane_split_directions.remove(&fallback)
            }
        })
    }

    fn prune_pane_layouts(&mut self) {
        if self.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.mux.sessions() {
            for window in &session.windows {
                live.push(self.window_id(session.id.clone(), window.id.clone()));
                live.push(self.window_id(session.name.clone(), window.id.clone()));
            }
        }
        live.push(self.current_window_id());
        self.pane_layouts.retain(|key, _| live.contains(key));
    }

    pub(super) fn sync_terminal_panes(&mut self) -> Result<()> {
        if self.mux.unavailable_reason().is_some() {
            return Ok(());
        }
        let phase = crate::diagnostics::latency_start();
        self.prune_pane_layouts();
        crate::diagnostics::trace_slow("panes.prune_pane_layouts", phase, 2.0);
        let phase = crate::diagnostics::latency_start();
        let config = self.multiplexer.clone();
        crate::diagnostics::trace_slow("panes.clone_config", phase, 2.0);
        if !self.uses_native_terminal_layout() {
            let phase = crate::diagnostics::latency_start();
            let result = self.terminal.sync_scoped_mux_anchor(
                self.scope,
                &config,
                self.mux.selected_session_anchor(),
            );
            crate::diagnostics::trace_slow("panes.sync_scoped_mux_anchor", phase, 2.0);
            return result;
        }
        let panes: Vec<MuxPaneAnchor> = self.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            return self.terminal.sync_scoped_mux_anchor(
                self.scope,
                &config,
                self.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_id();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self
            .mux
            .selected_window_layout()
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), &pane_ids));
        let layout_missing = !self.pane_layouts.contains_key(&key);
        let stale_layout = self
            .pane_layouts
            .get(&key)
            .is_some_and(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server = false;
        if (layout_missing || stale_layout)
            && let Some(layout) = server_layout.clone()
        {
            self.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        }

        let previous_panes = self
            .pane_layouts
            .get(&key)
            .map(PaneLayout::panes)
            .unwrap_or_default();
        let new_panes = pane_ids
            .iter()
            .filter(|pane| !previous_panes.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let has_new_pane = !new_panes.is_empty();
        {
            let layout = self
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(pane_ids[0].clone()));
            if layout.panes().iter().all(|pane| !pane_ids.contains(pane)) {
                *layout = PaneLayout::single(pane_ids[0].clone());
            }
        }
        let removed_panes = previous_panes
            .iter()
            .filter(|pane| !pane_ids.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let pane_set_changed = has_new_pane || !removed_panes.is_empty();
        if pane_set_changed && let Some(layout) = server_layout {
            self.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        } else if pane_set_changed {
            let direction = self
                .take_pending_split_direction(&key)
                .unwrap_or(SplitDirection::Right);
            self.pane_layouts
                .get_mut(&key)
                .expect("native layout should be initialized")
                .reconcile_with_new_pane_direction(&pane_ids, direction);
        }
        let layout = self
            .pane_layouts
            .get_mut(&key)
            .expect("native layout should be initialized");
        if let Some(focus) =
            focus_after_reconcile(restored_from_server, &new_panes, selected_pane.as_deref())
        {
            layout.set_focus(&focus);
        }
        let focused_id = layout.focused().to_owned();
        let focused_anchor = panes
            .iter()
            .find(|pane| pane.pane_id.as_deref() == Some(focused_id.as_str()))
            .cloned();
        self.terminal.sync_scoped_native_window(
            self.scope,
            &panes,
            focused_anchor.as_ref(),
            window_id.as_deref(),
            selected_backend(&config),
            config.hide_tmux_status,
        )
    }

    pub(super) fn native_multi_pane(&self) -> bool {
        self.current_pane_layout()
            .is_some_and(|layout| !layout.is_single())
    }

    pub(super) fn focused_pane(&self) -> Option<String> {
        self.current_pane_layout()
            .map(|layout| layout.focused().to_owned())
    }

    pub(super) fn pane_rects(&self, area: Rect, gap: f32) -> Vec<(String, Rect)> {
        self.current_pane_layout()
            .map(|layout| layout.rects(area, gap))
            .unwrap_or_default()
    }

    pub(super) fn pane_dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        self.current_pane_layout()
            .map(|layout| layout.dividers(area, gap))
            .unwrap_or_default()
    }

    pub(super) fn focus_pane(&mut self, pane_id: &str) {
        let key = self.current_window_id();
        let moved = match self.pane_layouts.get_mut(&key) {
            Some(layout) if layout.focused() != pane_id => layout.set_focus(pane_id),
            _ => false,
        };
        if moved {
            let _ = self.sync_terminal_panes();
        }
    }

    pub(super) fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        let key = self.current_window_id();
        if let Some(layout) = self.pane_layouts.get_mut(&key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub(super) fn terminal_runtime_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.terminal.terminal_runtime_for_pane(pane_id)
    }

    pub(super) fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    pub(super) fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.terminal.resize_native_layout_window(cols, rows)
    }

    pub(super) fn split_focused_pane(
        &mut self,
        repaint: &RepaintHandle,
        direction: SplitDirection,
        target_pane_id: Option<&str>,
    ) {
        let session = self.mux.selected_session().unwrap_or("local").to_owned();
        let config = self.multiplexer.clone();
        if !self.uses_native_terminal_layout() {
            self.mux.execute_command(
                repaint,
                &config,
                MuxCommand::SplitPane {
                    session_id: session,
                    pane_id: target_pane_id.map(str::to_owned),
                    direction: mux_split_direction(direction),
                },
            );
            return;
        }
        let key = self.current_window_id();
        let focused = target_pane_id.map(str::to_owned).or_else(|| {
            self.pane_layouts
                .get(&key)
                .map(|layout| layout.focused().to_owned())
                .or_else(|| {
                    self.mux
                        .selected_session_anchor()
                        .and_then(|anchor| anchor.pane_id.clone())
                })
        });
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::SplitPane {
                session_id: session,
                pane_id: focused.clone(),
                direction: mux_split_direction(direction),
            },
        );
        self.apply_split_layout_after_command(key, focused, direction);
    }

    fn apply_split_layout_after_command(
        &mut self,
        key: ScopedWindowId,
        focused: Option<String>,
        direction: SplitDirection,
    ) {
        match self.backend_policy.panes.topology {
            PaneTopology::BackendReconciled => {
                self.pending_pane_split_directions.insert(key, direction);
                return;
            }
            PaneTopology::ProcessLocal => {}
            PaneTopology::Attach => {
                unreachable!("attach topology cannot publish a process-local split")
            }
        }
        let new_pane = self
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        if let Some(new_pane) = new_pane {
            let layout = self
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(new_pane.clone()));
            if let Some(focused) = &focused {
                layout.set_focus(focused);
            }
            if !layout.contains(&new_pane) {
                layout.split_focused(new_pane, direction);
            }
            self.pending_pane_split_directions.remove(&key);
            let _ = self.sync_terminal_panes();
        }
    }

    pub(super) fn focus_pane_neighbor(&mut self, direction: MuxDirection, area: Rect, gap: f32) {
        let key = self.current_window_id();
        let neighbor = self.pane_layouts.get(&key).and_then(|layout| {
            layout.neighbor(layout.focused(), layout_direction(direction), area, gap)
        });
        if let Some(neighbor) = neighbor {
            self.focus_pane(&neighbor);
        }
    }

    pub(super) fn focus_pane_relative(&mut self, delta: isize) {
        let key = self.current_window_id();
        let Some(layout) = self.pane_layouts.get(&key) else {
            return;
        };
        let panes = layout.panes();
        if panes.len() < 2 {
            return;
        }
        let Some(index) = panes.iter().position(|pane| pane == layout.focused()) else {
            return;
        };
        let pane =
            panes[(index as isize + delta).rem_euclid(panes.len() as isize) as usize].clone();
        self.focus_pane(&pane);
    }

    pub(super) fn remove_pane_from_layout(
        &mut self,
        window: &ScopedWindowId,
        pane_id: &str,
        sync_current_window: bool,
    ) {
        if let Some(layout) = self.pane_layouts.get_mut(window) {
            layout.remove(pane_id);
        }
        if sync_current_window {
            let _ = self.sync_terminal_panes();
        }
    }

    pub(super) fn close_focused_pane(&mut self, repaint: &RepaintHandle, pane_id: &str) {
        let session_id = self.mux.selected_session().unwrap_or("local").to_owned();
        let config = self.multiplexer.clone();
        self.mux.execute_command(
            repaint,
            &config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.terminal.discard_pane(pane_id);
        let window = self.current_window_id();
        self.remove_pane_from_layout(&window, pane_id, true);
    }
}
