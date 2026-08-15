use bootty_ui::Theme;
use eframe::egui;

use bootty_ui::overlay::{self, FloatingWindow, ListRow, ListView};

use crate::mux::snapshot::MuxSession;
use crate::ui::session_navigation::{BindingSessionGroup, ScopedSessionTarget};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionPickerDialog {
    filter: String,
    selected: usize,
    focus_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPickerEvent {
    None,
    Close,
    ActivateSession(ScopedSessionTarget),
}

#[derive(Clone, Debug)]
struct PickerRow {
    row: ListRow,
    target: Option<ScopedSessionTarget>,
}

impl SessionPickerDialog {
    pub fn open() -> Self {
        Self {
            filter: String::new(),
            selected: 0,
            focus_filter: true,
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        groups: &[BindingSessionGroup],
    ) -> SessionPickerEvent {
        let picker_rows = picker_rows(groups, &self.filter);
        self.selected = overlay::clamp_selection(self.selected, picker_rows.len());
        let rows = picker_rows
            .iter()
            .map(|entry| entry.row.clone())
            .collect::<Vec<_>>();
        let match_count = picker_rows
            .iter()
            .filter(|row| row.target.is_some())
            .count();
        let session_count = groups
            .iter()
            .map(|group| group.sessions.len())
            .sum::<usize>();
        let list_max_height = overlay::list_max_height(ctx, 150.0, 520.0);

        let result = FloatingWindow::new("session-picker-dialog", "Session Finder")
            .icon("terminal")
            .shortcut_hint([("enter", "select"), ("esc", "close")])
            .footer(format!("{match_count} / {session_count} sessions"))
            .width(overlay::panel_width(ctx, 780.0, 520.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("session-picker-filter"),
                    &mut self.filter,
                    theme,
                    "filter sessions or bindings...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(8.0);
                let outcome = ListView::new("session-picker-list", &rows, self.selected)
                    .max_height(list_max_height)
                    .empty_text("no matching sessions")
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(target) = picker_rows.get(index).and_then(|row| row.target.as_ref())
        {
            return SessionPickerEvent::ActivateSession(target.clone());
        }
        if result.escaped || result.clicked_outside {
            return SessionPickerEvent::Close;
        }
        SessionPickerEvent::None
    }
}

fn picker_rows(groups: &[BindingSessionGroup], filter: &str) -> Vec<PickerRow> {
    let show_sections = groups.len() > 1;
    let mut rows = Vec::new();
    for group in groups {
        let binding_matches = fuzzy_matches(&group.label, filter);
        let matching_sessions = group
            .sessions
            .iter()
            .filter(|session| {
                binding_matches
                    || session_matches(session, filter)
                    || fuzzy_matches(group.display_name(session), filter)
            })
            .collect::<Vec<_>>();
        if matching_sessions.is_empty() {
            continue;
        }
        if show_sections {
            rows.push(PickerRow {
                row: ListRow {
                    primary: group.label.clone(),
                    section: true,
                    ..ListRow::default()
                },
                target: None,
            });
        }
        rows.extend(matching_sessions.into_iter().map(|session| {
            PickerRow {
                row: ListRow {
                    icon: Some("terminal".to_owned()),
                    primary: group.display_name(session).to_owned(),
                    trailing: session
                        .anchor
                        .process
                        .as_deref()
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned),
                    current: group.session_is_current(session),
                    ..ListRow::default()
                },
                target: Some(group.target(session)),
            }
        }));
    }
    rows
}

fn fuzzy_matches(value: &str, filter: &str) -> bool {
    let filter = filter.trim();
    filter.is_empty() || overlay::fuzzy_match(value, filter)
}

fn session_matches(session: &MuxSession, filter: &str) -> bool {
    fuzzy_matches(&session.name, filter)
        || overlay::fuzzy_match(&session.id, filter.trim())
        || session
            .anchor
            .process
            .as_deref()
            .is_some_and(|process| overlay::fuzzy_match(process, filter.trim()))
}
