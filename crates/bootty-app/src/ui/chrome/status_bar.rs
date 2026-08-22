use std::ops::Range;

use bootty_extension::{ExtensionUiAction, ModuleItem};
use bootty_ui::{
    ThemePalette,
    icons::{has_slug, paint_icon_slug},
    readable_color,
};
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, StrokeKind};

use bootty_config::config::SegmentAlign;

use super::{
    item_primitives::{PrimitivePaintStyle, paint_item_primitives_inner, primitive_background},
    start_window_drag_on_primary_press,
};

/// Semantic identity of the built-in window-tab surface. A segment naming it gets the tab context
/// menu and its reorder is a window move, not an extension action. This is the surface id a module
/// declares, never the producing module's file name.
const STATUS_WINDOWS_SURFACE: &str = "windows";

/// The action encoding the window-tab surface publishes per tab.
const ACTIVATE_WINDOW_ACTION: &str = "activate-window:";

/// Whether `surface` is the built-in window-tab surface.
#[must_use]
pub fn is_windows_surface(surface: &str) -> bool {
    surface == STATUS_WINDOWS_SURFACE
}

/// The window id an `activate-window:<id>` action targets.
#[must_use]
pub fn activate_window_target(action: &str) -> Option<&str> {
    action.strip_prefix(ACTIVATE_WINDOW_ACTION)
}

#[derive(Clone)]
pub struct StatusBarModel<'a, 'segments> {
    /// Frame-local geometry built from the resolved segments before the bar height is allocated.
    pub layout: &'a StatusBarLayout<'segments>,
    /// Targets that may receive the built-in tab context menu in the current selected session.
    pub tab_context: Option<&'a TabContext>,
    /// Bar fill; set to the sidebar fullscreen background when the bar sits in the notch band.
    pub background: egui::Color32,
    /// Height of the drawable status row. When the allocated bar is taller, items are bottom-aligned
    /// so extra notch-clearance space appears above them instead of stretching the row.
    pub row_height: f32,
    /// Stable per-bar interaction state key, so top and bottom bar drags cannot collide.
    pub interaction_id: &'static str,
}

/// Borrowed, frame-local status geometry. Text is measured once while this is built; painting,
/// hit-testing, and drag/drop all consume the same coordinates.
pub struct StatusBarLayout<'a> {
    segments: &'a [ResolvedSegment<'a>],
    items: Vec<StatusLayoutItem>,
    row_count: usize,
}

impl StatusBarLayout<'_> {
    pub fn row_count(&self) -> usize {
        self.row_count
    }
}

struct StatusLayoutItem {
    segment: usize,
    item: usize,
    row: usize,
    x: f32,
    width: f32,
    run_start: bool,
}

struct StatusLayoutBlock<'a> {
    anchor: &'a str,
    row: usize,
    start_x: f32,
    end_x: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabContext {
    pub session_id: String,
    pub targets: Vec<TabContextTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabContextTarget {
    pub window_id: String,
    pub is_active: bool,
    pub can_close_pane: bool,
}

/// A status segment resolved for this frame: a module's items plus where the segment is aligned.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSegment<'a> {
    pub align: SegmentAlign,
    /// Stable slot from the configured segment list, retained when window tabs wrap into rows.
    pub source_slot: usize,
    /// Producer identity used to route interactions back to the exact extension generation.
    pub module: &'a str,
    pub generation: u64,
    pub surface: &'a str,
    pub items: Vec<ResolvedItem<'a>>,
}

fn is_windows_segment(segment: &ResolvedSegment<'_>) -> bool {
    is_windows_surface(segment.surface)
}

/// One drawable element from a module. `action` (e.g. `activate-window:<id>`) is dispatched on click.
#[derive(Clone, Debug)]
pub struct ResolvedItem<'a> {
    pub item: &'a ModuleItem,
    pub icon: Option<&'a str>,
    pub fg: Option<egui::Color32>,
    pub bg: Option<egui::Color32>,
    pub stroke: Option<egui::Color32>,
}

