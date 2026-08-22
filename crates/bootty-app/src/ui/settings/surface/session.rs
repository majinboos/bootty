use std::path::PathBuf;

use bootty_terminal::terminal_engine::NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
use eframe::egui;

use super::SettingsSurface;

const MAX_SCROLLBACK_ROWS: i64 = 10_000_000;

fn scrollback_rows(bytes: usize) -> i64 {
    bytes.div_ceil(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE) as i64
}

fn scrollback_bytes(rows: i64) -> usize {
    (rows.max(0) as usize).saturating_mul(NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE)
}

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;

    super::section(ui, palette, "SHELL");
    super::settings_row(
        ui,
        palette,
        "Shell",
        "Empty uses the macOS account login shell. Applies to new sessions.",
        |ui| {
            let mut shell = win.config.session.shell.clone().unwrap_or_default();
            if super::settings_text_edit(ui, palette, &mut shell, "default login shell").changed() {
                win.config.session.shell = super::nonempty(&shell);
                super::write_optional_text(&mut win.writeback, &["session", "shell"], &shell);
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "Working directory",
        "Empty starts new sessions in your home directory.",
        |ui| {
            let mut cwd = win
                .config
                .session
                .working_directory
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            if super::settings_text_edit(ui, palette, &mut cwd, "inherit from launcher").changed() {
                win.config.session.working_directory = super::nonempty(&cwd).map(PathBuf::from);
                super::write_optional_text(
                    &mut win.writeback,
                    &["session", "working-directory"],
                    &cwd,
                );
            }
        },
    );

    super::section(ui, palette, "TERMINAL IDENTITY");
    super::settings_row(
        ui,
        palette,
        "TERM",
        "Advertised terminal type for new shells.",
        |ui| {
            let mut term = win.config.session.term.clone();
            if super::settings_text_edit(ui, palette, &mut term, "xterm-256color").changed() {
                win.config.session.term = term.clone();
                super::write_optional_text(&mut win.writeback, &["session", "term"], &term);
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "COLORTERM",
        "Advertised color capability for new shells.",
        |ui| {
            let mut colorterm = win.config.session.colorterm.clone();
            if super::settings_text_edit(ui, palette, &mut colorterm, "truecolor").changed() {
                win.config.session.colorterm = colorterm.clone();
                super::write_optional_text(
                    &mut win.writeback,
                    &["session", "colorterm"],
                    &colorterm,
                );
            }
        },
    );
    super::settings_row(
        ui,
        palette,
        "Max scrollback",
        "Lines retained per pane. 0 disables scrollback. Applies to new panes.",
        |ui| {
            let mut rows = scrollback_rows(win.config.session.max_scrollback);
            if ui
                .add(
                    egui::DragValue::new(&mut rows)
                        .speed(1_000.0)
                        .range(0..=MAX_SCROLLBACK_ROWS),
                )
                .changed()
            {
                let bytes = scrollback_bytes(rows);
                win.config.session.max_scrollback = bytes;
                win.writeback
                    .set_i64(&["session", "max-scrollback"], bytes as i64);
            }
        },
    );
    super::settings_toggle_row(
        ui,
        palette,
        "Glyph protocol",
        "Expose terminal image/glyph protocol support to new sessions.",
        win.config.session.glyph_protocol,
        |enabled| {
            win.config.session.glyph_protocol = enabled;
            win.writeback
                .set_bool(&["session", "glyph-protocol"], enabled);
        },
    );

    super::section(ui, palette, "ENVIRONMENT");
    super::settings_notice(
        ui,
        palette.muted,
        "Extra variables exported to every new shell. Incomplete rows are ignored while editing.",
    );
    ui.add_space(6.0);

    let mut env = win
        .session_env
        .take()
        .unwrap_or_else(|| win.config.session.env.clone());
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, (name, value)) in env.iter_mut().enumerate() {
        super::settings_row(ui, palette, "Variable", "NAME=value", |ui| {
            if super::settings_text_edit(ui, palette, name, "NAME").changed() {
                changed = true;
            }
            ui.label("=");
            if super::settings_text_edit(ui, palette, value, "value").changed() {
                changed = true;
            }
            if ui.small_button("Remove").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        env.remove(index);
        changed = true;
    }
    if super::settings_button(ui, palette, "+ Add variable").clicked() {
        env.push((String::new(), String::new()));
        changed = true;
    }
    if changed {
        // Incomplete pairs stay in the editor draft; only complete ones reach the document.
        let valid: Vec<(String, String)> = env
            .iter()
            .filter(|(name, _)| !name.trim().is_empty())
            .cloned()
            .collect();
        if valid.is_empty() {
            win.writeback.remove(&["session", "env"]);
        } else {
            win.writeback.set_env(&["session", "env"], &valid);
        }
    }
    win.session_env = Some(env);
}
