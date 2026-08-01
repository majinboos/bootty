use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use anyhow::Result;
use eframe::egui::CursorIcon;
#[cfg(target_os = "macos")]
use objc2::runtime::NSObjectProtocol;
#[cfg(target_os = "macos")]
use objc2::{MainThreadMarker, sel};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSCursor, NSScreen, NSTitlebarSeparatorStyle, NSWindow};

use crate::config::{BoottyConfig, MacosTitlebarStyle, WindowConfig};

pub fn read_clipboard_text() -> Result<Option<String>> {
    if let Some(paths) = read_clipboard_file_paths()
        && let Some(text) = bootty_winit::file_paths::format_file_paths_for_paste(
            paths.iter().map(PathBuf::as_path),
        )
    {
        return Ok(Some(text));
    }

    let mut clipboard = arboard::Clipboard::new()?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Ok(Some(text)),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn write_clipboard_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text.to_owned())?;
    Ok(())
}

pub fn show_desktop_notification(title: &str, body: &str) -> Result<()> {
    platform_show_desktop_notification(title, body)
}

#[cfg(target_os = "macos")]
fn platform_show_desktop_notification(title: &str, body: &str) -> Result<()> {
    let script = format!(
        "display notification {} with title {}",
        osascript_quote(body),
        osascript_quote(title)
    );
    std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn platform_show_desktop_notification(title: &str, body: &str) -> Result<()> {
    std::process::Command::new("notify-send")
        .args([title, body])
        .spawn()?;
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_show_desktop_notification(_title: &str, _body: &str) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn osascript_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(target_os = "macos")]
fn read_clipboard_file_paths() -> Option<Vec<PathBuf>> {
    macos_clipboard::read_file_paths()
}

#[cfg(not(target_os = "macos"))]
fn read_clipboard_file_paths() -> Option<Vec<PathBuf>> {
    None
}

#[cfg(target_os = "macos")]
static MACOS_CURSOR_ICON: AtomicU8 = AtomicU8::new(0);

#[cfg(target_os = "macos")]
pub fn set_macos_cursor_icon(icon: CursorIcon) {
    MACOS_CURSOR_ICON.store(
        match icon {
            CursorIcon::Text => 1,
            CursorIcon::PointingHand => 2,
            CursorIcon::ResizeHorizontal => 3,
            CursorIcon::ResizeVertical => 4,
            _ => 0,
        },
        Ordering::Relaxed,
    );
}

#[cfg(not(target_os = "macos"))]
pub fn set_macos_cursor_icon(_icon: CursorIcon) {}

#[cfg(target_os = "macos")]
pub fn reapply_macos_cursor_icon() {
    let cursor = match MACOS_CURSOR_ICON.load(Ordering::Relaxed) {
        1 => NSCursor::IBeamCursor(),
        2 => NSCursor::pointingHandCursor(),
        3 => NSCursor::columnResizeCursor(),
        4 => NSCursor::rowResizeCursor(),
        _ => return,
    };
    cursor.set();
}

#[cfg(not(target_os = "macos"))]
pub fn reapply_macos_cursor_icon() {}

pub fn apply_macos_non_native_fullscreen_presentation(window: &WindowConfig) -> bool {
    set_macos_non_native_fullscreen_presentation(
        window.non_native_fullscreen_enabled()
            && window.hides_macos_menu_bar_in_non_native_fullscreen(),
    )
}

pub fn restore_macos_presentation() -> bool {
    set_macos_non_native_fullscreen_presentation(false)
}

/// Whether the active window's screen has a camera-housing notch. Detected by display name (the
/// built-in Liquid Retina panel on 2021+ Macs) because `safeAreaInsets`/`auxiliaryTopLeftArea` zero
/// out when the menu bar is hidden in fullscreen. Mirrors wezterm's detection.
pub fn macos_active_screen_is_notched() -> bool {
    platform_active_screen_is_notched()
}

#[cfg(target_os = "macos")]
fn name_reads_as_notched(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("built-in") || name.contains("builtin") || name.contains("liquid retina")
}

#[cfg(target_os = "macos")]
fn platform_active_screen_is_notched() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    // Prefer the active window's screen, but fall back to scanning every screen: when the window's
    // `screen()` is unresolved mid-transition, a built-in panel still present in the list is enough.
    let app = NSApplication::sharedApplication(mtm);
    if let Some(screen) = app
        .keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
        .and_then(|window| window.screen())
        && name_reads_as_notched(&screen.localizedName().to_string())
    {
        return true;
    }
    let screens = NSScreen::screens(mtm);
    (0..screens.count())
        .map(|index| screens.objectAtIndex(index))
        .any(|screen| name_reads_as_notched(&screen.localizedName().to_string()))
}