/// The outcome of a status-bar frame: an item was clicked, or a draggable item was reordered
/// (routed to `module`'s `on_reorder`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusBarEvent {
    ExtensionAction(ExtensionUiAction),
    ContextAction {
        session_id: String,
        window_id: String,
        action: TabContextAction,
    },
    Reorder {
        module: String,
        generation: u64,
        surface: String,
        source: String,
        before: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabContextAction {
    Activate,
    NewTab,
    PreviousTab,
    NextTab,
    LastTab,
    Rename,
    MoveLeft,
    MoveRight,
    ClosePane,
}

pub const STATUS_EDGE_PAD: f32 = 12.0;
const STATUS_ITEM_GAP: f32 = 4.0;
const STATUS_ITEM_PAD: f32 = 10.0;
const STATUS_ICON_GAP: f32 = 6.0;
/// Square edge of a status-bar icon glyph, matched to the 12pt text.
const STATUS_ICON_SIZE: f32 = 14.0;
/// Battery meter dimensions for a `gauge` item (body width excludes the nub).
const STATUS_GAUGE_WIDTH: f32 = 22.0;
const STATUS_GAUGE_HEIGHT: f32 = 11.0;
/// Corner radius (logical px) for status-bar pills and the strip's outer ends.
const STATUS_PILL_RADIUS: u8 = 6;
const STATUS_DIAGONAL_JOIN_WIDTH: f32 = 8.0;

pub fn status_bar_layout<'a>(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &'a [ResolvedSegment<'a>],
    left_padding: f32,
    notch_x: Option<(f32, f32)>,
) -> StatusBarLayout<'a> {
    let font = egui::FontId::monospace(12.0);
    let widths = segments
        .iter()
        .map(|segment| {
            segment
                .items
                .iter()
                .map(|item| item_width(ui, item, &font))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let (mut left, mut center, mut right) = (Vec::new(), Vec::new(), Vec::new());
    for (index, segment) in segments
        .iter()
        .enumerate()
        .filter(|(_, s)| !s.items.is_empty())
    {
        match segment.align {
            SegmentAlign::Left => &mut left,
            SegmentAlign::Center => &mut center,
            SegmentAlign::Right => &mut right,
        }
        .push(index);
    }
    let full_bound = bar_rect.max.x - STATUS_EDGE_PAD;
    let right_width = segments_width(segments, &widths, &right);
    let bottom_bound = full_bound - right_width - STATUS_ITEM_GAP;
    let top_bound = notch_x
        .map(|(left, _)| left - STATUS_ITEM_GAP)
        .unwrap_or(full_bound);
    let left_start = bar_rect.min.x + left_padding;
    let window = left
        .iter()
        .copied()
        .find(|index| is_windows_segment(&segments[*index]));
    let (window_start, window_span) =
        window_geometry(segments, &widths, &left, left_start, bottom_bound);
    let notch_collision = notch_x
        .is_some_and(|notch| window_span.is_some_and(|span| span.0 < notch.1 && notch.0 < span.1));
    let group_count = window.map_or(1, |window| window_group_count(&segments[window]));
    let row_bounds = WindowRowBounds {
        left_start,
        bottom_start: window_start,
        top: top_bound,
        full: full_bound,
        bottom: bottom_bound,
    };
    let mut row_count = usize::from(notch_collision) + 1;
    let window_rows = window.map(|window| {
        loop {
            let (mut rows, leftover) =
                window_rows(&segments[window], &widths[window], row_count, row_bounds);
            if !leftover || row_count >= group_count {
                if leftover && let Some(row) = rows.last_mut().filter(|row| row.start == row.end) {
                    row.end = item_group_end(&segments[window].items, row.start);
                }
                break rows;
            }
            row_count += 1;
        }
    });
    let mut layout = StatusBarLayout {
        segments,
        items: Vec::new(),
        row_count,
    };
    if let (Some(window), Some(rows)) = (window, window_rows.as_ref()) {
        for (row, range) in rows.iter().take(row_count.saturating_sub(1)).enumerate() {
            let bound = if row == 0 { top_bound } else { full_bound };
            layout.add_run(
                window,
                range.clone(),
                row,
                left_start,
                bound,
                &widths[window],
            );
        }
    }
    let bottom_row = row_count - 1;
    let right_start = bar_rect.max.x - STATUS_EDGE_PAD - right_width;
    layout.add_segments(&right, bottom_row, right_start, full_bound, &widths);
    let left_end = layout.add_segment_ranges(
        left.into_iter().map(|segment| {
            let range = if Some(segment) == window {
                window_rows
                    .as_ref()
                    .and_then(|rows| rows.get(bottom_row))
                    .cloned()
                    .unwrap_or(0..0)
            } else {
                0..segments[segment].items.len()
            };
            (segment, range)
        }),
        bottom_row,
        left_start,
        bottom_bound,
        &widths,
    );
    let center_width = segments_width(segments, &widths, &center);
    let center_start = bar_rect.center().x - center_width / 2.0;
    if center_start >= left_end + STATUS_ITEM_GAP && center_start + center_width <= bottom_bound {
        layout.add_segments(&center, bottom_row, center_start, bottom_bound, &widths);
    }
    layout
}

/// Native replacement for the tmux status line. Flattens each alignment group's module items and
/// lays them out: left from the left edge, right anchored to the right edge, center centered. Items
/// with a `bg` render as pills; items with an `action` are clickable. Returns a clicked action.
pub fn show_status_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: StatusBarModel<'_, '_>,
) -> Option<StatusBarEvent> {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ui.available_height()),
        egui::Sense::click_and_drag(),
    );
    ui.painter_at(rect).rect_filled(rect, 0.0, model.background);

    let drag_id = egui::Id::new(model.interaction_id);
    let mut dragging = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<StatusDragState>(drag_id));
    let (primary_press_pos, primary_down, pointer_pos) = ui.input(|input| {
        let pointer = &input.pointer;
        (
            pointer
                .button_pressed(egui::PointerButton::Primary)
                .then_some(pointer.interact_pos())
                .flatten(),
            pointer.primary_down(),
            pointer.latest_pos().or_else(|| pointer.hover_pos()),
        )
    });
    let mut input = StatusInput {
        palette,
        event: None,
        tab_context: model.tab_context,
        interaction_id: model.interaction_id,
        primary_press_pos,
        drag_blocked: false,
        suppress_click: dragging.is_some(),
        started: None,
    };
    draw_status_layout(ui, rect, model.row_height, model.layout, &mut input);

    if !input.drag_blocked {
        start_window_drag_on_primary_press(&response);
    }

    // A press that crosses the drag threshold begins a reorder; persist it so the gesture spans
    // frames (egui per-widget drag tracking would lapse as the bar reflows).
    if let Some(started) = input.started.take() {
        ui.ctx()
            .data_mut(|data| data.insert_persisted(drag_id, started.clone()));
        dragging = Some(started);
        ui.ctx().request_repaint();
    }

    let mut event = input.event.take();
    if let Some(drag) = dragging.as_ref() {
        let drop = pointer_pos.and_then(|pos| {
            status_drop_target(
                model.layout,
                drag.source_slot,
                &drag.anchor,
                pos,
                model.row_height,
                rect,
            )
        });
        if let Some((_, indicator_x)) = drop.as_ref() {
            ui.painter_at(rect).line_segment(
                [
                    Pos2::new(*indicator_x, rect.min.y + 2.0),
                    Pos2::new(*indicator_x, rect.max.y - 2.0),
                ],
                Stroke::new(2.0, palette.primary),
            );
        }
        if primary_down {
            ui.ctx().request_repaint();
            event = None;
        } else {
            event = drop.map(|(before, _)| StatusBarEvent::Reorder {
                module: drag.module.clone(),
                generation: drag.generation,
                surface: drag.surface.clone(),
                source: drag.anchor.clone(),
                before,
            });
            ui.ctx()
                .data_mut(|data| data.remove::<StatusDragState>(drag_id));
        }
    }
    event
}

