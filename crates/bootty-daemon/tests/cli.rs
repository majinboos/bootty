use std::process::Command;

#[test]
fn ping_reports_the_compatible_protocol_and_release() {
    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .arg("remote-ping")
        .output()
        .expect("ping daemon");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8"),
        format!(
            "{}:{}\n",
            bootty_mux::REMOTE_DAEMON_PROTOCOL_VERSION,
            env!("CARGO_PKG_VERSION")
        )
    );
}

#[test]
fn daemon_owns_a_persistent_remote_space_catalog() {
    let directory = tempfile::tempdir().expect("tempdir");
    let state = directory.path().join("daemon.sqlite");
    let config = directory.path().join("config");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    let created = Command::new(daemon)
        .env("BOOTTY_DAEMON_STATE", &state)
        .env("XDG_CONFIG_HOME", &config)
        .args([
            "remote-space",
            "create",
            "--name",
            "Lab",
            "--backend",
            "tmux",
        ])
        .output()
        .expect("create remote Space");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let listed = Command::new(daemon)
        .env("BOOTTY_DAEMON_STATE", &state)
        .env("XDG_CONFIG_HOME", &config)
        .args(["remote-space", "list"])
        .output()
        .expect("list remote Spaces");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let spaces: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("catalog JSON");

    assert_eq!(spaces[0]["catalog_version"], 3);
    assert_eq!(spaces[0]["name"], "Lab");
    assert_eq!(spaces[0]["backend"], "tmux");
}

#[test]
fn daemon_discovers_remote_projects_with_the_shared_heuristics() {
    let directory = tempfile::tempdir().expect("tempdir");
    let home = directory.path();
    std::fs::create_dir_all(home.join("src/project")).expect("project");
    std::fs::create_dir_all(home.join("src/.hidden")).expect("hidden");
    std::fs::create_dir_all(home.join("dotfiles")).expect("dotfiles");

    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["remote-project", "list"])
        .output()
        .expect("list remote projects");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let projects: Vec<bootty_mux::project::ProjectPickerEntry> =
        serde_json::from_slice(&output.stdout).expect("project JSON");
    assert!(
        projects
            .iter()
            .any(|project| project.path.ends_with("src/project"))
    );
    assert!(
        projects
            .iter()
            .any(|project| project.path.ends_with("dotfiles"))
    );
    assert!(
        !projects
            .iter()
            .any(|project| project.path.ends_with(".hidden"))
    );
}

#[test]
fn daemon_marks_canonical_worktree_aliases_as_occupied() {
    let directory = tempfile::tempdir().expect("tempdir");
    let project = directory.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let alias = project.join("..").join("project");

    let output = Command::new(env!("CARGO_BIN_EXE_bootty-daemon"))
        .args(["remote-worktree", "list", "--project"])
        .arg(&project)
        .arg("--open-cwd")
        .arg(alias)
        .output()
        .expect("list remote worktrees");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let worktrees: Vec<bootty_mux::project::WorktreePickerEntry> =
        serde_json::from_slice(&output.stdout).expect("worktree JSON");
    assert!(worktrees[0].occupied);
}
