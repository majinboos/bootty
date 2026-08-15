use bootty_app::{
    config::{MultiplexerBackendConfig, MultiplexerConfig, SshRemoteConfig},
    workspace::{
        DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceRepository,
    },
};
use tempfile::TempDir;

fn repository() -> (TempDir, WorkspaceRepository) {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let repository = WorkspaceRepository::open(&config_path).expect("workspace repository");
    (directory, repository)
}

#[test]
fn a_fresh_repository_has_one_default_space_and_binding() {
    let (_directory, repository) = repository();

    assert_eq!(repository.spaces().len(), 1);
    assert_eq!(repository.spaces()[0].name(), "Default Space");
    assert_eq!(repository.spaces()[0].bindings().len(), 1);
    assert_eq!(
        repository.spaces()[0].bindings()[0].name(),
        "Default Binding"
    );
    assert_eq!(
        repository.spaces()[0].bindings()[0].backend_override(),
        None
    );
}

#[test]
fn spaces_preserve_identity_appearance_and_remote_placement() {
    let (directory, mut repository) = repository();
    let remote = SshRemoteConfig {
        host: "devbox".to_owned(),
        user: Some("dev".to_owned()),
        port: Some(2222),
        program: "ssh".to_owned(),
        args: vec!["-i".to_owned(), "~/.ssh/devbox".to_owned()],
    };

    let created = repository
        .create_space(
            " Review ",
            "terminal",
            [1, 2, 3],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inline(remote.clone()),
            },
            &MultiplexerConfig::default(),
        )
        .expect("create space")
        .expect("valid space");

    let reopened = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("reopen repository");
    let stored = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == created.id())
        .expect("stored space");
    assert_eq!(stored.name(), "Review");
    assert_eq!(stored.icon(), "terminal");
    assert_eq!(stored.color(), [1, 2, 3]);
    assert!(stored.tint_sidebar());
    assert_eq!(
        stored.bindings()[0].remote_override(),
        &SpaceRemoteOverride::Inline(remote)
    );
}

#[test]
fn space_creation_rejects_blank_values_and_uniquifies_names() {
    let (_directory, mut repository) = repository();

    assert!(
        repository
            .create_space(
                "   ",
                DEFAULT_SPACE_ICON,
                DEFAULT_SPACE_COLOR,
                false,
                SpaceMuxOverride::default(),
                &MultiplexerConfig::default(),
            )
            .expect("reject blank name")
            .is_none()
    );
    repository
        .create_space(
            "Review",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            &MultiplexerConfig::default(),
        )
        .expect("create first space")
        .expect("valid first space");
    let duplicate = repository
        .create_space(
            "review",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            &MultiplexerConfig::default(),
        )
        .expect("create duplicate space")
        .expect("valid duplicate space");

    assert_eq!(duplicate.name(), "review 2");
}

#[test]
fn session_order_is_binding_scoped_and_persists() {
    let (directory, mut repository) = repository();
    let first_binding = repository.default_binding_id().expect("default binding");
    let second = repository
        .create_space(
            "Second",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            &MultiplexerConfig::default(),
        )
        .expect("create second space")
        .expect("second space");
    let second_binding = second.bindings()[0].mux_scope().binding_id();

    let mut first = repository.session_order(first_binding);
    let mut second = repository.session_order(second_binding);
    first.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]);
    second.add_session("other");
    assert!(first.move_session_before(
        "agents",
        Some("arc/migrations"),
        ["arc/migrations", "arc/readiness", "agents", "bootty"],
    ));

    repository = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("reopen repository");
    assert_eq!(
        repository.session_order(first_binding).sync_sessions([
            "arc/migrations",
            "arc/readiness",
            "agents",
            "bootty",
            "other"
        ]),
        vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
    );
    assert_eq!(
        repository
            .session_order(second_binding)
            .sync_sessions(["arc/migrations", "other"]),
        vec!["other"]
    );
}

#[test]
fn an_empty_backend_refresh_does_not_erase_session_order() {
    let (directory, repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let mut order = repository.session_order(binding);
    order.sync_sessions(["first", "second"]);
    assert!(order.move_session_before("second", Some("first"), ["first", "second"]));
    assert!(order.sync_sessions(std::iter::empty()).is_empty());

    let reopened = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("reopen repository");
    assert_eq!(
        reopened
            .session_order(binding)
            .sync_sessions(["first", "second"]),
        vec!["second", "first"]
    );
}

#[test]
fn generated_names_survive_backend_id_discovery_and_explicit_renames() {
    let (directory, repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let mut names = repository.session_names(binding);
    names.remember_generated("bootty/main", "/repo", "bootty/main", "bootty/main");

    let discovered = names
        .observe_session("$1", "bootty/main", "/repo")
        .expect("stored generated name");
    assert_eq!(discovered.session_id, "$1");
    names.mark_explicit("$1", "release", "release", "/repo");
    names.remember_generated("$1", "/repo", "project/feature", "project/feature");

    let reopened = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect("reopen repository");
    let record = reopened
        .session_names(binding)
        .observe_session("$1", "release", "/repo")
        .expect("stored explicit name");
    assert!(record.explicit);
    assert_eq!(record.generated_name, "bootty/main");
    assert_eq!(record.display_name, "release");
}

#[test]
fn a_reused_backend_id_does_not_transfer_name_metadata_between_directories() {
    let (_directory, repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let mut names = repository.session_names(binding);
    names.remember_generated("$1", "/old", "project/main", "project/main");
    names.mark_explicit("$1", "release", "release", "/old");
    names.remember_generated("$1", "/new", "other/main", "other/main");

    let record = names
        .observe_session("$1", "other/main", "/new")
        .expect("new directory metadata");
    assert!(!record.explicit);
    assert_eq!(record.generated_name, "other/main");
    assert_eq!(record.cwd, "/new");
}