/// Reorder gesture for the status bar, persisted across frames while the pointer is held.
#[derive(Clone)]
struct StatusDragState {
    source_slot: usize,
    module: String,
    generation: u64,
    surface: String,
    anchor: String,
}

/// Paint context plus per-frame interaction accumulators.
struct StatusInput<'a> {
    palette: ThemePalette,
    event: Option<StatusBarEvent>,
    tab_context: Option<&'a TabContext>,
    interaction_id: &'static str,
    primary_press_pos: Option<Pos2>,
    drag_blocked: bool,
    suppress_click: bool,
    started: Option<StatusDragState>,
}

/// Picks the insertion slot for a horizontal drag: scans same-segment blocks left to right and
/// drops before the first whose midpoint is past the pointer (or at the end). Returns the anchor
/// to insert before (`None` = end) and the indicator x, or `None` when the drop is a no-op.
fn status_drop_target(
    layout: &StatusBarLayout<'_>,
    source_slot: usize,
    anchor: &str,
    pointer: Pos2,
    row_height: f32,
    rect: Rect,
) -> Option<(Option<String>, f32)> {
    let segment_blocks = status_blocks(layout, source_slot);
    let source_index = segment_blocks
        .iter()
        .position(|block| block.anchor == anchor)?;
    let target_row = segment_blocks
        .iter()
        .find(|block| {
            let row = status_row_rect(rect, row_height, layout.row_count, block.row);
            pointer.y >= row.min.y + 3.0 && pointer.y <= row.max.y - 3.0
        })
        .map(|block| block.row);
    let in_row = |block: &StatusLayoutBlock<'_>| target_row.is_none_or(|row| block.row == row);
    let target_index = segment_blocks
        .iter()
        .position(|block| in_row(block) && pointer.x < (block.start_x + block.end_x) * 0.5)
        .unwrap_or_else(|| {
            segment_blocks
                .iter()
                .rposition(in_row)
                .map_or(segment_blocks.len(), |index| index + 1)
        });
    if target_index == source_index || target_index == source_index + 1 {
        return None;
    }
    let before = segment_blocks
        .get(target_index)
        .map(|block| block.anchor.to_owned());
    let indicator_x = match segment_blocks.get(target_index) {
        Some(block) => block.start_x,
        None => segment_blocks.last().map_or(pointer.x, |block| block.end_x),
    };
    Some((before, indicator_x))
}

