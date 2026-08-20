use std::{fs, path::Path, process::Command};

use bootty_app::{
    git::{
        WorktreeStatus, add_worktree, delete_branch, detach_head, head_branch, remove_worktree,
        status, suggested_session_name, trunk_branch, worktree_count,
    },
    strings::session_name_for_path,
};

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

fn repo_with_worktree() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("temporary repository root");
    let main = root.path().join("main");
    fs::create_dir(&main).expect("create main worktree");
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
    (root, main, worktree)
}

#[test]
fn branch_labels_preserve_slashed_names_and_report_detached_heads() {
    let (_root, main, worktree) = repo_with_worktree();
    git_ok(&worktree, &["checkout", "-q", "-b", "one/two/three"]);
    let nested = worktree.join("nested");
    fs::create_dir(&nested).expect("create nested directory");
    assert_eq!(head_branch(main.to_str().unwrap()).as_deref(), Some("main"));
    assert_eq!(
        head_branch(nested.to_str().unwrap()).as_deref(),
        Some("one/two/three")
    );

    let commit = git_read(&main, &["rev-parse", "HEAD"]);
    git_ok(&main, &["checkout", "-q", "--detach"]);
    assert_eq!(
        head_branch(main.to_str().unwrap()),
        Some(format!("detached {}", &commit[..7]))
    );
}

#[test]
fn git_queries_are_safe_outside_a_repository() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().to_str().unwrap();
    assert_eq!(head_branch(path), None);
    assert_eq!(status(path), WorktreeStatus::default());
    assert_eq!(suggested_session_name(path), session_name_for_path(path));
}

#[test]
fn suggested_names_group_linked_worktrees_by_repository_and_branch() {
    let (_root, main, worktree) = repo_with_worktree();
    let nested = worktree.join("nested");
    fs::create_dir(&nested).expect("create nested directory");

    assert_eq!(suggested_session_name(main.to_str().unwrap()), "main/main");
    assert_eq!(
        suggested_session_name(nested.to_str().unwrap()),
        "main/feature"
    );
}

#[test]
fn detached_worktrees_use_their_directory_as_the_session_leaf() {
    let (root, main, _worktree) = repo_with_worktree();
    let detached = root.path().join("detached");
    git_ok(
        &main,
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

#[test]
fn status_distinguishes_main_linked_and_dirty_worktrees() {
    let (_root, main, worktree) = repo_with_worktree();
    let main_status = status(main.to_str().unwrap());
    assert!(main_status.in_repo);
    assert!(!main_status.is_linked_worktree);
    assert_eq!(main_status.branch.as_deref(), Some("main"));

    let linked = status(worktree.to_str().unwrap());
    assert!(linked.in_repo && linked.is_linked_worktree);
    assert_eq!(linked.branch.as_deref(), Some("feature"));
    assert!(!linked.dirty);

    fs::write(worktree.join("scratch"), "wip").expect("write untracked file");
    assert!(status(worktree.to_str().unwrap()).dirty);
}

#[test]
fn detach_preserves_the_worktree_and_current_commit() {
    let (_root, _main, worktree) = repo_with_worktree();
    let before = git_read(&worktree, &["rev-parse", "HEAD"]);
    detach_head(worktree.to_str().unwrap()).expect("detach HEAD");

    assert!(status(worktree.to_str().unwrap()).branch.is_none());
    assert!(worktree.exists());
    assert_eq!(git_read(&worktree, &["rev-parse", "HEAD"]), before);
}

#[test]
fn worktree_removal_updates_the_repository() {
    let (_root, main, worktree) = repo_with_worktree();
    remove_worktree(worktree.to_str().unwrap(), false).expect("remove worktree");
    assert!(!worktree.exists());
    assert!(!git_read(&main, &["worktree", "list"]).contains("wt"));
}

#[test]
fn forced_branch_deletion_removes_unmerged_work() {
    let (_root, main, worktree) = repo_with_worktree();
    fs::write(worktree.join("feature.txt"), "work").expect("write branch file");
    git_ok(&worktree, &["add", "."]);
    git_ok(&worktree, &["commit", "-q", "-m", "feature work"]);
    remove_worktree(worktree.to_str().unwrap(), false).expect("remove worktree");
    delete_branch(main.to_str().unwrap(), "feature", true).expect("delete branch");
    assert!(git_read(&main, &["branch", "--list", "feature"]).is_empty());
}

#[test]
fn worktree_count_includes_main_and_linked_checkouts() {
    let (_root, main, worktree) = repo_with_worktree();
    assert_eq!(worktree_count(main.to_str().unwrap()), 2);
    assert_eq!(worktree_count(worktree.to_str().unwrap()), 2);
}

#[test]
fn trunk_uses_the_remote_default_then_falls_back_to_main_worktree() {
    let (_root, main, worktree) = repo_with_worktree();
    assert_eq!(
        trunk_branch(worktree.to_str().unwrap()).as_deref(),
        Some("main")
    );

    let head = git_read(&main, &["rev-parse", "HEAD"]);
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
fn add_worktree_creates_a_sibling_for_the_new_branch() {
    let (_root, main, _worktree) = repo_with_worktree();
    let created = add_worktree(main.to_str().unwrap(), "wip/login").expect("add worktree");
    assert!(Path::new(&created).is_dir());
    assert!(created.ends_with("main-wip-login"));
    let added = status(&created);
    assert!(added.is_linked_worktree);
    assert_eq!(added.branch.as_deref(), Some("wip/login"));
}
