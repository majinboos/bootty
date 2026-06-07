use bootty_ui::ThemePalette;
use eframe::egui::{self, Pos2, Rect, RichText, Stroke};

use crate::{
    config::ChromeConfig,
    diagnostics::{StatusMetrics, us_to_ms},
    mux::{
        config::MuxBackendKind,
        snapshot::{MuxSession, MuxSnapshot, MuxWindow},
    },
    strings::truncate_label,
};

#[derive(Clone, Debug)]
pub struct StatusBarModel<'a> {
    pub backend: MuxBackendKind,
    pub selected_session_name: Option<&'a str>,
    pub metrics: StatusMetrics,
    pub last_error: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct SidebarModel<'a> {
    pub sessions: &'a [MuxSession],
    pub selected_session: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct WindowTabsModel<'a> {
    pub windows: &'a [MuxWindow],
    pub selected_window: Option<&'a str>,
}

pub fn show_status_bar(ui: &mut egui::Ui, palette: ThemePalette, model: StatusBarModel<'_>) {
    egui::Frame::NONE.fill(palette.base).show(ui, |ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), 30.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add_space(8.0);
                ui.label(RichText::new("Bootty").color(palette.text).strong());
                ui.separator();
                ui.label(
                    RichText::new(format!("backend: {}", backend_label(model.backend)))
                        .color(palette.subtext),
                );
                ui.separator();
                let target = model.selected_session_name.unwrap_or("no mux session");
                ui.label(RichText::new(format!("active: {target}")).color(palette.subtext));
                ui.separator();
                let metrics = model.metrics;
                ui.label(
                    RichText::new(format!("{}×{}", metrics.cols, metrics.rows))
                        .color(palette.muted),
                );
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "drain {:.2}ms/{}b · update {:.2}ms · extract {:.2}ms · paint {:.2}ms · {} runs",
                        us_to_ms(metrics.drain.elapsed_us),
                        metrics.drain.bytes,
                        us_to_ms(metrics.renderer.render_state_update_us),
                        us_to_ms(metrics.renderer.frame_extraction_us),
                        us_to_ms(metrics.renderer.paint_us),
                        metrics.renderer.text_runs
                    ))
                    .color(palette.muted),
                );
                if let Some(error) = model.last_error {
                    ui.separator();
                    ui.colored_label(palette.warning, truncate_label(error, 80));
                }
            },
        );
    });
}

pub fn show_sidebar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    model: SidebarModel<'_>,
) -> Option<String> {
    let width = 286.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.mantle);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(1.0, palette.surface),
        egui::StrokeKind::Inside,
    );

    let header_h = 44.0;
    let footer_h = 58.0;
    let row_h = 30.0;
    let pad_x = 14.0;

    painter.text(
        Pos2::new(rect.min.x + pad_x, rect.min.y + header_h * 0.5),
        egui::Align2::LEFT_CENTER,
        "",
        egui::FontId::monospace(17.0),
        palette.primary,
    );
    painter.text(
        Pos2::new(rect.min.x + pad_x + 28.0, rect.min.y + header_h * 0.5),
        egui::Align2::LEFT_CENTER,
        "mux sessions",
        egui::FontId::monospace(14.0),
        palette.text,
    );
    painter.text(
        Pos2::new(rect.max.x - pad_x, rect.min.y + header_h * 0.5),
        egui::Align2::RIGHT_CENTER,
        model.sessions.len().to_string(),
        egui::FontId::monospace(13.0),
        palette.muted,
    );

    let list_top = rect.min.y + header_h;
    let list_bottom = (rect.max.y - footer_h).max(list_top);
    if model.sessions.is_empty() {
        painter.text(
            Pos2::new(rect.center().x, list_top + 42.0),
            egui::Align2::CENTER_CENTER,
            "no mux sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
    }

    let mut activated = None;
    let max_rows = ((list_bottom - list_top) / row_h).floor().max(0.0) as usize;
    for (index, session) in model.sessions.iter().take(max_rows).enumerate() {
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, list_top + index as f32 * row_h),
            egui::vec2(width, row_h),
        );
        let is_selected = model.selected_session == Some(session.id.as_str())
            || model.selected_session == Some(session.name.as_str());
        if session_row(ui, row_rect, index, session, is_selected, palette).clicked() {
            activated = Some(session.id.clone());
        }
    }

    paint_sidebar_footer(ui, rect, footer_h, palette);
    activated
}

pub fn sidebar_rect(rect: Rect, chrome: &ChromeConfig) -> Rect {
    let width = if chrome.sidebar {
        chrome.sidebar_width
    } else {
        0.0
    };
    Rect::from_min_max(
        rect.min,
        Pos2::new((rect.min.x + width).min(rect.max.x), rect.max.y),
    )
}

