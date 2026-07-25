use bootty_ui::Theme;
use eframe::egui;

use crate::{
    project_catalog::{
        ProjectPickerEntry, discover_project_picker_entries, toggle_favorite_project_path,
    },
    strings::display_path,
    ui::overlay::{self, FloatingWindow, ListRow, ListView, list},
    worktree_catalog::{WorktreePickerEntry, discover_worktree_picker_entries},
};

mod model;

use model::{NewMuxSessionStep, filtered_worktree_entries, project_entries_for_filter};

pub use crate::mux::controller::NewMuxSessionRequest;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMuxSessionDialog {
    step: NewMuxSessionStep,
    filter: String,
    selected: usize,
    projects: Vec<ProjectPickerEntry>,
    worktrees: Vec<WorktreePickerEntry>,
    selected_project: Option<ProjectPickerEntry>,
    focus_filter: bool,
    branch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSessionPickerEvent {
    None,
    Close,
    Error(String),
    CreateWorktree { repo: String, branch: String },
    CreateSession { cwd: String },
}

impl NewMuxSessionDialog {
    pub fn open() -> Self {
        Self {
            step: NewMuxSessionStep::Project,
            filter: String::new(),
            selected: 0,
            projects: discover_project_picker_entries(),
            worktrees: Vec::new(),
            selected_project: None,
            focus_filter: true,
            branch: String::new(),
        }
    }

    /// `open_cwds` lists the working directories of sessions already open, so the
    /// worktree step can default away from worktrees that are already in use.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        open_cwds: &[String],
    ) -> NewSessionPickerEvent {
        match self.step {
            NewMuxSessionStep::Project => self.show_project_step(ctx, theme, open_cwds),
            NewMuxSessionStep::Worktree => self.show_worktree_step(ctx, theme),
            NewMuxSessionStep::BranchName => self.show_branch_step(ctx, theme),
        }
    }

    fn show_project_step(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        open_cwds: &[String],
    ) -> NewSessionPickerEvent {
        let entries = project_entries_for_filter(&self.projects, &self.filter);
        self.selected = list::clamp_selection(self.selected, entries.len());
        let favorite = favorite_shortcut_pressed(ctx)
            .then(|| entries.get(self.selected).cloned())
            .flatten();
        let rows = project_rows(&entries);

        let result = self
            .frame(ctx, "Directory", "folder", project_step_hint())
            .show(ctx, theme, |ui, palette| {
                self.body(
                    ui,
                    palette,
                    theme,
                    "filter directories...",
                    &rows,
                    "no matching directories",
                )
            });

        if let Some(entry) = favorite {
            return self.toggle_project_favorite(entry);
        }

        if let Some(index) = result.inner.activated
            && let Some(entry) = entries.get(index).cloned()
        {
            return self.activate_project(entry, open_cwds);
        }
        self.close_if_dismissed(&result)
    }

    fn show_worktree_step(&mut self, ctx: &egui::Context, theme: Theme) -> NewSessionPickerEvent {
        let entries = filtered_worktree_entries(&self.worktrees, &self.filter);
        self.selected = list::clamp_selection(self.selected, entries.len());
        let rows = worktree_rows(&entries, theme);

        let result = self
            .frame(ctx, "Worktree", "git-branch", WORKTREE_STEP_HINT)
            .show(ctx, theme, |ui, palette| {
                self.body(
                    ui,
                    palette,
                    theme,
                    "filter worktrees...",
                    &rows,
                    "no matching worktrees",
                )
            });

        if let Some(index) = result.inner.activated
            && let Some(entry) = entries.get(index).cloned()
        {
            return self.activate_worktree(entry);
        }
        self.close_if_dismissed(&result)
    }

    fn show_branch_step(&mut self, ctx: &egui::Context, theme: Theme) -> NewSessionPickerEvent {
        let Some(repo) = self
            .selected_project
            .as_ref()
            .map(|project| project.path.clone())
        else {
            return NewSessionPickerEvent::Close;
        };
        let caption = format!("new branch in {}", display_path(&repo));
        let branch = self.branch.trim().to_owned();

        let result = self
            .frame(ctx, "New Worktree", "git-branch", BRANCH_STEP_HINT)
            .show(ctx, theme, |ui, _palette| {
                overlay::TextPrompt::new("new-worktree-branch")
                    .caption(&caption)
                    .hint("branch name...")
                    .submit_disabled(branch.is_empty())
                    .show(ui, theme, &mut self.branch, &mut self.focus_filter)
            });

        if result.inner.submitted && !branch.is_empty() {
            return NewSessionPickerEvent::CreateWorktree { repo, branch };
        }
        self.close_if_dismissed(&result)
    }

    /// Build the shell for the current step; `id` is stable across steps so the
    /// panel stays centered and the filter keeps focus as the body swaps.
    fn frame(
        &self,
        ctx: &egui::Context,
        title: &'static str,
        icon: &'static str,
        hint: &'static [(&'static str, &'static str)],
    ) -> FloatingWindow {
        FloatingWindow::new("new-mux-session-dialog", title)
            .icon(icon)
            .shortcut_hint(hint.iter().copied())
            .width(overlay::panel_width(ctx, 860.0, 560.0))
    }

    fn body(
        &mut self,
        ui: &mut egui::Ui,
        palette: bootty_ui::ThemePalette,
        theme: Theme,
        hint: &str,
        rows: &[ListRow],
        empty_text: &str,
    ) -> overlay::ListOutcome {
        let filter = overlay::filter_field(
            ui,
            egui::Id::new("new-session-picker-filter"),
            &mut self.filter,
            theme,
            hint,
        );
        if self.focus_filter {
            filter.request_focus();
            self.focus_filter = false;
        }
        ui.add_space(8.0);
        let outcome = ListView::new("new-session-picker-list", rows, self.selected)
            .max_height(overlay::list_max_height(ui.ctx(), 150.0, 520.0))
            .empty_text(empty_text)
            .show(ui, palette);
        self.selected = outcome.selected;
        outcome
    }

    fn close_if_dismissed<R>(&self, result: &overlay::OverlayResult<R>) -> NewSessionPickerEvent {
        if result.escaped || result.clicked_outside {
            NewSessionPickerEvent::Close
        } else {
            NewSessionPickerEvent::None
        }
    }

    fn toggle_project_favorite(&mut self, project: ProjectPickerEntry) -> NewSessionPickerEvent {
        match toggle_favorite_project_path(&project.path) {
            Ok(favorite) => {
                self.set_project_favorite(&project.path, favorite);
                NewSessionPickerEvent::None
            }
            Err(error) => NewSessionPickerEvent::Error(format!(
                "favorite {}: {error}",
                display_path(&project.path)
            )),
        }
    }

    fn set_project_favorite(&mut self, path: &str, favorite: bool) {
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| same_dir(&project.path, path))
        {
            project.favorite = favorite;
        } else if favorite {
            self.projects.push(ProjectPickerEntry {
                path: path.to_owned(),
                favorite,
            });
        }
    }

    /// Discover the project's worktrees and decide what to show next. A repository's only
    /// worktree skips straight to session creation only while it is unused; selecting it again
    /// opens the worktree chooser with "New worktree" selected.
    fn activate_project(
        &mut self,
        project: ProjectPickerEntry,
        open_cwds: &[String],
    ) -> NewSessionPickerEvent {
        let worktrees = discover_worktree_picker_entries(&project.path);
        if let Some(cwd) = single_unused_worktree_cwd(&worktrees, open_cwds) {
            return NewSessionPickerEvent::CreateSession { cwd };
        }

        self.selected = default_worktree_selection(&worktrees, open_cwds);
        self.step = NewMuxSessionStep::Worktree;
        self.filter.clear();
        self.focus_filter = true;
        self.worktrees = worktrees;
        self.selected_project = Some(project);
        NewSessionPickerEvent::None
    }

    /// Selecting the "New worktree" row advances to the branch-name prompt;
    /// an existing worktree creates a session directly.
    fn activate_worktree(&mut self, entry: WorktreePickerEntry) -> NewSessionPickerEvent {
        if entry.is_new {
            self.step = NewMuxSessionStep::BranchName;
            self.branch.clear();
            self.focus_filter = true;
            NewSessionPickerEvent::None
        } else if let Some(cwd) = entry.path {
            NewSessionPickerEvent::CreateSession { cwd }
        } else {
            NewSessionPickerEvent::Close
        }
    }
}

