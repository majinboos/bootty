use std::ffi::OsString;

use assert_fs::{TempDir, prelude::*};
use bootty_mux::project::{
    WorktreePickerEntry, discover_project_picker_entries, discover_worktree_picker_entries,
    home_dir_from, mark_occupied_worktrees, toggle_favorite_project_path,
};
use pretty_assertions::assert_eq;
use rstest::{fixture, rstest};

#[fixture]
fn home() -> TempDir {
    TempDir::new().expect("temporary home")
}

#[rstest]
#[case(None)]
#[case(Some(OsString::new()))]
fn absent_or_empty_home_is_not_a_project_root(#[case] value: Option<OsString>) {
    assert_eq!(
        home_dir_from(|name| (name == "HOME").then(|| value.clone()).flatten()),
        None
    );
}

#[rstest]
fn discovery_includes_visible_project_roots_and_excludes_hidden_entries(home: TempDir) {
    home.child("src/project").create_dir_all().expect("project");
    home.child("src/.hidden").create_dir_all().expect("hidden");
    home.child("dotfiles").create_dir_all().expect("dotfiles");

    let actual = discover_project_picker_entries(Some(home.path()));

    assert!(
        actual
            .iter()
            .any(|entry| entry.path.ends_with("src/project"))
            && actual.iter().any(|entry| entry.path.ends_with("dotfiles"))
            && !actual.iter().any(|entry| entry.path.ends_with(".hidden")),
        "{actual:#?}"
    );
}

#[rstest]
fn favorite_toggle_and_discovery_share_the_same_file(home: TempDir) {
    let project = home.child("projects/bootty");
    project.create_dir_all().expect("project");
    let project_path = project.path().to_string_lossy().into_owned();

    assert!(toggle_favorite_project_path(Some(home.path()), &project_path).expect("favorite"));
    let discovered = discover_project_picker_entries(Some(home.path()));
    assert!(
        discovered
            .iter()
            .any(|entry| entry.path == project_path && entry.favorite),
        "{discovered:#?}"
    );
    assert!(!toggle_favorite_project_path(Some(home.path()), &project_path).expect("unfavorite"));
    home.child(".config/tmux/.session-favorites").assert("");
}

#[rstest]
fn canonical_path_aliases_mark_the_same_worktree_occupied(home: TempDir) {
    let project = home.child("project");
    project.create_dir_all().expect("project");
    let path = project.path().to_string_lossy().into_owned();
    let alias = project
        .path()
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

    assert!(entries[0].occupied, "{entries:#?}");
}

#[rstest]
fn non_git_directory_offers_only_its_main_entry(home: TempDir) {
    let path = home.path().to_string_lossy().into_owned();
    let directory_name = home
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .expect("directory name");

    assert_eq!(
        discover_worktree_picker_entries(&path),
        vec![WorktreePickerEntry {
            label: format!("{directory_name} (main)"),
            path: Some(path),
            is_new: false,
            occupied: false,
        }]
    );
}

#[cfg(unix)]
#[rstest]
fn favorite_replacement_preserves_file_permissions(home: TempDir) {
    use std::{fs, os::unix::fs::PermissionsExt};

    let favorites = home.child(".config/tmux/.session-favorites");
    home.child(".config/tmux").create_dir_all().unwrap();
    favorites.write_str("~/projects/old\n").unwrap();
    fs::set_permissions(favorites.path(), fs::Permissions::from_mode(0o444)).unwrap();
    let project = home.child("projects/new");
    project.create_dir_all().unwrap();

    assert!(
        toggle_favorite_project_path(Some(home.path()), &project.path().to_string_lossy()).unwrap()
    );
    favorites.assert(format!("~/projects/old\n{}\n", project.path().display()).as_str());
    assert_eq!(
        fs::metadata(favorites.path()).unwrap().permissions().mode() & 0o777,
        0o444
    );
}