#[cfg(not(target_os = "macos"))]
fn platform_active_screen_is_notched() -> bool {
    false
}

/// Raw height of the active window's camera-housing/menu-bar exclusion band, in points. Returns
/// `0.0` off macOS or when it can't be measured. The layout layer calibrates this value to the
/// physical notch-clear line.
pub fn macos_active_screen_notch_height() -> f32 {
    platform_active_screen_notch_height()
}

#[cfg(target_os = "macos")]
static CACHED_NOTCH_HEIGHT: AtomicU32 = AtomicU32::new(0);

#[cfg(target_os = "macos")]
fn platform_active_screen_notch_height() -> f32 {
    let measured = measure_active_screen_notch_height();
    if measured > 0.0 {
        // The notch is fixed hardware; cache it so a query that transiently reads 0 can't drop the
        // offset mid-session.
        CACHED_NOTCH_HEIGHT.store(measured.to_bits(), Ordering::Relaxed);
        return measured;
    }
    f32::from_bits(CACHED_NOTCH_HEIGHT.load(Ordering::Relaxed))
}

#[cfg(target_os = "macos")]
fn measure_active_screen_notch_height() -> f32 {
    let Some(mtm) = MainThreadMarker::new() else {
        return 0.0;
    };
    let app = NSApplication::sharedApplication(mtm);
    let Some(screen) = app
        .keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
        .and_then(|window| window.screen())
    else {
        return 0.0;
    };
    // auxiliaryTopLeftArea is Apple's API for laying out around the camera housing, so it stays
    // valid in fullscreen with the menu bar hidden (where safeAreaInsets zeroes out). Its band can
    // track the menu-bar exclusion line, which is slightly lower than the physical notch.
    if screen.respondsToSelector(sel!(auxiliaryTopLeftArea)) {
        let height = screen.auxiliaryTopLeftArea().size.height as f32;
        if height > 0.0 {
            return height;
        }
    }
    if screen.respondsToSelector(sel!(safeAreaInsets)) {
        return screen.safeAreaInsets().top as f32;
    }
    0.0
}

#[cfg(not(target_os = "macos"))]
fn platform_active_screen_notch_height() -> f32 {
    0.0
}

/// Horizontal span of the active screen's camera housing in window points from the left screen
/// edge. Returns `None` off macOS or when the notched display geometry can't be inferred.
pub fn macos_active_screen_notch_span() -> Option<(f32, f32)> {
    platform_active_screen_notch_span()
}

