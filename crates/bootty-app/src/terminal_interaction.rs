mod copy_mode;
mod selection;

use anyhow::Result;
use bootty_mux::terminal::{ActiveTerminal, TerminalRuntime};
use bootty_render::geometry::{TerminalSurface, ViewTransform};
use bootty_winit::input_binding::CopyToClipboard;
use eframe::egui::{self, Rect};

use super::AppEffect;
use crate::{
    input::focus::InputFocus,
    platform::{write_clipboard_html, write_clipboard_text},
    ui::terminal_find::{TerminalFindDialog, TerminalFindEvent, TerminalFindResult},
};
use bootty_terminal::{
    terminal_engine::{TerminalCopyModeAction, TerminalSearchDirection, TerminalSelectionFormat},
    terminal_input_model::{KeyInput, MouseButton},
};

use copy_mode::{
    CopyModeKeyAction, copy_mode_action_for_egui_event, copy_mode_action_for_input,
    copy_mode_egui_key_may_emit_text, copy_mode_egui_key_should_pass_to_app,
    copy_mode_input_should_pass_to_app, copy_mode_key_input_present, copy_shortcut_pressed,
    direct_copy_shortcut_pressed,
};
use selection::{TerminalSelectionAction, TerminalSelectionRouteContext, TerminalSelectionRouter};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum TerminalFocusIntent {
    #[default]
    None,
    Terminal,
    Find,
}

#[derive(Debug, Default)]
pub(super) struct TerminalInteractionOutcome {
    pub(super) events: Vec<egui::Event>,
    pub(super) effects: Vec<AppEffect>,
    pub(super) last_error: Option<String>,
    pub(super) focus_intent: TerminalFocusIntent,
    pub(super) handled_count: usize,
}

pub(super) struct TerminalInteractionInput<'a> {
    pub(super) events: Vec<egui::Event>,
    pub(super) modifiers: egui::Modifiers,
    pub(super) pressed_mouse_button: Option<MouseButton>,
    pub(super) input_focus: InputFocus,
    pub(super) terminal_input_enabled: bool,
    pub(super) surface: Option<TerminalSurface>,
    pub(super) view: ViewTransform,
    pub(super) chrome_handle_rects: &'a [Rect],
    pub(super) copy_on_select: bool,
}

#[derive(Debug, Default)]
pub(super) struct TerminalDirectInputOutcome {
    pub(super) consumed: bool,
    pub(super) copy_mode_active: bool,
    pub(super) effects: Vec<AppEffect>,
    pub(super) last_error: Option<String>,
    pub(super) focus_intent: TerminalFocusIntent,
}

pub(super) struct TerminalInteractionRuntime {
    selection: TerminalSelectionRouter,
    find_dialog: Option<TerminalFindDialog>,
    find_return_focus: bool,
    last_search: String,
    last_search_direction: TerminalSearchDirection,
    suppress_next_egui_paste: bool,
    last_error: Option<String>,
    pending_focus_intent: TerminalFocusIntent,
}

impl Default for TerminalInteractionRuntime {
    fn default() -> Self {
        Self {
            selection: TerminalSelectionRouter::default(),
            find_dialog: None,
            find_return_focus: false,
            last_search: String::new(),
            last_search_direction: TerminalSearchDirection::Next,
            suppress_next_egui_paste: false,
            last_error: None,
            pending_focus_intent: TerminalFocusIntent::None,
        }
    }
}

impl TerminalInteractionRuntime {
    pub(super) fn take_find_dialog(&mut self) -> Option<TerminalFindDialog> {
        self.find_dialog.take()
    }

    pub(super) fn find_dialog(&self) -> Option<&TerminalFindDialog> {
        self.find_dialog.as_ref()
    }

    pub(super) fn dismiss_find(&mut self) {
        self.find_dialog = None;
        self.find_return_focus = false;
    }

    pub(super) fn close_overlay_dialogs(
        &mut self,
        terminal: &mut ActiveTerminal,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        if self.find_dialog.is_some() {
            self.clear_find_search(terminal);
        }
        self.finish(TerminalInteractionOutcome::default())
    }

