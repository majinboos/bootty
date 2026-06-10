use std::{borrow::Cow, fmt::Write as _, time::Duration};

use bootty_ui::ThemePalette;
use eframe::egui::{self, Pos2, Rect, RichText, Stroke, TextureHandle};

use crate::{
    assets,
    config::ChromeConfig,
    diagnostics::{StatusMetrics, us_to_ms},
    mux::{
        config::MuxBackendKind,
        sidebar_meta::{DiffStat, SidebarMetadata},
        snapshot::{MuxSession, MuxWindow},
    },
    strings::{push_truncated_label, truncate_label},
    ui::sidebar::{
        SidebarItem, SidebarItemKind, build_visible_sidebar_items, item_label, push_prefixed_label,
    },
};

#[derive(Clone, Debug)]
pub struct StatusBarModel<'a> {
    pub backend: MuxBackendKind,
    pub selected_session_name: Option<&'a str>,
    pub metrics: StatusMetrics,
    pub last_error: Option<&'a str>,
}

#[derive(Clone)]
pub struct SidebarModel<'a> {
    pub sessions: &'a [MuxSession],
    pub selected_session: Option<&'a str>,
    pub metadata: &'a SidebarMetadata,
    pub title_visible: bool,
    pub reserve_titlebar_buttons: bool,
    pub title_icon: Option<&'a TextureHandle>,
    pub top_inset: f32,
    pub border_visible: bool,
    pub separator_visible: bool,
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

const SIDEBAR_HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_FOOTER_BASE_HEIGHT: f32 = 44.0;
const SIDEBAR_MAX_USAGE_BARS: usize = 3;
const SIDEBAR_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_PAD_X: f32 = 14.0;
const MACOS_TITLEBAR_BUTTON_SAFE_WIDTH: f32 = 72.0;
const AGENT_DETAIL_MAX_CHARS: usize = 18;
const MACOS_TITLEBAR_BUTTON_CENTER_Y: f32 = 16.0;

pub fn show_sidebar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    model: SidebarModel<'_>,
) -> Option<String> {
    let palette = sidebar_palette(palette);
    let width = ui.max_rect().width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, palette.base);
    if model.border_visible {
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, subtle_border(palette)),
            egui::StrokeKind::Inside,
        );
    }

    let header_h = sidebar_header_height(model.title_visible);
    let content_top = rect.min.y + model.top_inset;
    let title_rect = Rect::from_min_max(
        Pos2::new(rect.min.x, content_top),
        Pos2::new(rect.max.x, (content_top + header_h).min(rect.max.y)),
    );
    if model.title_visible {
        paint_sidebar_title(ui, title_rect, palette, &model);
    }

    let list_top = content_top + header_h;
    let usage_bars = parse_usage_bars(model.metadata.usage_lines());
    let footer_h = sidebar_footer_height(usage_bars.len());
    if model.sessions.is_empty() {
        painter.text(
            Pos2::new(rect.center().x, list_top + 42.0),
            egui::Align2::CENTER_CENTER,
            "no mux sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
    }

    let max_rows = visible_sidebar_row_capacity(height, model.top_inset, header_h, footer_h);
    let items = build_visible_sidebar_items(
        model.sessions,
        model.selected_session,
        model.metadata,
        max_rows,
    );
    let pointer_pos = ui.input(|input| input.pointer.hover_pos());
    let hovered_session = pointer_pos
        .and_then(|pos| sidebar_hovered_row(pos, rect.min.x, list_top, width, max_rows))
        .and_then(|index| items.get(index))
        .and_then(|item| item.session_id);

    let mut activated = None;
    for (index, item) in items.iter().enumerate() {
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, list_top + index as f32 * SIDEBAR_ROW_HEIGHT),
            egui::vec2(width, SIDEBAR_ROW_HEIGHT),
        );
        let hovered = item
            .session_id
            .is_some_and(|session_id| Some(session_id) == hovered_session);
        let response = sidebar_item_row(ui, row_rect, item, hovered, palette);
        if response.clicked()
            && let Some(session_id) = &item.session_id
        {
            activated = Some((*session_id).to_owned());
        }
    }

    paint_sidebar_footer(
        ui,
        rect,
        footer_h,
        &usage_bars,
        model.separator_visible,
        palette,
    );
    activated
}