#[cfg(target_os = "macos")]
fn platform_active_screen_notch_span() -> Option<(f32, f32)> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let screen = app
        .keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
        .and_then(|window| window.screen())?;
    let frame = screen.frame();
    let width = frame.size.width as f32;
    if width <= 0.0 {
        return None;
    }

    if screen.respondsToSelector(sel!(auxiliaryTopLeftArea))
        && screen.respondsToSelector(sel!(auxiliaryTopRightArea))
    {
        let left = screen.auxiliaryTopLeftArea();
        let right = screen.auxiliaryTopRightArea();
        let notch_left = (left.origin.x + left.size.width - frame.origin.x) as f32;
        let notch_right = (right.origin.x - frame.origin.x) as f32;
        if left.size.height > 0.0 && right.size.height > 0.0 && notch_right > notch_left {
            return Some((notch_left.max(0.0), notch_right.min(width)));
        }
    }

    if platform_active_screen_is_notched() {
        let fallback_width = 220.0_f32.min(width * 0.35);
        let center = width * 0.5;
        Some((center - fallback_width * 0.5, center + fallback_width * 0.5))
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_active_screen_notch_span() -> Option<(f32, f32)> {
    None
}

/// Remove the 1px titlebar separator macOS draws under the transparent titlebar. In fullscreen it
/// reads as a stray border across the top of the window; wezterm suppresses the same line.
pub fn macos_disable_titlebar_separator() {
    platform_disable_titlebar_separator();
}

/// Toggle the window drop shadow. Disabled in fullscreen so the shadow rim doesn't read as a border
/// around the screen-filling window (wezterm's `MACOS_FORCE_DISABLE_SHADOW`).
pub fn macos_set_window_shadow(enabled: bool) {
    platform_set_window_shadow(enabled);
}

#[cfg(target_os = "macos")]
fn platform_set_window_shadow(enabled: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if let Some(window) = app
        .keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
    {
        window.setHasShadow(enabled);
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_set_window_shadow(_enabled: bool) {}

#[cfg(target_os = "macos")]
fn platform_disable_titlebar_separator() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if let Some(window) = app
        .keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
    {
        window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
    }
}

#[cfg(not(target_os = "macos"))]
fn platform_disable_titlebar_separator() {}

pub fn macos_handles_non_native_fullscreen_frame(window: &WindowConfig) -> bool {
    window.non_native_fullscreen_enabled()
        && window.hides_macos_menu_bar_in_non_native_fullscreen()
        && platform_handles_macos_non_native_fullscreen_frame()
}

pub fn native_options_for_config(config: &BoottyConfig) -> eframe::NativeOptions {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(config.window.title.clone())
        .with_inner_size([config.window.width, config.window.height])
        .with_decorations(config.window.decorations_enabled())
        .with_fullscreen(config.window.native_fullscreen_enabled())
        .with_maximized(
            config.window.non_native_fullscreen_enabled()
                && !macos_handles_non_native_fullscreen_frame(&config.window),
        );
    viewport = apply_native_icon_to_viewport(viewport);

    viewport = match config.window.macos_titlebar_style {
        MacosTitlebarStyle::Native => viewport,
        MacosTitlebarStyle::Transparent => viewport
            .with_title_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true),
        MacosTitlebarStyle::Hidden => viewport
            .with_title_shown(false)
            .with_titlebar_buttons_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true),
    };

    eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport,
        ..Default::default()
    }
}

// macOS automatic window tabbing claims Cmd+T (newWindowForTab:) at the OS level before it reaches
// the app, which would shadow Bootty's new-tab shortcut. Opt out so the key reaches us. Must run
// before any window is created, since the class flag is read at window-creation time.
#[cfg(target_os = "macos")]
pub fn disable_automatic_window_tabbing() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn disable_automatic_window_tabbing() {}

#[cfg(target_os = "macos")]
pub fn new_tab_shortcut_trigger() -> &'static str {
    "cmd+t"
}

#[cfg(not(target_os = "macos"))]
pub fn new_tab_shortcut_trigger() -> &'static str {
    "ctrl+shift+t"
}

#[cfg(target_os = "macos")]
fn apply_native_icon_to_viewport(
    viewport: eframe::egui::ViewportBuilder,
) -> eframe::egui::ViewportBuilder {
    viewport.with_icon(eframe::egui::IconData::default())
}

#[cfg(not(target_os = "macos"))]
fn apply_native_icon_to_viewport(
    viewport: eframe::egui::ViewportBuilder,
) -> eframe::egui::ViewportBuilder {
    viewport.with_icon(crate::assets::native_app_icon_data())
}

#[cfg(target_os = "macos")]
fn platform_handles_macos_non_native_fullscreen_frame() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
fn platform_handles_macos_non_native_fullscreen_frame() -> bool {
    false
}

