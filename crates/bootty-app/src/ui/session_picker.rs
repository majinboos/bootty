use bootty_ui::Theme;
use eframe::egui;

use crate::mux::snapshot::MuxSession;
use crate::ui::{
    overlay::{self, FloatingWindow, ListRow, ListView, list},
    session_navigation::{BindingSessionGroup, ScopedSessionTarget},
};

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
        self.selected = list::clamp_selection(self.selected, picker_rows.len());
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
            .filter(|session| binding_matches || session_matches(session, filter))
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
                    primary: session.name.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mux::snapshot::MuxPaneAnchor;

    fn test_scope() -> crate::mux::controller::MuxScope {
        crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(1),
            crate::mux::controller::BindingId::from_persistence(10),
        )
    }

    fn group(sessions: Vec<MuxSession>, selected_session: Option<&str>) -> BindingSessionGroup {
        BindingSessionGroup {
            scope: test_scope(),
            label: "Local".to_owned(),
            sessions,
            selected_session: selected_session.map(str::to_owned),
            active: true,
            can_return_to_last_session: false,
        }
    }

    fn session(id: &str, name: &str, process: Option<&str>) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: false,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                process: process.map(str::to_owned),
                ..Default::default()
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn filters_sessions_by_fuzzy_name_id_process_or_binding() {
        let group = group(
            vec![
                session("s1", "bootty", Some("cargo")),
                session("s2", "dotfiles", Some("nvim")),
                session("s3", "blueprints", Some("zsh")),
            ],
            None,
        );
        let groups = [group];
        let matching_ids = |filter: &str| {
            picker_rows(&groups, filter)
                .into_iter()
                .filter_map(|row| row.target.map(|target| target.session_id))
                .collect::<Vec<_>>()
        };

        assert_eq!(matching_ids("bty"), vec!["s1"]);
        assert_eq!(matching_ids("nv"), vec!["s2"]);
        assert_eq!(matching_ids("s3"), vec!["s3"]);
        assert_eq!(matching_ids("Local"), vec!["s1", "s2", "s3"]);
        assert!(matching_ids("missing").is_empty());
    }

    #[test]
    fn picker_groups_colliding_session_ids_by_binding_and_marks_only_active_target() {
        let local_scope = crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(1),
            crate::mux::controller::BindingId::from_persistence(10),
        );
        let remote_scope = crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(1),
            crate::mux::controller::BindingId::from_persistence(20),
        );
        let groups = vec![
            BindingSessionGroup {
                scope: local_scope,
                label: "Local".to_owned(),
                sessions: vec![session("$1", "work", Some("zsh"))],
                selected_session: Some("$1".to_owned()),
                active: true,
                can_return_to_last_session: false,
            },
            BindingSessionGroup {
                scope: remote_scope,
                label: "Remote".to_owned(),
                sessions: vec![session("$1", "work", Some("ssh"))],
                selected_session: Some("$1".to_owned()),
                active: false,
                can_return_to_last_session: false,
            },
        ];

        let rows = picker_rows(&groups, "");

        assert_eq!(rows.len(), 4);
        assert!(rows[0].row.section);
        assert_eq!(rows[0].row.primary, "Local");
        assert_eq!(
            rows[1].target.as_ref().map(|target| target.scope),
            Some(local_scope)
        );
        assert!(rows[1].row.current);
        assert!(rows[2].row.section);
        assert_eq!(rows[2].row.primary, "Remote");
        assert_eq!(
            rows[3].target.as_ref().map(|target| target.scope),
            Some(remote_scope)
        );
        assert!(!rows[3].row.current);
        assert_ne!(rows[1].target, rows[3].target);
    }

    #[test]
    fn current_session_row_is_marked_by_id_or_name() {
        let sessions = vec![
            session("s1", "bootty", None),
            session("s2", "dotfiles", None),
        ];

        let by_id = picker_rows(&[group(sessions.clone(), Some("s1"))], "");
        assert!(by_id[0].row.current && !by_id[1].row.current);

        let by_name = picker_rows(&[group(sessions, Some("dotfiles"))], "");
        assert!(!by_name[0].row.current && by_name[1].row.current);
    }
}
