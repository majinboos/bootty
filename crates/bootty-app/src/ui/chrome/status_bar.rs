use std::ops::Range;

use bootty_extension::ModulePrimitive;
use bootty_ui::{
    ThemePalette,
    icons::{has_slug, paint_icon_slug},
    readable_color,
};
use eframe::egui::{self, CornerRadius, Pos2, Rect, Stroke, StrokeKind};

use crate::config::SegmentAlign;

use super::{
    item_primitives::{paint_item_hover_overlay, paint_item_primitives, primitive_background},
    start_window_drag_on_primary_press,
};

const STATUS_WINDOWS_MODULE: &str = "windows";

#[derive(Clone, Debug)]
pub struct StatusBarModel<'a> {
    /// Ordered, resolved segments. Every segment is a Luau module's items; the app fills these in.
    pub segments: &'a [ResolvedSegment],
    /// Targets that may receive the built-in tab context menu in the current selected session.
    pub tab_context: Option<&'a TabContext>,
    /// Bar fill; set to the sidebar fullscreen background when the bar sits in the notch band.
    pub background: egui::Color32,
    /// Left edge inset before left-aligned segments; zero lets tab strips sit flush to adjacent chrome.
    pub left_padding: f32,
    /// Height of the drawable status row. When the allocated bar is taller, items are bottom-aligned
    /// so extra notch-clearance space appears above them instead of stretching the row.
    pub row_height: f32,
    /// Active fullscreen notch x-range in the same coordinate space as the status bar.
    pub notch_x: Option<Range<f32>>,
    /// Number of tab rows to reserve for the windows segment. Other status modules stay on the
    /// bottom row.
    pub tab_rows: usize,
    /// Stable per-bar interaction state key, so top and bottom bar drags cannot collide.
    pub interaction_id: &'static str,
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
pub struct ResolvedSegment {
    pub align: SegmentAlign,
    /// Stable slot from the configured segment list, retained when window tabs wrap into rows.
    pub source_slot: usize,
    pub items: Vec<ResolvedItem>,
}

/// One drawable element from a module. `action` (e.g. `activate-window:<id>`) is dispatched on click.
#[derive(Clone, Debug, Default)]
pub struct ResolvedItem {
    pub text: String,
    pub icon: Option<String>,
    pub fg: Option<egui::Color32>,
    pub bg: Option<egui::Color32>,
    pub stroke: Option<egui::Color32>,
    /// 0.0-1.0 fill drawn as a battery meter before the text.
    pub gauge: Option<f32>,
    pub primitives: Vec<ModulePrimitive>,
    pub pad_left: f32,
    pub pad_right: f32,
    /// Whether this item may visually connect its background to adjacent items. Defaults to true.
    pub join: Option<bool>,
    /// Whether to keep the normal inter-item gap before this item. Defaults to true.
    pub gap: Option<bool>,
    pub action: Option<String>,
    /// Drag-to-reorder anchor; contiguous items sharing one anchor form a draggable block.
    pub reorder_anchor: Option<String>,
    /// The module that produced this item, so a reorder routes back to its `on_reorder`.
    pub module: String,
    pub generation: u64,
    pub surface: String,
}

/// The outcome of a status-bar frame: an item was clicked, or a draggable item was reordered
/// (routed to `module`'s `on_reorder`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusBarEvent {
    Action {
        module: String,
        generation: u64,
        surface: String,
        action: String,
    },
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

pub fn status_bar_windows_intersect_x_range(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &[ResolvedSegment],
    left_padding: f32,
    x_range: (f32, f32),
) -> bool {
    let font = egui::FontId::monospace(12.0);
    let right = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Right && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let right_start = bar_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, &right, &font);
    let bound = right_start - STATUS_ITEM_GAP;
    let mut x = bar_rect.min.x + left_padding;
    let mut drawn = 0;

    for segment in segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Left && !segment.items.is_empty())
    {
        let width = segment_width(ui, segment, &font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        let window_segment = segment_contains_module(segment, STATUS_WINDOWS_MODULE);
        let visible_end = (x + width).min(bound);
        if window_segment && ranges_intersect((x, visible_end), x_range) {
            return true;
        }
        if x + width > bound {
            break;
        }
        drawn += 1;
        x += width;
    }

    false
}