fn status_blocks<'a>(
    layout: &'a StatusBarLayout<'a>,
    source_slot: usize,
) -> Vec<StatusLayoutBlock<'a>> {
    let mut blocks = Vec::<StatusLayoutBlock<'_>>::new();
    for placed in &layout.items {
        let segment = &layout.segments[placed.segment];
        if segment.source_slot != source_slot {
            continue;
        }
        let Some(anchor) = segment.items[placed.item].item.reorder_anchor.as_deref() else {
            continue;
        };
        match blocks.last_mut() {
            Some(block) if block.row == placed.row && block.anchor == anchor => {
                block.end_x = placed.x + placed.width;
            }
            _ => blocks.push(StatusLayoutBlock {
                anchor,
                row: placed.row,
                start_x: placed.x,
                end_x: placed.x + placed.width,
            }),
        }
    }
    blocks
}

fn laid_out_width(items: &[ResolvedItem<'_>], widths: &[f32], range: Range<usize>) -> f32 {
    range
        .clone()
        .map(|index| gap_before(items, index, range.start) + widths[index])
        .sum()
}

fn segment_width(segment: &ResolvedSegment<'_>, widths: &[f32]) -> f32 {
    laid_out_width(&segment.items, widths, 0..segment.items.len())
}

fn segments_width(segments: &[ResolvedSegment<'_>], widths: &[Vec<f32>], indices: &[usize]) -> f32 {
    indices
        .iter()
        .enumerate()
        .map(|(position, &index)| {
            segment_width(&segments[index], &widths[index])
                + if position > 0 { STATUS_ITEM_GAP } else { 0.0 }
        })
        .sum()
}

fn status_row_rect(rect: Rect, row_height: f32, row_count: usize, row: usize) -> Rect {
    let row_height = row_height.max(0.0).min(rect.height());
    let first_y = rect.max.y - row_height * row_count as f32;
    let y = first_y + row_height * row as f32;
    Rect::from_min_size(
        Pos2::new(rect.min.x, y),
        egui::vec2(rect.width(), row_height),
    )
}

fn window_geometry(
    segments: &[ResolvedSegment<'_>],
    widths: &[Vec<f32>],
    left: &[usize],
    left_start: f32,
    bound: f32,
) -> (f32, Option<(f32, f32)>) {
    let mut x = left_start;
    let mut drawn = 0;
    let mut visible = true;
    for &index in left {
        let width = segment_width(&segments[index], &widths[index]);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        if is_windows_segment(&segments[index]) {
            return (x, visible.then(|| (x, (x + width).min(bound))));
        }
        if x + width > bound {
            visible = false;
        }
        drawn += 1;
        x += width;
    }
    (left_start, None)
}

fn window_group_count(segment: &ResolvedSegment<'_>) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < segment.items.len() {
        count += 1;
        index = item_group_end(&segment.items, index);
    }
    count.max(1)
}

fn item_group_end(items: &[ResolvedItem<'_>], start: usize) -> usize {
    let anchor = items[start].item.reorder_anchor.as_deref();
    let mut end = start + 1;
    while end < items.len()
        && anchor.is_some()
        && items[end].item.reorder_anchor.as_deref() == anchor
    {
        end += 1;
    }
    end
}

#[derive(Clone, Copy)]
struct WindowRowBounds {
    left_start: f32,
    bottom_start: f32,
    top: f32,
    full: f32,
    bottom: f32,
}

fn window_rows(
    segment: &ResolvedSegment<'_>,
    widths: &[f32],
    row_count: usize,
    bounds: WindowRowBounds,
) -> (Vec<Range<usize>>, bool) {
    let mut rows = Vec::with_capacity(row_count);
    let mut item = 0;
    for row in 0..row_count {
        let start = item;
        let mut x = if row + 1 == row_count {
            bounds.bottom_start
        } else {
            bounds.left_start
        };
        let bound = if row + 1 == row_count {
            bounds.bottom
        } else if row == 0 {
            bounds.top
        } else {
            bounds.full
        };
        while item < segment.items.len() {
            let end = item_group_end(&segment.items, item);
            let gap = gap_before(&segment.items, item, start);
            let width = laid_out_width(&segment.items, widths, item..end);
            if x + gap + width > bound {
                break;
            }
            x += gap + width;
            item = end;
        }
        rows.push(start..item);
    }
    (rows, item < segment.items.len())
}

impl StatusBarLayout<'_> {
    fn add_segments(
        &mut self,
        segments: &[usize],
        row: usize,
        start_x: f32,
        bound: f32,
        widths: &[Vec<f32>],
    ) -> f32 {
        self.add_segment_ranges(
            segments
                .iter()
                .map(|&segment| (segment, 0..self.segments[segment].items.len())),
            row,
            start_x,
            bound,
            widths,
        )
    }

    fn add_segment_ranges(
        &mut self,
        segments: impl IntoIterator<Item = (usize, Range<usize>)>,
        row: usize,
        start_x: f32,
        bound: f32,
        widths: &[Vec<f32>],
    ) -> f32 {
        let mut x = start_x;
        let mut drawn = 0;
        for (segment, range) in segments {
            if range.is_empty() {
                continue;
            }
            let width = laid_out_width(
                &self.segments[segment].items,
                &widths[segment],
                range.clone(),
            );
            let gap = if drawn > 0 { STATUS_ITEM_GAP } else { 0.0 };
            if x + gap + width > bound && !is_windows_segment(&self.segments[segment]) {
                break;
            }
            x += gap;
            let before = self.items.len();
            self.add_run(segment, range, row, x, bound, &widths[segment]);
            if self.items.len() == before {
                break;
            }
            x = self.items.last().map_or(x, |item| item.x + item.width);
            drawn += 1;
        }
        x
    }

    fn add_run(
        &mut self,
        segment_index: usize,
        range: Range<usize>,
        row: usize,
        start_x: f32,
        bound: f32,
        widths: &[f32],
    ) {
        let segment = &self.segments[segment_index];
        let item_start = self.items.len();
        let mut x = start_x;
        for index in range.clone() {
            x += gap_before(&segment.items, index, range.start);
            let width = widths[index];
            if x + width > bound {
                break;
            }
            self.items.push(StatusLayoutItem {
                segment: segment_index,
                item: index,
                row,
                x,
                width,
                run_start: self.items.len() == item_start,
            });
            x += width;
        }
    }
}

/// Whether the item draws an iconflow glyph for the requested slug.
fn item_icon<'a>(item: &'a ResolvedItem<'a>) -> Option<&'a str> {
    item.icon.filter(|slug| has_slug(slug))
}

fn item_width(ui: &egui::Ui, item: &ResolvedItem<'_>, font: &egui::FontId) -> f32 {
    let icon = item_icon(item).is_some();
    let gauge = item.item.gauge.is_some();
    ui.painter()
        .layout_no_wrap(item.item.text.clone(), font.clone(), egui::Color32::WHITE)
        .size()
        .x
        + if icon { STATUS_ICON_SIZE } else { 0.0 }
        + if gauge { STATUS_GAUGE_WIDTH } else { 0.0 }
        + STATUS_ICON_GAP * usize::from(icon && gauge) as f32
        + STATUS_ICON_GAP * usize::from((icon || gauge) && !item.item.text.is_empty()) as f32
        + STATUS_ITEM_PAD * 2.0
        + item.item.pad_left
        + item.item.pad_right
}

/// Adjacent items that both carry a background render as one connected strip (no
/// gap), like the tmux/mux segmented bar, unless a module opts either item out.
fn connected(prev: Option<&ResolvedItem<'_>>, cur: &ResolvedItem<'_>) -> bool {
    prev.is_some_and(|prev| {
        prev.item.join.unwrap_or(true)
            && cur.item.join.unwrap_or(true)
            && prev.bg.is_some()
            && prev.stroke.is_none()
    }) && cur.bg.is_some()
        && cur.stroke.is_none()
}

fn gap_before(items: &[ResolvedItem<'_>], index: usize, start: usize) -> f32 {
    if index > start
        && items[index].item.gap.unwrap_or(true)
        && !connected(Some(&items[index - 1]), &items[index])
    {
        STATUS_ITEM_GAP
    } else {
        0.0
    }
}

/// A battery meter (rounded body + terminal nub) filled to `ratio`, tinted `color`.
fn paint_battery_gauge(
    painter: &egui::Painter,
    left: f32,
    center_y: f32,
    ratio: f32,
    color: egui::Color32,
) {
    let body_w = STATUS_GAUGE_WIDTH - 3.0;
    let body = Rect::from_min_size(
        Pos2::new(left, center_y - STATUS_GAUGE_HEIGHT / 2.0),
        egui::vec2(body_w, STATUS_GAUGE_HEIGHT),
    );
    painter.rect_stroke(body, 2.0, Stroke::new(1.0, color), StrokeKind::Inside);
    let nub = Rect::from_min_size(
        Pos2::new(body.max.x + 1.0, center_y - STATUS_GAUGE_HEIGHT * 0.22),
        egui::vec2(2.0, STATUS_GAUGE_HEIGHT * 0.44),
    );
    painter.rect_filled(nub, 0.0, color);
    let inset = 2.0;
    let fill_w = (body.width() - inset * 2.0) * ratio.clamp(0.0, 1.0);
    if fill_w > 0.5 {
        let fill = Rect::from_min_size(
            Pos2::new(body.min.x + inset, body.min.y + inset),
            egui::vec2(fill_w, body.height() - inset * 2.0),
        );
        painter.rect_filled(fill, 1.0, color);
    }
}

fn paint_status_diagonal_join(painter: &egui::Painter, item_rect: Rect, color: egui::Color32) {
    let width = STATUS_DIAGONAL_JOIN_WIDTH.min(item_rect.width() / 2.0);
    if width <= 0.5 {
        return;
    }
    painter.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(item_rect.min.x - width, item_rect.min.y),
            item_rect.left_top(),
            item_rect.left_bottom(),
        ],
        color,
        Stroke::new(0.0, egui::Color32::TRANSPARENT),
    ));
}

