//! Thin wrappers over the `git` CLI for worktree-aware session cleanup
//! ("ditching"). Shelling out keeps us consistent with the tmux backend and the
//! dotfiles `mux` tool rather than taking on a libgit dependency.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

use serde::{Deserialize, Serialize};

use crate::automation::directory::{
    DirectoryClaim, DirectoryClaims, DirectoryClaimsError, DirectoryRef, InstanceRef,
    RepositoryRef, SessionRef, WorktreeCreator, WorktreeRef, WorktreeRemovalAssessment,
    WorktreeRemovalConfirmation, WorktreeRemovalRequest,
};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Git state of a session's working directory, used to decide which ditch
/// cleanup actions are safe to offer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    /// The cwd is inside a git work tree.
    pub in_repo: bool,
    /// The cwd is a *linked* worktree (not the repo's main working tree), so it
    /// can be removed without destroying the primary checkout.
    pub is_linked_worktree: bool,
    /// Current branch, or `None` when detached.
    pub branch: Option<String>,
    /// Has uncommitted changes (tracked edits or untracked files).
    pub dirty: bool,
    /// Commits on HEAD not present on its upstream (0 when no upstream).
    pub unpushed: u32,
    /// HEAD has a configured upstream branch.
    pub has_upstream: bool,
}

/// Inspect the git state of `cwd`. Any git failure yields a safe, empty status
/// (`in_repo == false`), so callers only ever offer "kill session".
pub fn status(cwd: &str) -> WorktreeStatus {
    status_path(Path::new(cwd)).unwrap_or_default()
}

/// Inspect a worktree through its exact filesystem path.
///
/// The string-facing [`status`] API is intentionally best-effort for UI refreshes,
/// but removal validation must distinguish a failed lookup from a clean tree.
fn status_path(cwd: &Path) -> Result<WorktreeStatus, String> {
    let mut status = WorktreeStatus::default();
    let inside = read_path(cwd, &["rev-parse", "--is-inside-work-tree"])?;
    if trim_git_line(&inside) != b"true" {
        return Ok(status);
    }
    status.in_repo = true;

    let git_dir = read_path(cwd, &["rev-parse", "--absolute-git-dir"])?;
    let common = read_path(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    status.is_linked_worktree = trim_git_line(&git_dir) != trim_git_line(&common);

    status.branch = read_path(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|out| String::from_utf8_lossy(trim_git_line(&out)).into_owned())
        .filter(|branch| !branch.is_empty());
    let porcelain = read_path(cwd, &["status", "--porcelain"])?;
    status.dirty = !porcelain.is_empty();
    if let Some(count) = read_path(cwd, &["rev-list", "--count", "@{u}..HEAD"])
        .ok()
        .and_then(|out| String::from_utf8_lossy(trim_git_line(&out)).parse().ok())
    {
        status.has_upstream = true;
        status.unpushed = count;
    }
    Ok(status)
}

fn trim_git_line(output: &[u8]) -> &[u8] {
    output.strip_suffix(b"\n").unwrap_or(output)
}

/// Detach HEAD in `worktree_path`, freeing its branch while keeping the
/// worktree directory and every commit. Fully non-destructive — the ditch
/// "detach" action runs this before killing the session.
pub fn detach_head(worktree_path: &str) -> Result<(), String> {
    run(worktree_path, &["checkout", "--detach"])
}

/// Number of worktrees attached to the repo containing `cwd` — the main working
/// tree plus every linked worktree. `0` when `cwd` is not in a git repo. Used to
/// gate the detach action, which only earns its keep in a multi-worktree repo.
pub fn worktree_count(cwd: &str) -> usize {
    read_bytes(cwd, &["worktree", "list", "--porcelain", "-z"])
        .map(|output| worktree_path_fields_from_porcelain(&output).count())
        .unwrap_or(0)
}

/// The repo's trunk branch — the one ditch must never offer to delete. Resolved
/// from the remote's default (`origin/HEAD`); for repos without a remote it
/// falls back to the branch checked out in the main worktree. `None` when
/// neither is known, in which case no branch is treated as the trunk.
pub fn trunk_branch(cwd: &str) -> Option<String> {
    read(
        cwd,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))
    .or_else(|| {
        let main = main_worktree(cwd)?;
        read(&main, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    })
}

/// Remove an already verified linked worktree through its pinned common Git
/// directory. The target is the same [`WorktreeRef`] that passed the final
/// identity recheck, never a repository rediscovered from its mutable path.
fn remove_worktree_from_repository(worktree: &WorktreeRef, force: bool) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(&worktree.repository.common_git_dir)
        .arg("worktree")
        .arg("remove");
    if force {
        command.arg("--force");
    }
    command.arg("--").arg(&worktree.path);
    hide_command_window(&mut command);
    run_git_command(command)
}

/// Serializable worktree state returned by the independent worktree service.
/// A worktree lifecycle request never starts, focuses, or closes a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeDetails {
    pub worktree: WorktreeRef,
    pub dirty: bool,
    pub claims: Vec<DirectoryClaim>,
}

/// Arguments for an independent linked-worktree creation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeCreateRequest {
    pub repository_path: PathBuf,
    pub branch: String,
    #[serde(default = "default_managed_by_bootty")]
    pub managed_by_bootty: bool,
    pub caller: String,
}

fn default_managed_by_bootty() -> bool {
    true
}

/// Arguments for a claim-safe worktree removal. `worktree` must be the exact
/// resolved identity returned by this service, not a display label or an
/// unchecked path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRemoveRequest {
    pub worktree: WorktreeRef,
    #[serde(default)]
    pub force: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requester_session: Option<SessionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<WorktreeRemovalConfirmation>,
}

/// Errors from an independent directory/worktree operation.
#[derive(Debug)]
pub enum WorktreeServiceError {
    Claims(DirectoryClaimsError),
    NotRepository { path: PathBuf },
    NotWorktree { path: PathBuf },
    NotLinkedWorktree { path: PathBuf },
    IdentityChanged { path: PathBuf },
    DirtyWorktree { path: PathBuf },
    Git { message: String },
}

impl fmt::Display for WorktreeServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Claims(error) => write!(formatter, "{error}"),
            Self::NotRepository { path } => {
                write!(formatter, "directory {path:?} is not in a Git repository")
            }
            Self::NotWorktree { path } => {
                write!(
                    formatter,
                    "directory {path:?} does not identify a Git worktree"
                )
            }
            Self::NotLinkedWorktree { path } => {
                write!(
                    formatter,
                    "directory {path:?} is not a removable linked worktree"
                )
            }
            Self::IdentityChanged { path } => {
                write!(
                    formatter,
                    "worktree identity changed before removal: {path:?}"
                )
            }
            Self::DirtyWorktree { path } => {
                write!(formatter, "worktree has uncommitted changes: {path:?}")
            }
            Self::Git { message } => formatter.write_str(message),
        }
    }
}

impl std::error::Error for WorktreeServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Claims(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DirectoryClaimsError> for WorktreeServiceError {
    fn from(error: DirectoryClaimsError) -> Self {
        Self::Claims(error)
    }
}

const WORKTREE_METADATA_FILE: &str = "bootty-worktree.json";

/// Durable metadata associated with one exact linked-worktree identity.
///
/// Git's per-worktree administrative directory survives process restarts and
/// is removed with the linked worktree, unlike advisory directory claims.
#[derive(Serialize, Deserialize)]
struct PersistedWorktreeMetadata {
    worktree: WorktreeRef,
}

fn worktree_metadata_path(worktree: &WorktreeRef) -> PathBuf {
    worktree.git_dir.join(WORKTREE_METADATA_FILE)
}

fn persist_worktree_metadata(worktree: &WorktreeRef) -> Result<(), WorktreeServiceError> {
    let path = worktree_metadata_path(worktree);
    let bytes = serde_json::to_vec(&PersistedWorktreeMetadata {
        worktree: worktree.clone(),
    })
    .map_err(|error| WorktreeServiceError::Git {
        message: format!("could not serialize worktree metadata: {error}"),
    })?;
    let temporary = worktree.git_dir.join(format!(
        ".{WORKTREE_METADATA_FILE}.{}.tmp",
        std::process::id()
    ));
    let mut file = fs::File::create(&temporary).map_err(|error| WorktreeServiceError::Git {
        message: format!("could not persist worktree metadata at {path:?}: {error}"),
    })?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| WorktreeServiceError::Git {
            message: format!("could not persist worktree metadata at {path:?}: {error}"),
        })?;
    fs::rename(&temporary, &path).map_err(|error| WorktreeServiceError::Git {
        message: format!("could not publish worktree metadata at {path:?}: {error}"),
    })?;
    fs::File::open(&worktree.git_dir)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| WorktreeServiceError::Git {
            message: format!("could not make worktree metadata durable at {path:?}: {error}"),
        })
}

