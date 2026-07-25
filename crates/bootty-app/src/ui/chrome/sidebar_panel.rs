use std::collections::HashMap;

use bootty_ui::{ThemePalette, readable_color};
use eframe::egui::{self, Pos2, Rect, Stroke, TextureHandle};

use crate::{
    assets,
    config::ChromeConfig,
    extensions::ModuleItem,
    mux::{controller::SpaceId, snapshot::MuxSession},
    strings::truncate_label,
    ui::{
        icons::paint_icon_slug,
        session_navigation::ScopedSessionTarget,
        sidebar::{SidebarDisplay, SidebarItem, SidebarItemKind, SidebarTree},
    },
};

use super::{item_primitives::paint_item_primitives, start_window_drag_on_primary_press};

#[derive(Clone)]
pub struct SidebarModel<'a> {
    pub items: &'a [SidebarItem<'a>],
    pub footer_items: &'a [ModuleItem],
    pub session_count: usize,
    pub has_sessions: bool,
    pub title_visible: bool,
    pub reserve_titlebar_buttons: bool,
    pub title_icon: Option<&'a TextureHandle>,
    pub top_inset: f32,
    pub border_visible: bool,
    pub border_bottom: bool,
    pub separator_visible: bool,
    pub focused: bool,
    pub hovered_session: Option<&'a ScopedSessionTarget>,
    /// Retained for callers that construct the model; `BoottyApp` dims after the Space strip.
    pub unfocused_dim: f32,
    /// Explicit color overrides from `[sidebar]`; each falls back to a theme-derived tint.
    pub fullscreen: bool,
    pub hover_override: Option<egui::Color32>,
    pub current_override: Option<egui::Color32>,
    pub border_override: Option<egui::Color32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarEvent {
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
const SIDEBAR_FOOTER_BASE_HEIGHT: f32 = 44.0;
const SIDEBAR_MAX_FOOTER_ITEMS: usize = 4;
const SIDEBAR_FOOTER_ITEM_HEIGHT: f32 = 30.0;
const SIDEBAR_ROW_HEIGHT: f32 = 24.0;
const SIDEBAR_PAD_X: f32 = 14.0;
pub(crate) const MACOS_TITLEBAR_BUTTON_SAFE_WIDTH: f32 = 72.0;
pub(crate) const SPACE_SWITCHER_HEIGHT: f32 = 44.0;
const SPACE_SWITCHER_BUTTON_SIZE: f32 = 28.0;
const SPACE_SWITCHER_BUTTON_GAP: f32 = 4.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceSwitcherItem {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceSwitcherEvent {
    Activate(SpaceId),
    Create,
    Edit(SpaceId),
    Close(SpaceId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarSpaceSwipeDirection {
    Negative,
    Positive,
}

impl SidebarSpaceSwipeDirection {
    fn from_delta(delta_x: f32) -> Option<Self> {
        (delta_x != 0.0).then(|| {
            if delta_x.is_sign_positive() {
                Self::Positive
            } else {
                Self::Negative
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SidebarSpaceSwipePhase {
    #[default]
    Idle,
    Active {
        direction: SidebarSpaceSwipeDirection,
    },
    AwaitingMomentum {
        direction: SidebarSpaceSwipeDirection,
    },
    Momentum,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SidebarSpaceSwipeState {
    phase: SidebarSpaceSwipePhase,
}

pub fn take_sidebar_space_swipe(
    ui: &mut egui::Ui,
    sidebar_rect: Rect,
    spaces: &[SpaceSwitcherItem],
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
    spaces: &[SpaceSwitcherItem],
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
                SidebarSpaceSwipePhase::Active { direction } => {
                    SidebarSpaceSwipePhase::AwaitingMomentum { direction }
                }
                SidebarSpaceSwipePhase::Momentum => SidebarSpaceSwipePhase::Idle,
                phase => phase,
            };
            return None;
        }
        egui::TouchPhase::Start | egui::TouchPhase::Move => {}
    }

    let direction = SidebarSpaceSwipeDirection::from_delta(delta_x)?;
    match (phase, state.phase) {
        (egui::TouchPhase::Start | egui::TouchPhase::Move, SidebarSpaceSwipePhase::Idle) => {
            state.phase = SidebarSpaceSwipePhase::Active { direction };
        }
        (
            egui::TouchPhase::Start,
            SidebarSpaceSwipePhase::AwaitingMomentum {
                direction: previous_direction,
            },
        ) if direction == previous_direction => {
            state.phase = SidebarSpaceSwipePhase::Momentum;
            return None;
        }
        (egui::TouchPhase::Start, SidebarSpaceSwipePhase::AwaitingMomentum { .. }) => {
            state.phase = SidebarSpaceSwipePhase::Active { direction };
        }
        _ => return None,
    }

    let active = spaces.iter().position(|space| space.active)?;
    let target = match direction {
        SidebarSpaceSwipeDirection::Positive => active.checked_sub(1),
        SidebarSpaceSwipeDirection::Negative => {
            active.checked_add(1).filter(|index| *index < spaces.len())
        }
    }?;
    Some(spaces[target].id)
}

pub fn show_space_switcher(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    spaces: &[SpaceSwitcherItem],
    transition: Option<(SpaceId, SpaceId, f32)>,
) -> Option<SpaceSwitcherEvent> {
    let width = ui.available_width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, SPACE_SWITCHER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    let item_count = spaces.len() + 1;
    let group_width = item_count as f32 * SPACE_SWITCHER_BUTTON_SIZE
        + item_count.saturating_sub(1) as f32 * SPACE_SWITCHER_BUTTON_GAP;
    let start_x = rect.center().x - group_width * 0.5;
    let item_center_x = |index: usize| {
        start_x
            + index as f32 * (SPACE_SWITCHER_BUTTON_SIZE + SPACE_SWITCHER_BUTTON_GAP)
            + SPACE_SWITCHER_BUTTON_SIZE * 0.5
    };
    if let Some(dot) = space_indicator_dot(rect, spaces, transition, &item_center_x) {
        painter.circle_filled(dot, 2.0, palette.primary);
    }
    let mut event = None;
    for (index, space) in spaces.iter().enumerate() {
        let item_rect = space_switcher_button_rect(rect, item_center_x(index));
        let response = ui
            .interact(
                item_rect,
                ui.id()
                    .with(("space-switcher", space.id.persistence_value())),
                egui::Sense::click(),
            )
            .on_hover_text(space.name.as_str());
        if response.hovered() && !space.active {
            painter.rect_filled(item_rect, 6.0, sidebar_hover_color(palette));
        }
        paint_icon_slug(
            &painter,
            &space.icon,
            item_rect.center(),
            16.0,
            egui::Color32::from_rgb(space.color[0], space.color[1], space.color[2]),
        );
        if event.is_none() && !space.active && response.clicked_by(egui::PointerButton::Primary) {
            event = Some(SpaceSwitcherEvent::Activate(space.id));
        }
        response.context_menu(|ui| {
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
    let plus_rect = space_switcher_button_rect(rect, item_center_x(spaces.len()));
    let response = ui
        .interact(
            plus_rect,
            ui.id().with("space-switcher-create"),
            egui::Sense::click(),
        )
        .on_hover_text("New Space");
    if response.hovered() {
        painter.rect_filled(plus_rect, 6.0, sidebar_hover_color(palette));
    }
    paint_icon_slug(&painter, "plus", plus_rect.center(), 16.0, palette.subtext);
    if event.is_none() && response.clicked_by(egui::PointerButton::Primary) {
        event = Some(SpaceSwitcherEvent::Create);
    }
    event
}

fn space_switcher_button_rect(strip: Rect, center_x: f32) -> Rect {
    Rect::from_center_size(
        Pos2::new(center_x, strip.center().y),
        egui::vec2(SPACE_SWITCHER_BUTTON_SIZE, SPACE_SWITCHER_BUTTON_SIZE),
    )
}

fn space_indicator_center(
    spaces: &[SpaceSwitcherItem],
    transition: Option<(SpaceId, SpaceId, f32)>,
    center_x: &impl Fn(usize) -> f32,
) -> Option<f32> {
    let active = spaces.iter().position(|space| space.active)?;
    let Some((from, to, progress)) = transition else {
        return Some(center_x(active));
    };
    let from = spaces.iter().position(|space| space.id == from);
    let to = spaces.iter().position(|space| space.id == to);
    match (from, to) {
        (Some(from), Some(to)) => Some(egui::lerp(center_x(from)..=center_x(to), progress)),
        _ => Some(center_x(active)),
    }
}

fn space_indicator_dot(
    rect: Rect,
    spaces: &[SpaceSwitcherItem],
    transition: Option<(SpaceId, SpaceId, f32)>,
    center_x: &impl Fn(usize) -> f32,
) -> Option<Pos2> {
    Some(Pos2::new(
        space_indicator_center(spaces, transition, center_x)?,
        rect.max.y - 4.0,
    ))
}
const MACOS_TITLEBAR_BUTTON_CENTER_Y: f32 = 16.0;
/// Fraction of a color kept when dimming an unfocused session row; the rest blends to the row
/// background, so each element fades in its own hue rather than washing toward white.
const UNFOCUSED_ROW_KEEP: f32 = 0.5;

fn sidebar_session_target(item: &SidebarItem<'_>) -> Option<ScopedSessionTarget> {
    if !matches!(&item.kind, SidebarItemKind::Session { .. }) {
        return None;
    }
    Some(ScopedSessionTarget::new(
        item.session_scope?,
        item.session_id?,
    ))
}

fn sidebar_context_session_target(item: &SidebarItem<'_>) -> Option<ScopedSessionTarget> {
    item.selectable
        .then(|| sidebar_session_target(item))
        .flatten()
}

fn sidebar_session_targets(items: &[SidebarItem<'_>]) -> Vec<ScopedSessionTarget> {
    let mut targets = Vec::new();
    for item in items {
        let Some(target) = sidebar_session_target(item) else {
            continue;
        };
        if !targets.contains(&target) {
            targets.push(target);
        }
    }
    targets
}

fn sidebar_binding_target_position(
    targets: &[ScopedSessionTarget],
    target: &ScopedSessionTarget,
) -> Option<(usize, usize)> {
    let binding_targets = targets
        .iter()
        .filter(|candidate| candidate.scope == target.scope)
        .collect::<Vec<_>>();
    let position = binding_targets
        .iter()
        .position(|candidate| *candidate == target)?;
    Some((position, binding_targets.len()))
}

fn sidebar_title_drag_rect(rect: Rect, reserve_titlebar_buttons: bool) -> Rect {
    let reserved = if reserve_titlebar_buttons {
        MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
    } else {
        0.0
    };
    Rect::from_min_max(
        Pos2::new((rect.min.x + reserved).min(rect.max.x), rect.min.y),
        rect.max,
    )
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
            sidebar_fullscreen_hover_color(palette)
        } else {
            sidebar_hover_color(palette)
        }
    });
    let current_color = model
        .current_override
        .unwrap_or_else(|| sidebar_current_color(palette));
    let border_color = model
        .border_override
        .unwrap_or_else(|| subtle_border(palette));
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

    let header_h = sidebar_header_height(model.title_visible);
    let content_top = rect.min.y + model.top_inset;
    let title_rect = Rect::from_min_max(
        Pos2::new(rect.min.x, content_top),
        Pos2::new(rect.max.x, (content_top + header_h).min(rect.max.y)),
    );
    if model.title_visible {
        paint_sidebar_title(ui, title_rect, palette, &model);
        let drag_rect = sidebar_title_drag_rect(title_rect, model.reserve_titlebar_buttons);
        let response = ui.interact(
            drag_rect,
            ui.id().with("sidebar-titlebar-drag"),
            egui::Sense::click_and_drag(),
        );
        start_window_drag_on_primary_press(&response);
    }

    let list_top = content_top + header_h;

    let footer_items = sidebar_footer_items(model.footer_items);
    let footer_h = sidebar_footer_height(footer_items.len());
    if !model.has_sessions {
        painter.text(
            Pos2::new(rect.center().x, list_top + 42.0),
            egui::Align2::CENTER_CENTER,
            "no sessions",
            egui::FontId::monospace(13.0),
            palette.muted,
        );
    }

    let max_rows = visible_sidebar_row_capacity(height, model.top_inset, header_h, footer_h);
    let items = model
        .items
        .iter()
        .take(max_rows)
        .cloned()
        .collect::<Vec<_>>();
    let session_targets = sidebar_session_targets(model.items);
    let preview_labels = sidebar_drag_preview_labels(&items);
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
        .and_then(sidebar_session_target);
    let suppress_click = dragged.is_some();

    let mut event = None;
    for (index, item) in items.iter().enumerate() {
        let row_rect = Rect::from_min_size(
            Pos2::new(rect.min.x, list_top + index as f32 * SIDEBAR_ROW_HEIGHT),
            egui::vec2(width, SIDEBAR_ROW_HEIGHT),
        );
        let item_target = sidebar_session_target(item);
        let hovered = item.selectable
            && item_target.as_ref().is_some_and(|target| {
                Some(target) == pointer_hovered_session.as_ref()
                    || model.focused && Some(target) == model.hovered_session
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
                preview: preview_labels
                    .get(anchor)
                    .cloned()
                    .unwrap_or_else(|| anchor.to_owned()),
            };
            ui.ctx()
                .data_mut(|data| data.insert_persisted(drag_id, state.clone()));
            dragged = Some(state);
            ui.ctx().request_repaint();
        }

        if event.is_none()
            && !suppress_click
            && response.clicked_by(egui::PointerButton::Primary)
            && item.selectable
            && let Some(target) = item_target.clone()
        {
            event = Some(SidebarEvent::ActivateSession(target));
        }
        if event.is_none()
            && let Some(target) = sidebar_context_session_target(item)
            && let Some((position, binding_session_count)) =
                sidebar_binding_target_position(&session_targets, &target)
            && let Some(action) = session_context_action(
                &response,
                !item.current,
                item.reorder_anchor.is_some() && position > 0,
                item.reorder_anchor.is_some() && position + 1 < binding_session_count,
                binding_session_count > 1,
                item.can_return_to_last_session,
            )
        {
            event = Some(SidebarEvent::ContextAction { target, action });
        }
    }

    let drop = dragged.as_ref().and_then(|drag| {
        sidebar_drop_target(
            &items,
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
            if event.is_none() {
                event = sidebar_reorder_event(dragged.as_ref(), drop);
            }
            ui.ctx()
                .data_mut(|data| data.remove::<SidebarDragState>(drag_id));
        }
    }

    paint_sidebar_footer(
        ui,
        rect,
        footer_h,
        footer_items,
        model.separator_visible,
        palette,
        border_color,
    );
    event
}

fn session_context_action(
    response: &egui::Response,
    can_activate: bool,
    can_move_up: bool,
    can_move_down: bool,
    can_navigate: bool,
    can_return_to_last_session: bool,
) -> Option<SessionContextAction> {
    let mut action = None;
    response.context_menu(|ui| {
        if ui
            .add_enabled(can_activate, egui::Button::new("Activate Session"))
            .clicked()
        {
            action = Some(SessionContextAction::Activate);
        }
        ui.separator();
        if action.is_none() && ui.button("New Session…").clicked() {
            action = Some(SessionContextAction::NewSession);
        }
        if action.is_none() && ui.button("Switch Session…").clicked() {
            action = Some(SessionContextAction::SwitchSession);
        }
        if action.is_none() {
            ui.menu_button("Navigate Sessions", |ui| {
                if ui
                    .add_enabled(can_navigate, egui::Button::new("Previous Session"))
                    .clicked()
                {
                    action = Some(SessionContextAction::PreviousSession);
                }
                if action.is_none()
                    && ui
                        .add_enabled(can_navigate, egui::Button::new("Next Session"))
                        .clicked()
                {
                    action = Some(SessionContextAction::NextSession);
                }
                if action.is_none()
                    && ui
                        .add_enabled(
                            can_return_to_last_session,
                            egui::Button::new("Last Session"),
                        )
                        .clicked()
                {
                    action = Some(SessionContextAction::LastSession);
                }
            });
        }
        ui.separator();
        if action.is_none() && ui.button("Rename Session…").clicked() {
            action = Some(SessionContextAction::Rename);
        }
        if action.is_none()
            && ui
                .add_enabled(can_move_up, egui::Button::new("Move Session Up"))
                .clicked()
        {
            action = Some(SessionContextAction::MoveUp);
        }
        if action.is_none()
            && ui
                .add_enabled(can_move_down, egui::Button::new("Move Session Down"))
                .clicked()
        {
            action = Some(SessionContextAction::MoveDown);
        }
        ui.separator();
        if action.is_none() && ui.button("Detach from Space").clicked() {
            action = Some(SessionContextAction::Detach);
        }
        if action.is_none() && ui.button("Ditch Session…").clicked() {
            action = Some(SessionContextAction::Ditch);
        }
        if action.is_some() {
            ui.close();
        }
    });
    action
}

#[cfg(test)]
fn sidebar_session_row_capacity(
    height: f32,
    top_inset: f32,
    title_visible: bool,
    footer_item_count: usize,
) -> usize {
    let header_h = sidebar_header_height(title_visible);
    let footer_h = sidebar_footer_height(footer_item_count);
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

fn sidebar_hover_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.045)
}

fn sidebar_fullscreen_hover_color(palette: ThemePalette) -> egui::Color32 {
    mix_color(palette.base, palette.text, 0.13)
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

fn sidebar_drag_preview_labels<'a>(items: &'a [SidebarItem<'a>]) -> HashMap<&'a str, String> {
    let mut labels = HashMap::new();
    for item in items {
        let Some(anchor) = item.reorder_anchor else {
            continue;
        };
        labels
            .entry(anchor)
            .or_insert_with(|| sidebar_drag_label(item));
    }
    labels
}

fn sidebar_drag_label(item: &SidebarItem<'_>) -> String {
    match item.display {
        SidebarDisplay::Text(text) => text.to_owned(),
        SidebarDisplay::Numbered { label, .. } => label.to_owned(),
    }
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
    painter.rect_filled(rect, 6.0, mix_color(palette.base, palette.text, 0.12));
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarDropTarget<'a> {
    Before(&'a str),
    End,
}

fn sidebar_drop_target<'a>(
    items: &'a [SidebarItem<'a>],
    pos: Option<Pos2>,
    left: f32,
    top: f32,
    width: f32,
    dragged_anchor: &str,
) -> Option<(SidebarDropTarget<'a>, f32)> {
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
        (
            SidebarDropTarget::Before(block.anchor),
            Some(block_index),
            block_top,
        )
    } else if let Some(next_block) = blocks.get(block_index + 1) {
        (
            SidebarDropTarget::Before(next_block.anchor),
            Some(block_index + 1),
            top + next_block.start_row as f32 * SIDEBAR_ROW_HEIGHT,
        )
    } else {
        (SidebarDropTarget::End, None, block_bottom)
    };

    if sidebar_drop_is_noop(source_index, target_index, blocks.len()) {
        return None;
    }

    Some((target, indicator_y))
}

fn sidebar_reorder_event(
    dragged: Option<&SidebarDragState>,
    drop: Option<(SidebarDropTarget<'_>, f32)>,
) -> Option<SidebarEvent> {
    let (drag, (drop_target, _)) = (dragged?, drop?);
    Some(SidebarEvent::Reorder {
        source: drag.anchor.clone(),
        before: match drop_target {
            SidebarDropTarget::Before(target) => Some(target.to_owned()),
            SidebarDropTarget::End => None,
        },
    })
}

fn sidebar_drop_is_noop(
    source_index: usize,
    target_index: Option<usize>,
    block_count: usize,
) -> bool {
    match target_index {
        Some(target_index) if source_index < target_index => source_index + 1 == target_index,
        Some(target_index) => source_index == target_index,
        None => source_index + 1 == block_count,
    }
}

fn sidebar_blocks<'a>(items: &'a [SidebarItem<'a>]) -> Vec<SidebarBlock<'a>> {
    let mut blocks: Vec<SidebarBlock<'a>> = Vec::new();
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
        model.session_count.to_string(),
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
    hover_color: egui::Color32,
    current_color: egui::Color32,
) -> egui::Response {
    // Any row carrying an anchor drags its whole block, so grabbing a detail row
    // (process/branch/status/progress) reorders just like grabbing the title row.
    let draggable = item.reorder_anchor.is_some();
    let clickable = item.selectable && item.session_id.is_some();
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("mux-sidebar-item", &item.id)),
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

        match &item.kind {
            SidebarItemKind::Group => paint_group_item(&painter, rect, item, bg),
            SidebarItemKind::Session { active } => {
                paint_session_item(&painter, rect, item, *active, palette, bg)
            }
            SidebarItemKind::Row => paint_generic_sidebar_item(&painter, rect, item, palette, bg),
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
        SidebarTree::None | SidebarTree::Blank => {}
        SidebarTree::Middle => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        SidebarTree::Last => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, cy)], stroke);
            painter.line_segment([Pos2::new(x, cy), Pos2::new(x + 5.0, cy)], stroke);
        }
        SidebarTree::Pipe => {
            painter.line_segment([Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)], stroke);
        }
    }
}

fn paint_group_item(
    painter: &egui::Painter,
    rect: Rect,
    item: &SidebarItem<'_>,
    background: egui::Color32,
) {
    let SidebarDisplay::Text(text) = item.display else {
        return;
    };
    // Tint the group title in its own group color (dim while inactive) rather than running
    // palette.muted through readable_color, whose AAA gate flattened it to flat white.
    let title_color = if item.current {
        item.color
    } else {
        item.dim_color
    };
    painter.text(
        Pos2::new(item_text_x(rect, item), rect.center().y),
        egui::Align2::LEFT_CENTER,
        truncate_label(text, 28),
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
    let (number, name) = match item.display {
        SidebarDisplay::Numbered { number, label } => (Some(number), label),
        SidebarDisplay::Text(text) => (None, text),
    };
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
            (number % 100).to_string(),
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
        truncate_label(name, 20),
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
    let text = match item.display {
        SidebarDisplay::Text(text) => text,
        SidebarDisplay::Numbered { label, .. } => label,
    };
    if !text.is_empty() {
        painter.text(
            Pos2::new(text_x, cy),
            egui::Align2::LEFT_CENTER,
            truncate_label(text, 28),
            egui::FontId::monospace(11.0),
            readable_color(background, palette.muted),
        );
    }
}

fn sidebar_footer_items(items: &[ModuleItem]) -> &[ModuleItem] {
    let len = items.len().min(SIDEBAR_MAX_FOOTER_ITEMS);
    &items[..len]
}

fn sidebar_footer_height(footer_item_count: usize) -> f32 {
    let footer_count = footer_item_count.min(SIDEBAR_MAX_FOOTER_ITEMS);
    SIDEBAR_FOOTER_BASE_HEIGHT + footer_count as f32 * SIDEBAR_FOOTER_ITEM_HEIGHT
}

fn paint_sidebar_footer(
    ui: &egui::Ui,
    rect: Rect,
    footer_h: f32,
    footer_items: &[ModuleItem],
    separator_visible: bool,
    palette: ThemePalette,
    border_color: egui::Color32,
) {
    let painter = ui.painter_at(rect);
    let y = rect.max.y - footer_h;
    if separator_visible {
        painter.line_segment(
            [Pos2::new(rect.min.x, y), Pos2::new(rect.max.x, y)],
            Stroke::new(1.0, border_color),
        );
    }

    let mut row_y = y + 18.0;
    for item in footer_items.iter() {
        let item_rect = Rect::from_min_size(
            Pos2::new(rect.min.x + 14.0, row_y - 10.0),
            egui::vec2(rect.width() - 28.0, 26.0),
        );
        let color = readable_color(palette.base, item.fg.unwrap_or(palette.subtext));
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
        row_y += SIDEBAR_FOOTER_ITEM_HEIGHT;
    }

    let shortcut_color = readable_color(palette.base, palette.muted);
    let galley = crate::ui::keycaps::shortcut_hint_galley_from_painter(
        &painter,
        palette,
        crate::platform::sidebar_shortcut_hints(),
        shortcut_color,
        rect.width() - 28.0,
        11.0,
    );
    painter.galley(
        Pos2::new(rect.min.x + 14.0, rect.max.y - 18.0 - galley.size().y * 0.5),
        galley,
        shortcut_color,
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::ChromeConfig,
        mux::snapshot::MuxSession,
        ui::{
            session_navigation::BindingSessionGroup,
            sidebar::{
                SidebarDisplay, SidebarItem, SidebarItemKind, SidebarTree,
                build_binding_sidebar_items, build_visible_sidebar_items,
            },
        },
    };

    fn test_scope() -> crate::mux::controller::MuxScope {
        crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(0),
            crate::mux::controller::BindingId::from_persistence(0),
        )
    }

    fn binding_scope(binding_id: i64) -> crate::mux::controller::MuxScope {
        crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(1),
            crate::mux::controller::BindingId::from_persistence(binding_id),
        )
    }

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
    fn sidebar_title_drag_rect_reserves_macos_titlebar_button_area() {
        let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(286.0, SIDEBAR_HEADER_HEIGHT));

        assert_eq!(sidebar_title_drag_rect(rect, false), rect);
        assert_eq!(
            sidebar_title_drag_rect(rect, true).min.x,
            MACOS_TITLEBAR_BUTTON_SAFE_WIDTH
        );
    }

    #[test]
    fn context_navigation_counts_sessions_within_the_target_binding() {
        let local_scope = binding_scope(10);
        let remote_scope = binding_scope(20);
        let targets = vec![
            ScopedSessionTarget::new(local_scope, "$1"),
            ScopedSessionTarget::new(local_scope, "$2"),
            ScopedSessionTarget::new(remote_scope, "$1"),
        ];

        assert_eq!(
            sidebar_binding_target_position(&targets, &ScopedSessionTarget::new(local_scope, "$2"),),
            Some((1, 2))
        );
        assert_eq!(
            sidebar_binding_target_position(
                &targets,
                &ScopedSessionTarget::new(remote_scope, "$1"),
            ),
            Some((0, 1))
        );
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

    fn test_session(id: &str, name: &str, active: bool) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active,
            anchor: crate::mux::snapshot::MuxPaneAnchor {
                session_id: id.to_owned(),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn show_test_sidebar(ui: &mut egui::Ui, sessions: &[MuxSession]) -> Option<SidebarEvent> {
        let items = build_visible_sidebar_items(sessions, Some("s1"), 32);
        show_sidebar(
            ui,
            ThemePalette::default(),
            300.0,
            SidebarModel {
                items: &items,
                footer_items: &[],
                session_count: sessions.len(),
                has_sessions: !sessions.is_empty(),
                title_visible: false,
                reserve_titlebar_buttons: false,
                title_icon: None,
                top_inset: 0.0,
                border_visible: false,
                border_bottom: false,
                separator_visible: false,
                focused: false,
                hovered_session: None,
                unfocused_dim: 0.0,
                fullscreen: false,
                hover_override: None,
                current_override: None,
                border_override: None,
            },
        )
    }

    #[test]
    fn sidebar_pointer_activation_preserves_binding_scope_for_colliding_session_ids() {
        let local_scope = binding_scope(10);
        let remote_scope = binding_scope(20);
        let groups = vec![
            BindingSessionGroup {
                scope: local_scope,
                label: "Local".to_owned(),
                sessions: vec![test_session("$1", "work", true)],
                selected_session: Some("$1".to_owned()),
                active: true,
                can_return_to_last_session: false,
            },
            BindingSessionGroup {
                scope: remote_scope,
                label: "Remote".to_owned(),
                sessions: vec![test_session("$1", "work", false)],
                selected_session: Some("$1".to_owned()),
                active: false,
                can_return_to_last_session: false,
            },
        ];
        let items = build_binding_sidebar_items(&groups);
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
        let show = |events: Vec<egui::Event>| {
            let mut event = None;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        event = show_sidebar(
                            ui,
                            ThemePalette::default(),
                            300.0,
                            SidebarModel {
                                items: &items,
                                footer_items: &[],
                                session_count: 2,
                                has_sessions: true,
                                title_visible: false,
                                reserve_titlebar_buttons: false,
                                title_icon: None,
                                top_inset: 0.0,
                                border_visible: false,
                                border_bottom: false,
                                separator_visible: false,
                                focused: false,
                                hovered_session: None,
                                unfocused_dim: 0.0,
                                fullscreen: false,
                                hover_override: None,
                                current_override: None,
                                border_override: None,
                            },
                        );
                    });
                },
            );
            event
        };
        let remote_row = Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 3.5);
        show(vec![egui::Event::PointerMoved(remote_row)]);
        show(vec![egui::Event::PointerButton {
            pos: remote_row,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let event = show(vec![egui::Event::PointerButton {
            pos: remote_row,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(
            event,
            Some(SidebarEvent::ActivateSession(ScopedSessionTarget::new(
                remote_scope,
                "$1",
            )))
        );
    }

    #[test]
    fn sidebar_session_ids_use_real_session_rows_in_order() {
        let sessions = vec![
            test_session("s1", "alpha", true),
            test_session("s2", "beta", false),
        ];
        let mut items = build_visible_sidebar_items(&sessions, Some("s1"), 32);
        let mut detail = items[0].clone();
        detail.id = crate::ui::sidebar::SidebarItemId::Row("s1-detail");
        detail.display = SidebarDisplay::Text("branch");
        detail.kind = SidebarItemKind::Row;
        detail.selectable = true;
        items.insert(1, detail);
        let duplicate = items[2].clone();
        items.push(duplicate);

        let targets = sidebar_session_targets(&items);
        assert_eq!(
            targets,
            vec![
                ScopedSessionTarget::new(test_scope(), "s1"),
                ScopedSessionTarget::new(test_scope(), "s2"),
            ]
        );
        assert_eq!(
            sidebar_context_session_target(&items[0]),
            Some(ScopedSessionTarget::new(test_scope(), "s1"))
        );
        assert_eq!(sidebar_context_session_target(&items[1]), None);
    }

    #[test]
    fn sidebar_header_collapses_when_title_is_hidden() {
        assert_eq!(sidebar_header_height(true), SIDEBAR_HEADER_HEIGHT);
        assert_eq!(sidebar_header_height(false), 0.0);
    }

    #[test]
    fn sidebar_rows_ignore_keyboard_activation_keys() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let context = egui::Context::default();
            crate::ui::icons::install_icon_fonts(&context);
            let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
            let sessions = vec![
                test_session("s1", "alpha", true),
                test_session("s2", "beta", false),
            ];

            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let item_id = crate::ui::sidebar::SidebarItemId::Session {
                            scope: test_scope(),
                            id: "s2",
                        };
                        let id = ui.make_persistent_id(("mux-sidebar-item", &item_id));
                        ui.memory_mut(|memory| memory.request_focus(id));
                        show_test_sidebar(ui, &sessions);
                    });
                },
            );

            let mut event = None;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        event = show_test_sidebar(ui, &sessions);
                    });
                },
            );

            assert_eq!(event, None);
        }
    }

    #[test]
    fn sidebar_context_menu_emits_the_clicked_session_target() {
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
        let sessions = vec![
            test_session("s1", "alpha", true),
            test_session("s2", "beta", false),
        ];
        let frame = |events: Vec<egui::Event>, captured: &mut Option<SidebarEvent>| {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let event = show_test_sidebar(ui, &sessions);
                        if event.is_some() {
                            *captured = event;
                        }
                    });
                },
            );
        };

        let session = Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 1.5);
        let activate = Pos2::new(60.0, SIDEBAR_ROW_HEIGHT * 2.15);
        let mut captured = None;
        frame(vec![egui::Event::PointerMoved(session)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: session,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: session,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(vec![egui::Event::PointerMoved(activate)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: activate,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: activate,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );

        assert_eq!(
            captured,
            Some(SidebarEvent::ContextAction {
                target: ScopedSessionTarget::new(test_scope(), "s2"),
                action: SessionContextAction::Activate,
            })
        );
    }

    #[test]
    fn sidebar_detail_rows_with_session_id_do_not_activate() {
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
        let items = vec![
            SidebarItem {
                id: crate::ui::sidebar::SidebarItemId::Session {
                    scope: test_scope(),
                    id: "s1",
                },
                display: SidebarDisplay::Text("alpha"),
                indent: 0,
                tree: SidebarTree::None,
                selectable: true,
                session_id: Some("s1"),
                session_scope: Some(test_scope()),
                reorder_anchor: Some("alpha"),
                color: egui::Color32::WHITE,
                dim_color: egui::Color32::GRAY,
                kind: SidebarItemKind::Session { active: true },
                current: true,
                can_return_to_last_session: false,
                icon: None,
                primitives: &[],
            },
            SidebarItem {
                id: crate::ui::sidebar::SidebarItemId::Row("s1-detail"),
                display: SidebarDisplay::Text("codex"),
                indent: 2,
                tree: SidebarTree::None,
                selectable: false,
                session_id: Some("s1"),
                session_scope: Some(test_scope()),
                reorder_anchor: Some("alpha"),
                color: egui::Color32::WHITE,
                dim_color: egui::Color32::GRAY,
                kind: SidebarItemKind::Row,
                current: true,
                can_return_to_last_session: false,
                icon: None,
                primitives: &[],
            },
        ];

        let show = |ui: &mut egui::Ui| {
            show_sidebar(
                ui,
                ThemePalette::default(),
                300.0,
                SidebarModel {
                    items: &items,
                    footer_items: &[],
                    session_count: 1,
                    has_sessions: true,
                    title_visible: false,
                    reserve_titlebar_buttons: false,
                    title_icon: None,
                    top_inset: 0.0,
                    border_visible: false,
                    border_bottom: false,
                    separator_visible: false,
                    focused: false,
                    hovered_session: None,
                    unfocused_dim: 0.0,
                    fullscreen: false,
                    hover_override: None,
                    current_override: None,
                    border_override: None,
                },
            )
        };

        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 1.5),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let _ = show(ui);
                });
            },
        );

        let mut event = None;
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                events: vec![egui::Event::PointerButton {
                    pos: Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 1.5),
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    event = show(ui);
                });
            },
        );

        assert_eq!(event, None);
    }

    fn sidebar_drag_event(button: egui::PointerButton) -> Option<SidebarEvent> {
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(286.0, 300.0));
        let sessions = vec![
            test_session("s1", "alpha", true),
            test_session("s2", "beta", false),
        ];

        let frame = |events: Vec<egui::Event>, captured: &mut Option<SidebarEvent>| {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let event = show_test_sidebar(ui, &sessions);
                        if event.is_some() {
                            *captured = event;
                        }
                    });
                },
            );
        };

        let row0 = Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 0.5);
        let row1_low = Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 1.9);
        let mut captured = None;

        frame(vec![egui::Event::PointerMoved(row0)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: row0,
                button,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerMoved(Pos2::new(
                20.0,
                SIDEBAR_ROW_HEIGHT * 1.0,
            ))],
            &mut captured,
        );
        frame(vec![egui::Event::PointerMoved(row1_low)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: row1_low,
                button,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );

        captured
    }

    #[test]
    fn sidebar_primary_drag_gesture_emits_reorder_event() {
        assert_eq!(
            sidebar_drag_event(egui::PointerButton::Primary),
            Some(SidebarEvent::Reorder {
                source: String::from("alpha"),
                before: None,
            })
        );
    }

    #[test]
    fn sidebar_secondary_drag_does_not_emit_reorder_event() {
        assert_eq!(sidebar_drag_event(egui::PointerButton::Secondary), None);
    }

    #[test]
    fn sidebar_drop_target_uses_group_midpoint_for_after_group_moves() {
        let sessions = vec![
            test_session("s1", "agents", true),
            test_session("s2", "arc/migrations", false),
            test_session("s3", "arc/readiness", false),
            test_session("s4", "bootty", false),
        ];
        let items = build_visible_sidebar_items(&sessions, Some("s1"), 32);

        assert_eq!(
            sidebar_drop_target(
                &items,
                Some(Pos2::new(20.0, SIDEBAR_ROW_HEIGHT * 3.5)),
                0.0,
                0.0,
                240.0,
                "agents",
            ),
            Some((
                SidebarDropTarget::Before("bootty"),
                SIDEBAR_ROW_HEIGHT * 4.0,
            ))
        );
    }

    #[test]
    fn sidebar_reorder_event_commits_drop_target() {
        let drag = SidebarDragState {
            anchor: String::from("agents"),
            preview: String::from("agents"),
        };

        assert_eq!(
            sidebar_reorder_event(
                Some(&drag),
                Some((
                    SidebarDropTarget::Before("bootty"),
                    SIDEBAR_ROW_HEIGHT * 4.0
                )),
            ),
            Some(SidebarEvent::Reorder {
                source: String::from("agents"),
                before: Some(String::from("bootty")),
            })
        );
        assert_eq!(sidebar_reorder_event(Some(&drag), None), None);
        assert_eq!(
            sidebar_reorder_event(Some(&drag), Some((SidebarDropTarget::End, 0.0))),
            Some(SidebarEvent::Reorder {
                source: String::from("agents"),
                before: None,
            })
        );
    }

    #[test]
    fn sidebar_session_row_capacity_tracks_visible_row_capacity() {
        let plain = sidebar_session_row_capacity(900.0, 0.0, true, 0);
        let with_footer = sidebar_session_row_capacity(900.0, 0.0, true, 2);

        assert!(with_footer < plain);
        assert_eq!(plain, 33);
    }

    fn space_swipe_test_items() -> (Vec<SpaceSwitcherItem>, SpaceId) {
        let second = SpaceSwitcherItem {
            id: SpaceId::from_persistence(2),
            name: "Review".to_owned(),
            icon: "folder".to_owned(),
            color: [0x7A, 0xA2, 0xF7],
            active: false,
        };
        (
            vec![
                SpaceSwitcherItem {
                    id: SpaceId::from_persistence(1),
                    name: "Work".to_owned(),
                    icon: "folder".to_owned(),
                    color: [0x7A, 0xA2, 0xF7],
                    active: true,
                },
                second.clone(),
            ],
            second.id,
        )
    }

    fn sidebar_space_swipe_frame(
        context: &egui::Context,
        sidebar_rect: Rect,
        spaces: &[SpaceSwitcherItem],
        state: &mut SidebarSpaceSwipeState,
        events: Vec<egui::Event>,
    ) -> (Option<SpaceId>, Vec<egui::Vec2>) {
        let mut selected = None;
        let mut remaining_wheels = Vec::new();
        let _ = context.run_ui(
            egui::RawInput {
                screen_rect: Some(sidebar_rect),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    selected = take_sidebar_space_swipe(ui, sidebar_rect, spaces, state);
                    remaining_wheels = ui.input(|input| {
                        input
                            .events
                            .iter()
                            .filter_map(|event| match event {
                                egui::Event::MouseWheel { delta, .. } => Some(*delta),
                                _ => None,
                            })
                            .collect()
                    });
                });
            },
        );
        (selected, remaining_wheels)
    }

    fn wheel(delta: egui::Vec2, phase: egui::TouchPhase) -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn horizontal_wheel(phase: egui::TouchPhase) -> egui::Event {
        wheel(egui::vec2(-12.0, 1.0), phase)
    }

    #[test]
    fn sidebar_swipe_repeated_start_events_switch_once() {
        let (spaces, second_id) = space_swipe_test_items();
        let context = egui::Context::default();
        let sidebar_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
        let mut state = SidebarSpaceSwipeState::default();

        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![
                    egui::Event::PointerMoved(sidebar_rect.center()),
                    horizontal_wheel(egui::TouchPhase::Start),
                ],
            )
            .0,
            Some(second_id)
        );
        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![horizontal_wheel(egui::TouchPhase::Start)],
            )
            .0,
            None
        );
    }

    #[test]
    fn sidebar_swipe_end_ignores_momentum_move() {
        let (spaces, second_id) = space_swipe_test_items();
        let context = egui::Context::default();
        let sidebar_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
        let mut state = SidebarSpaceSwipeState::default();

        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![
                    egui::Event::PointerMoved(sidebar_rect.center()),
                    horizontal_wheel(egui::TouchPhase::Move),
                ],
            )
            .0,
            Some(second_id)
        );
        let (selected, remaining_wheels) = sidebar_space_swipe_frame(
            &context,
            sidebar_rect,
            &spaces,
            &mut state,
            vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::End)],
        );
        assert_eq!(selected, None);
        assert!(remaining_wheels.is_empty());
        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![horizontal_wheel(egui::TouchPhase::Move)],
            )
            .0,
            None
        );
    }

    #[test]
    fn sidebar_swipe_ignores_macos_momentum_and_resets() {
        let (spaces, second_id) = space_swipe_test_items();
        let context = egui::Context::default();
        let sidebar_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
        let mut state = SidebarSpaceSwipeState::default();
        let mut replay = |events| {
            sidebar_space_swipe_frame(&context, sidebar_rect, &spaces, &mut state, events).0
        };

        let targets = [
            replay(vec![
                egui::Event::PointerMoved(sidebar_rect.center()),
                wheel(egui::vec2(-3.0, 1.0), egui::TouchPhase::Start),
            ]),
            replay(vec![wheel(egui::vec2(-3.0, 1.0), egui::TouchPhase::Move)]),
            replay(vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::End)]),
            replay(vec![wheel(egui::vec2(-21.0, 0.0), egui::TouchPhase::Start)]),
            replay(vec![wheel(egui::vec2(-21.0, 0.0), egui::TouchPhase::Move)]),
            replay(vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::End)]),
        ];
        assert_eq!(
            targets.into_iter().flatten().collect::<Vec<_>>(),
            [second_id]
        );

        let targets = [
            replay(vec![wheel(egui::vec2(-3.0, 1.0), egui::TouchPhase::Start)]),
            replay(vec![wheel(egui::vec2(-3.0, 1.0), egui::TouchPhase::Move)]),
            replay(vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::End)]),
            replay(vec![wheel(egui::vec2(-21.0, 0.0), egui::TouchPhase::Start)]),
            replay(vec![wheel(egui::vec2(-21.0, 0.0), egui::TouchPhase::Move)]),
            replay(vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::End)]),
        ];
        assert_eq!(
            targets.into_iter().flatten().collect::<Vec<_>>(),
            [second_id]
        );
    }

    #[test]
    fn sidebar_swipe_opposite_start_after_end_reverses_direction() {
        let (mut spaces, _) = space_swipe_test_items();
        let first_id = spaces[0].id;
        spaces[0].active = false;
        spaces[1].active = true;
        let mut state = SidebarSpaceSwipeState::default();

        assert_eq!(
            sidebar_space_swipe_target(&spaces, -3.0, egui::TouchPhase::Start, &mut state,),
            None
        );
        assert_eq!(
            sidebar_space_swipe_target(&spaces, 0.0, egui::TouchPhase::End, &mut state),
            None
        );
        assert_eq!(
            sidebar_space_swipe_target(&spaces, 3.0, egui::TouchPhase::Start, &mut state),
            Some(first_id)
        );
        assert_eq!(
            state.phase,
            SidebarSpaceSwipePhase::Active {
                direction: SidebarSpaceSwipeDirection::Positive,
            }
        );
    }

    #[test]
    fn sidebar_swipe_cancel_resets_for_the_next_gesture() {
        let (spaces, second_id) = space_swipe_test_items();
        let context = egui::Context::default();
        let sidebar_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
        let mut state = SidebarSpaceSwipeState::default();

        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![
                    egui::Event::PointerMoved(sidebar_rect.center()),
                    horizontal_wheel(egui::TouchPhase::Start),
                ],
            )
            .0,
            Some(second_id)
        );
        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![wheel(egui::Vec2::ZERO, egui::TouchPhase::Cancel)],
            )
            .0,
            None
        );
        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![horizontal_wheel(egui::TouchPhase::Start)],
            )
            .0,
            Some(second_id)
        );
    }

    #[test]
    fn sidebar_swipe_leaves_vertical_wheel_events_for_the_sidebar() {
        let (spaces, second_id) = space_swipe_test_items();
        let context = egui::Context::default();
        let sidebar_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 200.0));
        let mut state = SidebarSpaceSwipeState::default();
        let vertical = egui::vec2(0.0, 12.0);

        let (selected, remaining_wheels) = sidebar_space_swipe_frame(
            &context,
            sidebar_rect,
            &spaces,
            &mut state,
            vec![
                egui::Event::PointerMoved(sidebar_rect.center()),
                wheel(vertical, egui::TouchPhase::Move),
            ],
        );

        assert_eq!(selected, None);
        assert_eq!(remaining_wheels, vec![vertical]);
        assert_eq!(
            sidebar_space_swipe_frame(
                &context,
                sidebar_rect,
                &spaces,
                &mut state,
                vec![horizontal_wheel(egui::TouchPhase::Move)],
            )
            .0,
            Some(second_id)
        );
    }

    #[test]
    fn space_switcher_click_activates_a_full_window_space() {
        let first = SpaceSwitcherItem {
            id: SpaceId::from_persistence(1),
            name: "Work".to_owned(),
            icon: "folder".to_owned(),
            color: [0x7A, 0xA2, 0xF7],
            active: true,
        };
        let second = SpaceSwitcherItem {
            id: SpaceId::from_persistence(2),
            name: "Review".to_owned(),
            icon: "folder".to_owned(),
            color: [0x7A, 0xA2, 0xF7],
            active: false,
        };
        let spaces = vec![first, second.clone()];
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 44.0));
        let show = |events: Vec<egui::Event>| {
            let mut selected = None;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            selected =
                                show_space_switcher(ui, ThemePalette::default(), &spaces, None);
                        });
                },
            );
            selected
        };
        let second_center = Pos2::new(120.0, 22.0);
        show(vec![egui::Event::PointerMoved(second_center)]);
        show(vec![egui::Event::PointerButton {
            pos: second_center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let selected = show(vec![egui::Event::PointerButton {
            pos: second_center,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(selected, Some(SpaceSwitcherEvent::Activate(second.id)));
    }

    #[test]
    fn space_switcher_plus_emits_create_event() {
        let spaces = vec![SpaceSwitcherItem {
            id: SpaceId::from_persistence(1),
            name: "Work".to_owned(),
            icon: "folder".to_owned(),
            color: [0x7A, 0xA2, 0xF7],
            active: true,
        }];
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, 44.0));
        let show = |events: Vec<egui::Event>| {
            let mut event = None;
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            event = show_space_switcher(ui, ThemePalette::default(), &spaces, None);
                        });
                },
            );
            event
        };
        let plus_center = Pos2::new(135.0, 22.0);

        show(vec![egui::Event::PointerMoved(plus_center)]);
        show(vec![egui::Event::PointerButton {
            pos: plus_center,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let event = show(vec![egui::Event::PointerButton {
            pos: plus_center,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);

        assert_eq!(event, Some(SpaceSwitcherEvent::Create));
    }

    #[test]
    fn space_switcher_context_menu_emits_edit_for_clicked_space() {
        let spaces = vec![
            SpaceSwitcherItem {
                id: SpaceId::from_persistence(1),
                name: "Work".to_owned(),
                icon: "folder".to_owned(),
                color: [0x7A, 0xA2, 0xF7],
                active: true,
            },
            SpaceSwitcherItem {
                id: SpaceId::from_persistence(2),
                name: "Review".to_owned(),
                icon: "terminal".to_owned(),
                color: [0x7A, 0xA2, 0xF7],
                active: false,
            },
        ];
        let context = egui::Context::default();
        crate::ui::icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, SPACE_SWITCHER_HEIGHT));
        let frame = |events: Vec<egui::Event>, captured: &mut Option<SpaceSwitcherEvent>| {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        if let Some(event) =
                            show_space_switcher(ui, ThemePalette::default(), &spaces, None)
                        {
                            *captured = Some(event);
                        }
                    });
                },
            );
        };

        let control = Pos2::new(120.0, SPACE_SWITCHER_HEIGHT * 0.5);
        let edit = Pos2::new(150.0, 16.0);
        let mut captured = None;
        frame(vec![egui::Event::PointerMoved(control)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: control,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: control,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(vec![egui::Event::PointerMoved(edit)], &mut captured);
        frame(
            vec![egui::Event::PointerButton {
                pos: edit,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );
        frame(
            vec![egui::Event::PointerButton {
                pos: edit,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut captured,
        );

        assert_eq!(captured, Some(SpaceSwitcherEvent::Edit(spaces[1].id)));
    }

    #[test]
    fn space_switcher_controls_are_centered_in_the_44px_strip() {
        let strip = Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, SPACE_SWITCHER_HEIGHT));
        let control = space_switcher_button_rect(strip, 120.0);
        assert_eq!(control.center().y, strip.center().y);
    }

    #[test]
    fn space_indicator_is_a_borderless_dot_between_space_controls() {
        let spaces = vec![
            SpaceSwitcherItem {
                id: SpaceId::from_persistence(1),
                name: "Work".to_owned(),
                icon: "folder".to_owned(),
                color: [0x7A, 0xA2, 0xF7],
                active: false,
            },
            SpaceSwitcherItem {
                id: SpaceId::from_persistence(2),
                name: "Review".to_owned(),
                icon: "terminal".to_owned(),
                color: [0x7A, 0xA2, 0xF7],
                active: true,
            },
        ];

        assert_eq!(
            space_indicator_dot(
                Rect::from_min_size(Pos2::ZERO, egui::vec2(240.0, SPACE_SWITCHER_HEIGHT)),
                &spaces,
                Some((spaces[0].id, spaces[1].id, 0.5)),
                &|index| index as f32 * 32.0,
            ),
            Some(Pos2::new(16.0, SPACE_SWITCHER_HEIGHT - 4.0))
        );
    }
}
