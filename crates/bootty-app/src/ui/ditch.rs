use bootty_ui::{Theme, ThemePalette};
use eframe::egui;

use crate::automation::directory::WorktreeRemovalConfirmation;
use crate::git::{self, WorktreeStatus};
use crate::strings::display_path;
use crate::ui::overlay::{self, ActionItem, ActionMenu, ActionRisk, FloatingWindow, StatusLine};

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
    pending_worktree_confirmation: Option<(DitchAction, WorktreeRemovalConfirmation)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DitchSessionEvent {
    None,
    Close,
    Ditch {
        session_id: String,
        cwd: Option<String>,
        action: DitchAction,
        confirmation: Option<Box<WorktreeRemovalConfirmation>>,
    },
}

impl DitchSessionDialog {
    pub fn open(session_id: String, cwd: Option<String>) -> Self {
        let status = cwd.as_deref().map(git::status).unwrap_or_default();
        let main = cwd.as_deref().and_then(git::main_worktree);
        let trunk = cwd.as_deref().and_then(git::trunk_branch);
        let multi_worktree = cwd.as_deref().map(git::worktree_count).unwrap_or(0) > 1;
        let actions = actions_for(&status, main.as_deref(), trunk.as_deref(), multi_worktree);
        Self {
            session_id,
            cwd,
            status,
            actions,
            selected: 0,
            pending_worktree_confirmation: None,
        }
    }

    /// Require a second explicit activation after the exact cross-session
    /// claimant set has been bound by the locked worktree-removal preflight.
    pub fn require_worktree_removal_confirmation(
        &mut self,
        action: DitchAction,
        confirmation: WorktreeRemovalConfirmation,
    ) {
        self.pending_worktree_confirmation = Some((action, confirmation));
        self.selected = 0;
    }

