//! Laying out a status strip: measuring items, packing them into aligned groups, wrapping window
//! tabs around an obstruction (a display notch), and painting the primitive shapes an item can ask
//! for.
//!
//! Pure geometry over borrowed items. Interaction — clicks, drags, context menus — belongs to the
//! caller, which owns what an item *means*.

use std::ops::Range;

use bootty_item::ModuleItem;
use eframe::egui::{self, Pos2, Rect, Stroke, StrokeKind};

use crate::icons::has_slug;

/// Where a segment sits in the strip.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Borrowed, frame-local status geometry. Text is measured once while this is built; painting,
/// hit-testing, and drag/drop all consume the same coordinates.
pub struct StatusBarLayout<'a> {
    pub segments: &'a [ResolvedSegment<'a>],
    pub items: Vec<StatusLayoutItem>,
    pub row_count: usize,
}
impl StatusBarLayout<'_> {
    pub fn row_count(&self) -> usize {
        self.row_count
    }
}
pub struct StatusLayoutItem {
    pub segment: usize,
    pub item: usize,
    pub row: usize,
    pub x: f32,
    pub width: f32,
    pub run_start: bool,
    /// Whether this is the last item of its segment that got placed. A run cut short by the bar's
    /// edge must not be rounded off, or a half-drawn tab reads as a closed one.
    pub run_complete: bool,
}

/// A status segment resolved for this frame: a module's items plus where the segment is aligned.
#[derive(Clone, Debug, Default)]
pub struct ResolvedSegment<'a> {
    pub align: Align,
    /// Whether this segment's items may wrap into extra rows to clear an obstruction. The caller
    /// decides; the strip only honours it.
    pub wrappable: bool,
    /// Whether the trailing edge of this segment's last anchored run is rounded off, the way a row
    /// of tabs closes. Declared by the caller; the strip only honours it.
    pub round_run_end: bool,
    /// Stable slot from the configured segment list, retained when window tabs wrap into rows.
    pub source_slot: usize,
    /// Producer identity used to route interactions back to the exact extension generation.
    pub module: &'a str,
    pub generation: u64,
    pub surface: &'a str,
    pub items: Vec<ResolvedItem<'a>>,
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

pub const STATUS_EDGE_PAD: f32 = 12.0;
pub const STATUS_ITEM_GAP: f32 = 4.0;
pub const STATUS_ITEM_PAD: f32 = 10.0;
pub const STATUS_ICON_GAP: f32 = 6.0;
/// Extra clearance required before already-wrapped tabs collapse back to one row.
pub const STATUS_NOTCH_WRAP_HYSTERESIS: f32 = 4.0;
/// Square edge of a status-bar icon glyph, matched to the 12pt text.
pub const STATUS_ICON_SIZE: f32 = 14.0;
/// Battery meter dimensions for a `gauge` item (body width excludes the nub).
pub const STATUS_GAUGE_WIDTH: f32 = 22.0;
pub const STATUS_GAUGE_HEIGHT: f32 = 11.0;
/// Corner radius (logical px) for status-bar pills and the strip's outer ends.
pub const STATUS_PILL_RADIUS: u8 = 6;
pub const STATUS_DIAGONAL_JOIN_WIDTH: f32 = 8.0;
pub fn status_bar_layout<'a>(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &'a [ResolvedSegment<'a>],
    left_padding: f32,
    notch_x: Option<(f32, f32)>,
) -> StatusBarLayout<'a> {
    status_bar_layout_with_tab_wrap(ui, bar_rect, segments, left_padding, notch_x, false)
}

/// Lay out the status bar while retaining a small hysteresis window around the notch boundary.
/// `tabs_were_wrapped` is the previous frame's result for the top window-tab segment.
pub fn status_bar_layout_with_tab_wrap<'a>(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &'a [ResolvedSegment<'a>],
    left_padding: f32,
    notch_x: Option<(f32, f32)>,
    tabs_were_wrapped: bool,
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
            Align::Left => &mut left,
            Align::Center => &mut center,
            Align::Right => &mut right,
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
        .find(|index| segments[*index].wrappable);
    let (window_start, window_span) =
        window_geometry(segments, &widths, &left, left_start, bottom_bound);
    let notch_collision = notch_x.is_some_and(|notch| {
        window_span.is_some_and(|span| {
            let wrap_boundary = if tabs_were_wrapped {
                notch.0 - STATUS_NOTCH_WRAP_HYSTERESIS
            } else {
                notch.0
            };
            span.0 < notch.1 && wrap_boundary < span.1
        })
    });
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

/// One block per run of adjacent cells sharing a reorder anchor, in paint order.
pub fn status_blocks<'a>(
    layout: &'a StatusBarLayout<'a>,
    source_slot: usize,
) -> Vec<crate::reorder::ReorderBlock<'a>> {
    crate::reorder::blocks_from(layout.items.iter().map(|placed| {
        let segment = &layout.segments[placed.segment];
        let anchor = (segment.source_slot == source_slot)
            .then(|| segment.items[placed.item].item.reorder_anchor.as_deref())
            .flatten();
        (anchor, placed.row, placed.x, placed.x + placed.width)
    }))
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
pub fn status_row_rect(rect: Rect, row_height: f32, row_count: usize, row: usize) -> Rect {
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
        if segments[index].wrappable {
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
pub fn window_group_count(segment: &ResolvedSegment<'_>) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < segment.items.len() {
        count += 1;
        index = item_group_end(&segment.items, index);
    }
    count.max(1)
}
pub fn item_group_end(items: &[ResolvedItem<'_>], start: usize) -> usize {
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
            if x + gap + width > bound && !self.segments[segment].wrappable {
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
                run_complete: index + 1 == range.end,
            });
            x += width;
        }
    }
}

/// Whether the item draws an iconflow glyph for the requested slug.
pub fn item_icon<'a>(item: &'a ResolvedItem<'a>) -> Option<&'a str> {
    item.icon.filter(|slug| has_slug(slug))
}
pub fn item_width(ui: &egui::Ui, item: &ResolvedItem<'_>, font: &egui::FontId) -> f32 {
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
/// Adjacent items that both carry a background render as one connected strip (no
/// gap), like the tmux/mux segmented bar, unless a module opts either item out.
pub fn connected(prev: Option<&ResolvedItem<'_>>, cur: &ResolvedItem<'_>) -> bool {
    prev.is_some_and(|prev| {
        prev.item.join.unwrap_or(true)
            && cur.item.join.unwrap_or(true)
            && prev.bg.is_some()
            && prev.stroke.is_none()
    }) && cur.bg.is_some()
        && cur.stroke.is_none()
}
pub fn gap_before(items: &[ResolvedItem<'_>], index: usize, start: usize) -> f32 {
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
pub fn paint_battery_gauge(
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
pub fn paint_status_diagonal_join(painter: &egui::Painter, item_rect: Rect, color: egui::Color32) {
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
pub fn status_item_cell_index(items: &[ResolvedItem<'_>], index: usize) -> usize {
    let Some(anchor) = items[index].item.reorder_anchor.as_deref() else {
        return index;
    };
    items[..index]
        .iter()
        .rev()
        .take_while(|item| item.item.reorder_anchor.as_deref() == Some(anchor))
        .count()
}
pub fn status_segment_align_id(align: Align) -> &'static str {
    match align {
        Align::Left => "left",
        Align::Center => "center",
        Align::Right => "right",
    }
}
