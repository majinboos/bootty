use std::process::Command;

use rusqlite::{Connection, params};

fn run_daemon(
    daemon: &str,
    config_root: &std::path::Path,
    state_root: &std::path::Path,
    identity: Option<&str>,
    args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(daemon);
    command
        .env_remove("BOOTTY_DAEMON_STATE")
        .env_remove(bootty_identity::APPLICATION_IDENTITY_ENV)
        .env("XDG_CONFIG_HOME", config_root)
        .env("XDG_STATE_HOME", state_root)
        .env("HOME", config_root)
        .env("USERPROFILE", config_root);
    if let Some(identity) = identity {
        command.args(["--application-identity", identity]);
    }
    command.args(args).output().expect("run daemon")
}

fn seed_legacy_catalog(
    path: &std::path::Path,
    remote_id: &str,
    name: &str,
    stored_backend: &str,
    session_name: &str,
) {
    std::fs::create_dir_all(path.parent().expect("legacy catalog parent")).expect("legacy parent");
    let connection = Connection::open(path).expect("legacy catalog");
    connection
        .execute_batch(
            "CREATE TABLE workspace_spaces (
                 id INTEGER PRIMARY KEY,
                 remote_id TEXT,
                 name TEXT NOT NULL,
                 position INTEGER NOT NULL
             );
             CREATE TABLE workspace_bindings (
                 id INTEGER PRIMARY KEY,
                 space_id INTEGER NOT NULL,
                 backend TEXT NOT NULL,
                 remote TEXT
             );
             CREATE TABLE workspace_sessions (
                 binding_id INTEGER NOT NULL,
                 name TEXT NOT NULL,
                 position INTEGER NOT NULL
             );",
        )
        .expect("legacy schema");
    connection
        .execute(
            "INSERT INTO workspace_spaces (id, remote_id, name, position)
             VALUES (1, ?1, ?2, 0)",
            params![remote_id, name],
        )
        .expect("legacy space");
    connection
        .execute(
            "INSERT INTO workspace_bindings (id, space_id, backend, remote)
             VALUES (1, 1, ?1, '{\"source\":\"local\"}')",
            [stored_backend],
        )
        .expect("legacy binding");
    connection
        .execute(
            "INSERT INTO workspace_sessions (binding_id, name, position)
             VALUES (1, ?1, 0)",
            [session_name],
        )
        .expect("legacy session");
}

fn migration_marker(path: &std::path::Path) -> i64 {
    Connection::open(path)
        .expect("open daemon catalog")
        .query_row(
            "SELECT COUNT(*) FROM daemon_metadata
             WHERE key = 'legacy_catalog_migrated'",
            [],
            |row| row.get(0),
        )
        .expect("read migration marker")
}

fn destination_space_count(path: &std::path::Path) -> i64 {
    Connection::open(path)
        .expect("open daemon catalog")
        .query_row("SELECT COUNT(*) FROM remote_spaces", [], |row| row.get(0))
        .expect("read destination Space count")
}

