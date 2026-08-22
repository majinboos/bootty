use eframe::egui;
use std::borrow::Cow;

/// Lay a trigger out as keycaps. On macOS the modifier symbols come from the icon font (the UI font
/// has no command/option/control glyphs in some themes), elsewhere modifiers fall back to text
/// joined with `+`.
pub fn trigger_galley(
    ui: &egui::Ui,
    palette: crate::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    trigger_galley_from_painter(ui.painter(), palette, trigger, color, max_width)
}

pub fn trigger_galley_from_painter(
    painter: &egui::Painter,
    palette: crate::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> std::sync::Arc<egui::Galley> {
    painter.layout_job(trigger_layout_job(palette, trigger, color, max_width))
}

fn trigger_layout_job(
    palette: crate::ThemePalette,
    trigger: &str,
    color: egui::Color32,
    max_width: f32,
) -> egui::text::LayoutJob {
    let mut job = one_line_job(max_width);
    append_trigger(&mut job, palette, trigger, color);
    job
}

pub struct InlineShortcut<'a> {
    pub prefix: &'a str,
    pub trigger: &'a str,
    pub suffix: &'a str,
}

pub fn inline_shortcut_galley_from_painter(
    painter: &egui::Painter,
    palette: crate::ThemePalette,
    shortcut: InlineShortcut<'_>,
    color: egui::Color32,
    max_width: f32,
    text_size: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = one_line_job(max_width);
    append_text(
        &mut job,
        shortcut.prefix,
        0.0,
        egui::FontId::proportional(text_size),
        color,
    );
    append_trigger(&mut job, palette, shortcut.trigger, color);
    append_text(
        &mut job,
        shortcut.suffix,
        3.0,
        egui::FontId::proportional(text_size),
        color,
    );
    painter.layout_job(job)
}

pub fn shortcut_hint_galley_from_painter(
    painter: &egui::Painter,
    palette: crate::ThemePalette,
    sections: &[(&str, &str)],
    color: egui::Color32,
    max_width: f32,
    text_size: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = one_line_job(max_width);
    for (index, (trigger, label)) in sections.iter().enumerate() {
        if index > 0 {
            append_text(
                &mut job,
                "   ",
                0.0,
                egui::FontId::proportional(text_size),
                color,
            );
        }
        append_trigger(&mut job, palette, trigger, color);
        append_text(
            &mut job,
            " ",
            2.0,
            egui::FontId::proportional(text_size),
            color,
        );
        append_text(
            &mut job,
            label,
            0.0,
            egui::FontId::proportional(text_size),
            color,
        );
    }
    painter.layout_job(job)
}

fn one_line_job(max_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job
}

fn append_trigger(
    job: &mut egui::text::LayoutJob,
    palette: crate::ThemePalette,
    trigger: &str,
    color: egui::Color32,
) {
    let mut first_combo = true;
    for combo in trigger.split('>') {
        let combo = combo.trim();
        if combo.is_empty() {
            continue;
        }
        if !first_combo {
            append_combo_separator(job, palette);
        }
        first_combo = false;
        append_combo(job, palette, combo, color);
    }
}

fn append_text(
    job: &mut egui::text::LayoutJob,
    text: &str,
    leading_space: f32,
    font_id: egui::FontId,
    color: egui::Color32,
) {
    job.append(
        text,
        leading_space,
        egui::text::TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
}

fn append_combo_separator(job: &mut egui::text::LayoutJob, palette: crate::ThemePalette) {
    if let Some((glyph, family)) = crate::icons::icon_glyph("chevron-right") {
        job.append(
            &glyph.to_string(),
            5.0,
            egui::text::TextFormat {
                font_id: egui::FontId::new(12.0, egui::FontFamily::Name(family.into())),
                color: palette.muted,
                ..Default::default()
            },
        );
        append_text(
            job,
            " ",
            5.0,
            egui::FontId::proportional(12.0),
            palette.muted,
        );
    }
}

fn append_combo(
    job: &mut egui::text::LayoutJob,
    palette: crate::ThemePalette,
    combo: &str,
    color: egui::Color32,
) {
    use egui::text::TextFormat;
    let macos = cfg!(target_os = "macos");
    for (index, token) in combo
        .split('+')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .enumerate()
    {
        let leading = if index == 0 { 0.0 } else { 3.0 };
        if macos
            && let Some((glyph, family)) = modifier_icon(token).and_then(crate::icons::icon_glyph)
        {
            let glyph_leading = if let Some(side) = modifier_side_label(token) {
                job.append(
                    side,
                    leading,
                    TextFormat {
                        font_id: egui::FontId::monospace(9.0),
                        color: palette.muted,
                        ..Default::default()
                    },
                );
                1.0
            } else {
                leading
            };
            job.append(
                &glyph.to_string(),
                glyph_leading,
                TextFormat {
                    font_id: egui::FontId::new(15.0, egui::FontFamily::Name(family.into())),
                    color,
                    ..Default::default()
                },
            );
            continue;
        }
        if !macos && index > 0 {
            job.append(
                "+",
                2.0,
                TextFormat {
                    font_id: egui::FontId::proportional(12.0),
                    color: palette.muted,
                    ..Default::default()
                },
            );
        }
        let label = key_label(token);
        job.append(
            label.as_ref(),
            if macos { leading } else { leading.min(2.0) },
            TextFormat {
                font_id: egui::FontId::monospace(13.0),
                color,
                ..Default::default()
            },
        );
    }
}

/// Icon-font slug for a modifier token, so modifier symbols render from the icon font instead of
/// relying on the UI font.
fn modifier_icon(token: &str) -> Option<&'static str> {
    match token {
        "cmd" | "super" | "left_cmd" | "left_super" | "right_cmd" | "right_super" => {
            Some("command")
        }
        "alt" | "option" | "left_alt" | "left_option" | "right_alt" | "right_option" => {
            Some("option")
        }
        "shift" | "left_shift" | "right_shift" => Some("arrow-big-up"),
        "ctrl" | "control" | "left_ctrl" | "left_control" | "right_ctrl" | "right_control" => {
            Some("chevron-up")
        }
        _ => None,
    }
}

fn modifier_side_label(token: &str) -> Option<&'static str> {
    if token.starts_with("left_") {
        Some("L")
    } else if token.starts_with("right_") {
        Some("R")
    } else {
        None
    }
}

fn key_label(token: &str) -> Cow<'_, str> {
    match token {
        "cmd" | "super" => "Cmd".into(),
        "left_cmd" | "left_super" => "LCmd".into(),
        "right_cmd" | "right_super" => "RCmd".into(),
        "ctrl" | "control" => "Ctrl".into(),
        "left_ctrl" | "left_control" => "LCtrl".into(),
        "right_ctrl" | "right_control" => "RCtrl".into(),
        "alt" | "option" => "Alt".into(),
        "left_alt" | "left_option" => "LAlt".into(),
        "right_alt" | "right_option" => "RAlt".into(),
        "shift" => "Shift".into(),
        "left_shift" => "LShift".into(),
        "right_shift" => "RShift".into(),
        "enter" | "return" => "Enter".into(),
        "esc" | "escape" => "Esc".into(),
        "space" => "Space".into(),
        "scroll_up" => "Scroll ↑".into(),
        "scroll_down" => "Scroll ↓".into(),
        other if other.chars().count() == 1 => other.to_uppercase().into(),
        other => other.into(),
    }
}