pub(crate) fn sidebar_metadata_session_budget(
    height: f32,
    top_inset: f32,
    title_visible: bool,
    usage_lines: &[String],
) -> usize {
    let header_h = sidebar_header_height(title_visible);
    let usage_bars = parse_usage_bars(usage_lines);
    let footer_h = sidebar_footer_height(usage_bars.len());
    visible_sidebar_row_capacity(height, top_inset, header_h, footer_h)
}

fn visible_sidebar_row_capacity(
    height: f32,
    top_inset: f32,
    header_h: f32,
    footer_h: f32,
) -> usize {
    let list_top = top_inset + header_h;
    let list_bottom = (height - footer_h).max(list_top);
    ((list_bottom - list_top) / SIDEBAR_ROW_HEIGHT)
        .floor()
        .max(0.0) as usize
}

fn sidebar_palette(palette: ThemePalette) -> ThemePalette {
    palette
}
fn sidebar_hover_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.045)
}

fn sidebar_current_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.065)
}

fn sidebar_hovered_row(
    pos: Pos2,
    left: f32,
    top: f32,
    width: f32,
    max_rows: usize,
) -> Option<usize> {
    let list_rect = Rect::from_min_size(
        Pos2::new(left, top),
        egui::vec2(width, max_rows as f32 * SIDEBAR_ROW_HEIGHT),
    );
    if !list_rect.contains(pos) {
        return None;
    }
    let row = ((pos.y - top) / SIDEBAR_ROW_HEIGHT).floor() as usize;
    (row < max_rows).then_some(row)
}

fn subtle_border(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.09)
}

fn mix_color(a: egui::Color32, b: egui::Color32, amount: f32) -> egui::Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let inv = 1.0 - amount;
    egui::Color32::from_rgb(
        (f32::from(a.r()) * inv + f32::from(b.r()) * amount).round() as u8,
        (f32::from(a.g()) * inv + f32::from(b.g()) * amount).round() as u8,
        (f32::from(a.b()) * inv + f32::from(b.b()) * amount).round() as u8,
    )
}
pub fn load_app_icon_texture(
    ctx: &egui::Context,
    texture: &mut Option<TextureHandle>,
) -> TextureHandle {
    texture
        .get_or_insert_with(|| {
            ctx.load_texture(
                "bootty-app-icon",
                assets::title_icon_color_image(),
                egui::TextureOptions::LINEAR,
            )
        })
        .clone()
}

