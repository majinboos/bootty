//! Painting a status strip and running its interactions: item clicks, a cross-frame reorder drag,
//! and a per-item hook the caller uses to attach whatever an item means (a context menu, say).
//!
//! Geometry comes from [`crate::status_layout`]. Everything here works in terms of layout indices,
//! so the strip never needs to know which producer an item came from or what its action does.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind};

use crate::item_paint::{PrimitivePaintStyle, paint_item_primitives, primitive_background};
use crate::status_layout::{
    ResolvedItem, ResolvedSegment, STATUS_GAUGE_WIDTH, STATUS_ICON_GAP, STATUS_ICON_SIZE,
    STATUS_ITEM_PAD, STATUS_PILL_RADIUS, StatusBarLayout, connected, item_icon,
    paint_battery_gauge, paint_status_diagonal_join, status_blocks, status_item_cell_index,
    status_row_rect, status_segment_align_id,
};
use crate::{ThemePalette, icons::paint_icon_slug, readable_color};

pub struct StatusStrip<'a, 'segments> {
    /// Frame-local geometry built from the resolved segments before the bar height is allocated.
    pub layout: &'a StatusBarLayout<'segments>,
    /// Bar fill; the caller picks it (e.g. a notch-band bar matches the sidebar background).
    pub background: Color32,
    /// Height of the drawable status row. When the allocated bar is taller, items are bottom-aligned
    /// so extra clearance space appears above them instead of stretching the row.
    pub row_height: f32,
    /// Stable per-bar interaction key, so two strips in one window cannot collide.
    pub interaction_id: &'static str,
    /// Extra salt for per-item widget ids. A strip whose subject changed must re-key, or an open
    /// context menu would carry over to a different subject.
    pub identity_salt: &'a str,
}

/// What one strip frame produced. Indices address `layout.segments[segment].items[item]`.
pub enum StripEvent<T> {
    /// The per-item hook produced a value.
    Item(T),
    Clicked {
        segment: usize,
        item: usize,
    },
    Reorder {
        source_slot: usize,
        anchor: String,
        before: Option<String>,
    },
}

pub struct StripFrame<T> {
    pub event: Option<StripEvent<T>>,
    /// The bar's own response, and whether the primary press landed on bare bar (no item claimed
    /// it) — which is what lets a caller turn a press on the strip into a window drag.
    pub response: egui::Response,
    pub background_free: bool,
}

/// Paints the strip and resolves one frame of interaction. `on_item` is called for every
/// interactive item with its response, and may return a caller-defined event.
pub fn show<T>(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    strip: StatusStrip<'_, '_>,
    mut on_item: impl FnMut(&egui::Response, usize, usize) -> Option<T>,
) -> StripFrame<T> {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ui.available_height()),
        egui::Sense::click_and_drag(),
    );
    ui.painter_at(rect).rect_filled(rect, 0.0, strip.background);

    let drag_id = egui::Id::new(strip.interaction_id);
    let mut dragging = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<StripDragState>(drag_id));
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
    let mut input = StripInput {
        palette,
        event: None,
        item_event: false,
        interaction_id: strip.interaction_id,
        identity_salt: strip.identity_salt,
        primary_press_pos,
        drag_blocked: false,
        suppress_click: dragging.is_some(),
        started: None,
    };
    draw(
        ui,
        rect,
        strip.row_height,
        strip.layout,
        &mut input,
        &mut on_item,
    );

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
            drop_target(
                strip.layout,
                drag.source_slot,
                &drag.anchor,
                pos,
                strip.row_height,
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
            event = drop.map(|(before, _)| StripEvent::Reorder {
                source_slot: drag.source_slot,
                anchor: drag.anchor.clone(),
                before,
            });
            ui.ctx()
                .data_mut(|data| data.remove::<StripDragState>(drag_id));
        }
    }
    StripFrame {
        event,
        background_free: !input.drag_blocked,
        response,
    }
}

/// Reorder gesture for a status strip, persisted across frames while the pointer is held.
#[derive(Clone)]
struct StripDragState {
    source_slot: usize,
    anchor: String,
}

