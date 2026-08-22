use bootty_extension::{ExtensionUiAction, ModuleItem, PublishedSurfaceItem};
use bootty_mux::controller::{MuxScope, SpaceId};
use bootty_ui::{ThemePalette, icons::paint_icon_slug, mix, readable_color};
use eframe::egui::{self, Pos2, Rect, Stroke, TextureHandle};

use crate::{
    assets,
    strings::truncate_label,
    theme::module_color32,
    ui::{session_navigation::ScopedSessionTarget, sidebar::SidebarItem},
    workspace_runtime::SpaceSummary,
};

use super::{item_primitives::paint_item_primitives, start_window_drag_on_primary_press};

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
const SIDEBAR_FOOTER_ITEM_HEIGHT: f32 = 30.0;
const SIDEBAR_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_PAD_X: f32 = 14.0;
pub(crate) const MACOS_TITLEBAR_BUTTON_SAFE_WIDTH: f32 = 72.0;
pub(crate) const SPACE_SWITCHER_HEIGHT: f32 = 44.0;
const SPACE_SWITCHER_BUTTON_SIZE: f32 = 28.0;
const SPACE_SWITCHER_BUTTON_GAP: f32 = 4.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceSwitcherEvent {
    Activate(SpaceId),
    Create,
    Edit(SpaceId),
    Reconnect(SpaceId),
    Close(SpaceId),
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SidebarSpaceSwipeState {
    phase: SidebarSpaceSwipePhase,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SidebarSpaceSwipePhase {
    #[default]
    Idle,
    Active(bool),
    AwaitingMomentum(bool),
    Momentum,
}

pub fn take_sidebar_space_swipe(
    ui: &mut egui::Ui,
    sidebar_rect: Rect,
    spaces: &[SpaceSummary],
    state: &mut SidebarSpaceSwipeState,
) -> Option<SpaceId> {
    let hovered = ui
        .input(|input| input.pointer.hover_pos())
        .is_some_and(|pos| sidebar_rect.contains(pos));
    ui.input_mut(|input| {
        let mut selected = None;
        input.events.retain(|event| {
            let egui::Event::MouseWheel { delta, phase, .. } = event else {
                return true;
            };
            if !hovered {
                return true;
            }
            let is_zero_delta = delta.x == 0.0 && delta.y == 0.0;
            if delta.x.abs() <= delta.y.abs()
                && !(is_zero_delta
                    && matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel))
            {
                return true;
            }
            let target = sidebar_space_swipe_target(spaces, delta.x, *phase, state);
            if selected.is_none() {
                selected = target;
            }
            false
        });
        selected
    })
}

fn sidebar_space_swipe_target(
    spaces: &[SpaceSummary],
    delta_x: f32,
    phase: egui::TouchPhase,
    state: &mut SidebarSpaceSwipeState,
) -> Option<SpaceId> {
    match phase {
        egui::TouchPhase::Cancel => {
            state.phase = SidebarSpaceSwipePhase::Idle;
            return None;
        }
        egui::TouchPhase::End => {
            state.phase = match state.phase {
                SidebarSpaceSwipePhase::Active(direction) => {
                    SidebarSpaceSwipePhase::AwaitingMomentum(direction)
                }
                SidebarSpaceSwipePhase::Momentum => SidebarSpaceSwipePhase::Idle,
                phase => phase,
            };
            return None;
        }
        egui::TouchPhase::Start | egui::TouchPhase::Move => {}
    }

    let positive = (delta_x != 0.0).then_some(delta_x.is_sign_positive())?;
    match (phase, state.phase) {
        (egui::TouchPhase::Start | egui::TouchPhase::Move, SidebarSpaceSwipePhase::Idle) => {
            state.phase = SidebarSpaceSwipePhase::Active(positive);
        }
        (egui::TouchPhase::Start, SidebarSpaceSwipePhase::AwaitingMomentum(previous))
            if positive == previous =>
        {
            state.phase = SidebarSpaceSwipePhase::Momentum;
            return None;
        }
        (egui::TouchPhase::Start, SidebarSpaceSwipePhase::AwaitingMomentum(_)) => {
            state.phase = SidebarSpaceSwipePhase::Active(positive);
        }
        _ => return None,
    }

    let active = spaces.iter().position(|space| space.active)?;
    let target = if positive {
        active.checked_sub(1)
    } else {
        active.checked_add(1).filter(|index| *index < spaces.len())
    }?;
    Some(spaces[target].id)
}

pub fn show_space_switcher(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    spaces: &[SpaceSummary],
    transition: Option<(SpaceId, SpaceId, f32)>,
) -> Option<SpaceSwitcherEvent> {
    let width = ui.available_width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, SPACE_SWITCHER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let hover_color = mix(palette.base, palette.text, 0.045);
    let item_count = spaces.len() + 1;
    let group_width = item_count as f32 * SPACE_SWITCHER_BUTTON_SIZE
        + item_count.saturating_sub(1) as f32 * SPACE_SWITCHER_BUTTON_GAP;
    let start_x = rect.center().x - group_width * 0.5;
    let item_center_x = |index: usize| {
        start_x
            + index as f32 * (SPACE_SWITCHER_BUTTON_SIZE + SPACE_SWITCHER_BUTTON_GAP)
            + SPACE_SWITCHER_BUTTON_SIZE * 0.5
    };
    let active = spaces.iter().position(|space| space.active);
    let indicator_x = transition
        .and_then(|(from, to, progress)| {
            let from = spaces.iter().position(|space| space.id == from)?;
            let to = spaces.iter().position(|space| space.id == to)?;
            Some(egui::lerp(
                item_center_x(from)..=item_center_x(to),
                progress,
            ))
        })
        .or_else(|| active.map(&item_center_x));
    if let Some(x) = indicator_x {
        painter.circle_filled(Pos2::new(x, rect.max.y - 4.0), 2.0, palette.primary);
    }
    let button_rect = |index| {
        Rect::from_center_size(
            Pos2::new(item_center_x(index), rect.center().y),
            egui::vec2(SPACE_SWITCHER_BUTTON_SIZE, SPACE_SWITCHER_BUTTON_SIZE),
        )
    };
    let mut event = None;
    for (index, space) in spaces.iter().enumerate() {
        let item_rect = button_rect(index);
        let response = ui
            .interact(
                item_rect,
                ui.id()
                    .with(("space-switcher", space.id.persistence_value())),
                egui::Sense::click(),
            )
            .on_hover_ui(|ui| {
                ui.label(&space.name);
                if let Some(error) = &space.error {
                    ui.separator();
                    ui.label(error);
                }
            });
        if response.hovered() && !space.active {
            painter.rect_filled(item_rect, 6.0, hover_color);
        }
        paint_icon_slug(
            &painter,
            &space.icon,
            item_rect.center(),
            16.0,
            space.error.as_ref().map_or_else(
                || egui::Color32::from_rgb(space.color[0], space.color[1], space.color[2]),
                |_| palette.muted,
            ),
        );
        if event.is_none() && !space.active && response.clicked_by(egui::PointerButton::Primary) {
            event = Some(SpaceSwitcherEvent::Activate(space.id));
        }
        response.context_menu(|ui| {
            if space.error.is_some() && ui.button("Reconnect").clicked() {
                event = Some(SpaceSwitcherEvent::Reconnect(space.id));
                ui.close();
            }
            if ui.button("Edit Space").clicked() {
                event = Some(SpaceSwitcherEvent::Edit(space.id));
                ui.close();
            }
            if ui
                .add_enabled(spaces.len() > 1, egui::Button::new("Close"))
                .clicked()
            {
                event = Some(SpaceSwitcherEvent::Close(space.id));
                ui.close();
            }
        });
    }
    let plus_rect = button_rect(spaces.len());
    let response = ui
        .interact(
            plus_rect,
            ui.id().with("space-switcher-create"),
            egui::Sense::click(),
        )
        .on_hover_text("New Space");
    if response.hovered() {
        painter.rect_filled(plus_rect, 6.0, hover_color);
    }
    paint_icon_slug(&painter, "plus", plus_rect.center(), 16.0, palette.subtext);
    if event.is_none() && response.clicked_by(egui::PointerButton::Primary) {
        event = Some(SpaceSwitcherEvent::Create);
    }
    event
}

const MACOS_TITLEBAR_BUTTON_CENTER_Y: f32 = 16.0;
/// Fraction of a color kept when dimming an unfocused session row; the rest blends to the row
/// background, so each element fades in its own hue rather than washing toward white.
const UNFOCUSED_ROW_KEEP: f32 = 0.5;

/// Every row a session owns — its title row plus the detail/progress rows beneath it — points at
/// that session, so hovering or clicking anywhere in the block hits the whole session component.
fn sidebar_session_key<'a>(item: &'a SidebarItem<'a>) -> Option<(MuxScope, &'a str)> {
    Some((item.scope, item.session_id?))
}

