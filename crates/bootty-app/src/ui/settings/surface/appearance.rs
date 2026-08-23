use bootty_config::config::CursorStyleConfig;
use eframe::egui;

use super::SettingsSurface;

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "CURSOR");
    cursor_style(win, ui);
    super::settings_toggle_row(
        ui,
        palette,
        "Blink",
        "Allow the cursor to blink in the focused pane.",
        win.config.cursor.blink.unwrap_or(true),
        |enabled| {
            win.config.cursor.blink = Some(enabled);
            win.writeback.set_bool(&["cursor", "blink"], enabled);
        },
    );
    win.setting(ui, "cursor.dim-inactive-pane");

    super::section(ui, palette, "MOUSE POINTER");
    super::settings_toggle_row(
        ui,
        palette,
        "Hide while typing",
        "Hide the mouse pointer while typing until the pointer moves.",
        win.config.input.hide_mouse_pointer_while_typing,
        |enabled| {
            win.config.input.hide_mouse_pointer_while_typing = enabled;
            win.writeback
                .set_bool(&["input", "hide-mouse-pointer-while-typing"], enabled);
        },
    );

    super::section(ui, palette, "FULLSCREEN NOTCH");
    super::settings_toggle_row(
        ui,
        palette,
        "Use black notch chrome",
        "In dark mode on notched fullscreen displays, paint sidebar, status bar, and split dividers solid black.",
        win.config.chrome.notched_fullscreen_black_chrome,
        |enabled| {
            win.config.chrome.notched_fullscreen_black_chrome = enabled;
            win.writeback
                .set_bool(&["chrome", "notched-fullscreen-black-chrome"], enabled);
        },
    );
}

fn cursor_style(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::settings_row(
        ui,
        palette,
        "Style",
        "Default cursor shape used when an application resets the cursor.",
        |ui| {
            let labels = ["Block", "Bar", "Underline", "Hollow block"];
            let current = match win.config.cursor.style.unwrap_or(CursorStyleConfig::Block) {
                CursorStyleConfig::Block => 0,
                CursorStyleConfig::Bar => 1,
                CursorStyleConfig::Underline => 2,
                CursorStyleConfig::HollowBlock => 3,
            };
            if let Some(index) = super::settings_segmented(ui, palette, &labels, current) {
                let style = match index {
                    0 => CursorStyleConfig::Block,
                    1 => CursorStyleConfig::Bar,
                    2 => CursorStyleConfig::Underline,
                    _ => CursorStyleConfig::HollowBlock,
                };
                win.config.cursor.style = Some(style);
                win.writeback.set_str(
                    &["cursor", "style"],
                    match style {
                        CursorStyleConfig::Block => "block",
                        CursorStyleConfig::Bar => "bar",
                        CursorStyleConfig::Underline => "underline",
                        CursorStyleConfig::HollowBlock => "hollow-block",
                    },
                );
            }
        },
    );
}