#[cfg(target_os = "macos")]
mod macos_clipboard {
    use std::path::PathBuf;

    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL};

    pub fn read_file_paths() -> Option<Vec<PathBuf>> {
        let pasteboard = NSPasteboard::generalPasteboard();
        let items = pasteboard.pasteboardItems()?;
        let mut paths = Vec::new();
        for index in 0..items.count() {
            let item = items.objectAtIndex(index);
            if let Some(url) = item.stringForType(unsafe { NSPasteboardTypeFileURL })
                && let Some(path) = path_from_file_url(&url.to_string())
            {
                paths.push(path);
            }
        }
        if paths.is_empty() { None } else { Some(paths) }
    }

    fn path_from_file_url(url: &str) -> Option<PathBuf> {
        let rest = url.strip_prefix("file://")?;
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        if !rest.starts_with('/') {
            return None;
        }
        Some(PathBuf::from(percent_decode(rest)?))
    }

    fn percent_decode(input: &str) -> Option<String> {
        let bytes = input.as_bytes();
        let mut decoded = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let hi = hex_value(*bytes.get(index + 1)?)?;
                let lo = hex_value(*bytes.get(index + 2)?)?;
                decoded.push((hi << 4) | lo);
                index += 3;
            } else {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(decoded).ok()
    }

    fn hex_value(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn file_url_paths_are_percent_decoded() {
            assert_eq!(
                path_from_file_url("file:///Users/me/Screen%20Shot%201.png"),
                Some(PathBuf::from("/Users/me/Screen Shot 1.png"))
            );
        }

        #[test]
        fn file_url_localhost_paths_are_supported() {
            assert_eq!(
                path_from_file_url("file://localhost/tmp/a%27b.png"),
                Some(PathBuf::from("/tmp/a'b.png"))
            );
        }

        #[test]
        fn non_file_urls_are_ignored() {
            assert_eq!(path_from_file_url("https://example.com/image.png"), None);
        }
    }
}

#[cfg(target_os = "macos")]
fn set_macos_non_native_fullscreen_presentation(enabled: bool) -> bool {
    macos_presentation::set_non_native_fullscreen(enabled)
}

#[cfg(not(target_os = "macos"))]
fn set_macos_non_native_fullscreen_presentation(_enabled: bool) -> bool {
    true
}