fn draw_status_layout(
    ui: &mut egui::Ui,
    rect: Rect,
    row_height: f32,
    layout: &StatusBarLayout<'_>,
    input: &mut StatusInput,
) {
    let palette = input.palette;
    let font = egui::FontId::monospace(12.0);
    let hovered_anchor = ui.input(|input| input.pointer.hover_pos()).and_then(|pos| {
        layout.items.iter().find_map(|placed| {
            let item = &layout.segments[placed.segment].items[placed.item];
            let row = status_row_rect(rect, row_height, layout.row_count, placed.row);
            Rect::from_min_size(
                Pos2::new(placed.x, row.min.y + 3.0),
                egui::vec2(placed.width, row.height() - 6.0),
            )
            .contains(pos)
            .then_some(item.item.reorder_anchor.as_deref())
            .flatten()
            .map(|anchor| (placed.segment, anchor))
        })
    });
    for (layout_index, placed) in layout.items.iter().enumerate() {
        let segment = &layout.segments[placed.segment];
        let item = &segment.items[placed.item];
        let prev = (!placed.run_start).then(|| &segment.items[placed.item - 1]);
        let next = layout
            .items
            .get(layout_index + 1)
            .filter(|next| !next.run_start)
            .map(|next| &segment.items[next.item]);
        let row = status_row_rect(rect, row_height, layout.row_count, placed.row);
        let item_rect = Rect::from_min_size(
            Pos2::new(placed.x, row.min.y + 3.0),
            egui::vec2(placed.width, row.height() - 6.0),
        );

        // An anchored item drags; otherwise an action item just clicks. Keep the identity tied to
        // the bar, segment, and cell rather than x so an open context menu survives text reflow.
        let interactive = item
            .item
            .reorder_anchor
            .as_deref()
            .or(item.item.action.as_deref());
        let response = interactive.map(|key| {
            let id = status_item_id(
                input.interaction_id,
                input
                    .tab_context
                    .as_ref()
                    .map(|context| context.session_id.as_str()),
                segment,
                item,
                status_item_cell_index(&segment.items, placed.item),
                key,
            );
            let sense = if item.item.reorder_anchor.is_some() {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::click()
            };
            ui.interact(item_rect, id, sense)
        });
        if let (Some(response), Some(window_id)) =
            (response.as_ref(), tab_context_window_id(segment, item))
        {
            let context_action = input.tab_context.as_ref().and_then(|context| {
                let (target_index, target) = context
                    .targets
                    .iter()
                    .enumerate()
                    .find(|(_, target)| target.window_id == window_id)?;
                tab_context_action(
                    response,
                    !target.is_active,
                    target_index > 0,
                    target_index + 1 < context.targets.len(),
                    context.targets.len() > 1,
                    target.can_close_pane,
                )
                .map(|action| StatusBarEvent::ContextAction {
                    session_id: context.session_id.clone(),
                    window_id: window_id.to_owned(),
                    action,
                })
            });
            if let Some(context_action) = context_action {
                input.event = Some(context_action);
            }
        }
        if interactive.is_some()
            && input
                .primary_press_pos
                .is_some_and(|pos| item_rect.contains(pos))
        {
            input.drag_blocked = true;
        }
        if let Some(anchor) = item.item.reorder_anchor.as_deref()
            && response
                .as_ref()
                .is_some_and(|response| response.drag_started_by(egui::PointerButton::Primary))
        {
            input.started = Some(StatusDragState {
                source_slot: segment.source_slot,
                module: segment.module.to_owned(),
                generation: segment.generation,
                surface: segment.surface.to_owned(),
                anchor: anchor.to_owned(),
            });
        }
        let hovered = response.as_ref().is_some_and(egui::Response::hovered)
            || item.item.reorder_anchor.as_deref().is_some_and(|anchor| {
                hovered_anchor.is_some_and(|(segment, hovered)| {
                    segment == placed.segment && hovered == anchor
                })
            });

        let painter = ui.painter_at(rect);
        let primitive_bg = primitive_background(&item.item.primitives);
        let hover_background = hovered.then_some(palette.hover);
        let text_background = hover_background
            .or(item.bg)
            .or(primitive_bg)
            .unwrap_or(palette.base);
        if item.bg.is_some() || item.stroke.is_some() {
            let r = STATUS_PILL_RADIUS;
            let left_join = connected(prev, item);
            let right_join = next.is_some_and(|next| connected(Some(item), next));
            let corners = CornerRadius {
                nw: if left_join { 0 } else { r },
                sw: if left_join { 0 } else { r },
                ne: if right_join { 0 } else { r },
                se: if right_join { 0 } else { r },
            };
            let paint_background = |fill: Option<egui::Color32>, stroke: Option<egui::Color32>| {
                painter.rect(
                    item_rect,
                    corners,
                    fill.unwrap_or(egui::Color32::TRANSPARENT),
                    stroke.map_or(Stroke::NONE, |color| Stroke::new(1.0, color)),
                    StrokeKind::Inside,
                );
            };
            paint_background(item.bg, item.stroke);
            if let Some(bg) = item.bg
                && left_join
                && prev.and_then(|prev| prev.bg) != Some(bg)
            {
                paint_status_diagonal_join(&painter, item_rect, bg);
            }
            if let Some(hover_background) = hover_background {
                paint_background(Some(hover_background), None);
                if left_join {
                    paint_status_diagonal_join(&painter, item_rect, hover_background);
                }
            }
        } else if let Some(hover_background) = hover_background
            && primitive_bg.is_none()
        {
            painter.rect_filled(item_rect, STATUS_PILL_RADIUS, hover_background);
        }
        paint_item_primitives_inner(
            &painter,
            item_rect,
            &item.item.primitives,
            PrimitivePaintStyle {
                default_color: palette.subtext,
                background: text_background,
                respect_color: false,
                keep: 1.0,
                round_end: is_windows_segment(segment)
                    && next.is_none()
                    && item.item.reorder_anchor.is_some(),
                hover: (hovered && primitive_bg.is_some()).then_some(palette.hover),
            },
        );
        let color = readable_color(text_background, item.fg.unwrap_or(palette.subtext));
        let mut text_x = placed.x + STATUS_ITEM_PAD + item.item.pad_left;
        let icon = item_icon(item);
        if let Some(slug) = icon {
            let center = Pos2::new(text_x + STATUS_ICON_SIZE / 2.0, row.center().y);
            paint_icon_slug(&painter, slug, center, STATUS_ICON_SIZE, color);
            text_x += STATUS_ICON_SIZE;
        }
        if let Some(ratio) = item.item.gauge {
            if icon.is_some() {
                text_x += STATUS_ICON_GAP;
            }
            paint_battery_gauge(&painter, text_x, row.center().y, ratio, color);
            text_x += STATUS_GAUGE_WIDTH;
        }
        if (item.item.gauge.is_some() || icon.is_some()) && !item.item.text.is_empty() {
            text_x += STATUS_ICON_GAP;
        }
        if !item.item.text.is_empty() {
            painter.text(
                Pos2::new(text_x, row.center().y),
                egui::Align2::LEFT_CENTER,
                &item.item.text,
                font.clone(),
                color,
            );
        }

        if let (Some(resp), Some(action)) = (response.as_ref(), item.item.action.as_deref())
            && resp.clicked_by(egui::PointerButton::Primary)
            && !input.suppress_click
        {
            input.event = Some(StatusBarEvent::ExtensionAction(ExtensionUiAction {
                module: segment.module.to_owned(),
                generation: segment.generation,
                surface: segment.surface.to_owned(),
                action: action.to_owned(),
                payload: serde_json::Value::Null,
            }));
        }
    }
}

