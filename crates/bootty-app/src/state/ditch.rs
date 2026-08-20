use bootty_mux::project;

use crate::ui::ditch::DitchAction;

pub(super) enum DitchCleanupOutcome {
    Complete,
    NoAction(String),
    Partial { branch: String, error: String },
}

/// Run the git side of a ditch before the session is killed. The main worktree is
/// resolved up front because `cwd` stops resolving inside the repo once the linked
/// worktree is removed. Only a failure before destructive work keeps the session alive.
pub(super) fn run_ditch_cleanup(cwd: Option<&str>, action: &DitchAction) -> DitchCleanupOutcome {
    let Some(cwd) = cwd else {
        return DitchCleanupOutcome::Complete;
    };
    match action {
        DitchAction::KillOnly => DitchCleanupOutcome::Complete,
        DitchAction::DetachWorktree => project::detach_head(cwd)
            .map_or_else(DitchCleanupOutcome::NoAction, |_| {
                DitchCleanupOutcome::Complete
            }),
        DitchAction::RemoveWorktree { force } => project::remove_worktree(cwd, *force)
            .map_or_else(DitchCleanupOutcome::NoAction, |_| {
                DitchCleanupOutcome::Complete
            }),
        DitchAction::RemoveWorktreeAndBranch {
            force,
            branch,
            repo,
        } => {
            // Skip the worktree removal when its directory is already gone: a
            // prior attempt removed it but failed to delete the branch (e.g. it
            // was checked out elsewhere). Retrying the remove would error on a
            // missing path; instead finish by deleting the branch from `repo`,
            // resolved while the worktree still existed.
            let removed = if std::path::Path::new(cwd).exists() {
                if let Err(error) = project::remove_worktree(cwd, *force) {
                    return DitchCleanupOutcome::NoAction(error);
                }
                true
            } else {
                false
            };
            if let Err(error) = project::delete_branch(repo, branch, *force) {
                return if removed {
                    DitchCleanupOutcome::Partial {
                        branch: branch.clone(),
                        error,
                    }
                } else {
                    DitchCleanupOutcome::NoAction(error)
                };
            }
            DitchCleanupOutcome::Complete
        }
    }
}
