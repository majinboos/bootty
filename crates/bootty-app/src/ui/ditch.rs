use bootty_extension::display_path;
use bootty_mux::project::{self, WorktreeStatus};
use bootty_ui::overlay::{ActionItem, ActionMenu, ActionRisk, FloatingWindow, StatusLine};
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
    None,
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

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> DitchSessionEvent {
        let lines = status_lines(&self.status, self.cwd.as_deref(), theme.palette);
        let items = action_items(&self.actions);

        let result = FloatingWindow::new("ditch-session-dialog", "Ditch Session")
            .icon("trash-2")
            .hint("Enter confirm   Esc cancel")
            .width(overlay::panel_width(ctx, 600.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let outcome = ActionMenu::new(&lines, &items, self.selected).show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(action) = self.actions.get(index).cloned()
        {
            return DitchSessionEvent::Ditch {
                session_id: self.session_id.clone(),
                cwd: self.cwd.clone(),
                action,
            };
        }
        if result.escaped || result.clicked_outside {
            return DitchSessionEvent::Close;
        }
        DitchSessionEvent::None
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

fn action_items(actions: &[DitchAction]) -> Vec<ActionItem> {
    actions
        .iter()
        .map(|action| match action {
            DitchAction::DetachWorktree => ActionItem {
                icon: Some("unlink".to_owned()),
                label: "Detach worktree".to_owned(),
                description: Some(
                    "Detach HEAD to free the branch; keep the worktree, branch, and commits"
                        .to_owned(),
                ),
                risk: ActionRisk::Safe,
            },
            DitchAction::KillOnly => ActionItem {
                icon: Some("x".to_owned()),
                label: "Kill session".to_owned(),
                description: Some("Close the session; keep the worktree and branch".to_owned()),
                risk: ActionRisk::Safe,
            },
            DitchAction::RemoveWorktree { force } => ActionItem {
                icon: Some("trash-2".to_owned()),
                label: "Kill + remove worktree".to_owned(),
                description: Some(if *force {
                    "Discard uncommitted changes and remove the linked worktree".to_owned()
                } else {
                    "Remove the linked worktree".to_owned()
                }),
                risk: if *force {
                    ActionRisk::Danger
                } else {
                    ActionRisk::Caution
                },
            },
            DitchAction::RemoveWorktreeAndBranch { branch, .. } => ActionItem {
                icon: Some("trash-2".to_owned()),
                label: "Kill + remove worktree + delete branch".to_owned(),
                // This force-removes the worktree, so warn about both losses:
                // working-tree edits and any unmerged commits on the branch.
                description: Some(format!(
                    "Remove the worktree and delete branch '{branch}' (uncommitted changes and unmerged commits are lost)"
                )),
                risk: ActionRisk::Danger,
            },
        })
        .collect()
}

fn status_lines(
    status: &WorktreeStatus,
    cwd: Option<&str>,
    palette: ThemePalette,
) -> Vec<StatusLine> {
    let mut lines = vec![StatusLine {
        label: "path".to_owned(),
        value: cwd.map_or_else(
            || "(unknown)".to_owned(),
            |cwd| display_path(cwd, home_dir().as_deref()),
        ),
        tint: None,
    }];
    if !status.in_repo {
        lines.push(StatusLine {
            label: "git".to_owned(),
            value: "not a git repository".to_owned(),
            tint: Some(palette.muted),
        });
        return lines;
    }
    lines.push(StatusLine {
        label: "branch".to_owned(),
        value: status
            .branch
            .clone()
            .unwrap_or_else(|| "detached".to_owned()),
        tint: None,
    });
    lines.push(StatusLine {
        label: "worktree".to_owned(),
        value: if status.is_linked_worktree {
            "linked".to_owned()
        } else {
            "main".to_owned()
        },
        tint: Some(if status.is_linked_worktree {
            palette.accent
        } else {
            palette.muted
        }),
    });
    lines.push(StatusLine {
        label: "changes".to_owned(),
        value: if status.dirty {
            "uncommitted changes".to_owned()
        } else {
            "clean".to_owned()
        },
        tint: Some(if status.dirty {
            palette.warning
        } else {
            palette.success
        }),
    });
    if status.has_upstream {
        lines.push(StatusLine {
            label: "unpushed".to_owned(),
            value: if status.unpushed > 0 {
                format!("{} commit(s)", status.unpushed)
            } else {
                "up to date".to_owned()
            },
            tint: Some(if status.unpushed > 0 {
                palette.warning
            } else {
                palette.success
            }),
        });
    }
    lines
}
