use bootty_config::config::SshRemoteConfig;
use bootty_extension::display_path;
use bootty_mux::{
    controller::RepaintHandle,
    project::{
        ProjectPickerEntry, WorktreePickerEntry, discover_project_picker_entries,
        discover_worktree_picker_entries, home_dir as project_home_dir,
        toggle_favorite_project_path,
    },
};
use bootty_ui::overlay::{FloatingWindow, ListRow, ListView};
use bootty_ui::{Theme, overlay};
use eframe::egui;

use crate::new_session::{RemoteEffect, RemoteNewSession, RemoteOutcome};
use crate::strings::home_dir;

mod model;

use model::{NewMuxSessionStep, filtered_worktree_entries, project_entries_for_filter};

pub use bootty_mux::controller::NewMuxSessionRequest;

pub struct NewMuxSessionDialog {
    step: NewMuxSessionStep,
    filter: String,
    selected: usize,
    projects: Vec<ProjectPickerEntry>,
    worktrees: Vec<WorktreePickerEntry>,
    selected_project: Option<ProjectPickerEntry>,
    focus_filter: bool,
    branch: String,
    remote: Option<RemoteNewSession>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewSessionPickerEvent {
    Close,
    Error(String),
    CreateWorktree { repo: String, branch: String },
    CreateSession { cwd: String },
}

impl NewMuxSessionDialog {
    pub fn open() -> Self {
        Self::new(
            discover_project_picker_entries(project_home_dir().as_deref()),
            None,
        )
    }

    pub fn open_remote(remote: SshRemoteConfig, repaint: RepaintHandle) -> Self {
        Self::new(Vec::new(), Some(RemoteNewSession::new(remote, repaint)))
    }

    fn new(projects: Vec<ProjectPickerEntry>, remote: Option<RemoteNewSession>) -> Self {
        Self {
            step: NewMuxSessionStep::Project,
            filter: String::new(),
            selected: 0,
            projects,
            worktrees: Vec::new(),
            selected_project: None,
            focus_filter: true,
            branch: String::new(),
            remote,
        }
    }

    /// `open_cwds` lists the working directories of sessions already open, so the
    /// worktree step can default away from worktrees that are already in use.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        open_cwds: &[String],
    ) -> Option<NewSessionPickerEvent> {
        if let Some(event) = self.poll_remote_task() {
            return Some(event);
        }
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
    ) -> Option<NewSessionPickerEvent> {
        let entries = project_entries_for_filter(&self.projects, &self.filter);
        self.selected = overlay::clamp_selection(self.selected, entries.len());
        let busy = self.remote_busy();
        let favorite = (!busy && favorite_shortcut_pressed(ctx))
            .then(|| entries.get(self.selected).cloned())
            .flatten();
        let rows = project_rows(&entries, self.remote.is_some());

        let empty_text = if busy {
            "loading remote projects..."
        } else {
            "no matching directories"
        };
        let result = self
            .frame(ctx, "Directory", "folder", PROJECT_STEP_HINT)
            .show(ctx, theme, |ui, palette| {
                Self::body(
                    ui,
                    palette,
                    theme,
                    (&mut self.filter, &mut self.focus_filter, &mut self.selected),
                    "filter directories...",
                    &rows,
                    empty_text,
                )
            });

        if let Some(entry) = favorite {
            return self.toggle_project_favorite(entry);
        }

        if !busy
            && let Some(index) = result.inner.activated
            && let Some(entry) = entries.get(index).cloned()
        {
            return self.activate_project(entry, open_cwds);
        }
        self.close_if_dismissed(&result)
    }

    fn show_worktree_step(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
    ) -> Option<NewSessionPickerEvent> {
        let entries = filtered_worktree_entries(&self.worktrees, &self.filter);
        self.selected = overlay::clamp_selection(self.selected, entries.len());
        let rows = worktree_rows(&entries, theme);

        let empty_text = if self.remote_busy() {
            "loading remote worktrees..."
        } else {
            "no matching worktrees"
        };
        let result = self
            .frame(ctx, "Worktree", "git-branch", WORKTREE_STEP_HINT)
            .show(ctx, theme, |ui, palette| {
                Self::body(
                    ui,
                    palette,
                    theme,
                    (&mut self.filter, &mut self.focus_filter, &mut self.selected),
                    "filter worktrees...",
                    &rows,
                    empty_text,
                )
            });

        if !self.remote_busy()
            && let Some(index) = result.inner.activated
            && let Some(entry) = entries.get(index)
        {
            return self.activate_worktree((*entry).clone());
        }
        self.close_if_dismissed(&result)
    }

