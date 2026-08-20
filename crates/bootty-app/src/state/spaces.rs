use std::time::Instant;

use bootty_mux::controller::SpaceId;
use bootty_workspace::SpaceMuxOverride;

use super::AppState;
use crate::input::focus::InputFocus;
use crate::workspace_runtime::SpaceSummary;
impl AppState {
    pub fn space_summaries(&self) -> Vec<SpaceSummary> {
        self.workspace.space_summaries()
    }
    pub fn space_transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        self.workspace.transition(now)
    }
    pub(super) fn select_space(&mut self, index: u32) -> bool {
        let Some(index) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
        else {
            return false;
        };
        self.space_summaries()
            .get(index)
            .is_some_and(|space| self.activate_space_from_ui(space.id))
    }
    pub(super) fn create_space_with_backend_from_ui(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let config = self.config().clone();
        let space_id = match self.workspace.create_space(
            name,
            icon,
            color,
            tint_sidebar,
            mux,
            &config,
            self.active_appearance_variant,
        ) {
            Ok(Some(space_id)) => space_id,
            Ok(None) => return false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        self.activate_space_from_ui(space_id)
    }
    pub fn close_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let spaces = self.space_summaries();
        if spaces.len() <= 1 {
            return false;
        }
        let Some(index) = spaces.iter().position(|space| space.id == space_id) else {
            return false;
        };
        if space_id == self.workspace.active.id {
            let neighbor = spaces
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|index| spaces.get(index)));
            if !neighbor.is_some_and(|space| self.activate_space_from_ui(space.id)) {
                return false;
            }
        }
        match self.workspace.delete_space(space_id) {
            Ok(true) => true,
            Ok(_) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }
    pub fn update_space_from_ui(
        &mut self,
        space_id: SpaceId,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let Some(mut summary) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
        else {
            return false;
        };
        summary.name = name.to_owned();
        summary.icon = icon.to_owned();
        summary.color = color;
        summary.tint_sidebar = tint_sidebar;
        let runtime_config = self.config().clone();
        match self.workspace.update_space(
            &summary,
            mux,
            &runtime_config,
            self.active_appearance_variant,
        ) {
            Ok(outcome) if outcome.changed => {
                if outcome.active_placement_changed {
                    let app_key_bindings = self
                        .config_runtime
                        .prepare_backend_keybindings(self.workspace.multiplexer_backend());
                    self.config_runtime
                        .publish_backend_keybindings(app_key_bindings);
                    self.terminal_surface = None;
                    self.last_pane_area = None;
                    if let Err(error) = self.sync_terminal_panes() {
                        self.last_error = Some(error.to_string());
                    }
                }
                true
            }
            Ok(_) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }
    pub(super) fn activate_relative_space(&mut self, delta: isize) -> bool {
        let spaces = self.space_summaries();
        let Some(active) = spaces.iter().position(|space| space.active) else {
            return false;
        };
        let Some(target) = active
            .checked_add_signed(delta)
            .and_then(|index| spaces.get(index))
        else {
            return false;
        };
        self.activate_space_from_ui(target.id)
    }
    pub fn activate_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        if space_id == self.workspace.active.id {
            return false;
        }
        let Some(backend) = self.workspace.space_backend(space_id) else {
            return false;
        };
        let app_key_bindings = self.config_runtime.prepare_backend_keybindings(backend);
        let switch_started = crate::diagnostics::latency_start();
        let config = self.config().clone();
        if let Err(error) = self.workspace.activate_space(
            space_id,
            &self.window_state_key,
            &config,
            self.active_appearance_variant,
            &self.repaint,
            Instant::now(),
        ) {
            self.last_error = Some(error.to_string());
            return false;
        }
        self.config_runtime
            .publish_backend_keybindings(app_key_bindings);
        self.terminal_surface = None;
        self.last_pane_area = None;
        self.clear_space_context_dialogs();
        self.input_focus = InputFocus::Terminal;
        let phase = crate::diagnostics::latency_start();
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
        crate::diagnostics::trace_phase("space.sync_terminal_panes", phase);
        crate::diagnostics::trace_phase("space.TOTAL", switch_started);
        (self.repaint)();
        true
    }
    fn clear_space_context_dialogs(&mut self) {
        self.dialogs.clear_space_context();
        self.sidebar_hovered_session = None;
    }
    pub fn reconnect_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        self.workspace.reconnect_space(space_id, Instant::now())
    }
}
