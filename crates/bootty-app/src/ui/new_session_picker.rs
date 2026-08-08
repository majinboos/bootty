use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

use bootty_mux::process::{CancellableCommandRunner, CommandCancellation};
use bootty_ui::Theme;
use eframe::egui;

use crate::{
    config::SshRemoteConfig,
    mux::RepaintHandle,
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

pub struct NewMuxSessionDialog {
    step: NewMuxSessionStep,
    filter: String,
    selected: usize,
    projects: Vec<ProjectPickerEntry>,
    worktrees: Vec<WorktreePickerEntry>,
    selected_project: Option<ProjectPickerEntry>,
    focus_filter: bool,
    branch: String,
    remote: Option<SshRemoteConfig>,
    remote_task: Option<RemoteTask>,
    repaint: Option<RepaintHandle>,
}

struct RemoteTask {
    receiver: Receiver<Result<RemotePickerResult, String>>,
    cancellation: CommandCancellation,
}

impl Drop for RemoteTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

static REMOTE_PICKER_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct RemoteWorkerPermit;

impl RemoteWorkerPermit {
    fn acquire() -> Option<Self> {
        REMOTE_PICKER_WORKER_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for RemoteWorkerPermit {
    fn drop(&mut self) {
        REMOTE_PICKER_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}
enum RemotePickerResult {
    Projects(Vec<ProjectPickerEntry>),
    Worktrees(Vec<WorktreePickerEntry>),
    Favorite { path: String, favorite: bool },
    CreatedWorktree(String),
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
            remote: None,
            remote_task: None,
            repaint: None,
        }
    }

    pub fn open_remote(remote: SshRemoteConfig, repaint: RepaintHandle) -> Self {
        let mut dialog = Self {
            step: NewMuxSessionStep::Project,
            filter: String::new(),
            selected: 0,
            projects: Vec::new(),
            worktrees: Vec::new(),
            selected_project: None,
            focus_filter: true,
            branch: String::new(),
            remote: Some(remote),
            remote_task: None,
            repaint: Some(repaint),
        };
        dialog.start_remote_task(|remote, runner| {
            crate::remote_catalog::list_remote_projects_with_runner(&remote, runner)
                .map(RemotePickerResult::Projects)
                .map_err(|error| error.to_string())
        });
        dialog
    }

    /// `open_cwds` lists the working directories of sessions already open, so the
    /// worktree step can default away from worktrees that are already in use.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        open_cwds: &[String],
    ) -> NewSessionPickerEvent {
        if let Some(event) = self.poll_remote_task() {
            return event;
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
    ) -> NewSessionPickerEvent {
        let entries = project_entries_for_filter(&self.projects, &self.filter);
        self.selected = list::clamp_selection(self.selected, entries.len());
        let busy = self.remote_task.is_some();
        let favorite = (!busy && favorite_shortcut_pressed(ctx))
            .then(|| entries.get(self.selected).cloned())
            .flatten();
        let rows = project_rows(&entries, self.remote.is_some());

        let empty_text = if self.remote_task.is_some() {
            "loading remote projects..."
        } else {
            "no matching directories"
        };
        let result = self
            .frame(ctx, "Directory", "folder", project_step_hint())
            .show(ctx, theme, |ui, palette| {
                self.body(
                    ui,
                    palette,
                    theme,
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

    fn show_worktree_step(&mut self, ctx: &egui::Context, theme: Theme) -> NewSessionPickerEvent {
        let entries = filtered_worktree_entries(&self.worktrees, &self.filter);
        self.selected = list::clamp_selection(self.selected, entries.len());
        let rows = worktree_rows(&entries, theme);

        let empty_text = if self.remote_task.is_some() {
            "loading remote worktrees..."
        } else {
            "no matching worktrees"
        };
        let result = self
            .frame(ctx, "Worktree", "git-branch", WORKTREE_STEP_HINT)
            .show(ctx, theme, |ui, palette| {
                self.body(ui, palette, theme, "filter worktrees...", &rows, empty_text)
            });

        if self.remote_task.is_none()
            && let Some(index) = result.inner.activated
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
                    .submit_disabled(branch.is_empty() || self.remote_task.is_some())
                    .show(ui, theme, &mut self.branch, &mut self.focus_filter)
            });

        if result.inner.submitted && !branch.is_empty() && self.remote_task.is_none() {
            if self.remote.is_some() {
                let project = repo.clone();
                self.start_remote_task(move |remote, runner| {
                    crate::remote_catalog::create_remote_worktree_with_runner(
                        &remote, &project, &branch, runner,
                    )
                    .map(RemotePickerResult::CreatedWorktree)
                    .map_err(|error| error.to_string())
                });
                return NewSessionPickerEvent::None;
            }
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
        if self.remote.is_some() {
            let path = project.path;
            let task_path = path.clone();
            self.start_remote_task(move |remote, runner| {
                crate::remote_catalog::toggle_remote_project_favorite_with_runner(
                    &remote, &task_path, runner,
                )
                .map(|favorite| RemotePickerResult::Favorite { path, favorite })
                .map_err(|error| error.to_string())
            });
            return NewSessionPickerEvent::None;
        }
        match toggle_favorite_project_path(&project.path) {
            Ok(favorite) => {
                self.set_project_favorite(&project.path, favorite);
                NewSessionPickerEvent::None
            }
            Err(error) => NewSessionPickerEvent::Error(format!(
                "favorite {}: {error}",
                picker_display_path(&project.path, false)
            )),
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
    ) -> NewSessionPickerEvent {
        if self.remote.is_some() {
            let path = project.path.clone();
            self.selected = 0;
            self.step = NewMuxSessionStep::Worktree;
            self.filter.clear();
            self.focus_filter = true;
            self.worktrees.clear();
            self.selected_project = Some(project);
            let open_cwds = open_cwds.to_vec();
            self.start_remote_task(move |remote, runner| {
                crate::remote_catalog::list_remote_worktrees_with_runner(
                    &remote, &path, &open_cwds, runner,
                )
                .map(RemotePickerResult::Worktrees)
                .map_err(|error| error.to_string())
            });
            return NewSessionPickerEvent::None;
        }
        let worktrees = discover_worktree_picker_entries(&project.path);
        if let Some(cwd) = single_unused_worktree_cwd(&worktrees, open_cwds, false) {
            return NewSessionPickerEvent::CreateSession { cwd };
        }

        self.selected = default_worktree_selection(&worktrees, open_cwds, false);
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

    fn start_remote_task<T>(&mut self, task: T)
    where
        T: FnOnce(SshRemoteConfig, &CancellableCommandRunner) -> Result<RemotePickerResult, String>
            + Send
            + 'static,
    {
        let (Some(remote), Some(repaint)) = (self.remote.clone(), self.repaint.clone()) else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::new(cancellation.clone());
        let Some(permit) = RemoteWorkerPermit::acquire() else {
            let _ = sender.send(Err(
                "the previous remote project operation is still stopping".to_owned(),
            ));
            repaint();
            self.remote_task = Some(RemoteTask {
                receiver,
                cancellation,
            });
            return;
        };
        std::thread::spawn(move || {
            let _permit = permit;
            let _ = sender.send(task(remote, &runner));
            repaint();
        });
        self.remote_task = Some(RemoteTask {
            receiver,
            cancellation,
        });
    }

    fn poll_remote_task(&mut self) -> Option<NewSessionPickerEvent> {
        let result = match self.remote_task.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                self.remote_task = None;
                return Some(NewSessionPickerEvent::Error(
                    "remote project task stopped".to_owned(),
                ));
            }
        };
        self.remote_task = None;
        match result {
            Ok(RemotePickerResult::Projects(projects)) => {
                self.projects = projects;
                self.selected = 0;
                None
            }
            Ok(RemotePickerResult::Worktrees(worktrees)) => {
                if let Some(cwd) = single_unused_worktree_cwd(&worktrees, &[], true) {
                    return Some(NewSessionPickerEvent::CreateSession { cwd });
                }
                self.selected = default_worktree_selection(&worktrees, &[], true);
                self.worktrees = worktrees;
                None
            }
            Ok(RemotePickerResult::Favorite { path, favorite }) => {
                self.set_project_favorite(&path, favorite);
                None
            }
            Ok(RemotePickerResult::CreatedWorktree(cwd)) => {
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

/// Compare local directories through the filesystem. Compare remote paths as opaque target paths.
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
        display_path(path)
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
            occupied: false,
        }
    }

    fn worktree(path: &str) -> WorktreePickerEntry {
        WorktreePickerEntry {
            label: path.to_owned(),
            path: Some(path.to_owned()),
            is_new: false,
            occupied: false,
        }
    }

    fn occupied_worktree(path: &str) -> WorktreePickerEntry {
        WorktreePickerEntry {
            occupied: true,
            ..worktree(path)
        }
    }
    fn remote_task(receiver: Receiver<Result<RemotePickerResult, String>>) -> RemoteTask {
        RemoteTask {
            receiver,
            cancellation: CommandCancellation::default(),
        }
    }

    #[test]
    fn remote_picker_allows_only_one_worker() {
        let permit = RemoteWorkerPermit::acquire().expect("first worker");
        assert!(RemoteWorkerPermit::acquire().is_none());
        drop(permit);
        assert!(RemoteWorkerPermit::acquire().is_some());
    }

    #[test]
    fn repeated_single_worktree_routes_to_new_worktree_flow() {
        let entries = vec![new_row(), worktree("/repo")];
        assert_eq!(
            single_unused_worktree_cwd(&entries, &[], false),
            Some("/repo".to_owned())
        );
        assert_eq!(
            single_unused_worktree_cwd(&entries, &["/repo".to_owned()], false),
            None
        );
        assert_eq!(
            default_worktree_selection(&entries, &["/repo".to_owned()], false),
            0
        );
    }

    #[test]
    fn defaults_to_first_worktree_without_an_open_session() {
        let entries = vec![new_row(), worktree("/repo/a"), worktree("/repo/b")];
        // /repo/a is in use, so the cursor lands on /repo/b at index 2.
        let selected = default_worktree_selection(&entries, &["/repo/a".to_owned()], false);
        assert_eq!(selected, 2);
    }

    #[test]
    fn defaults_to_new_worktree_when_every_worktree_is_in_use() {
        let entries = vec![new_row(), worktree("/repo/a"), worktree("/repo/b")];
        // A trailing slash on a session cwd still counts as occupying the worktree.
        let open = vec!["/repo/a".to_owned(), "/repo/b/".to_owned()];
        assert_eq!(default_worktree_selection(&entries, &open, false), 0);
    }

    #[test]
    fn remote_occupancy_uses_the_daemon_marker() {
        let entries = vec![new_row(), occupied_worktree(r"C:\repo")];

        assert_eq!(single_unused_worktree_cwd(&entries, &[], true), None);
        assert_eq!(default_worktree_selection(&entries, &[], true), 0);
    }

    #[test]
    fn remote_worktree_paths_never_use_the_client_filesystem() {
        assert!(same_dir(
            r"C:\Users\developer\project\\",
            r"C:\Users\developer\project",
            true
        ));
    }

    #[test]
    fn remote_catalog_results_populate_the_project_picker() {
        let mut dialog = NewMuxSessionDialog::open();
        dialog.projects.clear();
        dialog.remote = Some(SshRemoteConfig::for_host("devbox"));
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(RemotePickerResult::Projects(vec![ProjectPickerEntry {
                path: "/srv/projects/bootty".to_owned(),
                favorite: false,
            }])))
            .expect("remote projects");
        dialog.remote_task = Some(remote_task(receiver));

        assert_eq!(dialog.poll_remote_task(), None);
        assert_eq!(dialog.projects[0].path, "/srv/projects/bootty");
    }

    #[test]
    fn remote_worktree_results_create_a_session_with_the_remote_path() {
        let mut dialog = NewMuxSessionDialog::open();
        dialog.remote = Some(SshRemoteConfig::for_host("devbox"));
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(RemotePickerResult::Worktrees(vec![worktree(
                r"C:\Users\developer\bootty",
            )])))
            .expect("remote worktrees");
        dialog.remote_task = Some(remote_task(receiver));

        assert_eq!(
            dialog.poll_remote_task(),
            Some(NewSessionPickerEvent::CreateSession {
                cwd: r"C:\Users\developer\bootty".to_owned(),
            })
        );
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
