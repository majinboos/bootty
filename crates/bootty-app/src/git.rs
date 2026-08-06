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
    let path = new_worktree_path(repo_dir, branch)?;
    run(repo_dir, &["worktree", "add", "-b", branch, &path])?;
    Ok(path)
}

/// Sibling path for a new worktree: `<repo-parent>/<repo-name>-<branch-slug>`.
fn new_worktree_path(repo_dir: &str, branch: &str) -> Result<String, String> {
    let main = main_worktree(repo_dir).unwrap_or_else(|| repo_dir.to_owned());
    let main = Path::new(&main);
    let parent = main
        .parent()
        .ok_or_else(|| "repository has no parent directory".to_owned())?;
    let repo_name = main
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "could not read repository name".to_owned())?;
    let slug = branch.replace('/', "-");
    Ok(parent
        .join(format!("{repo_name}-{slug}"))
        .to_string_lossy()
        .into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

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
        remove_worktree(worktree.to_str().unwrap(), false).expect("remove worktree");
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
        remove_worktree(worktree.to_str().unwrap(), false).expect("remove worktree");

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
        let created = add_worktree(main.to_str().unwrap(), "wip/login").expect("add worktree");

        assert!(
            Path::new(&created).is_dir(),
            "worktree dir missing: {created}"
        );
        // Slashes in the branch become dashes in the sibling directory name.
        assert!(
            created.ends_with("main-wip-login"),
            "unexpected path: {created}"
        );
        let added = status(&created);
        assert!(added.is_linked_worktree);
        assert_eq!(added.branch.as_deref(), Some("wip/login"));
    }
}