fn hydrate_worktree_metadata(
    mut worktree: WorktreeRef,
) -> Result<WorktreeRef, WorktreeServiceError> {
    let path = worktree_metadata_path(&worktree);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(worktree),
        Err(error) => {
            return Err(WorktreeServiceError::Git {
                message: format!("could not read worktree metadata at {path:?}: {error}"),
            });
        }
    };
    let persisted =
        serde_json::from_slice::<PersistedWorktreeMetadata>(&bytes).map_err(|error| {
            WorktreeServiceError::Git {
                message: format!("could not parse worktree metadata at {path:?}: {error}"),
            }
        })?;
    if !persisted.worktree.same_identity(&worktree) {
        return Err(WorktreeServiceError::IdentityChanged {
            path: worktree.path,
        });
    }
    worktree.created_by = persisted.worktree.created_by;
    worktree.managed_by_bootty = persisted.worktree.managed_by_bootty;
    Ok(worktree)
}

#[derive(Clone, Debug)]
struct CreatedBranch {
    repository: RepositoryRef,
    reference: String,
    oid: String,
}

/// The sole production interface for directory and Git-worktree lifecycle
/// operations. It shares the cloneable directory-claims store with AppState,
/// so removal holds the process-wide claims lock through its final reread and
/// the destructive Git command.
#[derive(Clone)]
pub struct WorktreeService {
    claims: DirectoryClaims,
    instance: InstanceRef,
}

impl WorktreeService {
    pub fn new(claims: DirectoryClaims, instance: InstanceRef) -> Self {
        Self { claims, instance }
    }

    pub fn resolve(&self, path: impl AsRef<Path>) -> Result<DirectoryRef, WorktreeServiceError> {
        DirectoryRef::resolve(path)
            .map_err(DirectoryClaimsError::from)
            .map_err(Into::into)
    }

