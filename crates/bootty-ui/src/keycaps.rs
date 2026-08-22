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

/// Shared control height for a chord recorder and the fields beside it, so a row lines up exactly.
pub const RECORD_CELL_HEIGHT: f32 = 36.0;

/// The clickable shortcut cell: the bound combo as keycaps, or a pulsing prompt while capturing.
/// Clicking it toggles capture, which is why it reports a click rather than owning the state.
pub fn record_cell(
    ui: &mut egui::Ui,
    palette: crate::ThemePalette,
    trigger: &str,
    recording: bool,
    capture_text: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(220.0, RECORD_CELL_HEIGHT),
        egui::Sense::click(),
    );
    let radius = egui::CornerRadius::same(palette.radius);
    let text_pos = rect.left_center() + egui::vec2(10.0, 0.0);
    if recording {
        // The glow has to animate, so the cell asks for the next frame itself.
        ui.ctx().request_repaint();
        let pulse = (ui.input(|input| input.time) * 3.0).sin() * 0.5 + 0.5;
        let glow = egui::Color32::from_rgba_unmultiplied(
            palette.primary.r(),
            palette.primary.g(),
            palette.primary.b(),
            (pulse * 90.0) as u8 + 45,
        );
        ui.painter().rect_filled(rect, radius, palette.mantle);
        ui.painter().rect_filled(rect, radius, glow);
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, palette.primary),
            egui::StrokeKind::Inside,
        );
        // A prompt is prose; anything else is a partial chord and belongs in keycaps.
        if capture_text.starts_with("Press keys") {
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                capture_text,
                egui::FontId::proportional(12.0),
                palette.text,
            );
        } else {
            paint_trigger(ui, rect, palette, capture_text);
        }
    } else {
        let fill = if response.hovered() {
            palette.hover
        } else {
            palette.mantle
        };
        ui.painter().rect_filled(rect, radius, fill);
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, palette.border),
            egui::StrokeKind::Inside,
        );
        if trigger.trim().is_empty() {
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                "Click to record",
                egui::FontId::proportional(12.0),
                palette.muted,
            );
        } else {
            paint_trigger(ui, rect, palette, trigger);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response.on_hover_text(if recording {
        "Recording — press keys or scroll, Esc cancels"
    } else {
        "Click to record a shortcut"
    })
}

fn paint_trigger(ui: &egui::Ui, rect: egui::Rect, palette: crate::ThemePalette, trigger: &str) {
    let galley = trigger_galley_from_painter(
        ui.painter(),
        palette,
        trigger,
        palette.text,
        rect.width() - 20.0,
    );
    let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
    ui.painter().galley(pos, galley, palette.text);
}

/// The record indicator beside a [`record_cell`]: a red ball at rest, a red square while capturing.
pub fn record_dot(
    ui: &mut egui::Ui,
    palette: crate::ThemePalette,
    recording: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(RECORD_CELL_HEIGHT), egui::Sense::click());
    let center = rect.center();
    let red = palette.destructive;
    if recording {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(center, egui::Vec2::splat(12.0)),
            egui::CornerRadius::same(3),
            red,
        );
    } else {
        ui.painter().circle_filled(center, 7.0, red);
        if response.hovered() {
            ui.painter()
                .circle_stroke(center, 10.0, egui::Stroke::new(1.5, red));
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(if recording {
        "Stop recording"
    } else {
        "Record shortcut"
    })
}

/// A read-only keycap pill, for listing a shortcut rather than editing it.
pub fn chip(ui: &mut egui::Ui, palette: crate::ThemePalette, trigger: &str) {
    egui::Frame::NONE
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            let galley = trigger_galley(ui, palette, trigger, palette.text, 320.0);
            ui.add(egui::Label::new(galley).selectable(false));
        });
}