fn paint_sidebar_title(ui: &egui::Ui, rect: Rect, palette: ThemePalette, model: &SidebarModel<'_>) {
    let painter = ui.painter_at(rect);
    let layout = sidebar_title_layout(rect, model.reserve_titlebar_buttons);
    if let Some(icon) = model.title_icon {
        painter.image(
            icon.id(),
            layout.icon_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.circle_filled(layout.icon_rect.center(), 8.0, palette.primary);
    }
    painter.text(
        layout.title_pos,
        egui::Align2::LEFT_CENTER,
        "Bootty",
        egui::FontId::proportional(15.0),
        palette.text,
    );
    painter.text(
        Pos2::new(rect.max.x - SIDEBAR_PAD_X, layout.title_pos.y),
        egui::Align2::RIGHT_CENTER,
        model.sessions.len().to_string(),
        egui::FontId::monospace(13.0),
        palette.muted,
    );
}

fn sidebar_header_height(title_visible: bool) -> f32 {
    if title_visible {
        SIDEBAR_HEADER_HEIGHT
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SidebarTitleLayout {
    icon_rect: Rect,
    title_pos: Pos2,
}

fn sidebar_title_layout(rect: Rect, reserve_titlebar_buttons: bool) -> SidebarTitleLayout {
    let (reserved, center_y) = if reserve_titlebar_buttons {
        (
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH,
            rect.min.y + MACOS_TITLEBAR_BUTTON_CENTER_Y,
        )
    } else {
        (0.0, rect.min.y + SIDEBAR_HEADER_HEIGHT * 0.5)
    };
    let icon_size = 18.0;
    let left = rect.min.x + SIDEBAR_PAD_X + reserved;
    let icon_rect = Rect::from_min_size(
        Pos2::new(left, center_y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    SidebarTitleLayout {
        icon_rect,
        title_pos: Pos2::new(icon_rect.max.x + 10.0, center_y),
    }
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

fn sidebar_item_row(
    ui: &mut egui::Ui,
    rect: Rect,
    item: &SidebarItem<'_>,
    hovered_session: bool,
    palette: ThemePalette,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-sidebar-item", &item.id)),
        if item.session_id.is_some() || item.selectable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let bg = if hovered_session {
            sidebar_hover_color(palette)
        } else if item.current {
            sidebar_current_color(palette)
        } else {
            palette.base
        };
        painter.rect_filled(rect, 0.0, bg);

        if item.current {
            let bar = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + 4.0, rect.max.y));
            painter.rect_filled(bar, 0.0, item.color);
        }

        match &item.kind {
            SidebarItemKind::Group => paint_group_item(&painter, rect, item, palette),
            SidebarItemKind::Session {
                active,
                process,
                diff,
            } => paint_session_item(&painter, rect, item, *active, *process, *diff, palette),
            SidebarItemKind::Process {
                name,
                cpu_pct,
                mem_bytes,
            } => paint_process_item(&painter, rect, item, name, *cpu_pct, *mem_bytes, palette),
            SidebarItemKind::Agent { text } => {
                let active = is_agent_active(text);
                if active {
                    ui.ctx().request_repaint_after(Duration::from_millis(180));
                }
                let time = ui.input(|input| input.time);
                paint_agent_item(&painter, rect, item, text, active, time, palette)
            }
            SidebarItemKind::Branch { name } => {
                paint_detail_item(&painter, rect, item, "", name, palette)
            }
            SidebarItemKind::Status { text } => {
                paint_detail_item(&painter, rect, item, "status", text, palette)
            }
            SidebarItemKind::Progress { pct } => {
                paint_progress_item(&painter, rect, item, *pct, palette)
            }
        }
    }
    response
}
fn paint_group_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    palette: ThemePalette,
) {
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        item_label(item, 30),
        egui::FontId::monospace(12.0),
        palette.border,
    );
}

fn paint_session_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    active: bool,
    process: Option<&str>,
    diff: Option<DiffStat>,
    palette: ThemePalette,
) {
    let label_color = if active { item.color } else { item.dim_color };
    let label = item_label(item, 24);
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(13.0),
        label_color,
    );

    if let Some(diff) = diff
        && (diff.added > 0 || diff.removed > 0)
    {
        paint_diff_stat(painter, rect, diff, palette);
    } else if let Some(process) = process {
        painter.text(
            Pos2::new(rect.max.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            truncate_label(process, 10),
            egui::FontId::monospace(12.0),
            palette.border,
        );
    }
}

fn paint_diff_stat(painter: &egui::Painter, rect: Rect, diff: DiffStat, palette: ThemePalette) {
    painter.text(
        Pos2::new(rect.max.x - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("-{}", diff.removed),
        egui::FontId::monospace(12.0),
        palette.destructive,
    );
    painter.text(
        Pos2::new(rect.max.x - 46.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("+{}", diff.added),
        egui::FontId::monospace(12.0),
        palette.success,
    );
}

fn paint_process_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    name: &str,
    cpu_pct: Option<f32>,
    mem_bytes: Option<u64>,
    palette: ThemePalette,
) {
    let (icon, color) = process_style(name, palette);
    let mut label = String::new();
    push_prefixed_label(&mut label, item.tree, item.indent, icon);
    label.push(' ');
    push_truncated_label(&mut label, name, 16);
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        color,
    );

    let mut metrics = String::new();
    if let Some(cpu_pct) = cpu_pct {
        let _ = write!(metrics, "{cpu_pct:.1}%");
    }
    if let Some(mem_bytes) = mem_bytes.filter(|bytes| *bytes > 0) {
        if !metrics.is_empty() {
            metrics.push(' ');
        }
        push_formatted_bytes(&mut metrics, mem_bytes);
    }
    if !metrics.is_empty() {
        painter.text(
            Pos2::new(rect.max.x - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            metrics,
            egui::FontId::monospace(11.0),
            palette.border,
        );
    }
}

fn process_style(name: &str, palette: ThemePalette) -> (&'static str, egui::Color32) {
    match name {
        "node" | "bun" | "deno" => ("󰎙", palette.success),
        "nvim" | "vim" => ("", palette.accent),
        "fish" | "zsh" | "bash" | "sh" => ("", palette.subtext),
        "cargo" | "rustc" | "rust-analyzer" => ("", palette.warning),
        "git" => ("", palette.destructive),
        "python" | "python3" => ("", palette.warning),
        _ => ("", palette.muted),
    }
}

fn push_formatted_bytes(out: &mut String, bytes: u64) {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        let _ = write!(out, "{:.1}g", bytes as f64 / GIB as f64);
    } else {
        let _ = write!(out, "{}m", bytes.div_ceil(MIB));
    }
}

