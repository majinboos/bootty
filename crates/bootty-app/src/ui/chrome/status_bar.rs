//! What a status-strip item *means*: which surface owns the window tabs, which click is an
//! extension action, and which right-click opens the built-in tab menu.
//!
//! Painting, hit-testing, and the reorder gesture live in [`bootty_ui::status_strip`].

use bootty_extension::ExtensionUiAction;
use bootty_ui::ThemePalette;
use bootty_ui::status_layout::StatusBarLayout;
use bootty_ui::status_strip::{self, StatusStrip, StripEvent};
use eframe::egui;

use super::start_window_drag_on_primary_press;

/// Semantic identity of the built-in window-tab surface. A segment naming it gets the tab context
/// menu and its reorder is a window move, not an extension action. This is the surface id a module
/// declares, never the producing module's file name.
const STATUS_WINDOWS_SURFACE: &str = "windows";

/// The action encoding the window-tab surface publishes per tab.
const ACTIVATE_WINDOW_ACTION: &str = "activate-window:";

/// Whether `surface` is the built-in window-tab surface.
#[must_use]
pub fn is_windows_surface(surface: &str) -> bool {
    surface == STATUS_WINDOWS_SURFACE
}

/// The window id an `activate-window:<id>` action targets.
#[must_use]
pub fn activate_window_target(action: &str) -> Option<&str> {
    action.strip_prefix(ACTIVATE_WINDOW_ACTION)
}

#[derive(Clone)]
pub struct StatusBarModel<'a, 'segments> {
    /// Frame-local geometry built from the resolved segments before the bar height is allocated.
    pub layout: &'a StatusBarLayout<'segments>,
    /// Targets that may receive the built-in tab context menu in the current selected session.
    pub tab_context: Option<&'a TabContext>,
    /// Bar fill; set to the sidebar fullscreen background when the bar sits in the notch band.
    pub background: egui::Color32,
    /// Height of the drawable status row. When the allocated bar is taller, items are bottom-aligned
    /// so extra notch-clearance space appears above them instead of stretching the row.
    pub row_height: f32,
    /// Stable per-bar interaction state key, so top and bottom bar drags cannot collide.
    pub interaction_id: &'static str,
}

pub struct TabContext {
    pub session_id: String,
    pub targets: Vec<TabContextTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabContextTarget {
    pub window_id: String,
    pub is_active: bool,
    pub can_close_pane: bool,
}

pub enum StatusBarEvent {
    ExtensionAction(ExtensionUiAction),
    ContextAction {
        session_id: String,
        window_id: String,
        action: TabContextAction,
    },
    Reorder {
        module: String,
        generation: u64,
        surface: String,
        source: String,
        before: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabContextAction {
    Activate,
    NewTab,
    PreviousTab,
    NextTab,
    LastTab,
    Rename,
    MoveLeft,
    MoveRight,
    ClosePane,
}

pub fn show_status_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: StatusBarModel<'_, '_>,
) -> Option<StatusBarEvent> {
    let layout = model.layout;
    let tab_context = model.tab_context;
    let frame = status_strip::show(
        ui,
        palette,
        StatusStrip {
            layout,
            background: model.background,
            row_height: model.row_height,
            interaction_id: model.interaction_id,
            identity_salt: tab_context.map_or("", |context| context.session_id.as_str()),
        },
        |response, segment, item| tab_context_menu(response, tab_context?, layout, segment, item),
    );
    if frame.background_free {
        start_window_drag_on_primary_press(&frame.response);
    }
    frame.event.map(|event| match event {
        StripEvent::Item(event) => event,
        StripEvent::Clicked { segment, item } => {
            let segment = &layout.segments[segment];
            StatusBarEvent::ExtensionAction(ExtensionUiAction {
                module: segment.module.to_owned(),
                generation: segment.generation,
                surface: segment.surface.to_owned(),
                action: segment.items[item].item.action.clone().unwrap_or_default(),
                payload: serde_json::Value::Null,
            })
        }
        StripEvent::Reorder {
            source_slot,
            anchor,
            before,
        } => {
            let segment = layout
                .segments
                .iter()
                .find(|segment| segment.source_slot == source_slot)
                .expect("the dragged slot is one of this frame's segments");
            StatusBarEvent::Reorder {
                module: segment.module.to_owned(),
                generation: segment.generation,
                surface: segment.surface.to_owned(),
                source: anchor,
                before,
            }
        }
    })
}

/// The built-in tab menu, offered only for a window-tab item whose action targets its own anchor.
fn tab_context_menu(
    response: &egui::Response,
    context: &TabContext,
    layout: &StatusBarLayout<'_>,
    segment: usize,
    item: usize,
) -> Option<StatusBarEvent> {
    let segment = &layout.segments[segment];
    let item = &segment.items[item];
    let window_id = item.item.reorder_anchor.as_deref()?;
    if !is_windows_surface(segment.surface)
        || item.item.action.as_deref().and_then(activate_window_target) != Some(window_id)
    {
        return None;
    }
    let (target_index, target) = context
        .targets
        .iter()
        .enumerate()
        .find(|(_, target)| target.window_id == window_id)?;
    tab_context_action(
        response,
        !target.is_active,
        target_index > 0,
        target_index + 1 < context.targets.len(),
        context.targets.len() > 1,
        target.can_close_pane,
    )
    .map(|action| StatusBarEvent::ContextAction {
        session_id: context.session_id.clone(),
        window_id: window_id.to_owned(),
        action,
    })
}

fn tab_context_action(
    response: &egui::Response,
    can_activate: bool,
    can_move_left: bool,
    can_move_right: bool,
    can_navigate: bool,
    can_close_pane: bool,
) -> Option<TabContextAction> {
    use TabContextAction as A;
    use bootty_ui::menu::MenuEntry as E;
    bootty_ui::menu::context_menu(
        response,
        &[
            E::enabled_item(can_activate, "Activate Tab", A::Activate),
            E::Separator,
            E::item("New Tab", A::NewTab),
            E::submenu(
                "Navigate Tabs",
                vec![
                    E::enabled_item(can_navigate, "Previous Tab", A::PreviousTab),
                    E::enabled_item(can_navigate, "Next Tab", A::NextTab),
                    E::enabled_item(can_navigate, "Last Tab", A::LastTab),
                ],
            ),
            E::Separator,
            E::item("Rename Tab", A::Rename),
            E::enabled_item(can_move_left, "Move Tab Left", A::MoveLeft),
            E::enabled_item(can_move_right, "Move Tab Right", A::MoveRight),
            E::Separator,
            E::enabled_item(can_close_pane, "Close Pane", A::ClosePane),
        ],
    )
}