pub fn show_sidebar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    model: SidebarModel<'_>,
) -> Option<SidebarEvent> {
    // `palette` arrives with `base`/`foreground` already overridden. Windowed hover derives from
    // the sidebar background; fullscreen uses a stronger lift so a black notch background still
    // has a visible, non-muddy hover state. Explicit hover override wins outright.
    let hover_color = model.hover_override.unwrap_or_else(|| {
        if model.fullscreen {
            mix(palette.base, palette.text, 0.13)
        } else {
            mix(palette.base, palette.text, 0.045)
        }
    });
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
    let max_rows = ((list_bottom - model.top_inset - header_h) / SIDEBAR_ROW_HEIGHT)
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
        .and_then(|pos| sidebar_hovered_row(pos, rect.min.x, list_top, width, max_rows))
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
            Pos2::new(rect.min.x, list_top + index as f32 * SIDEBAR_ROW_HEIGHT),
            egui::vec2(width, SIDEBAR_ROW_HEIGHT),
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
        paint_sidebar_drag_preview(ui, pointer_pos, &drag.preview, palette);
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

    let footer_action = paint_sidebar_footer(
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
    let mut action = None;
    response.context_menu(|ui| {
        pick(
            ui,
            &mut action,
            can_activate,
            "Activate Session",
            A::Activate,
        );
        ui.separator();
        pick(ui, &mut action, true, "New Session…", A::NewSession);
        pick(ui, &mut action, true, "Switch Session…", A::SwitchSession);
        if action.is_none() {
            ui.menu_button("Navigate Sessions", |ui| {
                pick(
                    ui,
                    &mut action,
                    can_navigate,
                    "Previous Session",
                    A::PreviousSession,
                );
                pick(
                    ui,
                    &mut action,
                    can_navigate,
                    "Next Session",
                    A::NextSession,
                );
                pick(
                    ui,
                    &mut action,
                    can_return_to_last_session,
                    "Last Session",
                    A::LastSession,
                );
            });
        }
        ui.separator();
        pick(ui, &mut action, true, "Rename Session…", A::Rename);
        pick(ui, &mut action, can_move_up, "Move Session Up", A::MoveUp);
        pick(
            ui,
            &mut action,
            can_move_down,
            "Move Session Down",
            A::MoveDown,
        );
        ui.separator();
        pick(ui, &mut action, true, "Detach from Space", A::Detach);
        pick(ui, &mut action, true, "Ditch Session…", A::Ditch);
        if action.is_some() {
            ui.close();
        }
    });
    action
}

fn pick(
    ui: &mut egui::Ui,
    action: &mut Option<SessionContextAction>,
    enabled: bool,
    label: &str,
    value: SessionContextAction,
) {
    if action.is_none() && ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        *action = Some(value);
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SidebarBlock<'a> {
    anchor: &'a str,
    start_row: usize,
    end_row: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SidebarDragState {
    anchor: String,
    preview: String,
}

fn paint_sidebar_drag_preview(
    ui: &egui::Ui,
    pointer_pos: Option<Pos2>,
    preview: &str,
    palette: ThemePalette,
) {
    let Some(pointer_pos) = pointer_pos else {
        return;
    };
    let preview = truncate_label(preview, 24);
    let font = egui::FontId::monospace(13.0);
    let width = preview.chars().count() as f32 * 7.4 + 18.0;
    let rect = Rect::from_min_size(
        pointer_pos + egui::vec2(14.0, 14.0),
        egui::vec2(width.max(48.0), SIDEBAR_ROW_HEIGHT - 2.0),
    );
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("mux-sidebar-drag-preview"),
    ));
    painter.rect_filled(rect, 6.0, mix(palette.base, palette.text, 0.12));
    painter.rect_stroke(
        rect,
        6.0,
        Stroke::new(1.0, palette.primary),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + egui::vec2(9.0, 0.0),
        egui::Align2::LEFT_CENTER,
        preview,
        font,
        palette.text,
    );
}