    fn ditch_event(&self, action: DitchAction) -> DitchSessionEvent {
        let confirmation = self.pending_worktree_confirmation.as_ref().and_then(
            |(confirmed_action, confirmation)| {
                (confirmed_action == &action).then(|| Box::new(confirmation.clone()))
            },
        );
        DitchSessionEvent::Ditch {
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            action,
            confirmation,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> DitchSessionEvent {
        let mut lines = status_lines(&self.status, self.cwd.as_deref(), theme.palette);
        let (actions, confirmation_pending) =
            if let Some((action, confirmation)) = &self.pending_worktree_confirmation {
                let sessions = confirmation
                    .conflicting_claims
                    .iter()
                    .map(|claim| claim.session.session_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(StatusLine {
                    label: "shared worktree".to_owned(),
                    value: format!("confirm removal despite active session(s): {sessions}"),
                    tint: Some(theme.palette.warning),
                });
                (vec![action.clone()], true)
            } else {
                (self.actions.clone(), false)
            };
        let items = action_items(&actions, confirmation_pending);

        let result = FloatingWindow::new("ditch-session-dialog", "Ditch Session")
            .icon("trash-2")
            .hint(if confirmation_pending {
                "Enter confirms removal shared with the listed sessions   Esc cancel"
            } else {
                "Enter confirm   Esc cancel"
            })
            .width(overlay::panel_width(ctx, 600.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let outcome = ActionMenu::new(&lines, &items, self.selected).show(ui, palette);
                self.selected = outcome.selected;
                outcome.activated
            });

        if let Some(index) = result.inner
            && let Some(action) = actions.get(index).cloned()
        {
            return self.ditch_event(action);
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

fn action_items(actions: &[DitchAction], confirmation_pending: bool) -> Vec<ActionItem> {
    if confirmation_pending {
        return actions
            .iter()
            .map(|_| ActionItem {
                icon: Some("alert-triangle".to_owned()),
                label: "Confirm shared-worktree removal".to_owned(),
                description: Some(
                    "Remove this worktree even though the listed sessions still claim it"
                        .to_owned(),
                ),
                risk: ActionRisk::Danger,
            })
            .collect();
    }
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
        value: cwd.map_or_else(|| "(unknown)".to_owned(), display_path),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::automation::directory::{RepositoryRef, WorktreeRef};

    fn linked(dirty: bool, branch: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            in_repo: true,
            is_linked_worktree: true,
            branch: branch.map(str::to_owned),
            dirty,
            ..WorktreeStatus::default()
        }
    }

    fn main_worktree(branch: Option<&str>) -> WorktreeStatus {
        WorktreeStatus {
            in_repo: true,
            is_linked_worktree: false,
            branch: branch.map(str::to_owned),
            ..WorktreeStatus::default()
        }
    }

    #[test]
    fn shared_worktree_removal_requires_a_second_explicit_confirmation() {
        let action = DitchAction::RemoveWorktree { force: true };
        let mut dialog = DitchSessionDialog::open("session-a".to_owned(), None);
        let worktree = WorktreeRef {
            repository: RepositoryRef {
                common_git_dir: PathBuf::from("/repo/.git"),
                root: Some(PathBuf::from("/repo")),
            },
            git_dir: PathBuf::from("/repo/.git/worktrees/feature"),
            path: PathBuf::from("/repo-feature"),
            branch: Some("feature".to_owned()),
            head: Some("deadbeef".to_owned()),
            created_by: None,
            managed_by_bootty: false,
        };
        let confirmation = WorktreeRemovalConfirmation {
            worktree,
            conflicting_claims: Vec::new(),
        };

        assert!(matches!(
            dialog.ditch_event(action.clone()),
            DitchSessionEvent::Ditch {
                confirmation: None,
                ..
            }
        ));
        dialog.require_worktree_removal_confirmation(action.clone(), confirmation.clone());
        assert!(matches!(
            dialog.ditch_event(DitchAction::KillOnly),
            DitchSessionEvent::Ditch {
                confirmation: None,
                ..
            }
        ));
        assert!(matches!(
            dialog.ditch_event(action),
            DitchSessionEvent::Ditch {
                confirmation: Some(replayed),
                ..
            } if *replayed == confirmation
        ));
    }

    #[test]
    fn single_worktree_repo_offers_only_kill() {
        // No sibling worktrees: detaching HEAD just to kill the session is
        // pointless, so the lone option stays a plain kill.
        assert_eq!(
            actions_for(&WorktreeStatus::default(), None, None, false),
            vec![DitchAction::KillOnly]
        );
        assert_eq!(
            actions_for(
                &main_worktree(Some("feature")),
                Some("/repo"),
                Some("main"),
                false
            ),
            vec![DitchAction::KillOnly]
        );
    }

    #[test]
    fn main_worktree_in_a_multi_worktree_repo_offers_detach_first() {
        // The screenshot case: session sits in the *main* worktree on a feature
        // branch, and the repo has other worktrees. Detach must be offered and
        // pre-selected, but the main tree can't be removed.
        let actions = actions_for(
            &main_worktree(Some("feature")),
            Some("/repo"),
            Some("main"),
            true,
        );
        assert_eq!(
            actions,
            vec![DitchAction::DetachWorktree, DitchAction::KillOnly]
        );
    }

    #[test]
    fn detached_head_does_not_offer_a_redundant_detach() {
        // Already detached: re-detaching is a no-op, so it must not appear.
        let actions = actions_for(&linked(false, None), Some("/repo"), Some("main"), true);
        assert!(!actions.contains(&DitchAction::DetachWorktree));
    }

    #[test]
    fn clean_linked_worktree_offers_non_forced_removal_and_branch_delete() {
        let actions = actions_for(
            &linked(false, Some("feature")),
            Some("/repo"),
            Some("main"),
            true,
        );
        assert_eq!(
            actions,
            vec![
                DitchAction::DetachWorktree,
                DitchAction::KillOnly,
                DitchAction::RemoveWorktree { force: false },
                DitchAction::RemoveWorktreeAndBranch {
                    force: true,
                    branch: "feature".to_owned(),
                    repo: "/repo".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn trunk_worktree_keeps_removal_but_never_offers_branch_delete() {
        // A linked worktree sitting on the trunk: removing the worktree is fine,
        // but deleting the trunk branch must never be offered.
        let actions = actions_for(
            &linked(false, Some("main")),
            Some("/repo"),
            Some("main"),
            true,
        );
        assert!(actions.contains(&DitchAction::RemoveWorktree { force: false }));
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, DitchAction::RemoveWorktreeAndBranch { .. })),
            "deleting the trunk branch must never be offered"
        );
    }

    #[test]
    fn branch_delete_is_withheld_without_a_resolved_main_worktree() {
        // A linked worktree whose main path could not be resolved must not offer
        // branch deletion — that cleanup would have no repo to run `git branch` in.
        let actions = actions_for(&linked(false, Some("feature")), None, Some("main"), true);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, DitchAction::RemoveWorktreeAndBranch { .. }))
        );
    }

    #[test]
    fn dirty_linked_worktree_forces_removal() {
        let actions = actions_for(
            &linked(true, Some("feature")),
            Some("/repo"),
            Some("main"),
            true,
        );
        assert!(actions.contains(&DitchAction::RemoveWorktree { force: true }));
    }
}