/// Paint context plus per-frame interaction accumulators.
struct StripInput<'a, T> {
    palette: ThemePalette,
    /// A hook event (a context-menu choice) outranks a plain click and must not be overwritten by
    /// one raised later in the same pass.
    event: Option<StripEvent<T>>,
    item_event: bool,
    interaction_id: &'static str,
    identity_salt: &'a str,
    primary_press_pos: Option<Pos2>,
    drag_blocked: bool,
    suppress_click: bool,
    started: Option<StripDragState>,
}

/// Picks the insertion slot for a horizontal drag: scans same-segment blocks left to right and
/// drops before the first whose midpoint is past the pointer (or at the end). Returns the anchor
/// to insert before (`None` = end) and the indicator x, or `None` when the drop is a no-op.
pub fn drop_target(
    layout: &StatusBarLayout<'_>,
    source_slot: usize,
    anchor: &str,
    pointer: Pos2,
    row_height: f32,
    rect: Rect,
) -> Option<(Option<String>, f32)> {
    let blocks = status_blocks(layout, source_slot);
    // Which wrapped row the pointer is in, if any. Off the rows, every row stays reachable, so a
    // drag along the bar's edge still lands.
    let lane = blocks
        .iter()
        .find(|block| {
            let row = status_row_rect(rect, row_height, layout.row_count, block.lane);
            pointer.y >= row.min.y + 3.0 && pointer.y <= row.max.y - 3.0
        })
        .map(|block| block.lane);
    let target = crate::reorder::drop_target(&blocks, anchor, pointer.x, lane, false)?;
    Some((target.before.map(str::to_owned), target.indicator))
}

fn draw<T>(
    ui: &mut egui::Ui,
    rect: Rect,
    row_height: f32,
    layout: &StatusBarLayout<'_>,
    input: &mut StripInput<'_, T>,
    on_item: &mut impl FnMut(&egui::Response, usize, usize) -> Option<T>,
) {
    let palette = input.palette;
    let font = egui::FontId::monospace(12.0);
    // A sweeping primitive animates off the frame clock, not off its producer's render interval.
    let time = ui.input(|input| input.time);
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
            let id = item_id(
                input.interaction_id,
                input.identity_salt,
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
        if let Some(response) = response.as_ref()
            && let Some(event) = on_item(response, placed.segment, placed.item)
        {
            input.event = Some(StripEvent::Item(event));
            input.item_event = true;
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
            input.started = Some(StripDragState {
                source_slot: segment.source_slot,
                anchor: anchor.to_owned(),
            });
        }
        let hovered = response.as_ref().is_some_and(egui::Response::hovered)
            || item.item.reorder_anchor.as_deref().is_some_and(|anchor| {
                hovered_anchor.is_some_and(|(segment, hovered)| {
                    segment == placed.segment && hovered == anchor
                })
            });

        // Clip to this item's own row: a primitive that overshoots must not bleed into the tab row
        // above or below it.
        let painter = ui.painter_at(row);
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
            let paint_background = |fill: Option<Color32>, stroke: Option<Color32>| {
                painter.rect(
                    item_rect,
                    corners,
                    fill.unwrap_or(Color32::TRANSPARENT),
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
        paint_item_primitives(
            &painter,
            item_rect,
            &item.item.primitives,
            PrimitivePaintStyle {
                default_color: palette.subtext,
                background: text_background,
                respect_color: false,
                keep: 1.0,
                round_end: segment.round_run_end
                    && next.is_none()
                    && placed.run_complete
                    && item.item.reorder_anchor.is_some(),
                hover: (hovered && primitive_bg.is_some()).then_some(palette.hover),
                time,
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

        if let Some(resp) = response.as_ref()
            && item.item.action.is_some()
            && resp.clicked_by(egui::PointerButton::Primary)
            && !input.suppress_click
            && !input.item_event
        {
            input.event = Some(StripEvent::Clicked {
                segment: placed.segment,
                item: placed.item,
            });
        }
    }
}

fn item_id(
    interaction_id: &'static str,
    identity_salt: &str,
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
        identity_salt,
        status_segment_align_id(segment.align),
        segment.source_slot,
        segment.module,
        role,
        key,
        item_index,
    ))
}
