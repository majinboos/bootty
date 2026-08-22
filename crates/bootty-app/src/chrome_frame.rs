//! Owner side of the chrome frame: prepare, then apply.
//!
//! [`prepare`] projects the mux facts extensions and the status bar read, before painting.
//! The chrome views then paint from that projection and hand back their existing leaf events;
//! [`apply`] runs the mux actions, dialogs, persistence and extension submissions they imply,
//! in the order the views produced them.

use std::collections::HashSet;

use eframe::egui;

use bootty_extension::{
    ExtensionHost, ExtensionUiAction, MuxView, SessionProgressView, SessionView, WindowView,
};

use crate::{
    commands::ExactMuxTarget,
    state::{AppEffect, AppState, ExactMuxAction},
    ui::chrome::{self, ChromeEvents, SidebarResize},
    workspace_runtime::TerminalProgressState,
};

fn color_hex(color: egui::Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

/// What one chrome frame needs from the owners: the mux facts published to extensions, and the
/// tab-context targets the status bar borrows while painting.
pub(crate) struct ChromeProjection {
    pub(crate) mux: MuxView,
    pub(crate) tab_context: Option<chrome::TabContext>,
}

/// Project the accepted mux state for one frame. A pure read: it publishes nothing and mutates
/// nothing, so the caller controls when the snapshot is taken and who receives it.
pub(crate) fn prepare(
    state: &AppState,
    sidebar_visible: bool,
    window_focused: bool,
) -> ChromeProjection {
    let palette = state.ui_theme().palette;
    let fallback_color = color_hex(palette.accent);
    let selected_session = state.mux().selected_session();
    let sessions = state.mux().sessions();
    let display_names = state.session_display_names(sessions);
    let session_colors = crate::ui::sidebar::sidebar_session_colors(sessions, &display_names);
    let selected_window = state.mux().selected_window();
    let mut windows = Vec::new();
    let mut tab_context = None;
    let mut selected_name = None;
    let mut selected_color = None;
    let mut session_views = Vec::with_capacity(sessions.len());
    for ((session, display_name), (color, dim_color)) in
        sessions.iter().zip(display_names).zip(session_colors)
    {
        let selected = selected_session.map_or(session.active, |selected| {
            selected == session.id || selected == session.name
        });
        let color = color_hex(color);
        let dim_color = color_hex(dim_color);
        if selected && selected_name.is_none() {
            selected_name = Some(if display_name.is_empty() {
                session.name.clone()
            } else {
                display_name.clone()
            });
            selected_color = Some(color.clone());
        }
        if selected && selected_session.is_some() && tab_context.is_none() {
            let tab_active = selected_window.or(session.active_window_id.as_deref());
            let mut ordered = session.windows.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|window| window.index);
            let mut targets = Vec::with_capacity(ordered.len());
            for window in ordered {
                let active = selected_window == Some(window.id.as_str())
                    || (selected_window.is_none() && window.active);
                let progress = (!active).then(|| state.window_progress(window)).flatten();
                windows.push(WindowView {
                    id: window.id.clone(),
                    index: window.index,
                    name: window.name.clone(),
                    active,
                    progress,
                    progress_indeterminate: progress.is_some()
                        && state.window_has_indeterminate_progress(window),
                });
                targets.push(chrome::TabContextTarget {
                    window_id: window.id.clone(),
                    is_active: tab_active == Some(window.id.as_str()),
                    can_close_pane: window.anchor.pane_id.is_some(),
                });
            }
            tab_context = Some(chrome::TabContext {
                session_id: session.id.clone(),
                targets,
            });
        }
        let progress = session
            .windows
            .iter()
            .filter_map(|window| state.window_progress(window))
            .max();
        let progress_indeterminate = progress.is_some()
            && session
                .windows
                .iter()
                .any(|window| state.window_has_indeterminate_progress(window));
        let mut reported_panes = HashSet::new();
        let mut progresses = Vec::new();
        for window in &session.windows {
            for pane in window.panes.iter().chain(std::iter::once(&window.anchor)) {
                if let Some(pane_id) = pane.pane_id.as_deref()
                    && reported_panes.insert(pane_id)
                    && let Some(progress) = state.pane_progress(pane_id)
                {
                    progresses.push(SessionProgressView {
                        process: pane
                            .process
                            .clone()
                            .unwrap_or_else(|| "terminal".to_owned()),
                        value: progress.value.unwrap_or(50),
                        indeterminate: progress.state == TerminalProgressState::Indeterminate,
                    });
                }
            }
        }
        session_views.push(SessionView {
            id: session.id.clone(),
            name: session.name.clone(),
            display_name,
            active: session.active,
            selected,
            cwd: session.anchor.cwd.clone(),
            pane_id: session.anchor.pane_id.clone(),
            pane_pid: session.anchor.pane_pid,
            process: session.anchor.process.clone(),
            color: Some(color),
            dim_color: Some(dim_color),
            progress,
            progress_indeterminate,
            progresses,
            ports: state.session_ports(session),
        });
    }
    let scope = state.mux_scope();
    ChromeProjection {
        mux: MuxView {
            windows,
            sessions: session_views,
            scope_key: format!(
                "{}:{}",
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value()
            ),
            session: selected_name,
            sidebar_visible,
            session_color: Some(selected_color.unwrap_or(fallback_color)),
            keep_awake: state.keep_awake_active(),
            focused: window_focused,
        },
        tab_context,
    }
}