fn paint_agent_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    text: &str,
    active: bool,
    time: f64,
    palette: ThemePalette,
) {
    let (name, detail) = text.split_once(' ').unwrap_or((text, ""));
    let (icon, color) = agent_style(name, palette);
    let mut label = String::new();
    push_prefixed_label(&mut label, item.tree, item.indent, icon);
    label.push(' ');
    label.push_str(name);
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        color,
    );
    painter.text(
        Pos2::new(rect.max.x - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        agent_detail_label(detail, active, time),
        egui::FontId::monospace(11.0),
        if detail.contains("asking") {
            palette.warning
        } else {
            palette.subtext
        },
    );
}

fn is_agent_active(text: &str) -> bool {
    text.contains('…') || text.contains("Working")
}

fn agent_detail_label(detail: &str, active: bool, time: f64) -> String {
    let pulse_len = if active {
        ((time * 5.0) as usize % 4) + 1
    } else {
        0
    };
    let mut label = String::with_capacity(detail.len().min(AGENT_DETAIL_MAX_CHARS * 4) + pulse_len);
    let mut chars = detail.chars().chain(std::iter::repeat_n('.', pulse_len));
    for _ in 0..AGENT_DETAIL_MAX_CHARS {
        let Some(ch) = chars.next() else {
            return label;
        };
        label.push(ch);
    }

    if chars.next().is_some() {
        label.pop();
        label.push('…');
    }
    label
}

fn agent_style(name: &str, palette: ThemePalette) -> (&'static str, egui::Color32) {
    match name {
        "claude" => ("\u{e861}", palette.warning),
        "codex" => ("\u{e7cf}", egui::Color32::from_rgb(0x74, 0xc7, 0xec)),
        "opencode" => ("\u{f0b16}", egui::Color32::from_rgb(0x9a, 0x8f, 0xbf)),
        _ => ("", palette.subtext),
    }
}

fn paint_detail_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    kind: &str,
    text: &str,
    palette: ThemePalette,
) {
    let mut display = String::new();
    if kind.is_empty() {
        push_prefixed_label(&mut display, item.tree, item.indent, "");
        push_truncated_label(&mut display, text, 26);
    } else {
        push_prefixed_label(&mut display, item.tree, item.indent, kind);
        display.push(' ');
        push_truncated_label(&mut display, text, 22);
    }
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        display,
        egui::FontId::monospace(11.0),
        palette.muted,
    );
}

fn paint_progress_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    pct: u8,
    palette: ThemePalette,
) {
    let bar_cells = 12usize;
    let filled = (pct as usize * bar_cells) / 100;
    let empty = bar_cells.saturating_sub(filled);
    let mut label = String::new();
    push_prefixed_label(&mut label, item.tree, item.indent, "");
    label.extend(std::iter::repeat_n('█', filled));
    label.extend(std::iter::repeat_n('░', empty));
    let _ = write!(label, " {pct}%");
    painter.text(
        Pos2::new(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::monospace(11.0),
        if pct >= 100 {
            palette.success
        } else {
            item.dim_color
        },
    );
}

fn sidebar_footer_height(usage_bar_count: usize) -> f32 {
    let usage_count = usage_bar_count.min(SIDEBAR_MAX_USAGE_BARS);
    SIDEBAR_FOOTER_BASE_HEIGHT + usage_count as f32 * 30.0
}

fn paint_sidebar_footer(
    ui: &egui::Ui,
    rect: Rect,
    footer_h: f32,
    usage_bars: &UsageBars,
    separator_visible: bool,
    palette: ThemePalette,
) {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    if separator_visible {
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, subtle_border(palette)),
        );
    }

    let mut row_y = y + 18.0;
    for bar in usage_bars.iter() {
        paint_usage_bar(
            &painter,
            Rect::from_min_size(
                Pos2::new(rect.min.x + 14.0, row_y - 10.0),
                egui::vec2(rect.width() - 28.0, 26.0),
            ),
            bar,
            palette,
        );
        row_y += 30.0;
    }

    painter.text(
        Pos2::new(rect.min.x + 14.0, rect.max.y - 18.0),
        egui::Align2::LEFT_CENTER,
        "⌘1-9 session   ⌘⇧n/p nav   ⌘n new",
        egui::FontId::monospace(11.0),
        palette.muted,
    );
}