    fn show_branch_step(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
    ) -> Option<NewSessionPickerEvent> {
        let Some(repo) = self
            .selected_project
            .as_ref()
            .map(|project| project.path.clone())
        else {
            return Some(NewSessionPickerEvent::Close);
        };
        let caption = format!(
            "new branch in {}",
            picker_display_path(&repo, self.remote.is_some())
        );
        let branch = self.branch.trim().to_owned();

        let result = self
            .frame(ctx, "New Worktree", "git-branch", BRANCH_STEP_HINT)
            .show(ctx, theme, |ui, _palette| {
                overlay::TextPrompt::new("new-worktree-branch")
                    .caption(&caption)
                    .hint("branch name...")
                    .submit_disabled(branch.is_empty() || self.remote_busy())
                    .show(ui, theme, &mut self.branch, &mut self.focus_filter)
            });

        if result.inner.submitted && !branch.is_empty() && !self.remote_busy() {
            if let Some(remote) = &mut self.remote {
                remote.start(RemoteEffect::CreateWorktree(repo, branch));
                return None;
            }
            return Some(NewSessionPickerEvent::CreateWorktree { repo, branch });
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
        ui: &mut egui::Ui,
        palette: bootty_ui::ThemePalette,
        theme: Theme,
        state: (&mut String, &mut bool, &mut usize),
        hint: &str,
        rows: &[ListRow],
        empty_text: &str,
    ) -> overlay::ListOutcome {
        let (filter_text, focus_filter, selected) = state;
        let filter = overlay::filter_field(
            ui,
            egui::Id::new("new-session-picker-filter"),
            filter_text,
            theme,
            hint,
        );
        if *focus_filter {
            filter.request_focus();
            *focus_filter = false;
        }
        ui.add_space(8.0);
        let outcome = ListView::new("new-session-picker-list", rows, *selected)
            .max_height(overlay::list_max_height(ui.ctx(), 150.0, 520.0))
            .empty_text(empty_text)
            .show(ui, palette);
        *selected = outcome.selected;
        outcome
    }

    fn close_if_dismissed<R>(
        &self,
        result: &overlay::OverlayResult<R>,
    ) -> Option<NewSessionPickerEvent> {
        if result.escaped || result.clicked_outside {
            Some(NewSessionPickerEvent::Close)
        } else {
            None
        }
    }

    fn toggle_project_favorite(
        &mut self,
        project: ProjectPickerEntry,
    ) -> Option<NewSessionPickerEvent> {
        if let Some(remote) = &mut self.remote {
            remote.start(RemoteEffect::ToggleFavorite(project.path));
            return None;
        }
        match toggle_favorite_project_path(project_home_dir().as_deref(), &project.path) {
            Ok(favorite) => {
                self.set_project_favorite(&project.path, favorite);
                None
            }
            Err(error) => Some(NewSessionPickerEvent::Error(format!(
                "favorite {}: {error}",
                picker_display_path(&project.path, false)
            ))),
        }
    }

    fn set_project_favorite(&mut self, path: &str, favorite: bool) {
        if let Some(project) = self
            .projects
            .iter_mut()
            .find(|project| same_dir(&project.path, path, self.remote.is_some()))
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
    ) -> Option<NewSessionPickerEvent> {
        if let Some(remote) = &mut self.remote {
            let path = project.path.clone();
            self.selected = 0;
            self.step = NewMuxSessionStep::Worktree;
            self.filter.clear();
            self.focus_filter = true;
            self.worktrees.clear();
            self.selected_project = Some(project);
            remote.start(RemoteEffect::ListWorktrees(path, open_cwds.to_vec()));
            return None;
        }
        let worktrees = discover_worktree_picker_entries(&project.path);
        if let Some(cwd) = single_unused_worktree_cwd(&worktrees, open_cwds, false) {
            return Some(NewSessionPickerEvent::CreateSession { cwd });
        }

        self.selected = default_worktree_selection(&worktrees, open_cwds, false);
        self.step = NewMuxSessionStep::Worktree;
        self.filter.clear();
        self.focus_filter = true;
        self.worktrees = worktrees;
        self.selected_project = Some(project);
        None
    }

    /// Selecting the "New worktree" row advances to the branch-name prompt;
    /// an existing worktree creates a session directly.
    fn activate_worktree(&mut self, entry: WorktreePickerEntry) -> Option<NewSessionPickerEvent> {
        if entry.is_new {
            self.step = NewMuxSessionStep::BranchName;
            self.branch.clear();
            self.focus_filter = true;
            None
        } else if let Some(cwd) = entry.path {
            Some(NewSessionPickerEvent::CreateSession { cwd })
        } else {
            Some(NewSessionPickerEvent::Close)
        }
    }

    fn remote_busy(&self) -> bool {
        self.remote.as_ref().is_some_and(RemoteNewSession::is_busy)
    }

    fn poll_remote_task(&mut self) -> Option<NewSessionPickerEvent> {
        let result = self.remote.as_mut()?.poll()?;
        match result {
            Ok(RemoteOutcome::Projects(projects)) => {
                self.projects = projects;
                self.selected = 0;
                None
            }
            Ok(RemoteOutcome::Worktrees(worktrees)) => {
                if let Some(cwd) = single_unused_worktree_cwd(&worktrees, &[], true) {
                    return Some(NewSessionPickerEvent::CreateSession { cwd });
                }
                self.selected = default_worktree_selection(&worktrees, &[], true);
                self.worktrees = worktrees;
                None
            }
            Ok(RemoteOutcome::Favorite { path, favorite }) => {
                self.set_project_favorite(&path, favorite);
                None
            }
            Ok(RemoteOutcome::CreatedWorktree(cwd)) => {
                Some(NewSessionPickerEvent::CreateSession { cwd })
            }
            Err(error) => Some(NewSessionPickerEvent::Error(error)),
        }
    }
}

fn single_unused_worktree_cwd(
    entries: &[WorktreePickerEntry],
    open_cwds: &[String],
    remote: bool,
) -> Option<String> {
    let mut real = entries.iter().filter(|entry| !entry.is_new);
    let only = real.next()?;
    if real.next().is_some() || worktree_is_open(only, open_cwds, remote) {
        return None;
    }
    only.path.clone()
}

/// Index of the first worktree without an open session, or 0 ("New worktree")
/// when every existing worktree is already in use.
fn default_worktree_selection(
    entries: &[WorktreePickerEntry],
    open_cwds: &[String],
    remote: bool,
) -> usize {
    entries
        .iter()
        .position(|entry| !entry.is_new && !worktree_is_open(entry, open_cwds, remote))
        .unwrap_or(0)
}

fn worktree_is_open(entry: &WorktreePickerEntry, open_cwds: &[String], remote: bool) -> bool {
    if remote {
        return entry.occupied;
    }
    entry
        .path
        .as_deref()
        .is_some_and(|path| open_cwds.iter().any(|cwd| same_dir(cwd, path, false)))
}

const FAVORITE_SHORTCUT: &str = if cfg!(target_os = "macos") {
    "cmd+f"
} else {
    "ctrl+shift+f"
};
const PROJECT_STEP_HINT: &[(&str, &str)] = &[
    ("enter", "open"),
    (FAVORITE_SHORTCUT, "favorite"),
    ("esc", "close"),
];
const WORKTREE_STEP_HINT: &[(&str, &str)] = &[("enter", "create session"), ("esc", "close")];
const BRANCH_STEP_HINT: &[(&str, &str)] = &[("enter", "create"), ("esc", "cancel")];

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

/// Compare local directories through the filesystem. Compare remote paths as opaque target paths.
///
/// This is the one filesystem touch left in a view. It runs on a step transition, not per frame,
/// and only compares two paths. Move the canonicalization to the owner if it ever runs per frame
/// or over a list long enough to matter.
fn same_dir(a: &str, b: &str, remote: bool) -> bool {
    if remote {
        return a.trim_end_matches(['/', '\\']) == b.trim_end_matches(['/', '\\']);
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.trim_end_matches('/') == b.trim_end_matches('/'),
    }
}

fn picker_display_path(path: &str, remote: bool) -> String {
    if remote {
        path.to_owned()
    } else {
        display_path(path, home_dir().as_deref())
    }
}

fn project_rows(entries: &[ProjectPickerEntry], remote: bool) -> Vec<ListRow> {
    entries
        .iter()
        .map(|entry| ListRow {
            icon: Some(if entry.favorite { "star" } else { "folder" }.to_owned()),
            primary: picker_display_path(&entry.path, remote),
            ..ListRow::default()
        })
        .collect()
}

fn worktree_rows(entries: &[&WorktreePickerEntry], theme: Theme) -> Vec<ListRow> {
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