fn ranges_intersect(a: (f32, f32), b: (f32, f32)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

pub fn status_bar_window_tab_row_count(
    ui: &egui::Ui,
    bar_rect: Rect,
    segments: &[ResolvedSegment],
    left_padding: f32,
    notch_x: Option<(f32, f32)>,
) -> usize {
    let font = egui::FontId::monospace(12.0);
    let right = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Right && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let left = segments
        .iter()
        .filter(|segment| segment.align == SegmentAlign::Left && !segment.items.is_empty())
        .collect::<Vec<_>>();
    let bottom_bound =
        bar_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, &right, &font) - STATUS_ITEM_GAP;
    let top_bound = notch_x
        .map(|(left, _)| left - STATUS_ITEM_GAP)
        .unwrap_or(bar_rect.max.x - STATUS_EDGE_PAD);
    let notch_collision = notch_x.is_some_and(|range| {
        status_bar_windows_intersect_x_range(ui, bar_rect, segments, left_padding, range)
    });

    let mut row_count = if notch_collision { 2 } else { 1 };
    loop {
        let bounds = status_tab_row_bounds(bar_rect, row_count, top_bound, bottom_bound);
        let rows = split_left_segments_for_tab_rows(
            ui,
            &left,
            &font,
            bar_rect.min.x + left_padding,
            &bounds,
            false,
        );
        if rows.len() <= row_count || row_count >= status_window_group_count(&left) {
            return row_count.max(1);
        }
        row_count += 1;
    }
}