#[derive(Clone, Debug)]
struct UsageBar<'a> {
    label: Cow<'a, str>,
    pct: u8,
    pace: Cow<'a, str>,
    pace_marker: Option<f32>,
    color: egui::Color32,
}

#[derive(Clone, Debug)]
struct UsageBars<'a> {
    bars: [Option<UsageBar<'a>>; SIDEBAR_MAX_USAGE_BARS],
    len: usize,
}

impl<'a> UsageBars<'a> {
    fn new() -> Self {
        Self {
            bars: std::array::from_fn(|_| None),
            len: 0,
        }
    }

    fn push(&mut self, bar: UsageBar<'a>) -> bool {
        if self.len >= SIDEBAR_MAX_USAGE_BARS {
            return false;
        }
        self.bars[self.len] = Some(bar);
        self.len += 1;
        true
    }

    fn len(&self) -> usize {
        self.len
    }

    fn iter(&self) -> impl Iterator<Item = &UsageBar<'a>> {
        self.bars[..self.len]
            .iter()
            .filter_map(std::option::Option::as_ref)
    }
}

impl<'a> std::ops::Index<usize> for UsageBars<'a> {
    type Output = UsageBar<'a>;

    fn index(&self, index: usize) -> &Self::Output {
        self.bars[index].as_ref().expect("usage bar index in range")
    }
}
fn paint_usage_bar(painter: &egui::Painter, rect: Rect, bar: &UsageBar<'_>, palette: ThemePalette) {
    painter.text(
        Pos2::new(rect.min.x, rect.min.y + 6.0),
        egui::Align2::LEFT_CENTER,
        truncate_label(&bar.label, 22),
        egui::FontId::monospace(11.0),
        palette.subtext,
    );
    paint_usage_right_text(painter, rect, bar, palette);

    let track = Rect::from_min_size(
        Pos2::new(rect.min.x, rect.min.y + 17.0),
        egui::vec2(rect.width(), 4.0),
    );
    let fill_x = track.left() + track.width() * f32::from(bar.pct) / 100.0;
    painter.rect_filled(track, 2.0, palette.surface);
    painter.line_segment(
        [
            Pos2::new(track.left(), track.center().y),
            Pos2::new(fill_x, track.center().y),
        ],
        Stroke::new(2.0, bar.color),
    );
    if let Some(marker) = bar.pace_marker {
        let marker_x = track.left() + track.width() * marker;
        painter.line_segment(
            [
                Pos2::new(marker_x, track.top() - 3.0),
                Pos2::new(marker_x, track.bottom() + 3.0),
            ],
            Stroke::new(2.0, palette.success),
        );
    }
}