    pub(super) fn find_action_opens_dialog(
        &self,
        action: &crate::app_actions::TerminalFindAction,
    ) -> bool {
        matches!(action, crate::app_actions::TerminalFindAction::Prompt)
            || self.last_search.is_empty()
                && matches!(
                    action,
                    crate::app_actions::TerminalFindAction::Previous
                        | crate::app_actions::TerminalFindAction::Next
                )
    }

    pub(super) fn apply_find_event(
        &mut self,
        terminal: &mut ActiveTerminal,
        mut dialog: TerminalFindDialog,
        event: TerminalFindEvent,
        focused_pane_id: Option<&str>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match event {
            TerminalFindEvent::None => self.find_dialog = Some(dialog),
            TerminalFindEvent::Close => {
                self.clear_find_search(terminal);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            TerminalFindEvent::FocusFind => {
                self.find_dialog = Some(dialog);
                outcome.focus_intent = TerminalFocusIntent::Find;
            }
            TerminalFindEvent::FocusTerminal => {
                self.find_dialog = Some(dialog);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            TerminalFindEvent::Search { query, direction } => {
                let result = self.search_terminal(terminal, focused_pane_id, &query, direction);
                dialog.set_result(result);
                if direction != TerminalSearchDirection::Current && self.find_return_focus {
                    outcome.focus_intent = TerminalFocusIntent::Terminal;
                }
                self.find_dialog = Some(dialog);
            }
        }
        self.finish(outcome)
    }

    fn open_find(
        &mut self,
        terminal: &mut ActiveTerminal,
        direction: TerminalSearchDirection,
        focused_pane_id: Option<&str>,
    ) -> TerminalFocusIntent {
        let query = self.last_search.clone();
        self.dismiss_find();
        let mut dialog = TerminalFindDialog::open_with_direction(query.clone(), direction);
        if !query.trim().is_empty() {
            let result = self.search_terminal(
                terminal,
                focused_pane_id,
                &query,
                TerminalSearchDirection::Current,
            );
            dialog.set_result(result);
        }
        self.find_dialog = Some(dialog);
        self.find_return_focus = false;
        TerminalFocusIntent::Find
    }

    pub(super) fn apply_find_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: crate::app_actions::TerminalFindAction,
        focused_pane_id: Option<&str>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match action {
            crate::app_actions::TerminalFindAction::Prompt => {
                outcome.focus_intent =
                    self.open_find(terminal, TerminalSearchDirection::Next, focused_pane_id);
            }
            crate::app_actions::TerminalFindAction::Close => {
                self.clear_find_search(terminal);
                outcome.focus_intent = TerminalFocusIntent::Terminal;
            }
            crate::app_actions::TerminalFindAction::Search(query) => {
                self.search_terminal(
                    terminal,
                    focused_pane_id,
                    &query,
                    TerminalSearchDirection::Current,
                );
            }
            crate::app_actions::TerminalFindAction::SearchSelection => {
                let Some(query) = self.selected_terminal_text(terminal) else {
                    return self.finish(outcome);
                };
                self.search_terminal(
                    terminal,
                    focused_pane_id,
                    &query,
                    TerminalSearchDirection::Current,
                );
            }
            action @ (crate::app_actions::TerminalFindAction::Previous
            | crate::app_actions::TerminalFindAction::Next) => {
                let direction =
                    if matches!(action, crate::app_actions::TerminalFindAction::Previous) {
                        TerminalSearchDirection::Previous
                    } else {
                        TerminalSearchDirection::Next
                    };
                let query = self.last_search.clone();
                if query.is_empty() {
                    outcome.focus_intent =
                        self.open_find(terminal, TerminalSearchDirection::Next, focused_pane_id);
                } else {
                    self.search_terminal(terminal, focused_pane_id, &query, direction);
                }
            }
        }
        outcome.effects.push(AppEffect::RequestRepaint);
        self.finish(outcome)
    }

    pub(super) fn enter_copy_mode(
        &mut self,
        terminal: &mut ActiveTerminal,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        match TerminalRuntime::enter_copy_mode(terminal) {
            Ok(()) => outcome.effects.push(AppEffect::RequestRepaint),
            Err(error) => self.record_error(error),
        }
        self.finish(outcome)
    }

    pub(super) fn copy_selection_or_request(
        &mut self,
        terminal: &mut ActiveTerminal,
        format: CopyToClipboard,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let mut outcome = TerminalInteractionOutcome::default();
        if !self.copy_selection(terminal, format) {
            outcome.effects.push(AppEffect::RequestCopy);
        }
        self.finish(outcome)
    }

    pub(super) fn mark_paste_suppression(&mut self) {
        self.suppress_next_egui_paste = true;
    }

    pub(super) fn copy_mode_active(&mut self, terminal: &mut ActiveTerminal) -> Result<bool> {
        TerminalRuntime::copy_mode_active(terminal)
    }

    pub(super) fn handle_direct_input(
        &mut self,
        terminal: &mut ActiveTerminal,
        input: KeyInput,
        copy_mode_active: bool,
    ) -> TerminalDirectInputOutcome {
        self.begin_operation();
        let mut outcome = TerminalDirectInputOutcome::default();
        let mut copy_mode_active = copy_mode_active;
        outcome.copy_mode_active = copy_mode_active;
        if copy_mode_active {
            if let Some(action) = copy_mode_action_for_input(input) {
                copy_mode_active =
                    self.apply_copy_mode_key_action(terminal, action, &mut outcome.effects);
                outcome.consumed = true;
            } else if !copy_mode_input_should_pass_to_app(input) {
                outcome.consumed = true;
            }
        }
        if !outcome.consumed
            && direct_copy_shortcut_pressed(input)
            && self.copy_selection(terminal, CopyToClipboard::Mixed)
        {
            outcome.consumed = true;
        }
        outcome.copy_mode_active = copy_mode_active;
        self.finish_direct(outcome)
    }

    pub(super) fn handle_egui_input(
        &mut self,
        terminal: &mut ActiveTerminal,
        input: TerminalInteractionInput<'_>,
    ) -> TerminalInteractionOutcome {
        self.begin_operation();
        let TerminalInteractionInput {
            events,
            modifiers,
            pressed_mouse_button,
            input_focus,
            terminal_input_enabled,
            surface,
            view,
            chrome_handle_rects,
            copy_on_select,
        } = input;
        let mut events = events;
        if std::mem::take(&mut self.suppress_next_egui_paste) {
            remove_first_paste_event(&mut events);
        }
        let selection_surface = terminal_input_enabled.then_some(surface).flatten();
        let mouse_tracking = self.terminal_mouse_tracking_for_selection(
            terminal,
            &events,
            terminal_input_enabled,
            pressed_mouse_button,
        );
        let mut chrome_handle_rects = chrome_handle_rects.to_vec();
        if let Some(rect) = self
            .find_dialog
            .as_ref()
            .and_then(TerminalFindDialog::last_rect)
        {
            chrome_handle_rects.push(rect);
        }
        let (events, mut selection_actions) = self.selection.route_events(
            events,
            TerminalSelectionRouteContext {
                surface: selection_surface,
                view,
                mouse_tracking,
                frame_modifiers: modifiers,
                chrome_handle_rects: &chrome_handle_rects,
            },
        );
        selection_actions.extend(self.selection.autoscroll_actions(
            selection_surface,
            view,
            modifiers,
        ));
        let mut outcome = TerminalInteractionOutcome {
            events,
            ..TerminalInteractionOutcome::default()
        };
        outcome.handled_count += self.apply_selection_actions(
            terminal,
            selection_actions,
            copy_on_select,
            &mut outcome.effects,
        );
        outcome.handled_count += self.consume_copy_mode_egui_events(
            terminal,
            &mut outcome.events,
            input_focus,
            terminal_input_enabled,
            &mut outcome.effects,
        );
        outcome.handled_count +=
            self.consume_copy_shortcut_for_selection(terminal, &mut outcome.events);
        self.finish(outcome)
    }

    fn terminal_mouse_tracking_for_selection(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &[egui::Event],
        terminal_input_enabled: bool,
        pressed_mouse_button: Option<MouseButton>,
    ) -> bool {
        let primary_drag_active = pressed_mouse_button == Some(MouseButton::Left);
        if !terminal_input_enabled
            || !events.iter().any(|event| match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    ..
                } => true,
                egui::Event::PointerMoved(_) => primary_drag_active,
                _ => false,
            })
        {
            return false;
        }
        match TerminalRuntime::is_mouse_tracking(terminal) {
            Ok(mouse_tracking) => mouse_tracking,
            Err(error) => {
                self.record_error(error);
                false
            }
        }
    }

    fn apply_selection_actions(
        &mut self,
        terminal: &mut ActiveTerminal,
        actions: Vec<TerminalSelectionAction>,
        copy_on_select: bool,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let count = actions.len();
        for action in actions {
            let is_end = matches!(&action, TerminalSelectionAction::End(_));
            let result = match action {
                TerminalSelectionAction::Begin(event) => {
                    TerminalRuntime::begin_selection(terminal, event)
                }
                TerminalSelectionAction::Scroll(delta) => {
                    TerminalRuntime::scroll_viewport_delta(terminal, delta)
                }
                TerminalSelectionAction::Update(event) => {
                    TerminalRuntime::update_selection(terminal, event)
                }
                TerminalSelectionAction::End(event) => {
                    TerminalRuntime::end_selection(terminal, event)
                }
            };
            match result {
                Ok(()) => {
                    effects.push(AppEffect::RequestRepaint);
                    if copy_on_select && is_end {
                        self.copy_selection(terminal, CopyToClipboard::Mixed);
                    }
                }
                Err(error) => self.record_error(error),
            }
        }
        count
    }

    fn consume_copy_mode_egui_events(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &mut Vec<egui::Event>,
        input_focus: InputFocus,
        terminal_input_enabled: bool,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if !terminal_input_enabled
            || (self.find_dialog.is_some() && input_focus != InputFocus::Terminal)
            || !copy_mode_key_input_present(events)
        {
            return 0;
        }
        let mut copy_mode_active = match TerminalRuntime::copy_mode_active(terminal) {
            Ok(active) => active,
            Err(error) => {
                self.record_error(error);
                false
            }
        };
        if !copy_mode_active {
            return 0;
        }
        let mut count = 0;
        let mut retained = Vec::with_capacity(events.len());
        let mut suppress_next_text = false;
        let mut pass_next_text_to_app = false;
        let mut find_prompt_opened = false;
        for event in events.drain(..) {
            if find_prompt_opened {
                if matches!(event, egui::Event::Text(_)) && std::mem::take(&mut suppress_next_text)
                {
                    count += 1;
                } else {
                    suppress_next_text = false;
                    retained.push(event);
                }
                continue;
            }
            if !copy_mode_active {
                if matches!(event, egui::Event::Text(_)) && std::mem::take(&mut suppress_next_text)
                {
                    count += 1;
                } else {
                    retained.push(event);
                }
                continue;
            }
            match &event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if copy_mode_egui_key_should_pass_to_app(*key, *modifiers) => {
                    pass_next_text_to_app = copy_mode_egui_key_may_emit_text(*key);
                    retained.push(event);
                }
                egui::Event::Text(_) if std::mem::take(&mut pass_next_text_to_app) => {
                    retained.push(event);
                }
                _ if matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)) => {
                    pass_next_text_to_app = false;
                    count += 1;
                    if let Some(action) =
                        copy_mode_action_for_egui_event(&event, &mut suppress_next_text)
                    {
                        let opens_find = matches!(action, CopyModeKeyAction::SearchPrompt(_));
                        copy_mode_active =
                            self.apply_copy_mode_key_action(terminal, action, effects);
                        find_prompt_opened = opens_find;
                    }
                }
                _ => {
                    pass_next_text_to_app = false;
                    retained.push(event);
                }
            }
        }
        *events = retained;
        count
    }

    fn consume_copy_shortcut_for_selection(
        &mut self,
        terminal: &mut ActiveTerminal,
        events: &mut Vec<egui::Event>,
    ) -> usize {
        let Some(index) = events.iter().position(copy_shortcut_pressed) else {
            return 0;
        };
        if !self.copy_selection(terminal, CopyToClipboard::Mixed) {
            return 0;
        }
        events.remove(index);
        1
    }

    fn apply_copy_mode_key_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: CopyModeKeyAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        match action {
            CopyModeKeyAction::Terminal(action) => {
                self.apply_copy_mode_terminal_action(terminal, action, effects)
            }
            CopyModeKeyAction::SearchPrompt(direction) => {
                self.record_search_direction(direction);
                let focus = self.open_find(terminal, direction, None);
                self.find_return_focus = true;
                self.pending_focus_intent = focus;
                effects.push(AppEffect::RequestRepaint);
                true
            }
            CopyModeKeyAction::SearchWord(direction) => self.apply_copy_mode_terminal_action(
                terminal,
                TerminalCopyModeAction::SearchWord(direction),
                effects,
            ),
            CopyModeKeyAction::SearchRepeat(repeat) => {
                let direction = repeat.direction(self.last_search_direction);
                let query = self.last_search.clone();
                if !query.trim().is_empty() {
                    let result = self.search_terminal_with_direction_recording(
                        terminal, None, &query, direction, false,
                    );
                    if let Some(dialog) = self.find_dialog.as_mut() {
                        dialog.set_result(result);
                    }
                    effects.push(AppEffect::RequestRepaint);
                }
                true
            }
        }
    }

    fn apply_copy_mode_terminal_action(
        &mut self,
        terminal: &mut ActiveTerminal,
        action: TerminalCopyModeAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        let search_direction = match &action {
            TerminalCopyModeAction::Search { direction, .. }
            | TerminalCopyModeAction::SearchWord(direction) => Some(*direction),
            _ => None,
        };
        match TerminalRuntime::handle_copy_mode_action(terminal, action) {
            Ok(outcome) => {
                if let Some(bytes) = outcome.copied
                    && let Err(error) = write_clipboard_text(&String::from_utf8_lossy(&bytes))
                {
                    self.record_error(error);
                }
                if let Some(search) = outcome.search {
                    self.last_search = search.query;
                    if let Some(direction) = search_direction {
                        self.record_search_direction(direction);
                    }
                    let result = self.terminal_find_result_from_frame(terminal, search.found);
                    if let Some(dialog) = self.find_dialog.as_mut() {
                        dialog.set_result(result);
                    }
                }
                effects.push(AppEffect::RequestRepaint);
                outcome.active
            }
            Err(error) => {
                self.record_error(error);
                false
            }
        }
    }

    fn copy_selection(&mut self, terminal: &mut ActiveTerminal, format: CopyToClipboard) -> bool {
        let result = (|| -> Result<bool> {
            let mut selection = |format| terminal.format_selection(format);
            match format {
                format @ (CopyToClipboard::Plain | CopyToClipboard::Vt) => {
                    let selection_format = match format {
                        CopyToClipboard::Plain => TerminalSelectionFormat::PlainText,
                        CopyToClipboard::Vt => TerminalSelectionFormat::Vt,
                        _ => unreachable!(),
                    };
                    let Some(bytes) = selection(selection_format)? else {
                        return Ok(false);
                    };
                    write_clipboard_text(&String::from_utf8_lossy(&bytes))?;
                }
                CopyToClipboard::Html => {
                    let Some(bytes) = selection(TerminalSelectionFormat::Html)? else {
                        return Ok(false);
                    };
                    write_clipboard_html(&String::from_utf8_lossy(&bytes), None)?;
                }
                CopyToClipboard::Mixed => {
                    let Some(plain) = selection(TerminalSelectionFormat::PlainText)? else {
                        return Ok(false);
                    };
                    let Some(html) = selection(TerminalSelectionFormat::Html)? else {
                        return Ok(false);
                    };
                    write_clipboard_html(
                        &String::from_utf8_lossy(&html),
                        Some(&String::from_utf8_lossy(&plain)),
                    )?;
                }
            }
            Ok(true)
        })();
        match result {
            Ok(copied) => copied,
            Err(error) => {
                self.record_error(error);
                false
            }
        }
    }

    fn selected_terminal_text(&mut self, terminal: &mut ActiveTerminal) -> Option<String> {
        match terminal.format_selection(TerminalSelectionFormat::PlainText) {
            Ok(Some(bytes)) => Some(String::from_utf8_lossy(&bytes).trim().to_owned())
                .filter(|text| !text.is_empty()),
            Ok(None) => None,
            Err(error) => {
                self.record_error(error);
                None
            }
        }
    }

    fn clear_search(&mut self, terminal: &mut ActiveTerminal) {
        if let Err(error) = terminal.search_viewport("", TerminalSearchDirection::Current) {
            self.record_error(error);
        }
    }

    fn clear_find_search(&mut self, terminal: &mut ActiveTerminal) {
        self.dismiss_find();
        self.clear_search(terminal);
    }

    fn search_terminal(
        &mut self,
        terminal: &mut ActiveTerminal,
        focused_pane_id: Option<&str>,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        self.search_terminal_with_direction_recording(
            terminal,
            focused_pane_id,
            query,
            direction,
            true,
        )
    }

    fn search_terminal_with_direction_recording(
        &mut self,
        terminal: &mut ActiveTerminal,
        focused_pane_id: Option<&str>,
        query: &str,
        direction: TerminalSearchDirection,
        record_direction: bool,
    ) -> TerminalFindResult {
        let query = query.trim();
        if query.is_empty() {
            self.clear_search(terminal);
            return TerminalFindResult::default();
        }
        self.last_search = query.to_owned();
        if record_direction {
            self.record_search_direction(direction);
        }
        let copy_mode_active = match TerminalRuntime::copy_mode_active(terminal) {
            Ok(active) => active,
            Err(error) => {
                self.record_error(error);
                false
            }
        };
        if copy_mode_active {
            return self.search_copy_mode_terminal(terminal, query, direction);
        }
        if let Some(pane_id) = focused_pane_id
            && let Some(source) = terminal.focused_terminal_runtime(pane_id)
        {
            return self.resolve_find_result(search_runtime(source, query, direction));
        }
        self.resolve_find_result(search_runtime(terminal, query, direction))
    }

    fn resolve_find_result(&mut self, result: Result<TerminalFindResult>) -> TerminalFindResult {
        match result {
            Ok(result) => result,
            Err(error) => {
                self.record_error(error);
                TerminalFindResult::default()
            }
        }
    }

    fn search_copy_mode_terminal(
        &mut self,
        terminal: &mut ActiveTerminal,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        match TerminalRuntime::handle_copy_mode_action(
            terminal,
            TerminalCopyModeAction::Search {
                query: query.to_owned(),
                direction,
            },
        ) {
            Ok(outcome) => outcome
                .search
                .map_or_else(TerminalFindResult::default, |search| {
                    self.terminal_find_result_from_frame(terminal, search.found)
                }),
            Err(error) => {
                self.record_error(error);
                TerminalFindResult::default()
            }
        }
    }

    fn terminal_find_result_from_frame(
        &mut self,
        terminal: &mut ActiveTerminal,
        found: bool,
    ) -> TerminalFindResult {
        let (active_index, match_count) = terminal
            .extract_frame()
            .map(|frame| (frame.active_search_match_index, frame.search_match_count))
            .unwrap_or_else(|error| {
                self.record_error(error);
                (None, 0)
            });
        TerminalFindResult {
            found,
            active_index,
            match_count,
        }
    }

    fn record_search_direction(&mut self, direction: TerminalSearchDirection) {
        if direction != TerminalSearchDirection::Current {
            self.last_search_direction = direction;
        }
    }

    fn begin_operation(&mut self) {
        self.last_error = None;
        self.pending_focus_intent = TerminalFocusIntent::None;
    }

    fn record_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    fn finish(&mut self, mut outcome: TerminalInteractionOutcome) -> TerminalInteractionOutcome {
        let (last_error, focus_intent) = self.take_operation_state();
        outcome.last_error = last_error;
        if focus_intent != TerminalFocusIntent::None {
            outcome.focus_intent = focus_intent;
        }
        outcome
    }

    fn finish_direct(
        &mut self,
        mut outcome: TerminalDirectInputOutcome,
    ) -> TerminalDirectInputOutcome {
        (outcome.last_error, outcome.focus_intent) = self.take_operation_state();
        outcome
    }

    fn take_operation_state(&mut self) -> (Option<String>, TerminalFocusIntent) {
        (
            self.last_error.take(),
            std::mem::take(&mut self.pending_focus_intent),
        )
    }
}

fn search_runtime(
    terminal: &mut dyn TerminalRuntime,
    query: &str,
    direction: TerminalSearchDirection,
) -> Result<TerminalFindResult> {
    let found = terminal.search_viewport(query, direction)?;
    let frame = terminal.extract_frame()?;
    Ok(TerminalFindResult {
        found,
        active_index: frame.active_search_match_index,
        match_count: frame.search_match_count,
    })
}

fn remove_first_paste_event(events: &mut Vec<egui::Event>) {
    if let Some(index) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    {
        events.remove(index);
    }
}