/// Native replacement for the tmux status line. Flattens each alignment group's module items and
/// lays them out: left from the left edge, right anchored to the right edge, center centered. Items
/// with a `bg` render as pills; items with an `action` are clickable. Returns a clicked action.
pub fn show_status_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    model: StatusBarModel<'_>,
) -> Option<StatusBarEvent> {
    let height = ui.available_height();
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    ui.painter_at(rect).rect_filled(rect, 0.0, model.background);

    let font = egui::FontId::monospace(12.0);
    let segments_for = |align: SegmentAlign| {
        model
            .segments
            .iter()
            .filter(move |segment| segment.align == align && !segment.items.is_empty())
            .collect::<Vec<_>>()
    };
    let right = segments_for(SegmentAlign::Right);
    let center = segments_for(SegmentAlign::Center);
    let left = segments_for(SegmentAlign::Left);

    let drag_id = egui::Id::new(model.interaction_id);
    let mut dragging = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<StatusDragState>(drag_id));
    let primary_press_pos = ui.input(|input| {
        input
            .pointer
            .button_pressed(egui::PointerButton::Primary)
            .then(|| input.pointer.interact_pos())
            .flatten()
    });
    let primary_down = ui.input(|input| input.pointer.primary_down());
    let pointer_pos = ui.input(|input| {
        input
            .pointer
            .latest_pos()
            .or_else(|| input.pointer.hover_pos())
    });
    let mut input = StatusInput {
        rect,
        palette,
        font: font.clone(),
        clicked: None,
        context_action: None,
        tab_context: model.tab_context.cloned(),
        interaction_id: model.interaction_id,
        primary_press_pos,
        drag_blocked: false,
        suppress_click: dragging.is_some(),
        started: None,
        blocks: Vec::new(),
    };

    if model.tab_rows > 1 {
        draw_status_bar_tab_rows(ui, rect, &model, &left, &center, &right, &mut input);
    } else {
        draw_status_bar_row(
            ui,
            bottom_status_row(rect, model.row_height),
            model.left_padding,
            &left,
            &center,
            &right,
            &mut input,
        );
    }

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

    let mut event = input
        .context_action
        .take()
        .map(
            |(session_id, window_id, action)| StatusBarEvent::ContextAction {
                session_id,
                window_id,
                action,
            },
        )
        .or_else(|| {
            input.clicked.take().map(|action| StatusBarEvent::Action {
                module: action.module,
                generation: action.generation,
                surface: action.surface,
                action: action.action,
            })
        });
    if let Some(drag) = dragging.as_ref() {
        let drop = pointer_pos.and_then(|pos| {
            status_drop_target(
                &input.blocks,
                &drag.module,
                drag.generation,
                &drag.surface,
                &drag.anchor,
                pos,
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
    module: String,
    generation: u64,
    surface: String,
    anchor: String,
}

/// A contiguous run of items sharing a `reorder_anchor`, with its drawn horizontal extent.
struct StatusBlock {
    module: String,
    generation: u64,
    surface: String,
    anchor: String,
    start_x: f32,
    end_x: f32,
    start_y: f32,
    end_y: f32,
}

/// Layout context plus per-frame interaction accumulators, threaded through the status-bar draw
/// pass. Carrying `rect`/`palette`/`font` here keeps the draw fns to a few arguments.
struct StatusInput {
    rect: Rect,
    palette: ThemePalette,
    font: egui::FontId,
    clicked: Option<StatusAction>,
    context_action: Option<(String, String, TabContextAction)>,
    tab_context: Option<TabContext>,
    interaction_id: &'static str,
    primary_press_pos: Option<Pos2>,
    drag_blocked: bool,
    suppress_click: bool,
    started: Option<StatusDragState>,
    blocks: Vec<StatusBlock>,
}

struct StatusAction {
    module: String,
    generation: u64,
    surface: String,
    action: String,
}

/// Picks the insertion slot for a horizontal drag: scans same-module blocks left to right and
/// drops before the first whose midpoint is past the pointer (or at the end). Returns the anchor
/// to insert before (`None` = end) and the indicator x, or `None` when the drop is a no-op.
fn status_drop_target(
    blocks: &[StatusBlock],
    module: &str,
    generation: u64,
    surface: &str,
    anchor: &str,
    pointer: Pos2,
) -> Option<(Option<String>, f32)> {
    let module_blocks: Vec<&StatusBlock> = blocks
        .iter()
        .filter(|block| {
            block.module == module && block.generation == generation && block.surface == surface
        })
        .collect();
    let source_index = module_blocks
        .iter()
        .position(|block| block.anchor == anchor)?;
    let row_blocks = module_blocks
        .iter()
        .copied()
        .filter(|block| pointer.y >= block.start_y && pointer.y <= block.end_y)
        .collect::<Vec<_>>();
    let target_blocks = if row_blocks.is_empty() {
        module_blocks.as_slice()
    } else {
        row_blocks.as_slice()
    };
    let mut target_anchor = None;
    for block in target_blocks {
        if pointer.x < (block.start_x + block.end_x) * 0.5 {
            target_anchor = Some(block.anchor.as_str());
            break;
        }
    }
    let mut target_index = module_blocks.len();
    if let Some(anchor) = target_anchor {
        for (index, block) in module_blocks.iter().enumerate() {
            if block.anchor == anchor {
                target_index = index;
                break;
            }
        }
    } else if let Some(last) = target_blocks.last()
        && let Some(index) = module_blocks
            .iter()
            .position(|block| block.anchor == last.anchor)
    {
        target_index = index + 1;
    }
    if target_blocks.is_empty() {
        for (index, block) in module_blocks.iter().enumerate() {
            if pointer.x < (block.start_x + block.end_x) * 0.5 {
                target_index = index;
                break;
            }
        }
    }
    if target_index == source_index || target_index == source_index + 1 {
        return None;
    }
    let before = module_blocks
        .get(target_index)
        .map(|block| block.anchor.clone());
    let indicator_x = match module_blocks.get(target_index) {
        Some(block) => block.start_x,
        None => module_blocks.last().map_or(pointer.x, |block| block.end_x),
    };
    Some((before, indicator_x))
}

fn segment_width(ui: &egui::Ui, segment: &ResolvedSegment, font: &egui::FontId) -> f32 {
    let items = segment.items.iter().collect::<Vec<_>>();
    items_width(ui, &items, font)
}

/// Width of each item, in order. Laying an item's text out costs a text layout, and the draw path
/// wants the same widths three times over — to place the segment, to hit-test the pointer, and to
/// draw — so it measures once and passes these along.
fn item_widths(ui: &egui::Ui, items: &[&ResolvedItem], font: &egui::FontId) -> Vec<f32> {
    items
        .iter()
        .map(|item| item_width(ui, item, font))
        .collect()
}

/// Width of `items` laid out left to right, gaps included, from widths already measured.
fn laid_out_width(items: &[&ResolvedItem], widths: &[f32]) -> f32 {
    let mut total = 0.0;
    for (index, item) in items.iter().enumerate() {
        if index > 0 && item_gap_before(item) && !connected(Some(items[index - 1]), item) {
            total += STATUS_ITEM_GAP;
        }
        total += widths.get(index).copied().unwrap_or_default();
    }
    total
}

fn segments_width(ui: &egui::Ui, segments: &[&ResolvedSegment], font: &egui::FontId) -> f32 {
    let mut total = 0.0;
    for segment in segments.iter().filter(|segment| !segment.items.is_empty()) {
        if total > 0.0 {
            total += STATUS_ITEM_GAP;
        }
        total += segment_width(ui, segment, font);
    }
    total
}

fn clamped_status_row_height(rect: Rect, row_height: f32) -> f32 {
    row_height.max(0.0).min(rect.height())
}

fn bottom_status_row(rect: Rect, row_height: f32) -> Rect {
    let row_height = clamped_status_row_height(rect, row_height);
    Rect::from_min_max(Pos2::new(rect.min.x, rect.max.y - row_height), rect.max)
}

fn draw_status_bar_row(
    ui: &mut egui::Ui,
    row_rect: Rect,
    left_padding: f32,
    left: &[&ResolvedSegment],
    center: &[&ResolvedSegment],
    right: &[&ResolvedSegment],
    input: &mut StatusInput,
) -> f32 {
    input.rect = row_rect;
    let font = input.font.clone();
    let right_start = row_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, right, &font);
    draw_segments(
        ui,
        right_start,
        row_rect.max.x - STATUS_EDGE_PAD,
        right,
        input,
    );
    let left_end = draw_segments(
        ui,
        row_rect.min.x + left_padding,
        right_start - STATUS_ITEM_GAP,
        left,
        input,
    );
    if !center.is_empty() {
        let center_width = segments_width(ui, center, &font);
        let center_start = row_rect.center().x - center_width / 2.0;
        let center_bound = right_start - STATUS_ITEM_GAP;
        if center_start >= left_end + STATUS_ITEM_GAP && center_start + center_width <= center_bound
        {
            draw_segments(ui, center_start, center_bound, center, input);
        }
    }
    left_end
}

fn draw_status_bar_tab_rows(
    ui: &mut egui::Ui,
    rect: Rect,
    model: &StatusBarModel<'_>,
    left: &[&ResolvedSegment],
    center: &[&ResolvedSegment],
    right: &[&ResolvedSegment],
    input: &mut StatusInput,
) {
    let row_count = model.tab_rows.max(1);
    let bottom_rect = bottom_status_row(rect, model.row_height);
    let font = input.font.clone();
    let bottom_bound =
        bottom_rect.max.x - STATUS_EDGE_PAD - segments_width(ui, right, &font) - STATUS_ITEM_GAP;
    let top_bound = model
        .notch_x
        .as_ref()
        .map_or(rect.max.x - STATUS_EDGE_PAD, |range| {
            range.start - STATUS_ITEM_GAP
        });
    let bounds = status_tab_row_bounds(rect, row_count, top_bound, bottom_bound);
    let row_segments = split_left_segments_for_tab_rows(
        ui,
        left,
        &font,
        rect.min.x + model.left_padding,
        &bounds,
        true,
    );

    for (row_index, segments) in row_segments.iter().take(row_count - 1).enumerate() {
        if segments.is_empty() {
            continue;
        }
        let row_rect = status_row_from_top(rect, model.row_height, row_index);
        input.rect = row_rect;
        let row_refs = segments.iter().collect::<Vec<_>>();
        draw_segments(
            ui,
            row_rect.min.x + model.left_padding,
            bounds[row_index],
            &row_refs,
            input,
        );
    }

    let bottom_left = row_segments
        .get(row_count - 1)
        .map(|segments| segments.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    draw_status_bar_row(
        ui,
        bottom_rect,
        model.left_padding,
        &bottom_left,
        center,
        right,
        input,
    );
}

fn status_tab_row_bounds(
    rect: Rect,
    row_count: usize,
    first_top_bound: f32,
    bottom_bound: f32,
) -> Vec<f32> {
    (0..row_count)
        .map(|index| {
            if index + 1 == row_count {
                bottom_bound
            } else if index == 0 {
                first_top_bound
            } else {
                rect.max.x - STATUS_EDGE_PAD
            }
        })
        .collect()
}

fn status_row_from_top(rect: Rect, row_height: f32, row_index: usize) -> Rect {
    let row_height = clamped_status_row_height(rect, row_height);
    let y = rect.min.y + row_height * row_index as f32;
    Rect::from_min_max(
        Pos2::new(rect.min.x, y),
        Pos2::new(rect.max.x, y + row_height),
    )
}

fn split_left_segments_for_tab_rows(
    ui: &egui::Ui,
    left: &[&ResolvedSegment],
    font: &egui::FontId,
    left_start: f32,
    bounds: &[f32],
    force_last_row: bool,
) -> Vec<Vec<ResolvedSegment>> {
    let Some(window_index) = left
        .iter()
        .position(|segment| segment_contains_module(segment, STATUS_WINDOWS_MODULE))
    else {
        return vec![left.iter().map(|segment| (*segment).clone()).collect()];
    };
    if bounds.is_empty() {
        return Vec::new();
    }

    let window_segment = left[window_index];
    let items = window_segment.items.iter().collect::<Vec<_>>();
    let mut rows = vec![Vec::new(); bounds.len()];
    let mut item_start = 0;
    let bottom_index = bounds.len() - 1;
    let bottom_start = status_bottom_window_start(ui, left, font, left_start, window_index);

    for row_index in 0..bounds.len() {
        if item_start >= items.len() {
            break;
        }
        let row_start = if row_index == bottom_index {
            bottom_start
        } else {
            left_start
        };
        let split = status_items_split_index_before_x(
            ui,
            &items[item_start..],
            font,
            row_start,
            bounds[row_index],
        );
        let item_end = if split == 0 {
            if force_last_row && row_index == bottom_index {
                status_item_group_end(&items, item_start)
            } else {
                continue;
            }
        } else {
            item_start + split
        };
        let mut row_items = window_segment.items[item_start..item_end].to_vec();
        round_window_row_end(&mut row_items);
        rows[row_index].push(ResolvedSegment {
            align: window_segment.align,
            source_slot: window_segment.source_slot,
            items: row_items,
        });
        item_start = item_end;
    }
    if item_start < items.len() {
        let mut row_items = window_segment.items[item_start..].to_vec();
        round_window_row_end(&mut row_items);
        rows.push(vec![ResolvedSegment {
            align: window_segment.align,
            source_slot: window_segment.source_slot,
            items: row_items,
        }]);
    }

    let bottom_windows = rows[bottom_index].clone();
    let mut bottom = Vec::new();
    for (index, segment) in left.iter().enumerate() {
        if index == window_index {
            bottom.extend(bottom_windows.clone());
        } else {
            bottom.push((*segment).clone());
        }
    }
    rows[bottom_index] = bottom;
    rows
}

fn round_window_row_end(items: &mut [ResolvedItem]) {
    let Some(item) = items
        .last_mut()
        .filter(|item| item.module == STATUS_WINDOWS_MODULE && item.reorder_anchor.is_some())
    else {
        return;
    };

    for primitive in &mut item.primitives {
        if let ModulePrimitive::Rect { radius, .. } = primitive {
            radius.ne = STATUS_PILL_RADIUS;
            radius.se = STATUS_PILL_RADIUS;
        }
    }
}

fn status_bottom_window_start(
    ui: &egui::Ui,
    left: &[&ResolvedSegment],
    font: &egui::FontId,
    left_start: f32,
    window_index: usize,
) -> f32 {
    let mut x = left_start;
    let mut drawn = 0;
    for segment in left.iter().take(window_index) {
        let width = segment_width(ui, segment, font);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        x += width;
        drawn += 1;
    }
    if drawn > 0 {
        x += STATUS_ITEM_GAP;
    }
    x
}

fn status_window_group_count(left: &[&ResolvedSegment]) -> usize {
    let Some(segment) = left
        .iter()
        .find(|segment| segment_contains_module(segment, STATUS_WINDOWS_MODULE))
    else {
        return 1;
    };
    let items = segment.items.iter().collect::<Vec<_>>();
    let mut count = 0;
    let mut index = 0;
    while index < items.len() {
        count += 1;
        index = status_item_group_end(&items, index);
    }
    count.max(1)
}

fn segment_contains_module(segment: &ResolvedSegment, module: &str) -> bool {
    segment.items.iter().any(|item| item.module == module)
}

fn status_items_split_index_before_x(
    ui: &egui::Ui,
    items: &[&ResolvedItem],
    font: &egui::FontId,
    start_x: f32,
    bound: f32,
) -> usize {
    let mut x = start_x;
    let mut index = 0;
    while index < items.len() {
        let end = status_item_group_end(items, index);
        let width = status_item_group_width(ui, items, font, index, end);
        let gap = if index > 0
            && item_gap_before(items[index])
            && !connected(Some(items[index - 1]), items[index])
        {
            STATUS_ITEM_GAP
        } else {
            0.0
        };
        if x + gap + width > bound {
            return index;
        }
        x += gap + width;
        index = end;
    }
    items.len()
}

fn status_item_group_end(items: &[&ResolvedItem], start: usize) -> usize {
    let anchor = items[start].reorder_anchor.as_deref();
    let mut end = start + 1;
    while end < items.len() && anchor.is_some() && items[end].reorder_anchor.as_deref() == anchor {
        end += 1;
    }
    end
}

fn status_item_group_width(
    ui: &egui::Ui,
    items: &[&ResolvedItem],
    font: &egui::FontId,
    start: usize,
    end: usize,
) -> f32 {
    let mut width = 0.0;
    for index in start..end {
        if index > start
            && item_gap_before(items[index])
            && !connected(Some(items[index - 1]), items[index])
        {
            width += STATUS_ITEM_GAP;
        }
        width += item_width(ui, items[index], font);
    }
    width
}

fn draw_segments(
    ui: &mut egui::Ui,
    start_x: f32,
    bound: f32,
    segments: &[&ResolvedSegment],
    input: &mut StatusInput,
) -> f32 {
    let font = input.font.clone();
    let mut x = start_x;
    let mut drawn = 0;
    for segment in segments {
        let items = segment.items.iter().collect::<Vec<_>>();
        let widths = item_widths(ui, &items, &font);
        let width = laid_out_width(&items, &widths);
        if width <= 0.0 {
            continue;
        }
        if drawn > 0 {
            x += STATUS_ITEM_GAP;
        }
        if x + width > bound {
            if segment_contains_module(segment, STATUS_WINDOWS_MODULE) && x < bound {
                draw_items(ui, x, bound, segment, &items, &widths, input);
            }
            break;
        }
        draw_items(ui, x, x + width, segment, &items, &widths, input);
        drawn += 1;
        x += width;
    }
    x
}

fn text_width(ui: &egui::Ui, text: &str, font: &egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
        .size()
        .x
}

/// Whether the item draws an iconflow glyph for the requested slug.
fn item_icon(item: &ResolvedItem) -> Option<&str> {
    item.icon.as_deref().filter(|slug| has_slug(slug))
}

fn item_width(ui: &egui::Ui, item: &ResolvedItem, font: &egui::FontId) -> f32 {
    let mut inner = text_width(ui, &item.text, font);
    let mut lead = 0.0;
    if item.gauge.is_some() {
        lead += STATUS_GAUGE_WIDTH;
    }
    if item_icon(item).is_some() {
        if lead > 0.0 {
            lead += STATUS_ICON_GAP;
        }
        lead += STATUS_ICON_SIZE;
    }
    if lead > 0.0 {
        inner += lead;
        if !item.text.is_empty() {
            inner += STATUS_ICON_GAP;
        }
    }
    inner + STATUS_ITEM_PAD * 2.0 + item.pad_left + item.pad_right
}

/// Adjacent items that both carry a background render as one connected strip (no
/// gap), like the tmux/mux segmented bar, unless a module opts either item out.
fn connected(prev: Option<&ResolvedItem>, cur: &ResolvedItem) -> bool {
    prev.is_some_and(|prev| {
        item_join(prev) && item_join(cur) && prev.bg.is_some() && prev.stroke.is_none()
    }) && cur.bg.is_some()
        && cur.stroke.is_none()
}

fn item_join(item: &ResolvedItem) -> bool {
    item.join.unwrap_or(true)
}

fn item_gap_before(item: &ResolvedItem) -> bool {
    item.gap.unwrap_or(true)
}

fn items_width(ui: &egui::Ui, items: &[&ResolvedItem], font: &egui::FontId) -> f32 {
    laid_out_width(items, &item_widths(ui, items, font))
}

fn hovered_reorder_anchor(
    ui: &egui::Ui,
    rect: Rect,
    start_x: f32,
    bound: f32,
    items: &[&ResolvedItem],
    widths: &[f32],
) -> Option<(String, String)> {
    let hover_pos = ui.input(|input| input.pointer.hover_pos())?;
    let mut x = start_x;
    for index in 0..items.len() {
        let item = items[index];
        let prev = (index > 0).then(|| items[index - 1]);
        if prev.is_some() && item_gap_before(item) && !connected(prev, item) {
            x += STATUS_ITEM_GAP;
        }
        let width = widths.get(index).copied().unwrap_or_default();
        if x + width > bound {
            break;
        }
        let item_rect = Rect::from_min_size(
            Pos2::new(x, rect.min.y + 3.0),
            egui::vec2(width, rect.height() - 6.0),
        );
        if item_rect.contains(hover_pos)
            && let Some(anchor) = item.reorder_anchor.as_deref()
        {
            return Some((item.module.clone(), anchor.to_owned()));
        }
        x += width;
    }
    None
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

fn paint_status_item_background(
    painter: &egui::Painter,
    item_rect: Rect,
    bg: Option<egui::Color32>,
    stroke: Option<egui::Color32>,
    corners: CornerRadius,
) {
    if let Some(bg) = bg {
        painter.rect_filled(item_rect, corners, bg);
    }
    if let Some(stroke) = stroke {
        painter.rect_stroke(
            item_rect,
            corners,
            Stroke::new(1.0, stroke),
            StrokeKind::Inside,
        );
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

fn draw_items(
    ui: &mut egui::Ui,
    start_x: f32,
    bound: f32,
    segment: &ResolvedSegment,
    items: &[&ResolvedItem],
    widths: &[f32],
    input: &mut StatusInput,
) {
    let rect = input.rect;
    let palette = input.palette;
    let font = input.font.clone();
    let hovered_anchor = hovered_reorder_anchor(ui, rect, start_x, bound, items, widths);
    let mut x = start_x;
    for index in 0..items.len() {
        let item = items[index];
        let prev = (index > 0).then(|| items[index - 1]);
        let next = items.get(index + 1).copied();
        if prev.is_some() && item_gap_before(item) && !connected(prev, item) {
            x += STATUS_ITEM_GAP;
        }
        let width = widths.get(index).copied().unwrap_or_default();
        if x + width > bound {
            break;
        }
        let item_rect = Rect::from_min_size(
            Pos2::new(x, rect.min.y + 3.0),
            egui::vec2(width, rect.height() - 6.0),
        );

        // An anchored item drags; otherwise an action item just clicks. Keep the identity tied to
        // the bar, segment, and cell rather than x so an open context menu survives text reflow.
        let interactive = item.reorder_anchor.as_deref().or(item.action.as_deref());
        let response = interactive.map(|key| {
            let id = status_item_id(
                input.interaction_id,
                input
                    .tab_context
                    .as_ref()
                    .map(|context| context.session_id.as_str()),
                segment,
                segment.source_slot,
                item,
                status_item_cell_index(items, index),
                key,
            );
            let sense = if item.reorder_anchor.is_some() {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::click()
            };
            ui.interact(item_rect, id, sense)
        });
        if let (Some(response), Some(window_id)) = (response.as_ref(), tab_context_window_id(item))
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
                .map(|action| (context.session_id.clone(), window_id.to_owned(), action))
            });
            if let Some(context_action) = context_action {
                input.context_action = Some(context_action);
            }
        }
        if interactive.is_some()
            && input
                .primary_press_pos
                .is_some_and(|pos| item_rect.contains(pos))
        {
            input.drag_blocked = true;
        }
        if let Some(anchor) = item.reorder_anchor.as_deref() {
            match input.blocks.last_mut() {
                Some(block)
                    if block.module == item.module
                        && block.generation == item.generation
                        && block.surface == item.surface
                        && block.anchor == anchor =>
                {
                    block.end_x = x + width;
                    block.start_y = block.start_y.min(item_rect.min.y);
                    block.end_y = block.end_y.max(item_rect.max.y);
                }
                _ => input.blocks.push(StatusBlock {
                    module: item.module.clone(),
                    generation: item.generation,
                    surface: item.surface.clone(),
                    anchor: anchor.to_owned(),
                    start_x: x,
                    end_x: x + width,
                    start_y: item_rect.min.y,
                    end_y: item_rect.max.y,
                }),
            }
        }
        if let Some(anchor) = item.reorder_anchor.as_deref()
            && response
                .as_ref()
                .is_some_and(|response| response.drag_started_by(egui::PointerButton::Primary))
        {
            input.started = Some(StatusDragState {
                module: item.module.clone(),
                generation: item.generation,
                surface: item.surface.clone(),
                anchor: anchor.to_owned(),
            });
        }
        let hovered = response.as_ref().is_some_and(egui::Response::hovered)
            || item.reorder_anchor.as_deref().is_some_and(|anchor| {
                hovered_anchor
                    .as_ref()
                    .is_some_and(|(module, hovered)| module == &item.module && hovered == anchor)
            });

        let painter = ui.painter_at(rect);
        let primitive_bg = primitive_background(&item.primitives);
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
            paint_status_item_background(&painter, item_rect, item.bg, item.stroke, corners);
            if let Some(bg) = item.bg
                && left_join
                && prev.and_then(|prev| prev.bg) != Some(bg)
            {
                paint_status_diagonal_join(&painter, item_rect, bg);
            }
            if let Some(hover_background) = hover_background {
                paint_status_item_background(
                    &painter,
                    item_rect,
                    Some(hover_background),
                    None,
                    corners,
                );
                if left_join {
                    paint_status_diagonal_join(&painter, item_rect, hover_background);
                }
            }
        } else if let Some(hover_background) = hover_background
            && primitive_bg.is_none()
        {
            painter.rect_filled(item_rect, STATUS_PILL_RADIUS, hover_background);
        }
        paint_item_primitives(
            &painter,
            item_rect,
            &item.primitives,
            palette.subtext,
            text_background,
            false,
            1.0,
        );
        if hovered && primitive_bg.is_some() {
            paint_item_hover_overlay(&painter, item_rect, &item.primitives, palette.hover);
        }
        let color = readable_color(text_background, item.fg.unwrap_or(palette.subtext));
        let mut text_x = x + STATUS_ITEM_PAD + item.pad_left;
        if let Some(slug) = item_icon(item) {
            let center = Pos2::new(text_x + STATUS_ICON_SIZE / 2.0, rect.center().y);
            paint_icon_slug(&painter, slug, center, STATUS_ICON_SIZE, color);
            text_x += STATUS_ICON_SIZE;
        }
        if let Some(ratio) = item.gauge {
            if item_icon(item).is_some() {
                text_x += STATUS_ICON_GAP;
            }
            paint_battery_gauge(&painter, text_x, rect.center().y, ratio, color);
            text_x += STATUS_GAUGE_WIDTH;
        }
        if (item.gauge.is_some() || item_icon(item).is_some()) && !item.text.is_empty() {
            text_x += STATUS_ICON_GAP;
        }
        if !item.text.is_empty() {
            painter.text(
                Pos2::new(text_x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &item.text,
                font.clone(),
                color,
            );
        }

        if let (Some(resp), Some(action)) = (response.as_ref(), item.action.as_deref())
            && resp.clicked_by(egui::PointerButton::Primary)
            && !input.suppress_click
        {
            input.clicked = Some(StatusAction {
                module: item.module.clone(),
                generation: item.generation,
                surface: item.surface.clone(),
                action: action.to_owned(),
            });
        }
        x += width;
    }
}

fn status_item_id(
    interaction_id: &'static str,
    session_id: Option<&str>,
    segment: &ResolvedSegment,
    segment_slot: usize,
    item: &ResolvedItem,
    item_index: usize,
    key: &str,
) -> egui::Id {
    let role = if item.reorder_anchor.is_some() {
        "reorder"
    } else {
        "action"
    };
    egui::Id::new((
        "status-item",
        interaction_id,
        session_id.unwrap_or_default(),
        status_segment_align_id(segment.align),
        segment_slot,
        item.module.as_str(),
        role,
        key,
        item_index,
    ))
}

fn status_item_cell_index(items: &[&ResolvedItem], index: usize) -> usize {
    let Some(anchor) = items[index].reorder_anchor.as_deref() else {
        return index;
    };
    items[..index]
        .iter()
        .rev()
        .take_while(|item| item.reorder_anchor.as_deref() == Some(anchor))
        .count()
}

fn status_segment_align_id(align: SegmentAlign) -> &'static str {
    match align {
        SegmentAlign::Left => "left",
        SegmentAlign::Center => "center",
        SegmentAlign::Right => "right",
    }
}

fn tab_context_window_id(item: &ResolvedItem) -> Option<&str> {
    let window_id = item.reorder_anchor.as_deref()?;
    (item.module == STATUS_WINDOWS_MODULE
        && item
            .action
            .as_deref()
            .and_then(|action| action.strip_prefix("activate-window:"))
            == Some(window_id))
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
    let mut action = None;
    response.context_menu(|ui| {
        if ui
            .add_enabled(can_activate, egui::Button::new("Activate Tab"))
            .clicked()
        {
            action = Some(TabContextAction::Activate);
        }
        ui.separator();
        if action.is_none() && ui.button("New Tab").clicked() {
            action = Some(TabContextAction::NewTab);
        }
        if action.is_none() {
            ui.menu_button("Navigate Tabs", |ui| {
                if ui
                    .add_enabled(can_navigate, egui::Button::new("Previous Tab"))
                    .clicked()
                {
                    action = Some(TabContextAction::PreviousTab);
                }
                if action.is_none()
                    && ui
                        .add_enabled(can_navigate, egui::Button::new("Next Tab"))
                        .clicked()
                {
                    action = Some(TabContextAction::NextTab);
                }
                if action.is_none()
                    && ui
                        .add_enabled(can_navigate, egui::Button::new("Last Tab"))
                        .clicked()
                {
                    action = Some(TabContextAction::LastTab);
                }
            });
        }
        ui.separator();
        if action.is_none() && ui.button("Rename Tab").clicked() {
            action = Some(TabContextAction::Rename);
        }
        if action.is_none()
            && ui
                .add_enabled(can_move_left, egui::Button::new("Move Tab Left"))
                .clicked()
        {
            action = Some(TabContextAction::MoveLeft);
        }
        if action.is_none()
            && ui
                .add_enabled(can_move_right, egui::Button::new("Move Tab Right"))
                .clicked()
        {
            action = Some(TabContextAction::MoveRight);
        }
        ui.separator();
        if action.is_none()
            && ui
                .add_enabled(can_close_pane, egui::Button::new("Close Pane"))
                .clicked()
        {
            action = Some(TabContextAction::ClosePane);
        }
        if action.is_some() {
            ui.close();
        }
    });
    action
}