fn sidebar_drop_target<'a>(
    items: &'a [SidebarItem<'a>],
    pos: Option<Pos2>,
    left: f32,
    top: f32,
    width: f32,
    dragged_anchor: &str,
) -> Option<(Option<&'a str>, f32)> {
    let pos = pos?;
    let row = sidebar_hovered_row(pos, left, top, width, items.len())?;
    let blocks = sidebar_blocks(items);
    let source_index = blocks
        .iter()
        .position(|block| block.anchor == dragged_anchor)?;
    let block_index = blocks
        .iter()
        .position(|block| block.start_row <= row && row <= block.end_row)?;
    let block = blocks[block_index];
    let block_top = top + block.start_row as f32 * SIDEBAR_ROW_HEIGHT;
    let block_bottom = top + (block.end_row + 1) as f32 * SIDEBAR_ROW_HEIGHT;
    let midpoint = (block_top + block_bottom) * 0.5;

    let (target, target_index, indicator_y) = if pos.y < midpoint {
        (Some(block.anchor), Some(block_index), block_top)
    } else if let Some(next_block) = blocks.get(block_index + 1) {
        (
            Some(next_block.anchor),
            Some(block_index + 1),
            top + next_block.start_row as f32 * SIDEBAR_ROW_HEIGHT,
        )
    } else {
        (None, None, block_bottom)
    };

    let noop = match target_index {
        Some(target) if source_index < target => source_index + 1 == target,
        Some(target) => source_index == target,
        None => source_index + 1 == blocks.len(),
    };
    if noop {
        return None;
    }

    Some((target, indicator_y))
}