pub fn show_window_tabs(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: WindowTabsModel<'_>,
) -> Option<String> {
    let height = 34.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.base);
    painter.line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, palette.surface),
    );

    let mut activated = None;
    let mut x = rect.min.x + 8.0;
    for window in model.windows {
        let label = format!("{}:{}", window.index, truncate_label(&window.name, 18));
        let width = (label.chars().count() as f32 * 8.0 + 28.0).clamp(56.0, 180.0);
        if x + width > rect.max.x - 8.0 {
            break;
        }
        let tab_rect = Rect::from_min_size(
            Pos2::new(x, rect.min.y + 5.0),
            egui::vec2(width, height - 10.0),
        );
        let selected = model.selected_window == Some(window.id.as_str())
            || (model.selected_window.is_none() && window.active);
        if window_tab(ui, tab_rect, window, &label, selected, palette).clicked() {
            activated = Some(window.id.clone());
        }
        x += width + 6.0;
    }
    activated
}

pub fn selected_session_name<'a>(
    sessions: &'a [MuxSession],
    selected_session: Option<&str>,
) -> Option<&'a str> {
    let selected = selected_session?;
    sessions
        .iter()
        .find(|session| session.id == selected || session.name == selected)
        .map(|session| session.name.as_str())
}

pub fn selection_after_refresh(current: Option<String>, snapshot: &MuxSnapshot) -> Option<String> {
    current.or_else(|| {
        snapshot
            .sessions
            .iter()
            .find(|session| session.active)
            .or_else(|| snapshot.sessions.first())
            .map(|session| session.id.clone())
    })
}

fn session_row(
    ui: &mut egui::Ui,
    rect: Rect,
    index: usize,
    session: &MuxSession,
    is_selected: bool,
    palette: ThemePalette,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-session-row", &session.id)),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let hover = response.hovered();
        let bg = if is_selected {
            palette.surface
        } else if hover {
            palette.hover
        } else {
            palette.mantle
        };
        painter.rect_filled(rect, 0.0, bg);

        if is_selected {
            let bar = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + 4.0, rect.max.y));
            painter.rect_filled(bar, 0.0, palette.primary);
        }

        let number = index + 1;
        let badge = format!("{number}");
        let name_color = if is_selected {
            palette.text
        } else {
            palette.subtext
        };
        let meta_color = if is_selected {
            palette.muted
        } else {
            palette.border
        };
        let x = rect.min.x + 12.0;
        let y = rect.center().y;

        painter.text(
            Pos2::new(x, y),
            egui::Align2::LEFT_CENTER,
            badge,
            egui::FontId::monospace(13.0),
            if is_selected {
                palette.primary
            } else {
                palette.border
            },
        );
        painter.text(
            Pos2::new(x + 26.0, y),
            egui::Align2::LEFT_CENTER,
            truncate_label(&session.name, 22),
            egui::FontId::monospace(14.0),
            name_color,
        );

        let marker = if session.active { "←" } else { "" };
        let right = session
            .anchor
            .process
            .as_deref()
            .map(|process| format!("{process} {marker}"))
            .unwrap_or_else(|| marker.to_owned());
        painter.text(
            Pos2::new(rect.max.x - 12.0, y),
            egui::Align2::RIGHT_CENTER,
            right,
            egui::FontId::monospace(12.0),
            meta_color,
        );
    }
    response
}

fn paint_sidebar_footer(ui: &egui::Ui, rect: Rect, footer_h: f32, palette: ThemePalette) {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    painter.line_segment(
        [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
        Stroke::new(1.0, palette.surface),
    );
    painter.text(
        Pos2::new(rect.min.x + 14.0, y + 20.0),
        egui::Align2::LEFT_CENTER,
        "# click   ↵ activate",
        egui::FontId::monospace(12.0),
        palette.muted,
    );
    painter.text(
        Pos2::new(rect.min.x + 14.0, y + 40.0),
        egui::Align2::LEFT_CENTER,
        "# cmd+n new session",
        egui::FontId::monospace(12.0),
        palette.border,
    );
}

fn window_tab(
    ui: &mut egui::Ui,
    rect: Rect,
    window: &MuxWindow,
    label: &str,
    selected: bool,
    palette: ThemePalette,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-window-tab", &window.id)),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let bg = if selected {
            palette.surface
        } else if response.hovered() {
            palette.hover
        } else {
            palette.base
        };
        painter.rect_filled(rect, 5.0, bg);
        painter.rect_stroke(
            rect,
            5.0,
            Stroke::new(
                1.0,
                if selected {
                    palette.primary
                } else {
                    palette.border
                },
            ),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::monospace(12.0),
            if selected {
                palette.text
            } else {
                palette.subtext
            },
        );
    }
    response
}

fn backend_label(backend: MuxBackendKind) -> &'static str {
    match backend {
        MuxBackendKind::Rmux => "rmux",
        MuxBackendKind::Native => "native",
        MuxBackendKind::Tmux => "tmux",
        MuxBackendKind::Zellij => "zellij",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_rect_uses_configured_width_and_can_be_disabled() {
        let rect = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(500.0, 300.0));
        let mut chrome = ChromeConfig {
            sidebar_width: 240.0,
            ..Default::default()
        };

        assert_eq!(sidebar_rect(rect, &chrome).width(), 240.0);

        chrome.sidebar = false;
        assert_eq!(sidebar_rect(rect, &chrome).width(), 0.0);
    }
}
