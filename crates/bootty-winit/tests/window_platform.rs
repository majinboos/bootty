#[cfg(not(target_os = "macos"))]
use pretty_assertions::assert_eq;
#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_window_adapter_keeps_native_fullscreen_with_app() {
    assert!(!bootty_winit::window::handles_macos_non_native_fullscreen_frame());
    assert_eq!(
        bootty_winit::window::macos_active_screen_notch_height(),
        0.0
    );
    assert!(!bootty_winit::window::macos_active_screen_is_notched());
    assert_eq!(bootty_winit::window::macos_active_screen_notch_span(), None);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_window_adapter_noops_native_mutations() {
    bootty_winit::window::set_macos_cursor_icon(eframe::egui::CursorIcon::Text);
    bootty_winit::window::reapply_macos_cursor_icon();
    bootty_winit::window::macos_disable_titlebar_separator();
    bootty_winit::window::macos_set_window_shadow(true);
    bootty_winit::window::disable_automatic_window_tabbing();
    bootty_winit::window::refresh_macos_non_native_fullscreen_frame();
    assert!(bootty_winit::window::set_macos_non_native_fullscreen(true));
    assert!(bootty_winit::window::restore_macos_presentation());
}
