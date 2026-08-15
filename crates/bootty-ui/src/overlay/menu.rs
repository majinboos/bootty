//! A confirmation / action menu: a read-only status report above a list of
//! risk-tinted actions. Backs the ditch and confirm windows.

use crate::{ThemePalette, icons};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense};

use super::list::selection_after_nav;

const ACTION_HEIGHT: f32 = 46.0;

/// How destructive an action is; selects its tint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionRisk {
    Safe,
    Caution,
    Danger,
}

impl ActionRisk {
    fn tint(self, palette: ThemePalette) -> Color32 {
        match self {
            Self::Safe => palette.success,
            Self::Caution => palette.warning,
            Self::Danger => palette.destructive,
        }
    }
}

/// One selectable action row.
#[derive(Clone, Debug)]
pub struct ActionItem {
    pub icon: Option<String>,
    pub label: String,
    pub description: Option<String>,
    pub risk: ActionRisk,
}

/// One line of the status report drawn above the actions.
#[derive(Clone, Debug)]
pub struct StatusLine {
    pub label: String,
    pub value: String,
    pub tint: Option<Color32>,
}

/// What an [`ActionMenu`] produced for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionMenuOutcome {
    pub selected: usize,
    pub activated: Option<usize>,
}

pub struct ActionMenu<'a> {
    status: &'a [StatusLine],
    actions: &'a [ActionItem],
    selected: usize,
}

impl<'a> ActionMenu<'a> {
    pub fn new(status: &'a [StatusLine], actions: &'a [ActionItem], selected: usize) -> Self {
        Self {
            status,
            actions,
            selected,
        }
    }

    pub fn show(self, ui: &mut egui::Ui, palette: ThemePalette) -> ActionMenuOutcome {
        for line in self.status {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{}:", line.label))
                        .monospace()
                        .size(12.0)
                        .color(palette.muted),
                );
                ui.label(
                    egui::RichText::new(&line.value)
                        .monospace()
                        .size(12.0)
                        .color(line.tint.unwrap_or(palette.text)),
                );
            });
        }
        if !self.status.is_empty() {
            ui.add_space(8.0);
        }

        let (next, previous, enter) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowDown)
                    || (input.key_pressed(egui::Key::N) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::ArrowUp)
                    || (input.key_pressed(egui::Key::P) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::Enter),
            )
        });
        let selected = selection_after_nav(self.selected, self.actions.len(), next, previous);
        let mut activated = (!self.actions.is_empty() && enter).then_some(selected);

        let width = ui.available_width();
        for (index, action) in self.actions.iter().enumerate() {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, ACTION_HEIGHT), Sense::click());
            paint_action(ui.painter(), rect, palette, action, index == selected);
            if response.clicked() {
                activated = Some(index);
            }
        }

        ActionMenuOutcome {
            selected,
            activated,
        }
    }
}

fn paint_action(
    painter: &egui::Painter,
    rect: Rect,
    palette: ThemePalette,
    action: &ActionItem,
    selected: bool,
) {
    let tint = action.risk.tint(palette);
    if selected {
        painter.rect_filled(rect, 0.0, palette.hover);
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            tint,
        );
    }

    let mut x = rect.left() + 14.0;
    if let Some(slug) = &action.icon
        && icons::paint_icon_slug(
            painter,
            slug,
            Pos2::new(x + 8.0, rect.center().y),
            16.0,
            tint,
        )
    {
        x += 28.0;
    }

    let has_description = action.description.is_some();
    let label_y = if has_description {
        rect.top() + 15.0
    } else {
        rect.center().y
    };
    painter.text(
        Pos2::new(x, label_y),
        egui::Align2::LEFT_CENTER,
        &action.label,
        FontId::monospace(14.0),
        tint,
    );
    if let Some(description) = &action.description {
        painter.text(
            Pos2::new(x, rect.bottom() - 15.0),
            egui::Align2::LEFT_CENTER,
            description,
            FontId::monospace(11.0),
            palette.muted,
        );
    }
}