fn destination_sessions(path: &std::path::Path, space_id: &str) -> Vec<String> {
    let connection = Connection::open(path).expect("open daemon catalog");
    let mut statement = connection
        .prepare(
            "SELECT session_name FROM remote_space_sessions
             WHERE space_id = ?1 ORDER BY position",
        )
        .expect("prepare destination session query");
    statement
        .query_map([space_id], |row| row.get(0))
        .expect("read destination sessions")
        .collect::<rusqlite::Result<_>>()
        .expect("collect destination sessions")
}

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
fn cli_rejects_malformed_options_and_application_identities() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    for (arguments, expected) in [
        (
            vec!["remote-space", "create", "--backend", "tmux"],
            "Error: missing option --name\n",
        ),
        (
            vec![
                "remote-space",
                "create",
                "--name",
                "Lab",
                "--name",
                "Prod",
                "--backend",
                "tmux",
            ],
            "Error: duplicate option --name\n",
        ),
        (
            vec!["remote-space", "create", "--name"],
            "Error: options require values\n",
        ),
    ] {
        let output = run_daemon(daemon, &config, &state, None, &arguments);
        assert!(!output.status.success());
        assert_eq!(String::from_utf8(output.stderr).expect("UTF-8"), expected);
    }

    for (arguments, expected) in [
        (
            vec!["--application-identity"],
            "Error: --application-identity requires a value\n",
        ),
        (
            vec!["--application-identity", "other", "remote-ping"],
            "Error: unknown application identity \"other\"\n",
        ),
    ] {
        let output = Command::new(daemon)
            .args(arguments)
            .output()
            .expect("run daemon");
        assert!(!output.status.success());
        assert_eq!(String::from_utf8(output.stderr).expect("UTF-8"), expected);
    }
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
fn production_and_development_catalogs_use_separate_default_state_paths() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    for (identity, name) in [
        (Some("bootty"), "Production"),
        (Some("bootty-dev"), "Development"),
    ] {
        let created = run_daemon(
            daemon,
            &config,
            &state,
            identity,
            &[
                "remote-space",
                "create",
                "--name",
                name,
                "--backend",
                "tmux",
            ],
        );
        assert!(
            created.status.success(),
            "{}",
            String::from_utf8_lossy(&created.stderr)
        );
    }

    let production = state.join("bootty/daemon.sqlite");
    let development = state.join("bootty-dev/daemon.sqlite");
    assert!(production.is_file());
    assert!(development.is_file());
    assert_ne!(production, development);

    let production_list = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(production_list.status.success());
    let production_spaces: serde_json::Value =
        serde_json::from_slice(&production_list.stdout).expect("production catalog JSON");
    assert_eq!(production_spaces[0]["name"], "Production");

    let development_list = run_daemon(
        daemon,
        &config,
        &state,
        Some("bootty-dev"),
        &["remote-space", "list"],
    );
    assert!(development_list.status.success());
    let development_spaces: serde_json::Value =
        serde_json::from_slice(&development_list.stdout).expect("development catalog JSON");
    assert_eq!(development_spaces[0]["name"], "Development");
}

#[test]
fn inherited_local_identity_does_not_change_a_remote_command() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    let created = Command::new(daemon)
        .env_remove("BOOTTY_DAEMON_STATE")
        .env(bootty_identity::APPLICATION_IDENTITY_ENV, "bootty-dev")
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .args([
            "remote-space",
            "create",
            "--name",
            "Remote Production",
            "--backend",
            "tmux",
        ])
        .output()
        .expect("run remote command with inherited local identity");

    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(state.join("bootty/daemon.sqlite").is_file());
    assert!(!state.join("bootty-dev/daemon.sqlite").exists());
}

#[test]
fn explicit_daemon_state_override_is_exact_and_ignores_identity_namespace() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let override_path = directory.path().join("custom/catalog.sqlite");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    let created = Command::new(daemon)
        .env("BOOTTY_DAEMON_STATE", &override_path)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .args([
            "--application-identity",
            "bootty-dev",
            "remote-space",
            "create",
            "--name",
            "Override",
            "--backend",
            "tmux",
        ])
        .output()
        .expect("create overridden remote Space");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(override_path.is_file());
    assert!(!state.join("bootty-dev/daemon.sqlite").exists());

    let listed = Command::new(daemon)
        .env("BOOTTY_DAEMON_STATE", &override_path)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .args([
            "--application-identity",
            "bootty-dev",
            "remote-space",
            "list",
        ])
        .output()
        .expect("list overridden remote Spaces");
    assert!(listed.status.success());
    let spaces: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("catalog JSON");
    assert_eq!(spaces[0]["name"], "Override");

    let production = Command::new(daemon)
        .env("BOOTTY_DAEMON_STATE", &override_path)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_STATE_HOME", &state)
        .args(["remote-space", "list"])
        .output()
        .expect("list override with default Production identity");
    assert!(production.status.success());
    let spaces: serde_json::Value =
        serde_json::from_slice(&production.stdout).expect("production override JSON");
    assert_eq!(spaces[0]["name"], "Override");
}