/// Apply one frame of chrome events. Returns the effects the shell still has to run.
pub(crate) fn apply(
    ctx: &egui::Context,
    state: &mut AppState,
    extensions: &mut ExtensionHost,
    events: ChromeEvents,
) -> Vec<AppEffect> {
    let mut effects = Vec::new();
    // Swipe first: the sidebar reads the wheel before the switcher is drawn, so a frame carrying
    // both a swipe and a switcher click lands the swipe first, as it did when this ran mid-paint.
    if let Some(space_id) = events.swipe_space
        && state.activate_space_from_ui(space_id)
    {
        ctx.request_repaint();
    }
    if let Some(event) = events.sidebar
        && apply_sidebar_event(state, extensions, event)
    {
        ctx.request_repaint();
    }
    if let Some(event) = events.spaces {
        match event {
            chrome::SpaceSwitcherEvent::Activate(space_id) => {
                state.activate_space_from_ui(space_id);
            }
            chrome::SpaceSwitcherEvent::Create => {
                state.open_create_space_dialog_from_ui();
            }
            chrome::SpaceSwitcherEvent::Edit(space_id) => {
                state.open_edit_space_dialog_from_ui(space_id);
            }
            chrome::SpaceSwitcherEvent::Reconnect(space_id) => {
                state.reconnect_space_from_ui(space_id);
            }
            chrome::SpaceSwitcherEvent::Close(space_id) => {
                state.close_space_from_ui(space_id);
            }
        }
        ctx.request_repaint();
    }
    match events.resize {
        Some(SidebarResize::Live(width)) => state.set_sidebar_width_live(width),
        // One write on release: the live width already matches, so this only records it.
        Some(SidebarResize::Persist) => {
            let width = state.config().chrome.sidebar_width;
            state.persist_sidebar_width(width, &mut effects);
        }
        None => {}
    }
    for event in events.status {
        apply_status_bar_event(ctx, state, extensions, event);
    }
    effects
}