fn single_unused_worktree_cwd(
    entries: &[WorktreePickerEntry],
    open_cwds: &[String],
) -> Option<String> {
    let mut real = entries.iter().filter(|entry| !entry.is_new);
    let only = real.next()?;
    if real.next().is_some() {
        return None;
    }
    only.path
        .as_ref()
        .filter(|path| !open_cwds.iter().any(|cwd| same_dir(cwd, path)))
        .cloned()
}

/// Index of the first worktree without an open session, or 0 ("New worktree")
/// when every existing worktree is already in use.
fn default_worktree_selection(entries: &[WorktreePickerEntry], open_cwds: &[String]) -> usize {
    entries
        .iter()
        .position(|entry| {
            !entry.is_new
                && entry
                    .path
                    .as_deref()
                    .is_some_and(|path| !open_cwds.iter().any(|cwd| same_dir(cwd, path)))
        })
        .unwrap_or(0)
}

const PROJECT_STEP_HINT_MACOS: &[(&str, &str)] =
    &[("enter", "open"), ("cmd+f", "favorite"), ("esc", "close")];
const PROJECT_STEP_HINT_OTHER: &[(&str, &str)] = &[
    ("enter", "open"),
    ("ctrl+shift+f", "favorite"),
    ("esc", "close"),
];
const WORKTREE_STEP_HINT: &[(&str, &str)] = &[("enter", "create session"), ("esc", "close")];
const BRANCH_STEP_HINT: &[(&str, &str)] = &[("enter", "create"), ("esc", "cancel")];