#[test]
fn legacy_config_and_session_order_are_isolated_by_application_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    std::fs::create_dir_all(config.join("bootty")).expect("production config");
    std::fs::create_dir_all(config.join("bootty-dev")).expect("development config");
    std::fs::write(
        config.join("bootty/config.toml"),
        "[multiplexer]\nbackend = \"tmux\"\n",
    )
    .expect("production config file");
    std::fs::write(
        config.join("bootty-dev/config.toml"),
        "[multiplexer]\nbackend = \"zellij\"\n",
    )
    .expect("development config file");
    seed_legacy_catalog(
        &config.join("bootty/session-order.sqlite3"),
        "production-id",
        "Production legacy",
        "inherit",
        "production-session",
    );
    seed_legacy_catalog(
        &config.join("bootty-dev/session-order.sqlite3"),
        "development-id",
        "Development legacy",
        "inherit",
        "development-session",
    );

    let production = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(
        production.status.success(),
        "{}",
        String::from_utf8_lossy(&production.stderr)
    );
    let production_spaces: serde_json::Value =
        serde_json::from_slice(&production.stdout).expect("production legacy JSON");
    assert_eq!(production_spaces[0]["name"], "Production legacy");
    assert_eq!(production_spaces[0]["backend"], "tmux");

    let production_state = state.join("bootty/daemon.sqlite");
    assert_eq!(migration_marker(&production_state), 1);
    let production_bytes = std::fs::read(&production_state).expect("production state");

    let development = run_daemon(
        daemon,
        &config,
        &state,
        Some("bootty-dev"),
        &["remote-space", "list"],
    );
    assert!(
        development.status.success(),
        "{}",
        String::from_utf8_lossy(&development.stderr)
    );
    let development_spaces: serde_json::Value =
        serde_json::from_slice(&development.stdout).expect("development legacy JSON");
    assert_eq!(development_spaces[0]["name"], "Development legacy");
    assert_eq!(development_spaces[0]["backend"], "zellij");

    assert_eq!(migration_marker(&production_state), 1);
    assert_eq!(
        std::fs::read(&production_state).expect("production state"),
        production_bytes
    );
    assert_eq!(migration_marker(&state.join("bootty-dev/daemon.sqlite")), 1);
}

#[test]
fn corrupt_config_keeps_legacy_import_retryable_until_repaired() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let config_path = config.join("bootty/config.toml");
    let legacy_path = config.join("bootty/session-order.sqlite3");
    let destination = state.join("bootty/daemon.sqlite");

    std::fs::create_dir_all(config_path.parent().expect("config parent")).expect("config dir");
    std::fs::write(&config_path, "[multiplexer\nbackend = \"tmux\"\n").expect("corrupt config");
    seed_legacy_catalog(
        &legacy_path,
        "retry-id",
        "Retry legacy",
        "inherit",
        "retry-session",
    );

    let failed = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(!failed.status.success());
    assert!(destination.is_file());
    assert_eq!(destination_space_count(&destination), 0);
    assert_eq!(migration_marker(&destination), 0);

    std::fs::write(&config_path, "[multiplexer]\nbackend = \"tmux\"\n").expect("repair config");
    let repaired = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(
        repaired.status.success(),
        "{}",
        String::from_utf8_lossy(&repaired.stderr)
    );
    let spaces: serde_json::Value = serde_json::from_slice(&repaired.stdout).expect("catalog JSON");
    assert_eq!(spaces[0]["name"], "Retry legacy");
    assert_eq!(spaces[0]["backend"], "tmux");
    assert_eq!(migration_marker(&destination), 1);
}

