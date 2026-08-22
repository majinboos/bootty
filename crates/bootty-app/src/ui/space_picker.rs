use bootty_mux::controller::SpaceId;
use bootty_ui::{
    Theme, overlay,
    overlay::{FloatingWindow, ListRow, ListView},
};
use eframe::egui;

use crate::ui::chrome::SpaceMoveTarget;
use crate::ui::session_navigation::ScopedSessionTarget;

/// Picks the Space a session moves to. Unreachable Spaces are shown and greyed rather than
/// omitted, because "not from here" is a more useful answer than a missing row.
#[derive(Clone, Debug)]
pub struct SpacePickerDialog {
    session: ScopedSessionTarget,
    session_name: String,
    spaces: Vec<SpaceMoveTarget>,
    filter: String,
    selected: usize,
    focus_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpacePickerEvent {
    Close,
    Move {
        session: ScopedSessionTarget,
        space: Option<SpaceId>,
    },
}

impl SpacePickerDialog {
    pub fn open(
        session: ScopedSessionTarget,
        session_name: String,
        spaces: Vec<SpaceMoveTarget>,
    ) -> Self {
        Self {
            session,
            session_name,
            spaces,
            filter: String::new(),
            selected: 0,
            focus_filter: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<SpacePickerEvent> {
        let (rows, choices) = self.rows();
        self.selected = overlay::clamp_selection(self.selected, rows.len());
        let list_max = overlay::list_max_height(ctx, 200.0, 480.0);
        let scroll_selected = self.focus_filter;

        let result = FloatingWindow::new("space-picker-dialog", "Move Session to Space")
            .icon("shapes")
            .hint("Enter move   Esc close")
            .footer(self.session_name.clone())
            .width(overlay::panel_width(ctx, 620.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("space-picker-filter"),
                    &mut self.filter,
                    theme,
                    "filter spaces...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(6.0);
                let outcome = ListView::new("space-picker-list", &rows, self.selected)
                    .max_height(list_max)
                    .row_height(34.0)
                    .empty_text("no matching spaces")
                    .scroll_selected(scroll_selected)
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(choice) = choices.get(index)
        {
            return match choice {
                // A Space the session cannot reach stays inert rather than closing the dialog on a
                // move that would not happen.
                Choice::Unreachable => None,
                Choice::Space(id) => Some(SpacePickerEvent::Move {
                    session: self.session.clone(),
                    space: Some(*id),
                }),
                Choice::Unassign => Some(SpacePickerEvent::Move {
                    session: self.session.clone(),
                    space: None,
                }),
            };
        }
        (result.escaped || result.clicked_outside).then_some(SpacePickerEvent::Close)
    }

    fn rows(&self) -> (Vec<ListRow>, Vec<Choice>) {
        let filter = self.filter.trim().to_ascii_lowercase();
        let matches =
            |name: &str| filter.is_empty() || name.to_ascii_lowercase().contains(filter.as_str());

        let mut rows = Vec::new();
        let mut choices = Vec::new();
        for space in &self.spaces {
            if space.current || !matches(&space.name) {
                continue;
            }
            rows.push(ListRow {
                icon: Some(space.icon.clone()),
                primary: space.name.clone(),
                secondary: (!space.reachable).then(|| "runs on another multiplexer".to_owned()),
                ..ListRow::default()
            });
            choices.push(if space.reachable {
                Choice::Space(space.id)
            } else {
                Choice::Unreachable
            });
        }
        if matches("nothing unassign") {
            rows.push(ListRow {
                icon: Some("circle-dashed".to_owned()),
                primary: "Nothing".to_owned(),
                secondary: Some("leave it running, claimed by no Space".to_owned()),
                ..ListRow::default()
            });
            choices.push(Choice::Unassign);
        }
        (rows, choices)
    }
}

#[derive(Clone, Copy, Debug)]
enum Choice {
    Space(SpaceId),
    Unreachable,
    Unassign,
}