fn paint_usage_right_text(
    painter: &egui::Painter,
    rect: Rect,
    bar: &UsageBar<'_>,
    palette: ThemePalette,
) {
    if bar.pace.is_empty() {
        painter.text(
            Pos2::new(rect.max.x, rect.min.y + 6.0),
            egui::Align2::RIGHT_CENTER,
            format!("{}%", bar.pct),
            egui::FontId::monospace(11.0),
            bar.color,
        );
        return;
    }

    painter.text(
        Pos2::new(rect.max.x, rect.min.y + 6.0),
        egui::Align2::RIGHT_CENTER,
        &bar.pace,
        egui::FontId::monospace(11.0),
        palette.muted,
    );
    let pace_width = bar.pace.chars().count() as f32 * 7.0 + 8.0;
    painter.text(
        Pos2::new(rect.max.x - pace_width, rect.min.y + 6.0),
        egui::Align2::RIGHT_CENTER,
        format!("{}%", bar.pct),
        egui::FontId::monospace(11.0),
        bar.color,
    );
}
fn parse_usage_bars(lines: &[String]) -> UsageBars<'_> {
    let mut bars = UsageBars::new();
    for line in lines {
        let (pct, label, pace, pace_marker) = match strip_ansi(line) {
            Cow::Borrowed(text) => {
                let Some(pct) = parse_percent(text) else {
                    continue;
                };
                (
                    pct,
                    usage_label(text, true),
                    usage_pace(text, true),
                    usage_pace_marker(text),
                )
            }
            Cow::Owned(text) => {
                let Some(pct) = parse_percent(&text) else {
                    continue;
                };
                (
                    pct,
                    Cow::Owned(usage_label(&text, true).into_owned()),
                    Cow::Owned(usage_pace(&text, true).into_owned()),
                    usage_pace_marker(&text),
                )
            }
        };
        if !bars.push(UsageBar {
            label: if label.is_empty() {
                Cow::Borrowed("usage")
            } else {
                label
            },
            pct,
            pace,
            pace_marker,
            color: first_ansi_color(line).unwrap_or(egui::Color32::from_rgb(0x89, 0xb4, 0xfa)),
        }) {
            break;
        }
    }
    bars
}

fn usage_label<'a>(text: &'a str, borrow_single_part: bool) -> Cow<'a, str> {
    let mut label = None;
    for part in text
        .split_whitespace()
        .filter(|part| !part.contains('%'))
        .filter(|part| !part.starts_with('+') && !part.contains(':') && !part.contains('↺'))
        .filter(|part| part.chars().any(|ch| ch.is_ascii_alphanumeric()))
        .take(2)
    {
        let Some(first) = label else {
            label = Some(Cow::Borrowed(part));
            continue;
        };
        let mut joined = String::with_capacity(first.len() + 1 + part.len());
        joined.push_str(&first);
        joined.push(' ');
        joined.push_str(part);
        return Cow::Owned(joined);
    }
    match label {
        Some(label) if borrow_single_part => label,
        Some(label) => Cow::Owned(label.into_owned()),
        None => Cow::Borrowed(""),
    }
}

fn usage_pace<'a>(text: &'a str, borrow_single_part: bool) -> Cow<'a, str> {
    let mut seen_pct = false;
    let mut first = None;
    let mut joined = None::<String>;
    let mut count = 0usize;
    for part in text.split_whitespace() {
        if part.ends_with('%') {
            seen_pct = true;
            continue;
        }
        if !seen_pct || !(part.starts_with('+') || part.contains(':') || part.contains('↺')) {
            continue;
        }
        count += 1;
        if count == 1 {
            first = Some(part);
        } else {
            let joined = joined.get_or_insert_with(|| {
                let first = first.unwrap_or_default();
                let mut joined = String::with_capacity(first.len() + 1 + part.len());
                joined.push_str(first);
                joined
            });
            joined.push(' ');
            joined.push_str(part);
        }
        if count == 3 {
            break;
        }
    }
    match (joined, first) {
        (Some(pace), _) => Cow::Owned(pace),
        (None, Some(pace)) if borrow_single_part => Cow::Borrowed(pace),
        (None, Some(pace)) => Cow::Owned(pace.to_owned()),
        (None, None) => Cow::Borrowed(""),
    }
}

fn usage_pace_marker(text: &str) -> Option<f32> {
    if !text.contains('↺') {
        return None;
    }

    let total = text
        .split_whitespace()
        .take_while(|part| !part.ends_with('%'))
        .filter_map(parse_duration_seconds)
        .last()?;
    let reset = text
        .split_whitespace()
        .skip_while(|part| !part.ends_with('%'))
        .find_map(|part| part.strip_prefix('↺').and_then(parse_duration_seconds))?;
    (total > 0).then(|| (reset as f32 / total as f32).clamp(0.0, 1.0))
}

