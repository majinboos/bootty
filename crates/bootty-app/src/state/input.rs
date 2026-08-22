use bootty_terminal::terminal_input_model::MouseButton;
use bootty_winit::direct_input::{DirectKeyInput, ModifierSideState};
use eframe::egui::{self, Pos2};
use std::path::PathBuf;

use super::recorded_chord::normalize_recorded_chord;
use super::{AppEffect, AppState, ViewportSnapshot};
use crate::app_actions::{SidebarAction, builtin_app_invocation_for_direct_key};
use crate::input::{
    InputSnapshot, TerminalInputCommand,
    focus::InputFocus,
    router::{RoutedInput, route_events},
};
use crate::terminal_interaction::TerminalInteractionInput;
use crate::ui::command_palette::CommandPaletteDialog;
use crate::ui::session_navigation::ScopedSessionTarget;
use crate::ui::terminal_find::TerminalFindDialog;
#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalFileHandoff {
    Ready(String),
    Rejected(&'static str),
}

impl AppState {
    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }
    pub fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }
    pub fn sidebar_hovered_session(&self) -> Option<&ScopedSessionTarget> {
        self.sidebar_hovered_session.as_ref()
    }
    pub fn direct_input_suppresses_egui_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    /// Own the settings overlay's open/closed state so the direct input path stops feeding the
    /// terminal behind it (otherwise shortcuts like ⌘V paste into the hidden terminal).
    pub(crate) fn set_settings_open(&mut self, open: bool) {
        self.settings_open = open;
    }
    pub(crate) fn settings_open(&self) -> bool {
        self.settings_open
    }

    /// Mirror whether a Luau floating window is showing so the direct input path stops feeding the
    /// terminal behind it, matching how the native overlays gate input.
    pub fn set_extension_overlay_open(&mut self, open: bool) {
        self.extension_overlay_open = open;
    }
    pub fn drain_direct_input(&mut self) {
        if let Some(rx) = &self.modifier_side_rx
            && let Some(latest) = rx.try_iter().last()
        {
            self.modifier_sides = latest;
        }
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }
    pub(super) fn effective_terminal_cursor_icon(&self) -> egui::CursorIcon {
        if self.mouse_pointer_hidden_while_typing {
            egui::CursorIcon::None
        } else {
            self.terminal_cursor_icon
        }
    }
    pub(super) fn set_mouse_pointer_hidden_while_typing(
        &mut self,
        hidden: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let hidden = hidden && self.config().input.hide_mouse_pointer_while_typing;
        if self.mouse_pointer_hidden_while_typing == hidden {
            return;
        }
        self.mouse_pointer_hidden_while_typing = hidden;
        effects.push(AppEffect::SetTerminalCursorIcon(
            self.effective_terminal_cursor_icon(),
        ));
    }
    pub(super) fn hide_mouse_pointer_for_terminal_typing(&mut self, effects: &mut Vec<AppEffect>) {
        self.set_mouse_pointer_hidden_while_typing(true, effects);
    }
    pub(super) fn restore_mouse_pointer_after_pointer_moved(
        &mut self,
        events: &[egui::Event],
        hover_pos: Option<Pos2>,
        effects: &mut Vec<AppEffect>,
    ) {
        let moved_by_event = events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerMoved(_)));
        let moved_by_hover_pos = hover_pos.is_some() && hover_pos != self.last_mouse_hover_pos;
        self.last_mouse_hover_pos = hover_pos;

        if moved_by_event || moved_by_hover_pos {
            self.set_mouse_pointer_hidden_while_typing(false, effects);
        }
    }
    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    /// The modifier keys held right now, with their left/right sides, as tracked by the direct
    /// winit input path. The settings recorder needs this for wheel steps, which arrive as egui
    /// events with side-less modifiers.
    pub fn modifier_sides(&self) -> ModifierSideState {
        self.modifier_sides
    }

    /// Drain the pending direct-input chords as binding-trigger strings for the settings keybind
    /// recorder. This is how the recorder captures cmd-modified chords like ⌘V and ⌘⌥X: egui
    /// collapses those into copy/cut/paste events with no key event, but bootty's direct winit path
    /// keeps the full key + modifiers. Only meaningful while settings is open (the terminal is not
    /// consuming this input).
    pub fn take_settings_capture_chords(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_direct_input)
            .into_iter()
            .map(|direct| {
                let chord =
                    bootty_winit::input_binding::BindingTrigger::from_key_input_with_modifier_sides(
                        direct.input(),
                    )
                    .format_entry();
                normalize_recorded_chord(chord)
            })
            .collect()
    }
    pub(super) fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && !self.dialogs.has_modal()
            && !self.extension_overlay_open
            && !self.settings_open
    }
    pub(super) fn handle_egui_input(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        hover_pos: Option<Pos2>,
        pressed_mouse_button: Option<MouseButton>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let copy_on_select = self.config().input.copy_on_select;
        let surface = self.terminal_surface;
        let view = self.terminal_view_transform;
        let input_focus = self.input_focus;
        let outcome = self.terminal_interaction.handle_egui_input(
            &mut self.workspace.active.binding.terminal,
            TerminalInteractionInput {
                events,
                modifiers,
                pressed_mouse_button,
                input_focus,
                terminal_input_enabled,
                surface,
                view,
                chrome_handle_rects: &self.chrome_handle_rects,
                copy_on_select,
            },
        );
        let count = outcome.handled_count;
        effects.extend(outcome.effects);
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);

        let mut events = outcome.events;
        // `cmd+shift+,` over a palette row jumps to that command's keybinding editor.
        // Consume it here so it does not also fire its own global binding.
        if self.take_configure_keybind_chord(&mut events) {
            let action = self
                .dialogs
                .command_palette()
                .and_then(CommandPaletteDialog::current_action)
                .map(str::to_owned);
            self.close_overlay_dialogs();
            self.input_focus = InputFocus::Terminal;
            if let Some(action) = action {
                effects.push(AppEffect::ConfigureKeybind(action));
            }
        }
        let (events, actions) = self.split_app_actions(events);
        let routed = if let Some(find_rect) = self
            .terminal_interaction
            .find_dialog()
            .and_then(TerminalFindDialog::last_rect)
        {
            route_find_modeless_events(self.input_focus, events, Some(find_rect), hover_pos)
        } else {
            route_events(self.input_focus, events)
        };
        let sidebar_count = self.handle_sidebar_input(routed.ui_events, viewport, effects);
        let terminal_events =
            if terminal_input_enabled || self.terminal_interaction.find_dialog().is_some() {
                routed.terminal_events
            } else {
                Vec::new()
            };
        let snapshot = InputSnapshot {
            events: terminal_events,
            modifiers,
            modifier_sides: self.modifier_sides,
            hover_pos,
            pressed_mouse_button,
            surface: self.terminal_surface,
            mouse_exclusion: self
                .terminal_surface
                .map(crate::renderer::scrollbar_hit_rect),
            view: self.terminal_view_transform,
        };
        let commands = self
            .config_runtime
            .terminal_input_commands(snapshot, &mut self.wheel_scroll_state);
        let count = count + commands.len() + actions.len() + sidebar_count;
        for invocation in actions {
            let _ = self.dispatch_command(invocation, viewport, effects);
        }
        for command in commands {
            self.apply_terminal_input(command, effects);
        }
        count
    }
    pub(super) fn handle_dropped_file_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        if !self.direct_terminal_input_enabled() {
            return 0;
        }
        if paths.is_empty() {
            return 0;
        }
        if self.workspace.active.binding.multiplexer.remote.is_some() {
            self.last_error = Some("File handoff to remote Spaces is not supported.".to_owned());
            return 0;
        }
        let text = match local_file_handoff(&paths) {
            LocalFileHandoff::Ready(text) => text,
            LocalFileHandoff::Rejected(message) => {
                self.last_error = Some(message.to_owned());
                return 0;
            }
        };
        if let Err(error) = self.workspace.active.binding.terminal.write_paste(&text) {
            self.last_error = Some(error.to_string());
            return 0;
        }
        1
    }
    pub(super) fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        // While settings is open, leave the pending direct input untouched so the keybind recorder
        // can read it in the UI pass; the terminal behind settings must not consume it.
        if self.settings_open {
            return self.pending_direct_input.len();
        }
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if count == 0 {
            return 0;
        }
        if !self.direct_terminal_input_enabled() {
            return count;
        }

        let mut copy_mode_active = match self
            .terminal_interaction
            .copy_mode_active(&mut self.workspace.active.binding.terminal)
        {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        };
        for input in inputs {
            let mut input = input.input();
            input.mods = self.config_runtime.remap_mods(input.mods);
            let interaction = self.terminal_interaction.handle_direct_input(
                &mut self.workspace.active.binding.terminal,
                input,
                copy_mode_active,
            );
            copy_mode_active = interaction.copy_mode_active;
            effects.extend(interaction.effects);
            self.apply_terminal_outcome(interaction.last_error, interaction.focus_intent);
            if interaction.consumed {
                continue;
            }
            if let Some(invocation) = self
                .config_runtime
                .invocation_for_input(input)
                .or_else(|| builtin_app_invocation_for_direct_key(input))
            {
                if invocation.command == "paste_from_clipboard" {
                    self.terminal_interaction.mark_paste_suppression();
                }
                let _ = self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if copy_mode_active || input.mods.command {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input), effects);
        }
        count
    }
    fn handle_sidebar_input(
        &mut self,
        events: Vec<egui::Event>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if self.input_focus != InputFocus::Sidebar {
            return 0;
        }
        self.ensure_sidebar_hovered_session();
        let count = events.len();
        for event in events {
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(invocation) = self.config_runtime.sidebar_invocation(key, modifiers) else {
                continue;
            };
            self.dispatch_command(invocation, viewport, effects);
        }
        count
    }
    pub(crate) fn apply_sidebar_action(&mut self, action: SidebarAction) -> bool {
        match action {
            SidebarAction::Ignore => {}
            SidebarAction::PreviousSession => self.move_sidebar_hover(-1),
            SidebarAction::NextSession => self.move_sidebar_hover(1),
            SidebarAction::ActivateSession => return self.activate_sidebar_hovered_session(),
            SidebarAction::FocusTerminal => self.input_focus = InputFocus::Terminal,
        }
        true
    }
    fn ensure_sidebar_hovered_session(&mut self) {
        let targets = self.session_navigation_targets();
        if self
            .sidebar_hovered_session
            .as_ref()
            .is_some_and(|hovered| targets.contains(hovered))
        {
            return;
        }
        self.sidebar_hovered_session = self
            .workspace
            .active
            .binding
            .mux
            .selected_session()
            .and_then(|selected| self.session_target_matching(selected))
            .or_else(|| targets.into_iter().next());
    }
    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let targets = self.session_navigation_targets();
        let Some(current) = self
            .sidebar_hovered_session
            .as_ref()
            .and_then(|hovered| targets.iter().position(|target| target == hovered))
        else {
            return;
        };
        let next = (current as isize + delta).rem_euclid(targets.len() as isize) as usize;
        self.sidebar_hovered_session = targets.get(next).cloned();
    }
    fn activate_sidebar_hovered_session(&mut self) -> bool {
        self.ensure_sidebar_hovered_session();
        let activated = self
            .sidebar_hovered_session
            .clone()
            .is_some_and(|target| self.activate_scoped_session_from_ui(&target));
        self.input_focus = InputFocus::Terminal;
        activated
    }
    pub(super) fn session_navigation_targets(&self) -> Vec<ScopedSessionTarget> {
        self.binding_session_groups()
            .into_iter()
            .flat_map(|group| {
                group
                    .sessions
                    .into_iter()
                    .map(move |session| ScopedSessionTarget::new(group.scope, session.id))
            })
            .collect()
    }
    pub(super) fn session_target_matching(&self, value: &str) -> Option<ScopedSessionTarget> {
        self.workspace
            .active
            .binding
            .mux
            .session_by_id_or_name(value)
            .map(|session| {
                ScopedSessionTarget::new(self.workspace.active.binding.scope, session.id.clone())
            })
    }
    pub(crate) fn apply_terminal_input(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) {
        let terminal = &mut self.workspace.active.binding.terminal;
        let (result, hides_pointer) = match command {
            TerminalInputCommand::Text(text) => (terminal.write_input(text.as_bytes()), true),
            TerminalInputCommand::Paste(text) => (terminal.write_paste(&text), false),
            TerminalInputCommand::Focus(focused) => (terminal.encode_focus(focused), false),
            TerminalInputCommand::Key(input) => (terminal.encode_key(input), true),
            TerminalInputCommand::Mouse(input) => (terminal.encode_mouse(input), false),
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => (terminal.handle_mouse_wheel(input, scroll_delta), false),
        };
        if let Err(error) = result {
            self.last_error = Some(error.to_string());
        } else if hides_pointer {
            self.hide_mouse_pointer_for_terminal_typing(effects);
        }
    }
}

