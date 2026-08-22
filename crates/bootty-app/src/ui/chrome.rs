use eframe::egui;

mod runtime;
mod sidebar_footer;
mod sidebar_panel;
mod space_switcher;
mod status_bar;

pub(crate) use runtime::{
    ChromeEvents, ChromeRuntime, SidebarResize, compose_session_module_items,
};

pub use bootty_ui::status_layout::{
    ResolvedItem, ResolvedSegment, STATUS_EDGE_PAD, StatusBarLayout, status_bar_layout,
};
pub(crate) use sidebar_panel::MACOS_TITLEBAR_BUTTON_SAFE_WIDTH;
pub use sidebar_panel::{
    SessionContextAction, SidebarEvent, SidebarModel, SpaceMoveTarget, UNASSIGNED_KIND,
    show_sidebar, sidebar_drop_target,
};
pub(crate) use space_switcher::SPACE_SWITCHER_HEIGHT;
pub use space_switcher::{
    SidebarSpaceSwipeState, SpaceSwitcherEvent, show_space_switcher, take_sidebar_space_swipe,
};
pub use status_bar::{
    StatusBarEvent, StatusBarModel, TabContext, TabContextAction, TabContextTarget,
    activate_window_target, is_windows_surface, show_status_bar,
};

/// The title icon, decoded and uploaded once per context and kept by the chrome runtime.
pub fn load_app_icon_texture(
    ctx: &egui::Context,
    texture: &mut Option<egui::TextureHandle>,
) -> egui::TextureHandle {
    texture
        .get_or_insert_with(|| {
            ctx.load_texture(
                "bootty-app-icon",
                crate::assets::title_icon_color_image(),
                egui::TextureOptions::LINEAR,
            )
        })
        .clone()
}

/// Hover lift for sidebar surfaces. Windowed hover derives from the sidebar background;
/// fullscreen uses a stronger lift so a black notch background still shows a visible, non-muddy
/// hover. Both the session rows and the Space switcher read it here, so they cannot disagree.
pub fn sidebar_hover_color(palette: bootty_ui::ThemePalette, fullscreen: bool) -> egui::Color32 {
    let lift = if fullscreen { 0.13 } else { 0.045 };
    bootty_ui::mix(palette.base, palette.text, lift)
}

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
