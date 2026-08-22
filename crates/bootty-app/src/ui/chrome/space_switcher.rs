//! The Space switcher strip at the foot of the sidebar, and the trackpad swipe that drives it.

use bootty_mux::controller::SpaceId;
use bootty_ui::{ThemePalette, icons::paint_icon_slug};
use eframe::egui::{self, Pos2, Rect};

use crate::ui::session_navigation::ScopedSessionTarget;
use crate::workspace_runtime::SpaceSummary;

use super::sidebar_panel::{end_session_drag, session_drag};

pub(crate) const SPACE_SWITCHER_HEIGHT: f32 = 44.0;
const SPACE_SWITCHER_BUTTON_SIZE: f32 = 28.0;
const SPACE_SWITCHER_BUTTON_GAP: f32 = 4.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceSwitcherEvent {
    Activate(SpaceId),
    Create,
    Edit(SpaceId),
    Reconnect(SpaceId),
    Close(SpaceId),
    /// Sessions dragged out of the sidebar and dropped on a Space icon.
    MoveSessions {
        sessions: Vec<ScopedSessionTarget>,
        to: SpaceId,
    },
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
    fullscreen: bool,
) -> Option<SpaceSwitcherEvent> {
    let width = ui.available_width().max(0.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, SPACE_SWITCHER_HEIGHT),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    // Same lift the session rows use: in notched fullscreen a 0.045 hover on a black background
    // is nearly invisible, which is what this strip used to paint.
    let hover_color = super::sidebar_hover_color(palette, fullscreen);
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
    // A session dragged out of the sidebar drops here, so the strip hit-tests the pointer itself:
    // egui withholds hover from a widget while another one is being dragged.
    let drag = session_drag(ui.ctx());
    let pointer = ui.input(|input| input.pointer.latest_pos());
    let released = ui.input(|input| !input.pointer.primary_down());

    let mut event = None;
    for (index, space) in spaces.iter().enumerate() {
        let item_rect = button_rect(index);
        if let Some(drag) = &drag
            && pointer.is_some_and(|pos| item_rect.contains(pos))
        {
            if space.accepts_moves {
                painter.rect_filled(item_rect, 6.0, hover_color);
                painter.rect_stroke(
                    item_rect,
                    6.0,
                    egui::Stroke::new(1.0, palette.primary),
                    egui::StrokeKind::Inside,
                );
                if released {
                    event = Some(SpaceSwitcherEvent::MoveSessions {
                        sessions: drag.sessions.clone(),
                        to: space.id,
                    });
                }
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
            }
        }
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
    // The sidebar hands a drag that left through its bottom edge over to this strip, so ending it
    // is this strip's job whether or not it landed on a Space.
    if drag.is_some() && released {
        end_session_drag(ui.ctx());
    }
    event
}