fn project_step_hint() -> &'static [(&'static str, &'static str)] {
    if cfg!(target_os = "macos") {
        PROJECT_STEP_HINT_MACOS
    } else {
        PROJECT_STEP_HINT_OTHER
    }
}

fn favorite_shortcut_pressed(ctx: &egui::Context) -> bool {
    ctx.input_mut(|input| {
        let Some(index) = input.events.iter().position(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::F,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if favorite_shortcut_matches(*modifiers)
            )
        }) else {
            return false;
        };
        input.events.remove(index);
        true
    })
}

fn favorite_shortcut_matches(modifiers: egui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        (modifiers.command || modifiers.mac_cmd)
            && !modifiers.shift
            && !modifiers.alt
            && !modifiers.ctrl
    } else {
        modifiers.ctrl && modifiers.shift && !modifiers.alt
    }
}

/// Compare two directory paths, tolerating symlinks and trailing slashes.
fn same_dir(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.trim_end_matches('/') == b.trim_end_matches('/'),
    }
}

fn project_rows(entries: &[ProjectPickerEntry]) -> Vec<ListRow> {
    entries
        .iter()
        .map(|entry| ListRow {
            icon: Some(if entry.favorite { "star" } else { "folder" }.to_owned()),
            primary: display_path(&entry.path),
            ..ListRow::default()
        })
        .collect()
}

fn worktree_rows(entries: &[WorktreePickerEntry], theme: Theme) -> Vec<ListRow> {
    entries
        .iter()
        .map(|entry| ListRow {
            icon: Some(if entry.is_new { "plus" } else { "git-branch" }.to_owned()),
            primary: entry.label.clone(),
            // The "create new" row stands out in the accent color.
            primary_tint: entry.is_new.then_some(theme.palette.accent),
            ..ListRow::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_row() -> WorktreePickerEntry {
        WorktreePickerEntry {
            label: "New worktree".to_owned(),
            path: None,
            is_new: true,
        }
    }

    fn worktree(path: &str) -> WorktreePickerEntry {
        WorktreePickerEntry {
            label: path.to_owned(),
            path: Some(path.to_owned()),
            is_new: false,
        }
    }

    #[test]
    fn repeated_single_worktree_routes_to_new_worktree_flow() {
        let entries = vec![new_row(), worktree("/repo")];
        assert_eq!(
            single_unused_worktree_cwd(&entries, &[]),
            Some("/repo".to_owned())
        );
        assert_eq!(
            single_unused_worktree_cwd(&entries, &["/repo".to_owned()]),
            None
        );
        assert_eq!(
            default_worktree_selection(&entries, &["/repo".to_owned()]),
            0
        );
    }

    #[test]
    fn defaults_to_first_worktree_without_an_open_session() {
        let entries = vec![new_row(), worktree("/repo/a"), worktree("/repo/b")];
        // /repo/a is in use, so the cursor lands on /repo/b at index 2.
        let selected = default_worktree_selection(&entries, &["/repo/a".to_owned()]);
        assert_eq!(selected, 2);
    }

    #[test]
    fn defaults_to_new_worktree_when_every_worktree_is_in_use() {
        let entries = vec![new_row(), worktree("/repo/a"), worktree("/repo/b")];
        // A trailing slash on a session cwd still counts as occupying the worktree.
        let open = vec!["/repo/a".to_owned(), "/repo/b/".to_owned()];
        assert_eq!(default_worktree_selection(&entries, &open), 0);
    }

    #[test]
    fn project_step_hint_shows_favorite_shortcut() {
        if cfg!(target_os = "macos") {
            assert!(project_step_hint().contains(&("cmd+f", "favorite")));
        } else {
            assert!(project_step_hint().contains(&("ctrl+shift+f", "favorite")));
        }
    }

    #[test]
    fn favorite_toggle_updates_project_rows_without_rediscovery() {
        let mut dialog = NewMuxSessionDialog::open();
        dialog.projects = vec![ProjectPickerEntry {
            path: "/repo".to_owned(),
            favorite: false,
        }];

        dialog.set_project_favorite("/repo", true);
        assert!(dialog.projects[0].favorite);

        dialog.set_project_favorite("/typed", true);
        assert!(
            dialog
                .projects
                .iter()
                .any(|project| project.path == "/typed" && project.favorite)
        );
    }

    #[test]
    fn favorite_shortcut_requires_exact_app_modifier_chord() {
        let mut modifiers = egui::Modifiers::default();
        if cfg!(target_os = "macos") {
            modifiers.command = true;
            assert!(favorite_shortcut_matches(modifiers));
            modifiers.alt = true;
            assert!(!favorite_shortcut_matches(modifiers));
            modifiers.alt = false;
            modifiers.shift = true;
            assert!(!favorite_shortcut_matches(modifiers));
        } else {
            modifiers.ctrl = true;
            modifiers.shift = true;
            assert!(favorite_shortcut_matches(modifiers));
            modifiers.alt = true;
            assert!(!favorite_shortcut_matches(modifiers));
            modifiers.alt = false;
            modifiers.command = true;
            assert!(favorite_shortcut_matches(modifiers));
        }
    }
}
