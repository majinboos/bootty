use eframe::egui;

mod item_primitives;
mod sidebar_panel;
mod status_bar;

pub(crate) use sidebar_panel::{MACOS_TITLEBAR_BUTTON_SAFE_WIDTH, SPACE_SWITCHER_HEIGHT};
pub use sidebar_panel::{
    SessionContextAction, SidebarEvent, SidebarModel, SidebarSpaceSwipeState, SpaceSwitcherEvent,
    SpaceSwitcherItem, load_app_icon_texture, show_sidebar, show_space_switcher,
    take_sidebar_space_swipe,
};
pub use status_bar::{
    ResolvedItem, ResolvedSegment, STATUS_EDGE_PAD, StatusBarEvent, StatusBarModel, TabContext,
    TabContextAction, TabContextTarget, show_status_bar, status_bar_window_tab_row_count,
    status_bar_windows_intersect_x_range,
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
