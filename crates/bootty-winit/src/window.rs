use eframe::egui::CursorIcon;

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

#[cfg(target_os = "macos")]
use objc2::runtime::NSObjectProtocol;
#[cfg(target_os = "macos")]
use objc2::{MainThreadMarker, sel};
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSApplication, NSCursor, NSScreen, NSTitlebarSeparatorStyle, NSWindow};

#[cfg(target_os = "macos")]
static MACOS_CURSOR_ICON: AtomicU8 = AtomicU8::new(0);

#[cfg(target_os = "macos")]
fn active_window(app: &NSApplication) -> Option<objc2::rc::Retained<NSWindow>> {
    app.keyWindow()
        .or_else(|| app.mainWindow())
        .or_else(|| app.windows().firstObject())
}

#[cfg(target_os = "macos")]
fn with_active_window(action: impl FnOnce(&NSWindow)) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Some(window) = active_window(&NSApplication::sharedApplication(mtm)) {
        action(&window);
    }
}

#[cfg(target_os = "macos")]
pub fn set_macos_cursor_icon(icon: CursorIcon) {
    MACOS_CURSOR_ICON.store(
        match icon {
            CursorIcon::Text => 1,
            CursorIcon::PointingHand => 2,
            CursorIcon::ResizeHorizontal => 3,
            CursorIcon::ResizeVertical => 4,
            CursorIcon::Default => 5,
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
        5 => NSCursor::arrowCursor(),
        _ => return,
    };
    cursor.set();
}

#[cfg(not(target_os = "macos"))]
pub fn reapply_macos_cursor_icon() {}

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
    if let Some(screen) = active_window(&app).and_then(|window| window.screen())
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
    let Some(screen) = active_window(&app).and_then(|window| window.screen()) else {
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
    let screen = active_window(&app).and_then(|window| window.screen())?;
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
    with_active_window(|window| window.setHasShadow(enabled));
}

#[cfg(not(target_os = "macos"))]
fn platform_set_window_shadow(_enabled: bool) {}

#[cfg(target_os = "macos")]
fn platform_disable_titlebar_separator() {
    with_active_window(|window| {
        window.setTitlebarSeparatorStyle(NSTitlebarSeparatorStyle::None);
    });
}

#[cfg(not(target_os = "macos"))]
fn platform_disable_titlebar_separator() {}

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

/// Apply the raw native presentation state for app-owned non-native fullscreen.
pub fn set_macos_non_native_fullscreen(enabled: bool) -> bool {
    platform_set_macos_non_native_fullscreen(enabled)
}

/// Restore the raw native presentation state saved before non-native fullscreen.
pub fn restore_macos_presentation() -> bool {
    platform_set_macos_non_native_fullscreen(false)
}

/// Resize app-owned non-native fullscreen to the active screen after display geometry changes.
pub fn refresh_macos_non_native_fullscreen_frame() {
    platform_refresh_macos_non_native_fullscreen_frame();
}

/// Whether this platform adapter owns the non-native fullscreen frame.
pub fn handles_macos_non_native_fullscreen_frame() -> bool {
    cfg!(target_os = "macos")
}

#[cfg(target_os = "macos")]
fn platform_set_macos_non_native_fullscreen(enabled: bool) -> bool {
    macos_presentation::set_non_native_fullscreen(enabled)
}

#[cfg(not(target_os = "macos"))]
fn platform_set_macos_non_native_fullscreen(_enabled: bool) -> bool {
    true
}

#[cfg(target_os = "macos")]
fn platform_refresh_macos_non_native_fullscreen_frame() {
    macos_presentation::refresh_non_native_fullscreen_frame();
}

#[cfg(not(target_os = "macos"))]
fn platform_refresh_macos_non_native_fullscreen_frame() {}

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

    #[derive(Clone, Copy, Debug, PartialEq)]
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
            if let Some(window) = super::active_window(&app) {
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
                fill_active_screen(&window);
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
            && let Some(window) = super::active_window(&app)
        {
            state.restore(&window);
        }
        true
    }

    pub fn refresh_non_native_fullscreen_frame() {
        if SAVED_WINDOW_STATE
            .lock()
            .expect("lock window state")
            .is_none()
        {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let Some(window) = super::active_window(&app) else {
            return;
        };
        fill_active_screen(&window);
    }

    fn fill_active_screen(window: &NSWindow) {
        let Some(screen) = window.screen() else {
            return;
        };
        let screen_frame = WindowFrame::from(screen.frame());
        if WindowFrame::from(window.frame()) != screen_frame {
            window.setFrame_display(screen_frame.into(), true);
        }
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
