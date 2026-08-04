//! Thin wrappers over the `git` CLI for worktree-aware session cleanup
//! ("ditching"). Shelling out keeps us consistent with the tmux backend and the
//! dotfiles `mux` tool rather than taking on a libgit dependency.

use std::path::Path;
use std::process::Command;

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