fn sidebar_blocks<'a>(items: &'a [SidebarItem<'a>]) -> Vec<SidebarBlock<'a>> {
    let mut blocks = Vec::<SidebarBlock<'a>>::with_capacity(items.len());
    for (row, item) in items.iter().enumerate() {
        let Some(anchor) = item.reorder_anchor else {
            continue;
        };
        if let Some(block) = blocks.last_mut()
            && block.anchor == anchor
        {
            block.end_row = row;
            continue;
        }
        blocks.push(SidebarBlock {
            anchor,
            start_row: row,
            end_row: row,
        });
    }
    blocks
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
    let (reserved, center_y) = if model.reserve_titlebar_buttons {
        (
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH,
            rect.min.y + MACOS_TITLEBAR_BUTTON_CENTER_Y,
        )
    } else {
        (0.0, rect.min.y + SIDEBAR_HEADER_HEIGHT * 0.5)
    };
    let icon_rect = Rect::from_min_size(
        Pos2::new(rect.min.x + SIDEBAR_PAD_X + reserved, center_y - 9.0),
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
        Pos2::new(rect.max.x - SIDEBAR_PAD_X, center_y),
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

        paint_tree_guide(&painter, rect, item);

        match item.kind {
            "group" => paint_group_item(&painter, rect, item, bg),
            "session" => paint_session_item(&painter, rect, item, item.active, palette, bg),
            _ => paint_generic_sidebar_item(&painter, rect, item, palette, bg),
        }
    }
    response
}
const SIDEBAR_INDENT_PX: f32 = 7.0;

fn item_text_x(rect: Rect, item: &SidebarItem<'_>) -> f32 {
    rect.min.x + 12.0 + f32::from(item.indent) * SIDEBAR_INDENT_PX
}

fn paint_tree_guide(painter: &egui::Painter, rect: Rect, item: &SidebarItem<'_>) {
    let x = rect.min.x + 15.5;
    let cy = rect.center().y;
    let stroke = Stroke::new(1.0, item.dim_color.gamma_multiply(0.8));
    match item.tree {
        Some(tree @ ("middle" | "last")) => {
            let bottom = if tree == "middle" { rect.max.y } else { cy };
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, bottom)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        Some("pipe") => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
        }
        _ => {}
    }
}

fn paint_group_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    background: egui::Color32,
) {
    let title_color = if item.current {
        item.color
    } else {
        item.dim_color
    };
    painter.text(
        Pos2::new(item_text_x(rect, item), rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_label(item.text, 28),
        egui::FontId::monospace(12.0),
        title_color,
    );
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        item.color,
        background,
        true,
        1.0,
    );
}

