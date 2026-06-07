use anyhow::Result;

use crate::config::{BoottyConfig, MacosTitlebarStyle, WindowConfig};

pub fn read_clipboard_text() -> Result<Option<String>> {
    let mut clipboard = arboard::Clipboard::new()?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Ok(Some(text)),
        Ok(_) | Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn spawn_new_window() -> Result<()> {
    std::process::Command::new(std::env::current_exe()?).spawn()?;
    Ok(())
}

pub fn apply_macos_non_native_fullscreen_presentation(window: &WindowConfig) {
    set_macos_non_native_fullscreen_presentation(
        window.non_native_fullscreen_enabled()
            && window.hides_macos_menu_bar_in_non_native_fullscreen(),
    );
}

pub fn restore_macos_presentation() {
    set_macos_non_native_fullscreen_presentation(false);
}

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

    viewport = match config.window.macos_titlebar_style {
        MacosTitlebarStyle::Native => viewport,
        MacosTitlebarStyle::Transparent => viewport
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true),
        MacosTitlebarStyle::Tabs => viewport
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

#[cfg(target_os = "macos")]
fn platform_handles_macos_non_native_fullscreen_frame() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
fn platform_handles_macos_non_native_fullscreen_frame() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn set_macos_non_native_fullscreen_presentation(enabled: bool) {
    macos_presentation::set_non_native_fullscreen(enabled);
}

#[cfg(not(target_os = "macos"))]
fn set_macos_non_native_fullscreen_presentation(_enabled: bool) {}

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

    pub fn set_non_native_fullscreen(enabled: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);

        if enabled {
            let mut saved = SAVED_PRESENTATION_OPTIONS
                .lock()
                .expect("lock presentation options");
            if saved.is_none() {
                *saved = Some(app.presentationOptions().bits());
            }
            app.setPresentationOptions(
                NSApplicationPresentationOptions::HideDock
                    | NSApplicationPresentationOptions::HideMenuBar,
            );
            if let Some(window) = active_window(&app) {
                let mut saved_state = SAVED_WINDOW_STATE.lock().expect("lock window state");
                if saved_state.is_none() {
                    *saved_state = Some(WindowState::from_window(&window));
                }
                let mut style_mask = window.styleMask();
                style_mask.remove(NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable);
                window.setStyleMask(style_mask);
                window.setMovable(false);
                window.setMovableByWindowBackground(false);
                if let Some(screen) = window.screen() {
                    window.setFrame_display(screen.frame(), true);
                }
            }
            return;
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
    }
}