fn parse_duration_seconds(token: &str) -> Option<u64> {
    let token = token.trim().trim_start_matches('+').trim_start_matches('↺');
    if token.is_empty() {
        return None;
    }
    if let Some((days, rest)) = token.split_once('d') {
        let days = days.parse::<u64>().ok()?;
        let rest_seconds = if rest.is_empty() {
            0
        } else {
            parse_clock_seconds(rest).or_else(|| parse_unit_seconds(rest))?
        };
        return Some(
            days.saturating_mul(24 * 60 * 60)
                .saturating_add(rest_seconds),
        );
    }
    parse_unit_seconds(token).or_else(|| parse_clock_seconds(token))
}

fn parse_unit_seconds(token: &str) -> Option<u64> {
    if let Some(value) = token.strip_suffix('d') {
        return value
            .parse::<u64>()
            .ok()
            .map(|value| value.saturating_mul(24 * 60 * 60));
    }
    if let Some(value) = token.strip_suffix('h') {
        return value
            .parse::<u64>()
            .ok()
            .map(|value| value.saturating_mul(60 * 60));
    }
    if let Some(value) = token.strip_suffix('m') {
        return value
            .parse::<u64>()
            .ok()
            .map(|value| value.saturating_mul(60));
    }
    None
}
fn parse_clock_seconds(token: &str) -> Option<u64> {
    let (hours, minutes) = token.split_once(':')?;
    let hours = hours.parse::<u64>().ok()?;
    let minutes = minutes.parse::<u64>().ok()?;
    (minutes < 60).then_some(hours.saturating_mul(60 * 60) + minutes * 60)
}

fn parse_percent(text: &str) -> Option<u8> {
    text.split_whitespace().find_map(|part| {
        part.strip_suffix('%')
            .and_then(|value| value.parse::<u8>().ok())
            .map(|pct| pct.min(100))
    })
}
fn strip_ansi(line: &str) -> Cow<'_, str> {
    if !line.contains('\x1b') && !line.chars().any(|ch| matches!(ch, '█' | '░' | '▒' | '▓'))
    {
        return Cow::Borrowed(line.trim());
    }

    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !matches!(ch, '█' | '░' | '▒' | '▓') {
            out.push(ch);
        }
    }
    Cow::Owned(out.trim().to_owned())
}

fn first_ansi_color(line: &str) -> Option<egui::Color32> {
    if !line.contains('\x1b') {
        return None;
    }

    let mut offset = 0;
    while let Some(start) = line[offset..].find("\x1b[") {
        let code_start = offset + start + 2;
        let code = &line[code_start..];
        let Some(command_offset) = code.bytes().position(|byte| byte.is_ascii_alphabetic()) else {
            break;
        };
        let command = code.as_bytes()[command_offset];
        if command == b'm' {
            let code = &code[..command_offset];
            if let Some(color) = sgr_color(code) {
                return Some(color);
            }
        }
        offset = code_start + command_offset + 1;
    }
    None
}