#[cfg(target_os = "macos")]
mod macos_presentation {
    use std::sync::Mutex;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationPresentationOptions, NSWindow, NSWindowStyleMask,
    };

    static SAVED_PRESENTATION_OPTIONS: Mutex<Option<usize>> = Mutex::new(None);
    static SAVED_WINDOW_STATE: Mutex<Option<WindowState>> = Mutex::new(None);

    #[derive(Clone, Copy, Debug)]
    struct WindowState {
        frame: WindowFrame,
        style_mask: usize,
        movable: bool,
        movable_by_window_background: bool,
    }

    #[derive(Clone, Copy, Debug)]
    struct WindowFrame {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    pub fn set_non_native_fullscreen(enabled: bool) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let app = NSApplication::sharedApplication(mtm);

        if enabled {
            // Measure the notch while the menu bar is still present; auto-hiding it below makes the
            // screen report a zero safe area, so the cached value is what the layout reads later.
            super::platform_active_screen_notch_height();
            let mut saved = SAVED_PRESENTATION_OPTIONS
                .lock()
                .expect("lock presentation options");
            if saved.is_none() {
                *saved = Some(app.presentationOptions().bits());
            }
            app.setPresentationOptions(
                NSApplicationPresentationOptions::AutoHideDock
                    | NSApplicationPresentationOptions::AutoHideMenuBar,
            );
            if let Some(window) = active_window(&app) {
                let mut saved_state = SAVED_WINDOW_STATE.lock().expect("lock window state");
                if saved_state.is_none() {
                    *saved_state = Some(WindowState::from_window(&window));
                }
                // Drop Titled so the window has no frame border (the 1px outline at the screen
                // edge). winit overrides canBecomeKeyWindow, so a borderless window keeps focus.
                let mut style_mask = window.styleMask();
                style_mask.remove(
                    NSWindowStyleMask::Resizable
                        | NSWindowStyleMask::Miniaturizable
                        | NSWindowStyleMask::Titled,
                );
                window.setStyleMask(style_mask);
                window.setMovable(false);
                window.setMovableByWindowBackground(false);
                if let Some(screen) = window.screen() {
                    window.setFrame_display(screen.frame(), true);
                }
                return true;
            }
            return false;
        }

        if let Some(options) = SAVED_PRESENTATION_OPTIONS
            .lock()
            .expect("lock presentation options")
            .take()
        {
            app.setPresentationOptions(NSApplicationPresentationOptions::from_bits_retain(options));
        }
        if let Some(state) = SAVED_WINDOW_STATE.lock().expect("lock window state").take()
            && let Some(window) = active_window(&app)
        {
            state.restore(&window);
        }
        true
    }

    fn active_window(app: &NSApplication) -> Option<objc2::rc::Retained<NSWindow>> {
        app.keyWindow()
            .or_else(|| app.mainWindow())
            .or_else(|| app.windows().firstObject())
    }

    impl WindowState {
        fn from_window(window: &NSWindow) -> Self {
            Self {
                frame: WindowFrame::from(window.frame()),
                style_mask: window.styleMask().bits(),
                movable: window.isMovable(),
                movable_by_window_background: window.isMovableByWindowBackground(),
            }
        }

        fn restore(self, window: &NSWindow) {
            window.setStyleMask(NSWindowStyleMask::from_bits_retain(self.style_mask));
            window.setMovable(self.movable);
            window.setMovableByWindowBackground(self.movable_by_window_background);
            window.setFrame_display(self.frame.into(), true);
        }
    }

    impl From<objc2_foundation::NSRect> for WindowFrame {
        fn from(rect: objc2_foundation::NSRect) -> Self {
            Self {
                x: rect.origin.x,
                y: rect.origin.y,
                width: rect.size.width,
                height: rect.size.height,
            }
        }
    }

    impl From<WindowFrame> for objc2_foundation::NSRect {
        fn from(frame: WindowFrame) -> Self {
            Self::new(
                objc2_foundation::NSPoint::new(frame.x, frame.y),
                objc2_foundation::NSSize::new(frame.width, frame.height),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    #[test]
    fn native_options_use_configured_window_settings() {
        let mut config = BoottyConfig::default();
        config.window.title = "Agent Shell".to_owned();
        config.window.width = 900.0;
        config.window.height = 700.0;
        config.window.fullscreen = crate::config::WindowFullscreen::NonNative;
        config.window.window_decoration = crate::config::WindowDecoration::None;
        config.window.macos_titlebar_style = MacosTitlebarStyle::Hidden;

        let options = native_options_for_config(&config);

        assert_eq!(options.viewport.title.as_deref(), Some("Agent Shell"));
        assert_eq!(
            options.viewport.inner_size.unwrap(),
            egui::vec2(900.0, 700.0)
        );
        assert_eq!(options.viewport.fullscreen, Some(false));
        assert_eq!(options.viewport.maximized, Some(!cfg!(target_os = "macos")));
        assert_eq!(options.viewport.decorations, Some(false));
        assert_eq!(options.viewport.title_shown, Some(false));
        assert_eq!(options.viewport.titlebar_buttons_shown, Some(false));
        assert!(viewport_has_expected_platform_icon(&options.viewport));
    }

    #[test]
    fn transparent_macos_titlebar_keeps_buttons_but_hides_native_title() {
        let mut config = BoottyConfig::default();
        config.window.macos_titlebar_style = MacosTitlebarStyle::Transparent;

        let options = native_options_for_config(&config);

        assert_eq!(options.viewport.title_shown, Some(false));
        assert_eq!(options.viewport.titlebar_shown, Some(false));
        assert_eq!(options.viewport.titlebar_buttons_shown, None);
    }

    #[test]
    fn native_icon_uses_platform_specific_icon_artwork() {
        let config = BoottyConfig::default();
        let options = native_options_for_config(&config);

        assert!(viewport_has_expected_platform_icon(&options.viewport));
        if !cfg!(target_os = "macos") {
            let icon = options.viewport.icon.expect("native app icon");
            assert_eq!((icon.width, icon.height), (256, 256));
        }
    }

    fn viewport_has_expected_platform_icon(viewport: &egui::ViewportBuilder) -> bool {
        if cfg!(target_os = "macos") {
            viewport
                .icon
                .as_deref()
                .is_some_and(|icon| *icon == egui::IconData::default())
        } else {
            viewport.icon.is_some()
        }
    }
}
