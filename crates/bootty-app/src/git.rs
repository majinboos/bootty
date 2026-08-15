//! Thin wrappers over the `git` CLI for worktree-aware session cleanup
//! ("ditching"). Shelling out keeps us consistent with the tmux backend and the
//! dotfiles `mux` tool rather than taking on a libgit dependency.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

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
    let mut status = WorktreeStatus::default();
    if read(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return status;
    }
    status.in_repo = true;
    // A linked worktree's own git dir differs from the shared common dir.
    if let (Some(git_dir), Some(common)) = (
        read(cwd, &["rev-parse", "--absolute-git-dir"]),
        read(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ),
    ) {
        status.is_linked_worktree = git_dir != common;
    }
    status.branch = read(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    status.dirty = read(cwd, &["status", "--porcelain"]).is_some_and(|out| !out.is_empty());
    if let Some(count) =
        read(cwd, &["rev-list", "--count", "@{u}..HEAD"]).and_then(|out| out.parse().ok())
    {
        status.has_upstream = true;
        status.unpushed = count;
    }
    status
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
    read(cwd, &["worktree", "list", "--porcelain"])
        .map(|out| {
            out.lines()
                .filter(|line| line.starts_with("worktree "))
                .count()
        })
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

/// Remove the linked worktree rooted at `worktree_path`. Runs from the main
/// working tree so git doesn't refuse to remove the tree you're standing in;
/// `force` is required when the worktree is dirty.
pub fn remove_worktree(worktree_path: &str, force: bool) -> Result<(), String> {
    let main = main_worktree(worktree_path)
        .ok_or_else(|| "could not locate the main worktree".to_owned())?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_path);
    run(&main, &args)
}

/// Create a new linked worktree on a fresh `branch` off the repo containing
/// `repo_dir`, returning the new worktree path (a sibling dir named
/// `<repo>-<branch-slug>`).
pub fn add_worktree(repo_dir: &str, branch: &str) -> Result<String, String> {
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
    let status = status(&worktree);

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
}

/// Revision counter and filesystem roots per worktree, shared with the watcher's event thread.
type WorktreeRevisions = Arc<Mutex<HashMap<PathBuf, WorktreeWatch>>>;

fn worktree_revisions() -> &'static WorktreeRevisions {
    static REVISIONS: OnceLock<WorktreeRevisions> = OnceLock::new();
    REVISIONS.get_or_init(WorktreeRevisions::default)
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
    if let Ok(revisions) = worktree_revisions().lock()
        && let Some(watched) = revisions.get(&root)
    {
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
        return 0;
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
            let pointer = std::fs::read_to_string(&candidate).ok()?;
            let target = Path::new(pointer.trim().strip_prefix("gitdir:")?.trim()).to_path_buf();
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
    bootty_runtime::perf::record_subprocess("git read");
    let output = git_command(cwd, args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run(cwd: &str, args: &[&str]) -> Result<(), String> {
    bootty_runtime::perf::record_subprocess("git run");
    let output = git_command(cwd, args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn git_command(cwd: &str, args: &[&str]) -> Command {
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
