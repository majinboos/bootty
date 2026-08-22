use bootty_extension::{ExtensionUiAction, PublishedSurfaceItem};
use bootty_mux::controller::MuxScope;
use bootty_ui::{ThemePalette, mix};
use eframe::egui::{self, Pos2, Rect, Stroke, TextureHandle};

use crate::ui::{session_navigation::ScopedSessionTarget, sidebar::SidebarItem};

use super::start_window_drag_on_primary_press;
use bootty_ui::item_list::{self, ListRow, ROW_HEIGHT, ROW_PAD_X};

#[derive(Clone)]
pub struct SidebarModel<'a> {
    pub items: &'a [SidebarItem<'a>],
    pub footer_items: &'a [PublishedSurfaceItem],
    pub session_count: usize,
    pub title_visible: bool,
    pub reserve_titlebar_buttons: bool,
    pub title_icon: Option<&'a TextureHandle>,
    pub top_inset: f32,
    pub border_visible: bool,
    pub border_bottom: bool,
    pub separator_visible: bool,
    pub focused: bool,
    pub hovered_session: Option<&'a ScopedSessionTarget>,
    /// Explicit color overrides from `[sidebar]`; each falls back to a theme-derived tint.
    pub fullscreen: bool,
    pub hover_override: Option<egui::Color32>,
    pub current_override: Option<egui::Color32>,
    pub border_override: Option<egui::Color32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarEvent {
    ExtensionAction(ExtensionUiAction),
    ActivateSession(ScopedSessionTarget),
    ContextAction {
        target: ScopedSessionTarget,
        action: SessionContextAction,
    },
    Reorder {
        source: String,
        before: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionContextAction {
    Activate,
    NewSession,
    SwitchSession,
    PreviousSession,
    NextSession,
    LastSession,
    Rename,
    MoveUp,
    MoveDown,
    Detach,
    Ditch,
}

const SIDEBAR_HEADER_HEIGHT: f32 = 44.0;
const SIDEBAR_FOOTER_BASE_HEIGHT: f32 = 14.0;
const SIDEBAR_MAX_FOOTER_ITEMS: usize = 4;
pub(super) const SIDEBAR_FOOTER_ITEM_HEIGHT: f32 = 30.0;
pub(crate) const MACOS_TITLEBAR_BUTTON_SAFE_WIDTH: f32 = 72.0;
const MACOS_TITLEBAR_BUTTON_CENTER_Y: f32 = 16.0;

/// Every row a session owns — its title row plus the detail/progress rows beneath it — points at
/// that session, so hovering or clicking anywhere in the block hits the whole session component.
/// The visual half of a row, for the shared painters. Identity and actions stay here.
fn row_visual<'a>(item: &'a SidebarItem<'a>) -> ListRow<'a> {
    ListRow {
        text: item.text,
        number: item.number,
        indent: item.indent,
        tree: item.tree,
        kind: item.kind,
        color: item.color,
        dim_color: item.dim_color,
        icon: item.icon,
        primitives: item.primitives,
        current: item.current,
        active: item.active,
    }
}

fn sidebar_session_key<'a>(item: &'a SidebarItem<'a>) -> Option<(MuxScope, &'a str)> {
    Some((item.scope, item.session_id?))
}

pub fn show_sidebar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    model: SidebarModel<'_>,
) -> Option<SidebarEvent> {
    // `palette` arrives with `base`/`foreground` already overridden. An explicit hover override
    // wins outright; otherwise the shared lift applies.
    let hover_color = model
        .hover_override
        .unwrap_or_else(|| super::sidebar_hover_color(palette, model.fullscreen));
    let current_color = model
        .current_override
        .unwrap_or_else(|| mix(palette.base, palette.text, 0.065));
    let border_color = model
        .border_override
        .unwrap_or_else(|| mix(palette.base, palette.text, 0.09));
    let width = ui.max_rect().width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    if model.border_visible {
        let stroke = Stroke::new(1.0, border_color);
        painter.line_segment([rect.left_top(), rect.right_top()], stroke);
        painter.line_segment([rect.left_top(), rect.left_bottom()], stroke);
        painter.line_segment([rect.right_top(), rect.right_bottom()], stroke);
        if model.border_bottom {
            painter.line_segment([rect.left_bottom(), rect.right_bottom()], stroke);
        }
    }

    let header_h = if model.title_visible {
        SIDEBAR_HEADER_HEIGHT
    } else {
        0.0
    };
    let content_top = rect.min.y + model.top_inset;
    let title_rect = Rect::from_min_max(
        Pos2::new(rect.min.x, content_top),
        Pos2::new(rect.max.x, (content_top + header_h).min(rect.max.y)),
    );
    if model.title_visible {
        paint_sidebar_title(ui, title_rect, palette, &model);
        let reserved = if model.reserve_titlebar_buttons {
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        } else {
            0.0
        };
        let drag_rect = Rect::from_min_max(
            Pos2::new(
                (title_rect.min.x + reserved).min(title_rect.max.x),
                title_rect.min.y,
            ),
            title_rect.max,
        );
        let response = ui.interact(
            drag_rect,
            ui.id().with("sidebar-titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        start_window_drag_on_primary_press(&response);
    }

    let list_top = content_top + header_h;

    let footer_items =
        &model.footer_items[..model.footer_items.len().min(SIDEBAR_MAX_FOOTER_ITEMS)];
    let footer_h =
        SIDEBAR_FOOTER_BASE_HEIGHT + footer_items.len() as f32 * SIDEBAR_FOOTER_ITEM_HEIGHT;
    if model.session_count == 0 {
        painter.text(
            Pos2::new(rect.center().x, list_top + 42.0),
            egui::Align2::CENTER_CENTER,
            "no sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
    }

    let list_bottom = (height - footer_h).max(model.top_inset + header_h);
    let max_rows = ((list_bottom - model.top_inset - header_h) / ROW_HEIGHT)
        .floor()
        .max(0.0) as usize;
    let items = &model.items[..model.items.len().min(max_rows)];
    let drag_id = egui::Id::new("mux-sidebar-drag-anchor");
    let mut dragged = ui
        .ctx()
        .data_mut(|data| data.get_persisted::<SidebarDragState>(drag_id));
    let pointer_pos = ui.input(|input| {
        input
            .pointer
            .latest_pos()
            .or_else(|| input.pointer.hover_pos())
    });
    let primary_down = ui.input(|input| input.pointer.primary_down());
    let pointer_hovered_session = pointer_pos
        .and_then(|pos| item_list::hovered_row(pos, rect.min.x, list_top, width, max_rows))
        .and_then(|index| items.get(index))
        .filter(|item| item.selectable)
        .and_then(sidebar_session_key);
    let model_hovered_session = model
        .hovered_session
        .map(|target| (target.scope, target.session_id.as_str()));
    let suppress_click = dragged.is_some();

    let mut event = None;
    for (index, item) in items.iter().enumerate() {
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, list_top + index as f32 * ROW_HEIGHT),
            egui::vec2(width, ROW_HEIGHT),
        );
        let item_key = sidebar_session_key(item);
        let hovered = item.selectable
            && item_key.is_some_and(|key| {
                Some(key) == pointer_hovered_session
                    || model.focused && Some(key) == model_hovered_session
            });
        let response = sidebar_item_row(
            ui,
            row_rect,
            item,
            hovered,
            palette,
            hover_color,
            current_color,
        );
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(anchor) = item.reorder_anchor
        {
            let state = SidebarDragState {
                anchor: anchor.to_owned(),
                preview: items
                    .iter()
                    .find(|item| item.reorder_anchor == Some(anchor))
                    .map_or_else(|| anchor.to_owned(), |item| item.text.to_owned()),
            };
            ui.ctx()
                .data_mut(|data| data.insert_persisted(drag_id, state.clone()));
            dragged = Some(state);
            ui.ctx().request_repaint();
        }

        let clicked = !suppress_click && response.clicked_by(egui::PointerButton::Primary);
        if event.is_none() && clicked {
            event = item
                .extension_action
                .clone()
                .map(SidebarEvent::ExtensionAction);
            if event.is_none()
                && item.selectable
                && let Some((scope, session_id)) = item_key
            {
                event = Some(SidebarEvent::ActivateSession(ScopedSessionTarget::new(
                    scope, session_id,
                )));
            }
        }
        if event.is_none()
            && item.selectable
            && let Some(key) = item_key
            && let Some((position, binding_session_count)) = item.context_position
            && let Some(action) = session_context_action(
                &response,
                !item.current,
                item.reorder_anchor.is_some() && position > 0,
                item.reorder_anchor.is_some() && position + 1 < binding_session_count,
                binding_session_count > 1,
                item.can_return_to_last_session,
            )
        {
            event = Some(SidebarEvent::ContextAction {
                target: ScopedSessionTarget::new(key.0, key.1),
                action,
            });
        }
    }

    let drop = dragged.as_ref().and_then(|drag| {
        sidebar_drop_target(
            items,
            pointer_pos,
            rect.min.x,
            list_top,
            width,
            &drag.anchor,
        )
    });
    if let Some((_, indicator_y)) = drop {
        painter.line_segment(
            [
                Pos2::new(rect.min.x, indicator_y),
                Pos2::new(rect.max.x, indicator_y),
            ],
            Stroke::new(2.0, palette.primary),
        );
    }

    if let Some(drag) = dragged.as_ref() {
        item_list::paint_drag_preview(ui, pointer_pos, &drag.preview, palette);
        if primary_down {
            ui.ctx().request_repaint();
        } else {
            if event.is_none()
                && let Some((target, _)) = drop
            {
                event = Some(SidebarEvent::Reorder {
                    source: drag.anchor.clone(),
                    before: target.map(str::to_owned),
                });
            }
            ui.ctx()
                .data_mut(|data| data.remove::<SidebarDragState>(drag_id));
        }
    }

    let footer_action = super::sidebar_footer::paint_sidebar_footer(
        ui,
        rect,
        footer_h,
        footer_items,
        model.separator_visible,
        palette,
        border_color,
    );
    event.or_else(|| footer_action.map(SidebarEvent::ExtensionAction))
}

fn session_context_action(
    response: &egui::Response,
    can_activate: bool,
    can_move_up: bool,
    can_move_down: bool,
    can_navigate: bool,
    can_return_to_last_session: bool,
) -> Option<SessionContextAction> {
    use SessionContextAction as A;
    use bootty_ui::menu::MenuEntry as E;
    bootty_ui::menu::context_menu(
        response,
        &[
            E::enabled_item(can_activate, "Activate Session", A::Activate),
            E::Separator,
            E::item("New Session…", A::NewSession),
            E::item("Switch Session…", A::SwitchSession),
            E::submenu(
                "Navigate Sessions",
                vec![
                    E::enabled_item(can_navigate, "Previous Session", A::PreviousSession),
                    E::enabled_item(can_navigate, "Next Session", A::NextSession),
                    E::enabled_item(can_return_to_last_session, "Last Session", A::LastSession),
                ],
            ),
            E::Separator,
            E::item("Rename Session…", A::Rename),
            E::enabled_item(can_move_up, "Move Session Up", A::MoveUp),
            E::enabled_item(can_move_down, "Move Session Down", A::MoveDown),
            E::Separator,
            E::item("Detach from Space", A::Detach),
            E::item("Ditch Session…", A::Ditch),
        ],
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidebarDragState {
    anchor: String,
    preview: String,
}

pub fn sidebar_drop_target<'a>(
    items: &'a [SidebarItem<'a>],
    pos: Option<Pos2>,
    left: f32,
    top: f32,
    width: f32,
    dragged_anchor: &str,
) -> Option<(Option<&'a str>, f32)> {
    let pos = pos?;
    // A sidebar drop needs the pointer over a row; off the rows is not a drop.
    let lane = item_list::hovered_row(pos, left, top, width, items.len()).map(|_| 0);
    let blocks = sidebar_blocks(items, top);
    let target = bootty_ui::reorder::drop_target(&blocks, dragged_anchor, pos.y, lane, true)?;
    Some((target.before, target.indicator))
}

/// One block per session: its title row plus the detail rows beneath it, in row order.
fn sidebar_blocks<'a>(
    items: &'a [SidebarItem<'a>],
    top: f32,
) -> Vec<bootty_ui::reorder::ReorderBlock<'a>> {
    bootty_ui::reorder::blocks_from(items.iter().enumerate().map(|(row, item)| {
        let start = top + row as f32 * ROW_HEIGHT;
        (item.reorder_anchor, 0, start, start + ROW_HEIGHT)
    }))
}