fn status_item_id(
    interaction_id: &'static str,
    session_id: Option<&str>,
    segment: &ResolvedSegment<'_>,
    item: &ResolvedItem<'_>,
    item_index: usize,
    key: &str,
) -> egui::Id {
    let role = if item.item.reorder_anchor.is_some() {
        "reorder"
    } else {
        "action"
    };
    egui::Id::new((
        "status-item",
        interaction_id,
        session_id.unwrap_or_default(),
        status_segment_align_id(segment.align),
        segment.source_slot,
        segment.module,
        role,
        key,
        item_index,
    ))
}

fn status_item_cell_index(items: &[ResolvedItem<'_>], index: usize) -> usize {
    let Some(anchor) = items[index].item.reorder_anchor.as_deref() else {
        return index;
    };
    items[..index]
        .iter()
        .rev()
        .take_while(|item| item.item.reorder_anchor.as_deref() == Some(anchor))
        .count()
}

fn status_segment_align_id(align: SegmentAlign) -> &'static str {
    match align {
        SegmentAlign::Left => "left",
        SegmentAlign::Center => "center",
        SegmentAlign::Right => "right",
    }
}

fn tab_context_window_id<'a>(
    segment: &ResolvedSegment<'_>,
    item: &'a ResolvedItem<'_>,
) -> Option<&'a str> {
    let window_id = item.item.reorder_anchor.as_deref()?;
    (is_windows_segment(segment)
        && item.item.action.as_deref().and_then(activate_window_target) == Some(window_id))
    .then_some(window_id)
}

