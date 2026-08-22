use bootty_extension::display_path;
use bootty_mux::project::{self, WorktreeStatus};
use bootty_ui::overlay::{FloatingWindow, ListRow, ListView};
use bootty_ui::{Theme, ThemePalette, overlay};
use eframe::egui;

use crate::strings::home_dir;

/// A cleanup action chosen in the ditch window, executed by the app layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchAction {
    /// Close the session after detaching HEAD in the worktree, freeing its
    /// branch while keeping the worktree, branch, and every commit.
    DetachWorktree,
    /// Close the session, leaving the worktree and branch untouched.
    KillOnly,
    /// Close the session and remove its linked worktree (`force` discards dirty state).
    RemoveWorktree { force: bool },
    /// Close the session, remove the worktree, and delete its branch. `repo` is
    /// the main worktree resolved up front, so branch deletion still works on a
    /// retry after the linked worktree (and its cwd) is already gone.
    RemoveWorktreeAndBranch {
        force: bool,
        branch: String,
        repo: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DitchSessionDialog {
    session_id: String,
    cwd: Option<String>,
    status: WorktreeStatus,
    actions: Vec<DitchAction>,
    selected: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchSessionEvent {
    Close,
    Ditch {
        session_id: String,
        cwd: Option<String>,
        action: DitchAction,
    },
}

impl DitchSessionDialog {
    pub fn open(session_id: String, cwd: Option<String>) -> Self {
        let status = cwd.as_deref().map(project::status).unwrap_or_default();
        let main = cwd.as_deref().and_then(project::main_worktree);
        let trunk = cwd.as_deref().and_then(project::trunk_branch);
        let multi_worktree = cwd.as_deref().map(project::worktree_count).unwrap_or(0) > 1;
        let actions = actions_for(&status, main.as_deref(), trunk.as_deref(), multi_worktree);
        Self {
            session_id,
            cwd,
            status,
            actions,
            selected: 0,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<DitchSessionEvent> {
        let rows = action_rows(&self.actions, theme.palette);

        let result = FloatingWindow::new("ditch-session-dialog", "Ditch Session")
            .icon("trash-2")
            .hint("Enter confirm   Esc cancel")
            .width(overlay::panel_width(ctx, 600.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                show_status(ui, &self.status, self.cwd.as_deref(), palette);
                let outcome = ListView::new("ditch-session-actions", &rows, self.selected)
                    .row_height(46.0)
                    // The safe action leads on purpose; the pointer passing over a destructive row
                    // must not make it what Enter does.
                    .hover_selects(false)
                    .show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(action) = self.actions.get(index).cloned()
        {
            return Some(DitchSessionEvent::Ditch {
                session_id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                action,
            });
        }
        if result.escaped || result.clicked_outside {
            return Some(DitchSessionEvent::Close);
        }
        None
    }
}

/// Offer only the cleanup actions that are safe and applicable. In a repo with
/// more than one worktree, detaching HEAD leads as the pre-selected default —
/// it frees the current branch for use elsewhere while keeping the worktree,
/// branch, and commits, so it suits the multi-worktree workflow whether the
/// session sits in the main or a linked worktree. Worktree/branch removal is
/// offered only inside a linked worktree (the main tree can't be removed).
fn actions_for(
    status: &WorktreeStatus,
    main: Option<&str>,
    trunk: Option<&str>,
    multi_worktree: bool,
) -> Vec<DitchAction> {
    let mut actions = Vec::new();
    // Detaching only does something when HEAD is actually on a branch.
    if multi_worktree && status.branch.is_some() {
        actions.push(DitchAction::DetachWorktree);
    }
    actions.push(DitchAction::KillOnly);
    if status.is_linked_worktree {
        actions.push(DitchAction::RemoveWorktree {
            force: status.dirty,
        });
        // Branch deletion needs the main worktree path; without it, offer only
        // the worktree removal so we never queue an un-runnable cleanup. Never
        // offer to delete the trunk — that branch outlives any single worktree.
        if let (Some(branch), Some(repo)) = (&status.branch, main)
            && trunk != Some(branch.as_str())
        {
            actions.push(DitchAction::RemoveWorktreeAndBranch {
                force: true,
                branch: branch.clone(),
                repo: repo.to_owned(),
            });
        }
    }
    actions
}

fn action_rows(actions: &[DitchAction], palette: ThemePalette) -> Vec<ListRow> {
    actions
        .iter()
        .map(|action| {
            let (icon, primary, secondary, tint) = match action {
                DitchAction::DetachWorktree => (
                    "unlink",
                    "Detach worktree",
                    "Detach HEAD to free the branch; keep the worktree, branch, and commits"
                        .to_owned(),
                    palette.success,
                ),
                DitchAction::KillOnly => (
                    "x",
                    "Kill session",
                    "Close the session; keep the worktree and branch".to_owned(),
                    palette.success,
                ),
                DitchAction::RemoveWorktree { force } => (
                    "trash-2",
                    "Kill + remove worktree",
                    if *force {
                        "Discard uncommitted changes and remove the linked worktree".to_owned()
                    } else {
                        "Remove the linked worktree".to_owned()
                    },
                    if *force {
                        palette.destructive
                    } else {
                        palette.warning
                    },
                ),
                DitchAction::RemoveWorktreeAndBranch { branch, .. } => (
                    "trash-2",
                    "Kill + remove worktree + delete branch",
                    // This force-removes the worktree, so warn about both losses:
                    // working-tree edits and any unmerged commits on the branch.
                    format!(
                        "Remove the worktree and delete branch '{branch}' (uncommitted changes and unmerged commits are lost)"
                    ),
                    palette.destructive,
                ),
            };
            ListRow {
                icon: Some(icon.to_owned()),
                icon_tint: Some(tint),
                primary: primary.to_owned(),
                primary_tint: Some(tint),
                secondary: Some(secondary),
                secondary_tint: Some(palette.muted),
                selection_tint: Some(tint),
                ..ListRow::default()
            }
        })
        .collect()
}

fn show_status(
    ui: &mut egui::Ui,
    status: &WorktreeStatus,
    cwd: Option<&str>,
    palette: ThemePalette,
) {
    let line = |ui: &mut egui::Ui, label: &str, value: &str, tint: Option<egui::Color32>| {
        ui.horizontal(|ui| {
            ui.monospace(
                egui::RichText::new(format!("{label}:"))
                    .size(12.0)
                    .color(palette.muted),
            );
            ui.monospace(
                egui::RichText::new(value)
                    .size(12.0)
                    .color(tint.unwrap_or(palette.text)),
            );
        });
    };
    let path = cwd.map_or_else(
        || "(unknown)".to_owned(),
        |cwd| display_path(cwd, home_dir().as_deref()),
    );
    line(ui, "path", &path, None);
    if !status.in_repo {
        line(ui, "git", "not a git repository", Some(palette.muted));
    } else {
        line(
            ui,
            "branch",
            status.branch.as_deref().unwrap_or("detached"),
            None,
        );
        let (worktree, worktree_tint) = if status.is_linked_worktree {
            ("linked", palette.accent)
        } else {
            ("main", palette.muted)
        };
        line(ui, "worktree", worktree, Some(worktree_tint));
        let (changes, change_tint) = if status.dirty {
            ("uncommitted changes", palette.warning)
        } else {
            ("clean", palette.success)
        };
        line(ui, "changes", changes, Some(change_tint));
        if status.has_upstream {
            let (unpushed, tint) = if status.unpushed > 0 {
                (format!("{} commit(s)", status.unpushed), palette.warning)
            } else {
                ("up to date".to_owned(), palette.success)
            };
            line(ui, "unpushed", &unpushed, Some(tint));
        }
    }
    ui.add_space(8.0);
}
