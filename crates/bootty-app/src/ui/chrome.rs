use eframe::egui;

mod item_primitives;
mod runtime;
mod sidebar_panel;
mod status_bar;

pub(crate) use runtime::{ChromeEvents, ChromeRuntime, SidebarResize};

pub(crate) use sidebar_panel::{MACOS_TITLEBAR_BUTTON_SAFE_WIDTH, SPACE_SWITCHER_HEIGHT};
pub use sidebar_panel::{
    SessionContextAction, SidebarEvent, SidebarModel, SidebarSpaceSwipeState, SpaceSwitcherEvent,
    load_app_icon_texture, show_sidebar, show_space_switcher, take_sidebar_space_swipe,
};
pub use status_bar::{
    ResolvedItem, ResolvedSegment, STATUS_EDGE_PAD, StatusBarEvent, StatusBarLayout,
    StatusBarModel, TabContext, TabContextAction, TabContextTarget, activate_window_target,
    is_windows_surface, show_status_bar, status_bar_layout,
};

fn start_window_drag_on_primary_press(response: &egui::Response) {
    let primary_press_pos = response.ctx.input(|input| {
        input
            .pointer
            .button_pressed(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
    });
    if primary_press_pos.is_some_and(|pos| response.rect.contains(pos)) {
        response
            .ctx
            .send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
}