#[test]
fn importer_selects_a_later_explicit_local_binding() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let legacy_path = config.join("bootty/session-order.sqlite3");
    let destination = state.join("bootty/daemon.sqlite");

    std::fs::create_dir_all(config.join("bootty")).expect("config dir");
    std::fs::write(
        config.join("bootty/config.toml"),
        "[multiplexer]\nbackend = \"tmux\"\n",
    )
    .expect("config");
    seed_legacy_catalog(
        &legacy_path,
        "later-local-id",
        "Later local",
        "tmux",
        "remote-binding-session",
    );
    let connection = Connection::open(&legacy_path).expect("legacy catalog");
    connection
        .execute(
            "UPDATE workspace_bindings SET remote = ?1 WHERE id = 1",
            [r#"{"source":"inline","value":{"host":"remote"}}"#],
        )
        .expect("remote first binding");
    connection
        .execute(
            "INSERT INTO workspace_bindings (id, space_id, backend, remote)
             VALUES (2, 1, 'zellij', '{\"source\":\"local\"}')",
            [],
        )
        .expect("later local binding");
    connection
        .execute(
            "INSERT INTO workspace_sessions (binding_id, name, position)
             VALUES (2, 'later-local-session', 0)",
            [],
        )
        .expect("later local session");

    let imported = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let spaces: serde_json::Value = serde_json::from_slice(&imported.stdout).expect("catalog JSON");
    assert_eq!(spaces[0]["backend"], "zellij");
    let id = spaces[0]["id"].as_str().expect("Space id");
    assert_eq!(
        destination_sessions(&destination, id),
        vec!["later-local-session".to_owned()]
    );
}

#[test]
fn importer_rejects_ambiguous_supported_local_bindings_without_a_marker() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let legacy_path = config.join("bootty/session-order.sqlite3");
    let destination = state.join("bootty/daemon.sqlite");

    std::fs::create_dir_all(config.join("bootty")).expect("config dir");
    std::fs::write(
        config.join("bootty/config.toml"),
        "[multiplexer]\nbackend = \"tmux\"\n",
    )
    .expect("config");
    seed_legacy_catalog(
        &legacy_path,
        "ambiguous-id",
        "Ambiguous legacy",
        "tmux",
        "ambiguous-session",
    );
    let connection = Connection::open(&legacy_path).expect("legacy catalog");
    connection
        .execute(
            "INSERT INTO workspace_bindings (id, space_id, backend, remote)
             VALUES (2, 1, 'zellij', '{\"source\":\"local\"}')",
            [],
        )
        .expect("second local binding");

    let failed = run_daemon(daemon, &config, &state, None, &["remote-space", "list"]);
    assert!(!failed.status.success());
    assert!(destination.is_file());
    assert_eq!(destination_space_count(&destination), 0);
    assert_eq!(migration_marker(&destination), 0);
}

#[test]
fn production_state_reopens_with_and_without_the_explicit_production_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");

    let created = run_daemon(
        daemon,
        &config,
        &state,
        None,
        &[
            "remote-space",
            "create",
            "--name",
            "Reopen",
            "--backend",
            "tmux",
        ],
    );
    assert!(created.status.success());

    let reopened = run_daemon(
        daemon,
        &config,
        &state,
        Some("bootty"),
        &["remote-space", "list"],
    );
    assert!(reopened.status.success());
    let spaces: serde_json::Value = serde_json::from_slice(&reopened.stdout).expect("catalog JSON");
    assert_eq!(spaces[0]["name"], "Reopen");
    assert!(state.join("bootty/daemon.sqlite").is_file());
}

#[test]
fn development_does_not_fall_back_to_a_production_legacy_catalog() {
    let directory = tempfile::tempdir().expect("tempdir");
    let config = directory.path().join("config");
    let state = directory.path().join("state");
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    seed_legacy_catalog(
        &config.join("bootty/session-order.sqlite3"),
        "production-id",
        "Production only",
        "tmux",
        "production-session",
    );

    let development = run_daemon(
        daemon,
        &config,
        &state,
        Some("bootty-dev"),
        &["remote-space", "list"],
    );
    assert!(
        development.status.success(),
        "{}",
        String::from_utf8_lossy(&development.stderr)
    );
    let spaces: serde_json::Value =
        serde_json::from_slice(&development.stdout).expect("development catalog JSON");
    assert_eq!(spaces, serde_json::json!([]));
    assert!(!state.join("bootty/daemon.sqlite").exists());
    assert_eq!(migration_marker(&state.join("bootty-dev/daemon.sqlite")), 1);
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