fn route_find_modeless_events(
    focus: InputFocus,
    events: Vec<egui::Event>,
    find_rect: Option<egui::Rect>,
    hover_pos: Option<Pos2>,
) -> RoutedInput {
    let Some(find_rect) = find_rect else {
        return route_events(focus, events);
    };

    let mut routed = RoutedInput::default();
    for event in events {
        let inside_find = event_pointer_pos(&event)
            .or(hover_pos.filter(|_| matches!(event, egui::Event::MouseWheel { .. })))
            .is_some_and(|pos| find_rect.contains(pos));
        if inside_find {
            routed.ui_events.push(event);
        } else if focus.terminal_owns_input() || event_is_terminal_pointer(&event) {
            routed.terminal_events.push(event);
        } else {
            routed.ui_events.push(event);
        }
    }
    routed
}

fn event_pointer_pos(event: &egui::Event) -> Option<Pos2> {
    match event {
        egui::Event::PointerMoved(pos) => Some(*pos),
        egui::Event::PointerButton { pos, .. } => Some(*pos),
        _ => None,
    }
}

fn event_is_terminal_pointer(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::MouseWheel { .. }
    )
}

fn local_file_handoff(paths: &[PathBuf]) -> LocalFileHandoff {
    if paths.iter().any(|path| !path.exists()) {
        return LocalFileHandoff::Rejected("file handoff rejected: local path is unavailable");
    }
    bootty_winit::file_paths::format_file_paths_for_paste(paths.iter().map(PathBuf::as_path))
        .map(LocalFileHandoff::Ready)
        .unwrap_or(LocalFileHandoff::Rejected(
            "file handoff rejected: unsupported local path",
        ))
}
