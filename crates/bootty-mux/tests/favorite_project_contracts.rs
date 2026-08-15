#[cfg(unix)]
#[test]
fn toggling_a_favorite_atomically_replaces_the_existing_file() {
    use std::{fs, os::unix::fs::PermissionsExt};

    let directory = tempfile::tempdir().expect("temporary directory");
    let home = directory.path();
    let favorites_file = home.join(".config/tmux/.session-favorites");
    fs::create_dir_all(favorites_file.parent().expect("favorites parent"))
        .expect("create favorites parent");
    fs::write(&favorites_file, "~/projects/old\n").expect("write favorites");
    fs::set_permissions(&favorites_file, fs::Permissions::from_mode(0o444))
        .expect("make favorites read-only");
    let project = home.join("projects/new");
    fs::create_dir_all(&project).expect("create project");

    assert!(
        bootty_mux::project::toggle_favorite_project_path(Some(home), &project.to_string_lossy())
            .expect("toggle favorite")
    );
    assert_eq!(
        fs::read_to_string(&favorites_file).expect("read favorites"),
        format!("~/projects/old\n{}\n", project.display())
    );
    assert_eq!(
        fs::metadata(&favorites_file)
            .expect("favorites metadata")
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
}