fn paint_session_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    active: bool,
    palette: ThemePalette,
    background: egui::Color32,
) {
    // Render the session name in its own session color verbatim — vivid when active, dim when not —
    // rather than through readable_color, whose AAA contrast gate flattens both tints to flat white.
    let label_color = if active { item.color } else { item.dim_color };
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let number = item.number;
    let mut text_x = x;
    if let Some(number) = number {
        let badge = Rect::from_center_size(Pos2::new(x + 7.0, cy), egui::vec2(14.0, 14.0));
        if active {
            painter.rect_filled(badge, 3.0, item.color);
        } else {
            painter.rect_stroke(
                badge,
                3.0,
                Stroke::new(1.0, item.dim_color),
                egui::StrokeKind::Inside,
            );
        }
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            number % 100,
            egui::FontId::monospace(10.0),
            if active {
                readable_color(item.color, palette.base)
            } else {
                item.dim_color
            },
        );
        text_x = badge.max.x + 6.0;
    }
    painter.text(
        Pos2::new(text_x, cy),
        egui::Align2::LEFT_CENTER,
        truncate_label(item.text, 20),
        egui::FontId::monospace(13.0),
        label_color,
    );
    let keep = if active { 1.0 } else { UNFOCUSED_ROW_KEEP };
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        item.dim_color,
        background,
        true,
        keep,
    );
}

fn paint_generic_sidebar_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    palette: ThemePalette,
    background: egui::Color32,
) {
    let keep = if item.current {
        1.0
    } else {
        UNFOCUSED_ROW_KEEP
    };
    paint_item_primitives(
        painter,
        rect,
        item.primitives,
        item.dim_color,
        background,
        true,
        keep,
    );
    if !item.primitives.is_empty() {
        return;
    }
    let x = item_text_x(rect, item);
    let cy = rect.center().y;
    let mut text_x = x;
    if let Some(icon) = item.icon
        && paint_icon_slug(
            painter,
            icon,
            Pos2::new(x + 6.0, cy),
            12.0,
            readable_color(background, item.color),
        )
    {
        text_x += 16.0;
    }
    if !item.text.is_empty() {
        painter.text(
            Pos2::new(text_x, cy),
            egui::Align2::LEFT_CENTER,
            truncate_label(item.text, 28),
            egui::FontId::monospace(11.0),
            readable_color(background, palette.muted),
        );
    }
}

fn paint_sidebar_footer(
    ui: &mut egui::Ui,
    rect: Rect,
    footer_h: f32,
    footer_items: &[PublishedSurfaceItem],
    separator_visible: bool,
    palette: ThemePalette,
    border_color: egui::Color32,
) -> Option<ExtensionUiAction> {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    if separator_visible {
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, border_color),
        );
    }

    let mut action = None;
    for (index, published) in footer_items.iter().enumerate() {
        let item = &published.item;
        let row_y = y + 18.0 + index as f32 * SIDEBAR_FOOTER_ITEM_HEIGHT;
        let item_rect = Rect::from_min_size(
            Pos2::new(rect.min.x + 14.0, row_y - 10.0),
            egui::vec2(rect.width() - 28.0, 26.0),
        );
        let response = ui.interact(
            item_rect,
            ui.make_persistent_id((
                "extension-sidebar-footer",
                published.module.as_str(),
                published.generation,
                published.surface.as_str(),
                index,
            )),
            if item.action.is_some() {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        if response.hovered() && item.action.is_some() {
            ui.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if action.is_none() && response.clicked_by(egui::PointerButton::Primary) {
            action = published.action();
        }
        let color = readable_color(
            palette.base,
            item.fg.map(module_color32).unwrap_or(palette.subtext),
        );
        paint_item_primitives(
            &painter,
            item_rect,
            &item.primitives,
            color,
            palette.base,
            false,
            1.0,
        );
        if item.primitives.is_empty() {
            paint_footer_fallback(&painter, item_rect, item, color, palette);
        }
    }
    action
}

fn paint_footer_fallback(
    painter: &egui::Painter,
    rect: Rect,
    item: &ModuleItem,
    color: egui::Color32,
    palette: ThemePalette,
) {
    let mut text_x = rect.min.x;
    if let Some(icon) = item.icon.as_deref()
        && paint_icon_slug(
            painter,
            icon,
            Pos2::new(rect.min.x + 6.0, rect.min.y + 6.0),
            12.0,
            readable_color(palette.base, color),
        )
    {
        text_x += 16.0;
    }
    if !item.text.is_empty() {
        painter.text(
            Pos2::new(text_x, rect.min.y + 6.0),
            egui::Align2::LEFT_CENTER,
            truncate_label(&item.text, 28),
            egui::FontId::monospace(11.0),
            readable_color(palette.base, palette.subtext),
        );
    }
}