fn apply_status_bar_event(
    ctx: &egui::Context,
    state: &mut AppState,
    extensions: &mut ExtensionHost,
    status_event: chrome::StatusBarEvent,
) {
    match status_event {
        chrome::StatusBarEvent::ExtensionAction(action) => match action.action.as_str() {
            "toggle-caffeinate" => {
                state.toggle_keep_awake();
                ctx.request_repaint();
            }
            other => {
                if let Some(window_id) = chrome::activate_window_target(other)
                    && let Some(session_id) = state.mux().selected_session().map(str::to_owned)
                {
                    state.apply_exact_mux_action(
                        ExactMuxAction::Activate,
                        ExactMuxTarget::window(state.mux_scope(), &session_id, window_id),
                    );
                    ctx.request_repaint();
                } else {
                    let _ = extensions.submit_ui_action(action);
                }
            }
        },
        chrome::StatusBarEvent::ContextAction {
            session_id,
            window_id,
            action,
        } => {
            let target = ExactMuxTarget::window(state.mux_scope(), &session_id, &window_id);
            let handled = match action {
                chrome::TabContextAction::Rename => {
                    state.open_rename_tab_dialog_for(&session_id, &window_id)
                }
                action => state.apply_exact_mux_action(
                    match action {
                        chrome::TabContextAction::Activate => ExactMuxAction::Activate,
                        chrome::TabContextAction::NewTab => ExactMuxAction::NewTab,
                        chrome::TabContextAction::PreviousTab => ExactMuxAction::RelativeWindow(-1),
                        chrome::TabContextAction::NextTab => ExactMuxAction::RelativeWindow(1),
                        chrome::TabContextAction::LastTab => ExactMuxAction::LastWindow,
                        chrome::TabContextAction::MoveLeft => ExactMuxAction::MoveWindow(-1),
                        chrome::TabContextAction::MoveRight => ExactMuxAction::MoveWindow(1),
                        chrome::TabContextAction::ClosePane => ExactMuxAction::CloseWindowPane,
                        chrome::TabContextAction::Rename => unreachable!(),
                    },
                    target,
                ),
            };
            if handled {
                ctx.request_repaint();
            }
        }
        // `surface` is the semantic surface identity: the built-in window tabs. `module` stays the
        // producer identity, and travels with `generation` so a stale one is rejected.
        chrome::StatusBarEvent::Reorder {
            module,
            generation,
            surface,
            source,
            before,
        } => {
            if chrome::is_windows_surface(&surface)
                && state.reorder_window_before_from_ui(&source, before.as_deref())
            {
                ctx.request_repaint();
            } else {
                let _ = extensions.submit_ui_action(ExtensionUiAction {
                    module,
                    generation,
                    surface,
                    action: "reorder".to_owned(),
                    payload: serde_json::json!({ "source": source, "before": before }),
                });
            }
        }
    }
}

fn apply_sidebar_event(
    state: &mut AppState,
    extensions: &mut ExtensionHost,
    event: chrome::SidebarEvent,
) -> bool {
    use chrome::SessionContextAction as Action;

    match event {
        chrome::SidebarEvent::ExtensionAction(action) => {
            extensions.submit_ui_action(action).is_ok()
        }
        chrome::SidebarEvent::ActivateSession(target) => {
            state.activate_scoped_session_from_ui(&target);
            false
        }
        chrome::SidebarEvent::Reorder { source, before } => {
            state.reorder_session_before(&source, before.as_deref())
        }
        chrome::SidebarEvent::ContextAction { target, action } => match action {
            Action::Activate => state.activate_scoped_session_from_ui(&target),
            Action::PreviousSession => state.activate_relative_scoped_session_from_ui(&target, -1),
            Action::NextSession => state.activate_relative_scoped_session_from_ui(&target, 1),
            _ if !state.activate_scoped_session_from_ui(&target) => false,
            Action::NewSession => state.open_new_session_dialog_from_ui(),
            Action::SwitchSession => state.open_session_picker_dialog_from_ui(),
            Action::LastSession => state.activate_last_session_from_ui(),
            Action::Rename => state.open_rename_session_dialog_for(&target.session_id),
            Action::MoveUp => state.move_session_from_ui(&target.session_id, -1),
            Action::MoveDown => state.move_session_from_ui(&target.session_id, 1),
            Action::Detach => state.detach_scoped_session_from_space(&target),
            Action::Ditch => state.open_ditch_session_dialog_for(&target.session_id),
        },
    }
}