fn paint_sidebar_title(ui: &egui::Ui, rect: Rect, palette: ThemePalette, model: &SidebarModel<'_>) {
    let painter = ui.painter_at(rect);
    let (reserved, center_y) = if model.reserve_titlebar_buttons {
        (
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH,
            rect.min.y + MACOS_TITLEBAR_BUTTON_CENTER_Y,
        )
    } else {
        (0.0, rect.min.y + SIDEBAR_HEADER_HEIGHT * 0.5)
    };
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + ROW_PAD_X + reserved, center_y - 9.0),
        egui::vec2(18.0, 18.0),
    );
    if let Some(icon) = model.title_icon {
        painter.image(
            icon.id(),
            icon_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        painter.circle_filled(icon_rect.center(), 8.0, palette.primary);
    }
    painter.text(
        Pos2::new(icon_rect.max.x + 10.0, center_y),
        egui::Align2::LEFT_CENTER,
        "Bootty",
        egui::FontId::proportional(15.0),
        palette.text,
    );
    painter.text(
        Pos2::new(rect.max.x - ROW_PAD_X, center_y),
        egui::Align2::RIGHT_CENTER,
        model.session_count,
        egui::FontId::monospace(13.0),
        palette.muted,
    );
}

fn sidebar_item_row(
    ui: &mut egui::Ui,
    rect: Rect,
    item: &SidebarItem<'_>,
    hovered_session: bool,
    palette: ThemePalette,
    hover_color: egui::Color32,
    current_color: egui::Color32,
) -> egui::Response {
    // Any row carrying an anchor drags its whole block, so grabbing a detail row
    // (process/branch/status/progress) reorders just like grabbing the title row.
    let draggable = item.reorder_anchor.is_some();
    let clickable = item.extension_action.is_some() || item.selectable && item.session_id.is_some();
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-sidebar-item", item.scope, item.id, item.kind)),
        if draggable {
            egui::Sense::click_and_drag()
        } else if clickable {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if response.hovered() && clickable {
        ui.set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let bg = if hovered_session {
            hover_color
        } else if item.current {
            current_color
        } else {
            palette.base
        };
        painter.rect_filled(rect, 0.0, bg);

        if item.current {
            let bar = Rect::from_min_max(rect.min, Pos2::new(rect.min.x + 4.0, rect.max.y));
            painter.rect_filled(bar, 0.0, item.color);
        }

        item_list::paint_tree_guide(&painter, rect, &row_visual(item));

        match item.kind {
            "group" => item_list::paint_group_item(&painter, rect, &row_visual(item), bg),
            "session" => item_list::paint_session_item(
                &painter,
                rect,
                &row_visual(item),
                item.active,
                palette,
                bg,
            ),
            _ => item_list::paint_generic_sidebar_item(
                &painter,
                rect,
                &row_visual(item),
                palette,
                bg,
            ),
        }
    }
    response
}
