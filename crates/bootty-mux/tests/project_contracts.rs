use std::process::Command;

use bootty_mux::project::{
    WorktreePickerEntry, discover_project_picker_entries, discover_worktree_picker_entries,
    home_dir, mark_occupied_worktrees, toggle_favorite_project_path,
};

const HELPER_ENV: &str = "BOOTTY_MUX_PROJECT_CONTRACT_HELPER";

#[cfg(not(windows))]
#[test]
fn an_empty_home_does_not_become_a_project_root() {
    let output = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "project_contract_helper", "--nocapture"])
        .env(HELPER_ENV, "empty-home")
        .env("HOME", "")
        .output()
        .expect("run isolated home contract");

    assert!(
        output.status.success(),
        "stdout={}; stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn project_contract_helper() {
    match std::env::var(HELPER_ENV).as_deref() {
        #[cfg(not(windows))]
        Ok("empty-home") => assert_eq!(home_dir(), None),
        _ => {}
    }
}

#[test]
fn project_discovery_uses_local_picker_heuristics() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path();
    std::fs::create_dir_all(home.join("src/project")).expect("project");
    std::fs::create_dir_all(home.join("src/.hidden")).expect("hidden");
    std::fs::create_dir_all(home.join("dotfiles")).expect("dotfiles");

    let entries = discover_project_picker_entries(Some(home));

    assert!(
        entries
            .iter()
            .any(|entry| entry.path.ends_with("src/project"))
    );
    assert!(entries.iter().any(|entry| entry.path.ends_with("dotfiles")));
    assert!(!entries.iter().any(|entry| entry.path.ends_with(".hidden")));
}

#[test]
fn favorite_paths_are_shared_by_toggle_and_discovery() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path();
    let project = home.join("projects/bootty");
    std::fs::create_dir_all(&project).expect("project");
    let project = project.to_string_lossy().into_owned();

    assert!(toggle_favorite_project_path(Some(home), &project).expect("favorite"));
    assert!(
        discover_project_picker_entries(Some(home))
            .iter()
            .any(|entry| entry.path == project && entry.favorite)
    );
    assert!(!toggle_favorite_project_path(Some(home), &project).expect("unfavorite"));
}

#[test]
fn canonical_worktree_aliases_are_marked_occupied() {
    let directory = tempfile::tempdir().expect("tempdir");
    let project = directory.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let path = project.to_string_lossy().into_owned();
    let alias = project
        .join("..")
        .join("project")
        .to_string_lossy()
        .into_owned();
    let mut entries = vec![WorktreePickerEntry {
        label: "project (main)".to_owned(),
        path: Some(path),
        is_new: false,
        occupied: false,
    }];

    mark_occupied_worktrees(&mut entries, &[alias]);

    assert!(entries[0].occupied);
}

#[test]
fn a_non_git_directory_offers_only_its_main_entry() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().to_string_lossy().into_owned();

    assert_eq!(
        discover_worktree_picker_entries(&path),
        vec![WorktreePickerEntry {
            label: format!(
                "{} (main)",
                directory
                    .path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("directory name")
            ),
            path: Some(path),
            is_new: false,
            occupied: false,
        }]
    );
}
