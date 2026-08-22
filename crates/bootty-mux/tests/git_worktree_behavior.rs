use std::{fs, path::Path, process::Command};

use assert_fs::{TempDir, prelude::*};
use bootty_mux::project::{
    WorktreeStatus, add_worktree, delete_branch, detach_head, remove_worktree, status,
    suggested_session_name, trunk_branch, worktree_count,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

fn git_ok(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_read(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("run git");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Repository {
    root: TempDir,
    main: std::path::PathBuf,
    worktree: std::path::PathBuf,
}

#[fixture]
fn repository() -> Repository {
    let root = TempDir::new().expect("temporary repository root");
    let main = root.path().join("main");
    root.child("main")
        .create_dir_all()
        .expect("create main worktree");
    git_ok(&main, &["init", "-q", "-b", "main"]);
    git_ok(&main, &["config", "user.email", "test@bootty.dev"]);
    git_ok(&main, &["config", "user.name", "Bootty Test"]);
    fs::write(main.join("README"), "hello").expect("write initial file");
    git_ok(&main, &["add", "."]);
    git_ok(&main, &["commit", "-q", "-m", "init"]);
    let worktree = root.path().join("wt");
    git_ok(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature",
            worktree.to_str().expect("UTF-8 worktree path"),
        ],
    );
    Repository {
        root,
        main,
        worktree,
    }
}

#[test]
fn git_queries_are_safe_outside_a_repository() {
    let directory = TempDir::new().expect("temporary directory");
    let path = directory.path().to_str().unwrap();
    let expected = directory
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap()
        .trim_end_matches(".git");
    assert_eq!(status(path), WorktreeStatus::default());
    assert_eq!(suggested_session_name(path), expected);
}

#[rstest]
fn suggested_names_group_linked_worktrees_by_repository_and_branch(repository: Repository) {
    let nested = repository.worktree.join("nested");
    fs::create_dir(&nested).expect("create nested directory");

    assert_eq!(
        suggested_session_name(repository.main.to_str().unwrap()),
        "main/main"
    );
    assert_eq!(
        suggested_session_name(nested.to_str().unwrap()),
        "main/feature"
    );
}

#[rstest]
fn detached_worktrees_use_their_directory_as_the_session_leaf(repository: Repository) {
    let detached = repository.root.path().join("detached");
    git_ok(
        &repository.main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().unwrap(),
        ],
    );
    assert_eq!(
        suggested_session_name(detached.to_str().unwrap()),
        "main/detached"
    );
}

#[rstest]
fn status_distinguishes_main_linked_and_dirty_worktrees(repository: Repository) {
    let main_status = status(repository.main.to_str().unwrap());
    assert!(main_status.in_repo);
    assert!(!main_status.is_linked_worktree);
    assert_eq!(main_status.branch.as_deref(), Some("main"));

    let linked = status(repository.worktree.to_str().unwrap());
    assert!(linked.in_repo && linked.is_linked_worktree);
    assert_eq!(linked.branch.as_deref(), Some("feature"));
    assert!(!linked.dirty);

    fs::write(repository.worktree.join("scratch"), "wip").expect("write untracked file");
    assert!(status(repository.worktree.to_str().unwrap()).dirty);
}

#[rstest]
fn detach_preserves_the_worktree_and_current_commit(repository: Repository) {
    let before = git_read(&repository.worktree, &["rev-parse", "HEAD"]);
    detach_head(repository.worktree.to_str().unwrap()).expect("detach HEAD");

    assert!(
        status(repository.worktree.to_str().unwrap())
            .branch
            .is_none()
    );
    assert_eq!(
        git_read(&repository.worktree, &["rev-parse", "HEAD"]),
        before
    );
}

#[rstest]
fn forced_branch_deletion_removes_unmerged_work(repository: Repository) {
    fs::write(repository.worktree.join("feature.txt"), "work").expect("write branch file");
    git_ok(&repository.worktree, &["add", "."]);
    git_ok(
        &repository.worktree,
        &["commit", "-q", "-m", "feature work"],
    );
    remove_worktree(repository.worktree.to_str().unwrap(), false).expect("remove worktree");
    assert!(!repository.worktree.exists());
    assert!(!git_read(&repository.main, &["worktree", "list"]).contains("wt"));
    delete_branch(repository.main.to_str().unwrap(), "feature", true).expect("delete branch");
    assert!(git_read(&repository.main, &["branch", "--list", "feature"]).is_empty());
}

#[rstest]
fn worktree_count_includes_main_and_linked_checkouts(repository: Repository) {
    assert_eq!(worktree_count(repository.main.to_str().unwrap()), 2);
    assert_eq!(worktree_count(repository.worktree.to_str().unwrap()), 2);
}

#[rstest]
fn trunk_uses_the_remote_default_then_falls_back_to_main_worktree(repository: Repository) {
    assert_eq!(
        trunk_branch(repository.worktree.to_str().unwrap()).as_deref(),
        Some("main")
    );

    let head = git_read(&repository.main, &["rev-parse", "HEAD"]);
    git_ok(
        &repository.main,
        &["update-ref", "refs/remotes/origin/release", &head],
    );
    git_ok(
        &repository.main,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/release",
        ],
    );
    assert_eq!(
        trunk_branch(repository.worktree.to_str().unwrap()).as_deref(),
        Some("release")
    );
}

#[rstest]
fn add_worktree_creates_a_sibling_for_the_new_branch(repository: Repository) {
    let created =
        add_worktree(repository.main.to_str().unwrap(), "wip/login").expect("add worktree");
    assert!(created.ends_with("main-wip-login"));
    let added = status(&created);
    assert!(added.is_linked_worktree);
    assert_eq!(added.branch.as_deref(), Some("wip/login"));
}