    pub fn usage(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<DirectoryClaim>, WorktreeServiceError> {
        let directory = self.resolve(path)?;
        self.claims.claims_for(&directory).map_err(Into::into)
    }

    /// List every Git worktree associated with the repository containing
    /// `path`, including currently discoverable directory claims.
    pub fn list(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<Vec<WorktreeDetails>, WorktreeServiceError> {
        let directory = self.resolve(path)?;
        let repository = directory
            .repository
            .ok_or(WorktreeServiceError::NotRepository {
                path: directory.canonical_path,
            })?;
        self.list_repository(&repository)
    }

    /// List the exact worktree inventory for a known repository identity.
    ///
    /// This remains usable after one linked worktree has been removed, when
    /// its old checkout path no longer resolves.
    pub fn list_repository(
        &self,
        repository: &RepositoryRef,
    ) -> Result<Vec<WorktreeDetails>, WorktreeServiceError> {
        let output = read_git_dir_bytes(
            &repository.common_git_dir,
            &["worktree", "list", "--porcelain", "-z"],
        )
        .ok_or_else(|| WorktreeServiceError::Git {
            message: format!(
                "could not list worktrees for repository at {:?}",
                repository.common_git_dir
            ),
        })?;
        let mut details = Vec::new();
        for path in worktree_paths_from_porcelain(&output) {
            match self.get(path) {
                Ok(detail) if detail.worktree.repository.same_identity(repository) => {
                    details.push(detail);
                }
                // Git retains prunable worktree registrations after their checkout has gone.
                // They are not inventory items, but must not hide the surviving worktrees.
                Ok(_) | Err(WorktreeServiceError::NotWorktree { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        details.sort_by(|left, right| left.worktree.path.cmp(&right.worktree.path));
        Ok(details)
    }

    /// Return one exact worktree identity and its independently observed state.
    pub fn get(&self, path: impl AsRef<Path>) -> Result<WorktreeDetails, WorktreeServiceError> {
        let worktree = self.resolve_worktree(path)?;
        self.details_for(worktree)
    }

    /// Preflight the current active claims for one exact worktree. A later
    /// [`Self::remove`] always repeats this under the global claims lock; this
    /// assessment exists only to bind an explicit confirmation to what the
    /// caller reviewed.
    pub fn assess_removal(
        &self,
        path: impl AsRef<Path>,
        requester_session: Option<&SessionRef>,
    ) -> Result<WorktreeRemovalAssessment, WorktreeServiceError> {
        let worktree = self.get(path)?.worktree;
        self.claims
            .assess_worktree_removal(&worktree, requester_session)
            .map_err(Into::into)
    }

    /// Create a linked worktree without creating or changing any session.
    pub fn create(
        &self,
        request: WorktreeCreateRequest,
    ) -> Result<WorktreeDetails, WorktreeServiceError> {
        self.create_after_add(
            request,
            add_worktree,
            |created_path| self.resolve(created_path),
            persist_worktree_metadata,
        )
    }

    fn create_after_add(
        &self,
        request: WorktreeCreateRequest,
        add: impl FnOnce(&Path, &str) -> Result<PathBuf, String>,
        resolve_created: impl FnOnce(&Path) -> Result<DirectoryRef, WorktreeServiceError>,
        persist_metadata: impl FnOnce(&WorktreeRef) -> Result<(), WorktreeServiceError>,
    ) -> Result<WorktreeDetails, WorktreeServiceError> {
        self.create_after_add_unlocked(request, add, resolve_created, persist_metadata)
    }

    fn create_after_add_unlocked(
        &self,
        request: WorktreeCreateRequest,
        add: impl FnOnce(&Path, &str) -> Result<PathBuf, String>,
        resolve_created: impl FnOnce(&Path) -> Result<DirectoryRef, WorktreeServiceError>,
        persist_metadata: impl FnOnce(&WorktreeRef) -> Result<(), WorktreeServiceError>,
    ) -> Result<WorktreeDetails, WorktreeServiceError> {
        let repository_directory = self.resolve(&request.repository_path)?;
        let repository =
            repository_directory
                .repository
                .ok_or_else(|| WorktreeServiceError::NotRepository {
                    path: repository_directory.canonical_path.clone(),
                })?;
        let repository_path = repository_directory.canonical_path.clone();
        let attempt = self.claims.with_worktree_mutation_lease(&repository, || {
            let created_path = add(&repository_path, &request.branch)
                .map_err(|message| WorktreeServiceError::Git { message })?;

            let mut created_worktree = None;
            let mut created_branch = None;
            let created = (|| {
                let created_directory = resolve_created(&created_path)?;
                let mut worktree = created_directory.worktree.ok_or_else(|| {
                    WorktreeServiceError::NotWorktree {
                        path: created_path.clone(),
                    }
                })?;
                if !worktree.repository.same_identity(&repository) {
                    return Err(WorktreeServiceError::IdentityChanged {
                        path: worktree.path,
                    });
                }
                let Some(branch) = worktree.branch.clone() else {
                    return Err(WorktreeServiceError::IdentityChanged {
                        path: worktree.path,
                    });
                };
                let Some(oid) = worktree.head.clone() else {
                    return Err(WorktreeServiceError::IdentityChanged {
                        path: worktree.path,
                    });
                };
                if branch != request.branch || oid.is_empty() {
                    return Err(WorktreeServiceError::IdentityChanged {
                        path: worktree.path,
                    });
                }
                created_branch = Some(CreatedBranch {
                    repository: worktree.repository.clone(),
                    reference: format!("refs/heads/{branch}"),
                    oid,
                });
                worktree.created_by = Some(WorktreeCreator {
                    instance: self.instance.clone(),
                    caller: request.caller.clone(),
                });
                worktree.managed_by_bootty = request.managed_by_bootty;
                created_worktree = Some(worktree.clone());
                persist_metadata(&worktree)?;
                self.details_for(worktree)
            })();

            Ok::<_, WorktreeServiceError>((created_path, created, created_worktree, created_branch))
        })?;

        let (created_path, created, created_worktree, created_branch) = attempt;
        match created {
            Ok(details) => Ok(details),
            Err(error) => {
                let Some(worktree) = created_worktree.as_ref() else {
                    return Err(WorktreeServiceError::Git {
                        message: format!(
                            "{error}; could not safely roll back newly added worktree at {created_path:?}: could not establish the created worktree identity"
                        ),
                    });
                };
                let Some(created_branch) = created_branch.as_ref() else {
                    return Err(WorktreeServiceError::Git {
                        message: format!(
                            "{error}; could not safely roll back newly added worktree at {created_path:?}: could not establish the created branch identity"
                        ),
                    });
                };
                if let Err(rollback_error) =
                    self.rollback_created_worktree(worktree, created_branch)
                {
                    return Err(WorktreeServiceError::Git {
                        message: format!(
                            "{error}; could not safely roll back newly added worktree at {created_path:?}: {rollback_error}"
                        ),
                    });
                }
                let current_repository = self
                    .resolve(&request.repository_path)
                    .ok()
                    .and_then(|directory| directory.repository);
                if current_repository
                    .as_ref()
                    .is_none_or(|current| !current.same_identity(&created_branch.repository))
                {
                    return Err(WorktreeServiceError::Git {
                        message: format!(
                            "{error}; removed the newly added worktree at {created_path:?}, but \
                             could not prove the creating repository still owns branch {:?}",
                            created_branch.reference
                        ),
                    });
                }
                delete_created_branch(&repository_path, created_branch).map_err(
                    |rollback_error| WorktreeServiceError::Git {
                        message: format!(
                            "{error}; removed the newly added worktree at {created_path:?}, but \
                             could not roll back its new branch {:?}: {rollback_error}",
                            request.branch
                        ),
                    },
                )?;
                Err(error)
            }
        }
    }

    /// Roll back a known creation through the claims store's final identity
    /// recheck, and remove the checkout only while it still names the exact
    /// worktree that was added.
    fn rollback_created_worktree(
        &self,
        expected: &WorktreeRef,
        created_branch: &CreatedBranch,
    ) -> Result<(), WorktreeServiceError> {
        let mut final_validation_error = None;
        let observed = self.resolve_worktree(&expected.path)?;
        let mut claims_expected = expected.clone();
        claims_expected.created_by = observed.created_by;
        claims_expected.managed_by_bootty = observed.managed_by_bootty;
        let result = self.claims.remove_worktree(
            &claims_expected,
            &WorktreeRemovalRequest {
                requester_session: None,
                confirmation: None,
            },
            |expected| {
                let actual = match self.resolve_worktree(&expected.path).and_then(|resolved| {
                    validate_created_rollback_target(expected, resolved, created_branch)
                }) {
                    Ok(actual) => actual,
                    Err(error) => {
                        final_validation_error = Some(error);
                        return Err("worktree identity changed before locked rollback".to_owned());
                    }
                };
                remove_worktree_from_repository(&actual, true)
            },
        );
        if let Some(error) = final_validation_error {
            return Err(error);
        }
        match result {
            Ok(_) => Ok(()),
            Err(DirectoryClaimsError::StaleRemovalTarget { .. }) => {
                Err(WorktreeServiceError::IdentityChanged {
                    path: expected.path.clone(),
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Remove one linked worktree after a final globally locked active-claim
    /// reread. The requester must re-submit the exact confirmation returned by
    /// a preceding assessment when another session is using it.
    pub fn remove(
        &self,
        request: WorktreeRemoveRequest,
    ) -> Result<WorktreeRemovalAssessment, WorktreeServiceError> {
        self.remove_after_final_recheck(request, remove_worktree_from_repository)
    }

    fn remove_after_final_recheck(
        &self,
        request: WorktreeRemoveRequest,
        remove_worktree: impl FnOnce(&WorktreeRef, bool) -> Result<(), String>,
    ) -> Result<WorktreeRemovalAssessment, WorktreeServiceError> {
        let actual = validate_removal_target(
            &request.worktree,
            self.resolve_worktree(&request.worktree.path)?,
            request.force,
        )?;
        let force = request.force;
        let removal = WorktreeRemovalRequest {
            requester_session: request.requester_session,
            confirmation: request.confirmation,
        };
        let mut final_validation_error = None;
        let result = self.claims.remove_worktree(&actual, &removal, |worktree| {
            let final_actual = match self
                .resolve_worktree(&worktree.path)
                .and_then(|resolved| validate_removal_target(worktree, resolved, force))
            {
                Ok(actual) => actual,
                Err(error) => {
                    final_validation_error = Some(error);
                    return Err("worktree identity changed before locked removal".to_owned());
                }
            };
            remove_worktree(&final_actual, force)
        });
        if let Some(error) = final_validation_error {
            return Err(error);
        }
        match result {
            Ok(assessment) => Ok(assessment),
            Err(DirectoryClaimsError::StaleRemovalTarget { .. }) => {
                Err(WorktreeServiceError::IdentityChanged {
                    path: request.worktree.path,
                })
            }
            Err(error) => Err(error.into()),
        }
    }

    fn resolve_worktree(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<WorktreeRef, WorktreeServiceError> {
        let directory = self.resolve(path)?;
        let worktree = directory
            .worktree
            .ok_or(WorktreeServiceError::NotWorktree {
                path: directory.canonical_path,
            })?;
        hydrate_worktree_metadata(worktree)
    }
    fn details_for(&self, worktree: WorktreeRef) -> Result<WorktreeDetails, WorktreeServiceError> {
        let claims = self.claims.claims_for_worktree(&worktree)?;
        let dirty = status_path(&worktree.path)
            .map_err(|message| WorktreeServiceError::Git { message })?
            .dirty;
        Ok(WorktreeDetails {
            worktree,
            dirty,
            claims,
        })
    }
}

fn validate_created_rollback_target(
    expected: &WorktreeRef,
    actual: WorktreeRef,
    created_branch: &CreatedBranch,
) -> Result<WorktreeRef, WorktreeServiceError> {
    let expected_branch = created_branch.reference.strip_prefix("refs/heads/");
    let actual_branch_oid = branch_oid(&created_branch.repository, &created_branch.reference)
        .map_err(|message| WorktreeServiceError::Git { message })?;
    if !actual.same_identity(expected)
        || !actual.repository.same_identity(&created_branch.repository)
        || actual.branch.as_deref() != expected_branch
        || actual.head.as_deref() != Some(created_branch.oid.as_str())
        || actual_branch_oid != created_branch.oid
    {
        return Err(WorktreeServiceError::IdentityChanged {
            path: expected.path.clone(),
        });
    }
    let mut rollback_expected = expected.clone();
    rollback_expected.created_by = actual.created_by.clone();
    rollback_expected.managed_by_bootty = actual.managed_by_bootty;
    validate_removal_target(&rollback_expected, actual, true)
}
fn validate_removal_target(
    expected: &WorktreeRef,
    actual: WorktreeRef,
    force: bool,
) -> Result<WorktreeRef, WorktreeServiceError> {
    if !actual.same_removal_target(expected) {
        return Err(WorktreeServiceError::IdentityChanged {
            path: expected.path.clone(),
        });
    }
    if !actual.is_linked() {
        return Err(WorktreeServiceError::NotLinkedWorktree { path: actual.path });
    }
    if !force
        && status_path(&actual.path)
            .map_err(|message| WorktreeServiceError::Git { message })?
            .dirty
    {
        return Err(WorktreeServiceError::DirtyWorktree { path: actual.path });
    }
    Ok(actual)
}

fn worktree_paths_from_porcelain(output: &[u8]) -> Vec<PathBuf> {
    worktree_path_fields_from_porcelain(output)
        .filter_map(path_from_git_porcelain)
        .collect()
}

fn worktree_path_fields_from_porcelain(output: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut first_field_in_record = true;
    output
        .split(|byte| *byte == b'\0')
        .filter_map(move |field| {
            if field.is_empty() {
                first_field_in_record = true;
                return None;
            }
            let path = first_field_in_record
                .then(|| field.strip_prefix(b"worktree "))
                .flatten();
            first_field_in_record = false;
            path
        })
}

fn path_from_git_porcelain(path: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        Some(PathBuf::from(OsString::from_vec(path.to_vec())))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(path).ok().map(PathBuf::from)
    }
}

/// Create a new linked worktree on a fresh `branch` off the repo containing
/// `repo_dir`, returning the new worktree path (a sibling dir named
/// `<repo>-<branch-slug>`).
pub fn add_worktree(repo_dir: &Path, branch: &str) -> Result<PathBuf, String> {
    bootty_mux::project::add_worktree(repo_dir, branch)
}

/// Delete `branch`, running git in `repo_dir` — any live working tree of the
/// repo. Pass the main worktree, since a just-removed linked worktree is gone.
/// `force` maps to `git branch -D` (drops unmerged commits) vs the safe `-d`.
pub fn delete_branch(repo_dir: &str, branch: &str, force: bool) -> Result<(), String> {
    run(
        repo_dir,
        &["branch", if force { "-D" } else { "-d" }, branch],
    )
}

fn branch_oid(repository: &RepositoryRef, reference: &str) -> Result<String, String> {
    let output = read_git_dir_bytes(
        &repository.common_git_dir,
        &["rev-parse", "--verify", reference],
    )
    .ok_or_else(|| format!("could not read created branch reference {reference:?}"))?;
    String::from_utf8(output)
        .map_err(|error| format!("created branch reference {reference:?} was not UTF-8: {error}"))
        .map(|oid| oid.trim().to_owned())
}

fn delete_created_branch(repository_path: &Path, expected: &CreatedBranch) -> Result<(), String> {
    let actual_oid = branch_oid(&expected.repository, &expected.reference)?;
    if actual_oid != expected.oid {
        return Err(format!(
            "created branch reference {:?} changed from {} to {}; leaving it intact",
            expected.reference, expected.oid, actual_oid
        ));
    }
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(&expected.repository.common_git_dir)
        .arg("update-ref")
        .arg("-d")
        .arg(&expected.reference)
        .arg(&expected.oid);
    hide_command_window(&mut command);
    run_git_command(command).map_err(|error| {
        format!(
            "could not delete created branch {:?} at repository {repository_path:?}: {error}",
            expected.reference
        )
    })
}

fn repository_for_path(repo_dir: &str) -> Result<RepositoryRef, String> {
    DirectoryRef::resolve(repo_dir)
        .map_err(|error| format!("could not resolve repository {repo_dir:?}: {error}"))?
        .repository
        .ok_or_else(|| format!("path {repo_dir:?} is not a Git repository"))
}

#[derive(Clone, Debug)]
pub struct BranchRemovalTarget {
    repository: RepositoryRef,
    reference: String,
    oid: String,
}

/// Capture the repository identity and object ID for a local branch before a
/// destructive cleanup.
pub fn capture_branch_removal_target(
    repo_dir: &str,
    branch: &str,
) -> Result<BranchRemovalTarget, String> {
    let repository = repository_for_path(repo_dir)?;
    let reference = format!("refs/heads/{branch}");
    let oid = branch_oid(&repository, &reference)?;
    Ok(BranchRemovalTarget {
        repository,
        reference,
        oid,
    })
}

/// Delete a branch only if its repository identity and object ID are unchanged.
///
/// The expected object ID is passed to `git update-ref` as its old value, so a
/// concurrent re-point leaves the branch intact instead of deleting unrelated
/// work.
pub fn delete_branch_if_unchanged(
    repo_dir: &str,
    target: &BranchRemovalTarget,
) -> Result<(), String> {
    let current_repository = repository_for_path(repo_dir)?;
    if !current_repository.same_identity(&target.repository) {
        return Err(format!(
            "repository identity changed at {repo_dir:?}; leaving branch {:?} intact",
            target.reference
        ));
    }
    let actual_oid = branch_oid(&target.repository, &target.reference)?;
    if actual_oid != target.oid {
        return Err(format!(
            "branch {:?} changed from {} to {}; leaving it intact",
            target.reference, target.oid, actual_oid
        ));
    }
    let mut command = Command::new("git");
    command
        .arg("--git-dir")
        .arg(&target.repository.common_git_dir)
        .arg("update-ref")
        .arg("-d")
        .arg(&target.reference)
        .arg(&target.oid);
    hide_command_window(&mut command);
    run_git_command(command).map_err(|error| {
        format!(
            "could not delete branch {:?} at repository {:?}: {error}",
            target.reference, target.repository.common_git_dir
        )
    })
}

/// The main working tree directory for the repo containing `cwd` — the parent
/// of the shared `.git` common dir.
pub fn main_worktree(cwd: &str) -> Option<String> {
    let common = read(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Path::new(&common)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
}
/// The root directory of the Git worktree containing `cwd`.
pub fn worktree_root(cwd: &str) -> Option<String> {
    read(cwd, &["rev-parse", "--show-toplevel"])
}

/// Suggest a grouped session name for a worktree, or a basename for a plain directory.
pub fn suggested_session_name(cwd: &str) -> String {
    let Some(worktree) = worktree_root(cwd) else {
        return crate::strings::session_name_for_path(cwd);
    };
    let status = status_path(Path::new(&worktree)).unwrap_or_default();

    let group = main_worktree(&worktree)
        .as_deref()
        .map(crate::strings::session_name_for_path)
        .unwrap_or_else(|| crate::strings::session_name_for_path(&worktree));
    let leaf = status
        .branch
        .as_deref()
        .and_then(|branch| branch.rsplit('/').next())
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| crate::strings::session_name_for_path(&worktree));
    format!("{group}/{leaf}")
}

/// What `HEAD` points at, read straight from the git directory: a branch name, or
/// `detached <short commit>` when `HEAD` holds a commit instead of a ref.
///
/// Reading the file keeps a per-session branch label from forking `git rev-parse` on every
/// sidebar refresh, which was one of the busiest subprocesses Bootty ran.
pub fn head_branch(cwd: &str) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(Path::new(cwd))?.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!branch.is_empty()).then(|| branch.to_owned());
    }
    let commit = head.get(..7).unwrap_or(head);
    (!commit.is_empty()).then(|| format!("detached {commit}"))
}

/// Most worktrees watched at once. A tree that misses out reports no revision, and its caller keeps
/// asking git on its own schedule.
const MAX_WATCHED_WORKTREES: usize = 64;

struct WorktreeWatch {
    revision: Arc<AtomicU64>,
    paths: Vec<PathBuf>,
    last_used: u64,
}

/// Revision counter and filesystem roots per worktree, shared with the watcher's event thread.
type WorktreeRevisions = Arc<Mutex<HashMap<PathBuf, WorktreeWatch>>>;

fn worktree_revisions() -> &'static WorktreeRevisions {
    static REVISIONS: OnceLock<WorktreeRevisions> = OnceLock::new();
    REVISIONS.get_or_init(WorktreeRevisions::default)
}
static WORKTREE_ACCESS_CLOCK: AtomicU64 = AtomicU64::new(0);

fn next_worktree_access() -> u64 {
    WORKTREE_ACCESS_CLOCK.fetch_add(1, Ordering::Relaxed) + 1
}

/// One watcher for every worktree: each `notify` watcher owns an event thread, and a thread per
/// repository is a lot of idling for a counter.
fn worktree_watcher() -> &'static Mutex<Option<RecommendedWatcher>> {
    static WATCHER: OnceLock<Mutex<Option<RecommendedWatcher>>> = OnceLock::new();
    WATCHER.get_or_init(|| {
        let revisions = Arc::clone(worktree_revisions());
        // Any event is enough: this says "ask git again", never what to believe instead. Bursts
        // collapse on their own, since callers compare the counter rather than draining events.
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else { return };
            let Ok(revisions) = revisions.lock() else {
                return;
            };
            record_worktree_events(&revisions, &event.paths);
        });
        Mutex::new(watcher.ok())
    })
}

/// A counter for the working tree holding `cwd`, bumped whenever the filesystem reports a change
/// under it. Watching the tree — rather than just `.git` — is what makes an uncommitted edit count,
/// and it is what an editor's git integration does instead of asking git on a timer.
///
/// The first call starts watching and answers 1. `0` means this tree is not watched, so the caller
/// learns nothing from it and should ask git as it would have anyway.
pub fn worktree_revision(cwd: &str) -> u64 {
    let Some(paths) = worktree_watch_paths(Path::new(cwd)) else {
        return 0;
    };
    let root = paths[0].clone();
    if let Ok(mut revisions) = worktree_revisions().lock()
        && let Some(watched) = revisions.get_mut(&root)
    {
        watched.last_used = next_worktree_access();
        return watched.revision.load(Ordering::Relaxed);
    }

    let Ok(mut watcher) = worktree_watcher().lock() else {
        return 0;
    };
    let Some(watcher) = watcher.as_mut() else {
        return 0;
    };
    let Ok(mut revisions) = worktree_revisions().lock() else {
        return 0;
    };
    if revisions.len() >= MAX_WATCHED_WORKTREES {
        let candidate = revisions
            .iter()
            .min_by_key(|(root, watched)| (root.exists(), watched.last_used))
            .map(|(root, _)| root.clone());
        if let Some(candidate) = candidate
            && let Some(evicted) = revisions.remove(&candidate)
        {
            for path in evicted.paths {
                let _ = watcher.unwatch(&path);
            }
        }
    }
    let mut registered: Vec<PathBuf> = Vec::new();
    for path in &paths {
        if watcher.watch(path, RecursiveMode::Recursive).is_err() {
            for registered_path in registered {
                let _ = watcher.unwatch(&registered_path);
            }
            return 0;
        }
        registered.push(path.clone());
    }
    revisions.insert(
        root,
        WorktreeWatch {
            revision: Arc::new(AtomicU64::new(1)),
            paths,
            last_used: next_worktree_access(),
        },
    );
    1
}

fn worktree_watch_paths(cwd: &Path) -> Option<Vec<PathBuf>> {
    let root = native_worktree_root(cwd)?;
    let git_dir = std::fs::canonicalize(git_dir(cwd)?).ok()?;
    let mut paths = vec![root.clone()];
    if !git_dir.starts_with(&root) {
        paths.push(git_dir);
    }
    Some(paths)
}

fn record_worktree_events(revisions: &HashMap<PathBuf, WorktreeWatch>, event_paths: &[PathBuf]) {
    for watched in revisions.values() {
        if event_paths
            .iter()
            .any(|event| watched.paths.iter().any(|root| event.starts_with(root)))
        {
            watched.revision.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The working tree containing `cwd`, found by walking up to the `.git` entry rather than asking
/// git. For a linked worktree this is that worktree's own root, not the main checkout.
///
/// The path is resolved, because filesystem events arrive resolved: on macOS a tree under `/var`
/// is reported under `/private/var`, and an unresolved root matches none of its own events.
fn native_worktree_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .and_then(|dir| std::fs::canonicalize(dir).ok())
}

/// The git directory governing `cwd`: `.git` in the nearest ancestor that has one, following the
/// `gitdir:` pointer a linked worktree leaves in place of a directory.
fn git_dir(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let pointer = std::fs::read(&candidate).ok()?;
            let line = pointer.split(|byte| *byte == b'\n').next()?;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let target = line.strip_prefix(b"gitdir:")?;
            let target = target.strip_prefix(b" ").unwrap_or(target);
            if target.is_empty() {
                return None;
            }
            #[cfg(unix)]
            let target = PathBuf::from(OsString::from_vec(target.to_vec()));
            #[cfg(not(unix))]
            let target = PathBuf::from(String::from_utf8(target.to_vec()).ok()?);
            return Some(if target.is_absolute() {
                target
            } else {
                dir.join(target)
            });
        }
    }
    None
}

fn read(cwd: &str, args: &[&str]) -> Option<String> {
    read_path(Path::new(cwd), args)
        .ok()
        .map(|output| String::from_utf8_lossy(&output).trim().to_owned())
}

fn read_bytes(cwd: &str, args: &[&str]) -> Option<Vec<u8>> {
    read_path(Path::new(cwd), args).ok()
}

fn read_path(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    bootty_runtime::perf::record_subprocess("git read");
    let output = git_command_path(cwd, args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn read_git_dir_bytes(git_dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    bootty_runtime::perf::record_subprocess("git read");
    let mut command = Command::new("git");
    command.arg("--git-dir").arg(git_dir).args(args);
    hide_command_window(&mut command);
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn run(cwd: &str, args: &[&str]) -> Result<(), String> {
    run_git_command(git_command(cwd, args))
}

fn run_git_command(mut command: Command) -> Result<(), String> {
    bootty_runtime::perf::record_subprocess("git run");
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn git_command(cwd: &str, args: &[&str]) -> Command {
    git_command_path(Path::new(cwd), args)
}

fn git_command_path(cwd: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    hide_command_window(&mut command);
    command
}

#[cfg(windows)]
fn hide_command_window(command: &mut Command) {
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_command_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use crate::automation::directory::{ClaimOwner, OwnerLiveness};
    #[test]
    fn worktree_events_under_content_or_git_metadata_move_the_revision() {
        let revision = Arc::new(AtomicU64::new(1));
        let mut revisions = HashMap::new();
        revisions.insert(
            PathBuf::from("/worktree"),
            WorktreeWatch {
                revision: Arc::clone(&revision),
                paths: vec![
                    PathBuf::from("/worktree"),
                    PathBuf::from("/repo/.git/worktrees/feature"),
                ],
                last_used: 0,
            },
        );

        record_worktree_events(&revisions, &[PathBuf::from("/worktree/src/main.rs")]);
        record_worktree_events(
            &revisions,
            &[PathBuf::from("/repo/.git/worktrees/feature/index")],
        );
        record_worktree_events(&revisions, &[PathBuf::from("/somewhere/else")]);

        assert_eq!(revision.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn git_porcelain_round_trips_worktree_path_whitespace() {
        let porcelain = b"worktree /repo/ leading-and-trailing \0HEAD deadbeef\0\0";

        assert_eq!(
            worktree_paths_from_porcelain(porcelain),
            vec![PathBuf::from("/repo/ leading-and-trailing ")]
        );
    }

    #[test]
    fn service_list_preserves_newline_worktree_identity() {
        let (root, main, _worktree) = repo_with_worktree();
        let newline_worktree = root.path().join("newline\nworktree");
        git_ok(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "newline-worktree",
                newline_worktree.to_str().expect("UTF-8 worktree path"),
            ],
        );
        let expected = DirectoryRef::resolve(&newline_worktree)
            .expect("resolve newline worktree")
            .worktree
            .expect("newline worktree identity");
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("newline-inventory").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "newline-inventory".to_owned(),
                generation: 1,
            },
        );

        let listed = service.list(&main).expect("list worktrees");
        let matches: Vec<_> = listed
            .iter()
            .filter(|details| details.worktree.same_identity(&expected))
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].worktree.path, expected.path);
    }

    fn git_ok(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .expect("run git");
        assert!(
            status.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// A repo at `main/` with one commit on `main`, plus a linked worktree at
    /// `wt/` on branch `feature`.
    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().expect("tempdir");
        let main = root.path().join("main");
        fs::create_dir(&main).expect("mkdir main");
        git_ok(&main, &["init", "-q", "-b", "main"]);
        git_ok(&main, &["config", "user.email", "t@t.test"]);
        git_ok(&main, &["config", "user.name", "tester"]);
        fs::write(main.join("README"), "hello").expect("write");
        git_ok(&main, &["add", "."]);
        git_ok(&main, &["commit", "-q", "-m", "init"]);
        let worktree = root.path().join("wt");
        git_ok(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        (root, main, worktree)
    }

    struct ReplaceWorktreeDuringLockedRecheck {
        armed: AtomicBool,
        worktree: PathBuf,
    }

    impl OwnerLiveness for ReplaceWorktreeDuringLockedRecheck {
        fn is_dead(&self, _owner: &ClaimOwner) -> bool {
            if self.armed.swap(false, Ordering::SeqCst) {
                let worktree = DirectoryRef::resolve(&self.worktree)
                    .expect("resolve original worktree")
                    .worktree
                    .expect("original worktree");
                remove_worktree_from_repository(&worktree, true).expect("remove original worktree");
                fs::create_dir(&self.worktree).expect("replace worktree directory");
                git_ok(&self.worktree, &["init", "-q"]);
            }
            false
        }
    }

    fn add_worktree_at(repository: &Path, branch: &str, path: &Path) -> Result<PathBuf, String> {
        let repository = repository
            .to_str()
            .ok_or_else(|| "repository path was not UTF-8".to_owned())?;
        let path_string = path
            .to_str()
            .ok_or_else(|| "worktree path was not UTF-8".to_owned())?;
        run(repository, &["worktree", "add", "-b", branch, path_string])?;
        Ok(path.to_path_buf())
    }

    fn resolve_created_worktree(created: &Path) -> Result<DirectoryRef, WorktreeServiceError> {
        Ok(DirectoryRef::resolve(created).map_err(DirectoryClaimsError::from)?)
    }

    fn injected_post_add_failure(_: &WorktreeRef) -> Result<(), WorktreeServiceError> {
        Err(WorktreeServiceError::Git {
            message: "injected post-add failure".to_owned(),
        })
    }

    #[test]
    fn head_branch_reads_slashed_names_through_a_worktree_pointer() {
        let (_root, main, worktree) = repo_with_worktree();
        // A `/` in the name is where naive parsing (taking the last path segment of the ref)
        // silently reports "three" for `one/two/three`.
        git_ok(&worktree, &["checkout", "-q", "-b", "one/two/three"]);
        let nested = worktree.join("nested");
        fs::create_dir(&nested).expect("create nested directory");

        assert_eq!(head_branch(main.to_str().unwrap()).as_deref(), Some("main"));
        assert_eq!(
            head_branch(nested.to_str().unwrap()).as_deref(),
            Some("one/two/three"),
            "a linked worktree's HEAD lives behind its `.git` gitdir pointer"
        );
    }

    #[test]
    fn linked_worktree_watches_its_external_head_and_index() {
        let (_root, _main, worktree) = repo_with_worktree();

        let paths = worktree_watch_paths(&worktree).expect("linked worktree watch paths");

        assert_eq!(paths.len(), 2);
        assert!(paths[0].join(".git").is_file());
        assert!(paths[1].join("HEAD").is_file());
        assert!(paths[1].join("index").is_file());
    }

    #[test]
    fn head_branch_reports_a_detached_head_as_its_short_commit() {
        let (_root, main, _worktree) = repo_with_worktree();
        let commit = read(main.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("rev-parse");
        git_ok(&main, &["checkout", "-q", "--detach"]);

        assert_eq!(
            head_branch(main.to_str().unwrap()).as_deref(),
            Some(format!("detached {}", &commit[..7]).as_str())
        );
    }

    #[test]
    fn head_branch_is_none_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(head_branch(dir.path().to_str().unwrap()), None);
    }

    #[test]
    fn suggested_session_name_groups_branch_worktrees() {
        let (_root, main, worktree) = repo_with_worktree();

        assert_eq!(suggested_session_name(main.to_str().unwrap()), "main/main");
        assert_eq!(
            suggested_session_name(worktree.to_str().unwrap()),
            "main/feature"
        );
    }
    #[test]
    fn suggested_session_name_uses_the_worktree_root_from_a_nested_directory() {
        let (_root, _main, worktree) = repo_with_worktree();
        let nested = worktree.join("nested");
        fs::create_dir(&nested).expect("create nested directory");

        assert_eq!(
            suggested_session_name(nested.to_str().unwrap()),
            "main/feature"
        );
    }

    #[test]
    fn suggested_session_name_uses_directory_for_detached_worktrees() {
        let (_root, main, worktree) = repo_with_worktree();
        git_ok(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                worktree
                    .parent()
                    .unwrap()
                    .join("detached")
                    .to_str()
                    .unwrap(),
            ],
        );

        assert_eq!(
            suggested_session_name(
                worktree
                    .parent()
                    .unwrap()
                    .join("detached")
                    .to_str()
                    .unwrap()
            ),
            "main/detached"
        );
    }

    #[test]
    fn suggested_session_name_keeps_plain_directories_ungrouped() {
        let dir = tempfile::tempdir().expect("tempdir");

        assert_eq!(
            suggested_session_name(dir.path().to_str().unwrap()),
            crate::strings::session_name_for_path(dir.path().to_str().unwrap())
        );
    }

    #[test]
    fn status_distinguishes_linked_worktree_from_main() {
        let (_root, main, worktree) = repo_with_worktree();

        let main_status = status(main.to_str().unwrap());
        assert!(main_status.in_repo && !main_status.is_linked_worktree);
        assert_eq!(main_status.branch.as_deref(), Some("main"));

        let wt_status = status(worktree.to_str().unwrap());
        assert!(wt_status.in_repo && wt_status.is_linked_worktree);
        assert_eq!(wt_status.branch.as_deref(), Some("feature"));
        assert!(!wt_status.dirty);
    }

    #[test]
    fn status_reports_dirty_worktree() {
        let (_root, _main, worktree) = repo_with_worktree();
        fs::write(worktree.join("scratch"), "wip").expect("write");
        assert!(status(worktree.to_str().unwrap()).dirty);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn remove_rejects_dirty_non_utf8_worktree_without_lossy_lookup() {
        let (root, main, _worktree) = repo_with_worktree();
        let non_utf8_name = OsString::from_vec(b"non-utf8-\xff".to_vec());
        let path = root.path().join(non_utf8_name);
        let output = Command::new("git")
            .arg("-C")
            .arg(&main)
            .args(["worktree", "add", "-b", "non-utf8"])
            .arg(&path)
            .output()
            .expect("add non-UTF-8 worktree");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("non-utf8-removal").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "non-utf8-removal".to_owned(),
                generation: 1,
            },
        );
        let details = service.get(&path).expect("resolve non-UTF-8 worktree");
        fs::write(details.worktree.path.join("dirty"), "wip").expect("dirty worktree");

        let result = service.remove(WorktreeRemoveRequest {
            worktree: details.worktree,
            force: false,
            requester_session: None,
            confirmation: None,
        });
        assert!(matches!(
            result,
            Err(WorktreeServiceError::DirtyWorktree { .. })
        ));
        assert!(
            path.exists(),
            "dirty worktree must survive a non-forced removal"
        );
    }

    #[test]
    fn status_outside_a_repo_is_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            status(dir.path().to_str().unwrap()),
            WorktreeStatus::default()
        );
    }

    #[test]
    fn remove_worktree_detaches_the_linked_checkout() {
        let (_root, main, worktree) = repo_with_worktree();
        let identity = DirectoryRef::resolve(&worktree)
            .expect("resolve worktree")
            .worktree
            .expect("worktree identity");
        remove_worktree_from_repository(&identity, false).expect("remove worktree");
        assert!(!worktree.exists());
        let list = read(main.to_str().unwrap(), &["worktree", "list"]).expect("list");
        assert!(!list.contains("wt"), "worktree still listed: {list}");
    }

    #[test]
    fn delete_branch_force_removes_unmerged_branch() {
        let (_root, main, worktree) = repo_with_worktree();
        // Commit on `feature` so it carries work absent from `main`; the forced
        // delete must still remove it (the ditch "delete branch" action).
        fs::write(worktree.join("feature.txt"), "work").expect("write");
        git_ok(&worktree, &["add", "."]);
        git_ok(&worktree, &["commit", "-q", "-m", "feature work"]);
        let identity = DirectoryRef::resolve(&worktree)
            .expect("resolve worktree")
            .worktree
            .expect("worktree identity");
        remove_worktree_from_repository(&identity, false).expect("remove worktree");

        delete_branch(main.to_str().unwrap(), "feature", true).expect("force delete");
        let branches =
            read(main.to_str().unwrap(), &["branch", "--list", "feature"]).expect("list");
        assert!(branches.is_empty(), "branch still present: {branches}");
    }

    #[test]
    fn detach_head_frees_the_branch_keeping_the_worktree_and_commit() {
        let (_root, _main, worktree) = repo_with_worktree();
        let before = read(worktree.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("rev-parse");

        detach_head(worktree.to_str().unwrap()).expect("detach head");

        let after = status(worktree.to_str().unwrap());
        assert!(after.branch.is_none(), "HEAD should be detached");
        assert!(worktree.exists(), "worktree dir must survive a detach");
        assert_eq!(
            read(worktree.to_str().unwrap(), &["rev-parse", "HEAD"]).as_deref(),
            Some(before.as_str()),
            "detach must not move HEAD off the current commit"
        );
    }

    #[test]
    fn worktree_count_includes_the_main_and_linked_trees() {
        let (_root, main, worktree) = repo_with_worktree();
        // main + one linked worktree, counted the same from either tree.
        assert_eq!(worktree_count(main.to_str().unwrap()), 2);
        assert_eq!(worktree_count(worktree.to_str().unwrap()), 2);
    }

    #[test]
    fn trunk_branch_prefers_the_remote_default_over_any_hardcoded_name() {
        let (_root, main, worktree) = repo_with_worktree();
        // Point origin/HEAD at a non-"main" default; trunk detection must read it
        // rather than assuming the conventional branch name.
        let head = read(main.to_str().unwrap(), &["rev-parse", "HEAD"]).expect("rev-parse");
        git_ok(&main, &["update-ref", "refs/remotes/origin/release", &head]);
        git_ok(
            &main,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/release",
            ],
        );

        assert_eq!(
            trunk_branch(worktree.to_str().unwrap()).as_deref(),
            Some("release")
        );
    }

    #[test]
    fn trunk_branch_falls_back_to_the_main_worktree_branch_without_a_remote() {
        let (_root, _main, worktree) = repo_with_worktree();
        // No remote, so origin/HEAD is unknown; the main worktree sits on `main`.
        assert_eq!(
            trunk_branch(worktree.to_str().unwrap()).as_deref(),
            Some("main")
        );
    }

    #[test]
    fn add_worktree_creates_a_linked_checkout_on_a_new_branch() {
        let (_root, main, _worktree) = repo_with_worktree();
        let created = add_worktree(&main, "wip/login").expect("add worktree");

        assert!(created.is_dir(), "worktree dir missing: {created:?}");
        // Slashes in the branch become dashes in the sibling directory name.
        assert!(
            created.ends_with("main-wip-login"),
            "unexpected path: {created:?}"
        );
        let created_string = created.to_str().expect("created path");
        let added = status(created_string);
        assert_eq!(added.branch.as_deref(), Some("wip/login"));
    }

    #[test]
    fn branch_cas_leaves_a_repointed_branch_intact() {
        let (_root, main, _worktree) = repo_with_worktree();
        let target =
            capture_branch_removal_target(main.to_str().unwrap(), "main").expect("capture branch");
        git_ok(&main, &["commit", "--allow-empty", "-q", "-m", "repoint"]);

        let result = delete_branch_if_unchanged(main.to_str().unwrap(), &target);
        assert!(
            result.is_err(),
            "a repointed branch must fail the compare-and-delete"
        );
        assert_eq!(
            read(main.to_str().unwrap(), &["rev-parse", "refs/heads/main"]).as_deref(),
            read(main.to_str().unwrap(), &["rev-parse", "HEAD"]).as_deref(),
            "the repointed branch must remain attached to the replacement commit"
        );
    }
    #[test]
    fn service_creates_and_removes_a_worktree_without_session_lifecycle() {
        let (root, main, _worktree) = repo_with_worktree();
        let claims = DirectoryClaims::at(
            root.path().join("claims"),
            ClaimOwner::current("service").expect("owner"),
        )
        .expect("claims");
        let service = WorktreeService::new(
            claims,
            InstanceRef {
                instance_id: "service".to_owned(),
                generation: 1,
            },
        );

        let created = service
            .create(WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "service-worktree".to_owned(),
                managed_by_bootty: true,
                caller: "test".to_owned(),
            })
            .expect("create");
        assert!(created.worktree.is_linked());
        assert!(created.worktree.managed_by_bootty);
        assert_eq!(
            created
                .worktree
                .created_by
                .as_ref()
                .map(|creator| creator.caller.as_str()),
            Some("test")
        );

        service
            .remove(WorktreeRemoveRequest {
                worktree: created.worktree.clone(),
                force: false,
                requester_session: None,
                confirmation: None,
            })
            .expect("remove");
        assert!(!created.worktree.path.exists());
    }

    #[test]
    fn service_hydrates_persisted_worktree_creator_metadata_by_identity() {
        let (root, main, _worktree) = repo_with_worktree();
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("creator").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "creator".to_owned(),
                generation: 7,
            },
        );
        let created = service
            .create(WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "persisted-metadata".to_owned(),
                managed_by_bootty: true,
                caller: "automation".to_owned(),
            })
            .expect("create");
        let reopened = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("reopened-claims"),
                ClaimOwner::current("reader").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "reader".to_owned(),
                generation: 1,
            },
        );

        let retrieved = reopened.get(&created.worktree.path).expect("get");
        let listed = reopened
            .list(&main)
            .expect("list")
            .into_iter()
            .find(|detail| detail.worktree.same_identity(&created.worktree))
            .expect("created worktree in inventory");

        for worktree in [&retrieved.worktree, &listed.worktree] {
            assert!(worktree.managed_by_bootty);
            let creator = worktree.created_by.as_ref().expect("persisted creator");
            assert_eq!(creator.instance.instance_id, "creator");
            assert_eq!(creator.instance.generation, 7);
            assert_eq!(creator.caller, "automation");
        }
    }

    #[test]
    fn service_list_skips_prunable_worktrees_without_hiding_live_inventory() {
        let (root, main, stale) = repo_with_worktree();
        let main_worktree = DirectoryRef::resolve(&main)
            .expect("resolve main")
            .worktree
            .expect("main worktree");
        let repository = main_worktree.repository.clone();
        fs::remove_dir_all(&stale).expect("remove stale checkout");

        let porcelain = read_git_dir_bytes(
            &repository.common_git_dir,
            &["worktree", "list", "--porcelain", "-z"],
        )
        .expect("list worktrees");
        assert!(
            worktree_paths_from_porcelain(&porcelain)
                .iter()
                .any(|path| !path.exists()),
            "fixture must expose the prunable registration"
        );

        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("inventory").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "inventory".to_owned(),
                generation: 1,
            },
        );

        let inventory = service
            .list_repository(&repository)
            .expect("stale entry must not abort inventory");
        assert_eq!(inventory.len(), 1);
        assert!(inventory[0].worktree.same_identity(&main_worktree));
    }

    #[test]
    fn service_leaves_created_worktree_when_post_add_resolution_has_no_identity() {
        let (root, main, _worktree) = repo_with_worktree();
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("rollback").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "rollback".to_owned(),
                generation: 1,
            },
        );
        let created_path = Arc::new(std::sync::Mutex::new(None));
        let captured_path = Arc::clone(&created_path);

        let result = service.create_after_add(
            WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "rollback-on-resolve-failure".to_owned(),
                managed_by_bootty: true,
                caller: "test".to_owned(),
            },
            move |repository, branch| {
                let created = add_worktree(repository, branch)?;

                *captured_path.lock().expect("created path lock") = Some(created.clone());
                Ok(created)
            },
            |created| {
                Ok(DirectoryRef {
                    canonical_path: created.to_path_buf(),
                    repository: None,
                    worktree: None,
                })
            },
            persist_worktree_metadata,
        );

        assert!(matches!(
            result,
            Err(WorktreeServiceError::Git { ref message })
                if message.contains("could not safely roll back newly added worktree")
                    && message.contains("could not establish the created worktree identity")
        ));
        let created = created_path
            .lock()
            .expect("created path lock")
            .clone()
            .expect("created path");
        assert!(
            created.exists(),
            "without an exact identity, rollback must leave the created worktree alone"
        );
        let inventory =
            read(main.to_str().expect("main path"), &["worktree", "list"]).expect("worktree list");
        assert!(
            inventory.contains("rollback-on-resolve-failure"),
            "unidentified worktree must remain registered: {inventory}"
        );
    }

    #[test]
    fn service_rolls_back_known_created_worktree_after_post_add_failure() {
        let (root, main, _worktree) = repo_with_worktree();
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("rollback-known").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "rollback-known".to_owned(),
                generation: 1,
            },
        );
        let created_path = root.path().join("rollback-known");
        let add_path = created_path.clone();

        let result = service.create_after_add(
            WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "rollback-known".to_owned(),
                managed_by_bootty: true,
                caller: "test".to_owned(),
            },
            move |repository, branch| add_worktree_at(repository, branch, &add_path),
            resolve_created_worktree,
            injected_post_add_failure,
        );

        assert!(matches!(
            result,
            Err(WorktreeServiceError::Git { ref message }) if message == "injected post-add failure"
        ));
        assert!(
            !created_path.exists(),
            "a known created worktree must be removed after the locked recheck"
        );
        let inventory =
            read(main.to_str().expect("main path"), &["worktree", "list"]).expect("worktree list");
        assert!(
            !inventory.contains("rollback-known"),
            "locked rollback must remove the worktree registration: {inventory}"
        );
        let branches = read(
            main.to_str().expect("main path"),
            &["branch", "--list", "rollback-known"],
        )
        .expect("branch list");
        assert!(
            branches.trim().is_empty(),
            "rollback must remove the branch created with the worktree: {branches}"
        );
    }
    #[test]
    fn service_rollback_leaves_repointed_created_worktree_intact() {
        let (root, main, _worktree) = repo_with_worktree();
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("rollback-branch-repoint").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "rollback-branch-repoint".to_owned(),
                generation: 1,
            },
        );
        let created_path = root.path().join("rollback-branch-repoint");
        let add_path = created_path.clone();
        let branch_main = main.clone();
        let result = service.create_after_add(
            WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "rollback-branch-repoint".to_owned(),
                managed_by_bootty: true,
                caller: "test".to_owned(),
            },
            move |repository, branch| add_worktree_at(repository, branch, &add_path),
            resolve_created_worktree,
            move |worktree| {
                git_ok(
                    &branch_main,
                    &["commit", "--allow-empty", "-q", "-m", "repoint"],
                );
                let oid = read(
                    branch_main.to_str().expect("main path"),
                    &["rev-parse", "HEAD"],
                )
                .expect("replacement commit");
                let reference = "refs/heads/rollback-branch-repoint";
                git_ok(&branch_main, &["update-ref", reference, oid.as_str()]);
                injected_post_add_failure(worktree)
            },
        );

        assert!(matches!(
            result,
            Err(WorktreeServiceError::Git { ref message })
                if message.contains("could not safely roll back")
                    && message.contains("identity changed")
        ));
        assert!(
            created_path.exists(),
            "a changed created worktree identity must survive partial rollback"
        );
        let branches = read(
            main.to_str().expect("main path"),
            &["branch", "--list", "rollback-branch-repoint"],
        )
        .expect("branch list");
        assert!(
            branches.contains("rollback-branch-repoint"),
            "a concurrently repointed branch must survive: {branches}"
        );
    }

    #[test]
    fn service_rollback_does_not_remove_replacement_after_locked_recheck() {
        let (root, main, _worktree) = repo_with_worktree();
        let created_path = root.path().join("rollback-race");
        let liveness = Arc::new(ReplaceWorktreeDuringLockedRecheck {
            armed: AtomicBool::new(false),
            worktree: created_path.clone(),
        });
        let claims = DirectoryClaims::at_with_liveness(
            root.path().join("claims"),
            ClaimOwner::current("rollback-race").expect("owner"),
            liveness.clone(),
        )
        .expect("claims");
        let service = WorktreeService::new(
            claims,
            InstanceRef {
                instance_id: "rollback-race".to_owned(),
                generation: 1,
            },
        );
        let add_path = created_path.clone();
        liveness.armed.store(true, Ordering::SeqCst);

        let result = service.create_after_add(
            WorktreeCreateRequest {
                repository_path: main.clone(),
                branch: "rollback-race".to_owned(),
                managed_by_bootty: true,
                caller: "test".to_owned(),
            },
            move |repository, branch| add_worktree_at(repository, branch, &add_path),
            resolve_created_worktree,
            injected_post_add_failure,
        );

        assert!(matches!(
            result,
            Err(WorktreeServiceError::Git { ref message })
                if message.contains("could not safely roll back newly added worktree")
        ));
        assert!(
            created_path.exists(),
            "the replacement path must survive a failed identity recheck"
        );
        let replacement = DirectoryRef::resolve(&created_path)
            .expect("resolve replacement")
            .worktree
            .expect("replacement worktree");
        let main_repository = DirectoryRef::resolve(&main)
            .expect("resolve main")
            .repository
            .expect("main repository");
        assert!(
            !replacement.repository.same_identity(&main_repository),
            "the replacement must belong to a different repository"
        );
    }

    #[test]
    fn service_rechecks_identity_inside_claim_lock_before_removal() {
        let (root, main, worktree) = repo_with_worktree();
        let liveness = Arc::new(ReplaceWorktreeDuringLockedRecheck {
            armed: AtomicBool::new(false),
            worktree: worktree.clone(),
        });
        let claims = DirectoryClaims::at_with_liveness(
            root.path().join("claims"),
            ClaimOwner::current("race").expect("owner"),
            liveness.clone(),
        )
        .expect("claims");
        let service = WorktreeService::new(
            claims,
            InstanceRef {
                instance_id: "race".to_owned(),
                generation: 1,
            },
        );
        let stale = service.get(&worktree).expect("resolve original").worktree;
        liveness.armed.store(true, Ordering::SeqCst);

        let result = service.remove(WorktreeRemoveRequest {
            worktree: stale,
            force: true,
            requester_session: None,
            confirmation: None,
        });

        assert!(matches!(
            result,
            Err(WorktreeServiceError::IdentityChanged { .. })
        ));
        assert!(
            worktree.exists(),
            "the replacement path must not be deleted after the final re-resolve"
        );
        let replacement = DirectoryRef::resolve(&worktree)
            .expect("resolve replacement")
            .worktree
            .expect("replacement repository");
        assert!(
            !replacement.repository.same_identity(
                &DirectoryRef::resolve(&main)
                    .expect("resolve main")
                    .repository
                    .expect("main repository")
            )
        );
    }

    #[test]
    fn service_remove_keeps_a_replacement_from_a_different_repository() {
        let (root, _, worktree) = repo_with_worktree();
        let service = WorktreeService::new(
            DirectoryClaims::at(
                root.path().join("claims"),
                ClaimOwner::current("replace-after-recheck").expect("owner"),
            )
            .expect("claims"),
            InstanceRef {
                instance_id: "replace-after-recheck".to_owned(),
                generation: 1,
            },
        );
        let stale = service.get(&worktree).expect("resolve original").worktree;
        let original_repository = stale.repository.clone();
        let replacement_main = root.path().join("replacement-main");
        let replacement_path = worktree.clone();
        let replacement_identity = Arc::new(std::sync::Mutex::new(None));
        let captured_identity = Arc::clone(&replacement_identity);

        let result = service.remove_after_final_recheck(
            WorktreeRemoveRequest {
                worktree: stale,
                force: true,
                requester_session: None,
                confirmation: None,
            },
            move |verified, force| {
                remove_worktree_from_repository(verified, true).expect("remove original worktree");
                fs::create_dir(&replacement_main).expect("create replacement main");
                git_ok(&replacement_main, &["init", "-q", "-b", "main"]);
                git_ok(&replacement_main, &["config", "user.email", "t@t.test"]);
                git_ok(&replacement_main, &["config", "user.name", "tester"]);
                fs::write(replacement_main.join("README"), "replacement")
                    .expect("write replacement");
                git_ok(&replacement_main, &["add", "."]);
                git_ok(&replacement_main, &["commit", "-q", "-m", "init"]);
                git_ok(
                    &replacement_main,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        "replacement",
                        replacement_path
                            .to_str()
                            .expect("UTF-8 replacement worktree path"),
                    ],
                );
                let replacement = DirectoryRef::resolve(&replacement_path)
                    .expect("resolve replacement")
                    .worktree
                    .expect("replacement worktree");
                *captured_identity.lock().expect("replacement identity lock") = Some(replacement);

                remove_worktree_from_repository(verified, force)
            },
        );

        assert!(
            result.is_err(),
            "the original repository must reject the no-longer-registered worktree"
        );
        let replacement = replacement_identity
            .lock()
            .expect("replacement identity lock")
            .clone()
            .expect("replacement identity");
        assert!(
            !replacement.repository.same_identity(&original_repository),
            "the replacement must belong to a different repository"
        );
        let surviving = DirectoryRef::resolve(&worktree)
            .expect("resolve surviving replacement")
            .worktree
            .expect("surviving replacement worktree");
        assert!(
            surviving.same_identity(&replacement),
            "removal must not switch to and delete the replacement worktree"
        );
    }
}