fn tab_context_action(
    response: &egui::Response,
    can_activate: bool,
    can_move_left: bool,
    can_move_right: bool,
    can_navigate: bool,
    can_close_pane: bool,
) -> Option<TabContextAction> {
    use TabContextAction as A;
    let mut action = None;
    response.context_menu(|ui| {
        choose(ui, &mut action, can_activate, "Activate Tab", A::Activate);
        ui.separator();
        choose(ui, &mut action, true, "New Tab", A::NewTab);
        if action.is_none() {
            ui.menu_button("Navigate Tabs", |ui| {
                for (label, candidate) in [
                    ("Previous Tab", A::PreviousTab),
                    ("Next Tab", A::NextTab),
                    ("Last Tab", A::LastTab),
                ] {
                    choose(ui, &mut action, can_navigate, label, candidate);
                }
            });
        }
        ui.separator();
        for (enabled, label, candidate) in [
            (true, "Rename Tab", A::Rename),
            (can_move_left, "Move Tab Left", A::MoveLeft),
            (can_move_right, "Move Tab Right", A::MoveRight),
        ] {
            choose(ui, &mut action, enabled, label, candidate);
        }
        ui.separator();
        choose(ui, &mut action, can_close_pane, "Close Pane", A::ClosePane);
        if action.is_some() {
            ui.close();
        }
    });
    action
}

fn choose(
    ui: &mut egui::Ui,
    action: &mut Option<TabContextAction>,
    enabled: bool,
    label: &str,
    candidate: TabContextAction,
) {
    if action.is_none() && ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        *action = Some(candidate);
    }
}