fn sgr_color(code: &str) -> Option<egui::Color32> {
    if code == "0" || code == "39" {
        return None;
    }

    let mut parts = code.split(';');
    if parts.next()? != "38" || parts.next()? != "2" {
        return None;
    }
    let r = parts.next()?.parse::<u8>().ok()?;
    let g = parts.next()?.parse::<u8>().ok()?;
    let b = parts.next()?.parse::<u8>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(egui::Color32::from_rgb(r, g, b))
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

    #[test]
    fn usage_lines_parse_to_native_bar_data() {
        let lines = vec![
            "\x1b[38;2;116;199;236m  5h  90% +38m\x1b[0m".to_owned(),
            "\x1b[38;2;116;199;236m████████░░\x1b[0m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), 1);
        assert_eq!(bars[0].pct, 90);
        assert!(bars[0].label.contains("5h"));
        assert_eq!(bars[0].label, "5h");
        assert_eq!(bars[0].pace, "+38m");
        assert_eq!(bars[0].color, egui::Color32::from_rgb(116, 199, 236));
        assert_eq!(bars[0].pace_marker, None);
    }

    #[test]
    fn usage_lines_parse_only_painted_bars() {
        let lines = vec![
            "not a usage line".to_owned(),
            "first 10% +1m".to_owned(),
            "second 20% +2m".to_owned(),
            "third 30% +3m".to_owned(),
            "fourth 40% +4m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars.len(), SIDEBAR_MAX_USAGE_BARS);
        assert_eq!(bars[0].label, "first");
        assert_eq!(bars[1].label, "second");
        assert_eq!(bars[2].label, "third");
    }

    #[test]
    fn usage_pace_marker_uses_reset_duration_not_percent() {
        let lines = vec![
            "\x1b[38;2;116;199;236m 5h 78% +3h03 ↺50m\x1b[0m".to_owned(),
            "\x1b[38;2;116;199;236m 7d 73% +1d06:20 ↺3d20:18\x1b[0m".to_owned(),
        ];

        let bars = parse_usage_bars(&lines);

        assert_eq!(bars[0].pace, "+3h03 ↺50m");
        assert_eq!(bars[1].pace, "+1d06:20 ↺3d20:18");
        assert!((bars[0].pace_marker.unwrap() - (50.0 / (5.0 * 60.0))).abs() < 0.001);
        assert!(
            (bars[1].pace_marker.unwrap()
                - ((3.0 * 24.0 * 60.0 + 20.0 * 60.0 + 18.0) / (7.0 * 24.0 * 60.0)))
                .abs()
                < 0.001
        );
    }

    #[test]
    fn agent_detail_label_formats_active_pulse_without_intermediate_text() {
        assert_eq!(agent_detail_label("Working", false, 0.0), "Working");
        assert_eq!(agent_detail_label("Working", true, 0.0), "Working.");
        assert_eq!(agent_detail_label("Working", true, 0.6), "Working....");
        assert_eq!(
            agent_detail_label("abcdefghijklmnop", true, 0.6),
            "abcdefghijklmnop.…"
        );
        assert_eq!(
            agent_detail_label("abcdefghijklmnopqrst", true, 0.0),
            "abcdefghijklmnopq…"
        );
    }

    #[test]
    fn sidebar_title_layout_reserves_macos_titlebar_button_area() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(286.0, 200.0));

        let normal = sidebar_title_layout(rect, false);
        let reserved = sidebar_title_layout(rect, true);

        assert_eq!(normal.icon_rect.min.x, SIDEBAR_PAD_X);
        assert_eq!(
            reserved.icon_rect.min.x,
            SIDEBAR_PAD_X + MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        );
        assert_eq!(normal.title_pos.y, SIDEBAR_HEADER_HEIGHT * 0.5);
        assert_eq!(reserved.title_pos.y, MACOS_TITLEBAR_BUTTON_CENTER_Y);
        assert!(reserved.title_pos.x > reserved.icon_rect.max.x);
    }

    #[test]
    fn sidebar_hovered_row_maps_pointer_to_visible_rows() {
        assert_eq!(
            sidebar_hovered_row(Pos2::new(20.0, 10.0), 10.0, 10.0, 100.0, 3),
            Some(0)
        );
        assert_eq!(
            sidebar_hovered_row(
                Pos2::new(20.0, 10.0 + SIDEBAR_ROW_HEIGHT * 2.0 + 1.0),
                10.0,
                10.0,
                100.0,
                3
            ),
            Some(2)
        );
        assert_eq!(
            sidebar_hovered_row(
                Pos2::new(20.0, 10.0 + SIDEBAR_ROW_HEIGHT * 3.0),
                10.0,
                10.0,
                100.0,
                3
            ),
            None
        );
        assert_eq!(
            sidebar_hovered_row(Pos2::new(9.0, 10.0), 10.0, 10.0, 100.0, 3),
            None
        );
    }

    #[test]
    fn sidebar_header_collapses_when_title_is_hidden() {
        assert_eq!(sidebar_header_height(true), SIDEBAR_HEADER_HEIGHT);
        assert_eq!(sidebar_header_height(false), 0.0);
    }

    #[test]
    fn sidebar_metadata_budget_tracks_visible_row_capacity() {
        let plain = sidebar_metadata_session_budget(900.0, 0.0, true, &[]);
        let with_usage = sidebar_metadata_session_budget(
            900.0,
            0.0,
            true,
            &[
                "terminal 5h 90% +38m".to_owned(),
                "agent 7d 73% +1d06:20".to_owned(),
            ],
        );

        assert!(with_usage < plain);
        assert_eq!(plain, 33);
    }
}
