#![allow(clippy::redundant_closure_for_method_calls)]

use bootty_config::config::{MultiplexerBackendConfig, SshRemoteConfig};
use bootty_mux::{controller::SpaceId, membership::BackendMembership};
use bootty_workspace::{
    BindingMembershipMutation, DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SessionMembership,
    SpaceMuxOverride, SpaceRemoteOverride, WorkspaceRepository, WorkspaceSession,
    WorkspaceSnapshot,
};
use rusqlite::Connection;
use tempfile::TempDir;

struct LoadedRepository {
    repository: WorkspaceRepository,
    snapshot: WorkspaceSnapshot,
}

impl LoadedRepository {
    fn open(config_path: &std::path::Path) -> Self {
        let (repository, snapshot) =
            WorkspaceRepository::open(config_path).expect("workspace repository");
        Self {
            repository,
            snapshot,
        }
    }

    fn spaces(&self) -> &[bootty_workspace::WorkspaceSpace] {
        self.snapshot.spaces()
    }

    fn default_space(&self) -> Option<SpaceId> {
        self.spaces().first().map(|space| space.id())
    }

    fn sessions(&self, space: SpaceId) -> Option<SessionMembership> {
        self.spaces()
            .iter()
            .map(|space| space.binding())
            .find(|binding| binding.mux_scope() == space)
            .map(|binding| binding.sessions().clone())
    }
}

fn session(identity: &str, backend_name: &str) -> WorkspaceSession {
    WorkspaceSession {
        identity: identity.to_owned(),
        backend_name: backend_name.to_owned(),
        display_name: String::new(),
        explicit: false,
        cwd: "/worktree".to_owned(),
    }
}

fn backend_names(sessions: &SessionMembership) -> Vec<String> {
    sessions.backend_names()
}

fn membership(id: &str, name: &str, identity: &str) -> BackendMembership {
    BackendMembership {
        id: id.to_owned(),
        name: name.to_owned(),
        identity: Some(identity.to_owned()),
    }
}

impl std::ops::Deref for LoadedRepository {
    type Target = WorkspaceRepository;

    fn deref(&self) -> &Self::Target {
        &self.repository
    }
}

impl std::ops::DerefMut for LoadedRepository {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.repository
    }
}

fn repository() -> (TempDir, LoadedRepository) {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let repository = LoadedRepository::open(&config_path);
    (directory, repository)
}

#[test]
fn an_invalid_database_is_reported_instead_of_becoming_an_empty_workspace() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    std::fs::write(
        directory.path().join("session-order.sqlite3"),
        "not a sqlite database",
    )
    .expect("write invalid database");

    assert!(WorkspaceRepository::open(&config_path).is_err());
}

#[test]
fn an_invalid_current_snapshot_is_rejected_instead_of_repaired() {
    let (directory, repository) = repository();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute("UPDATE workspace_spaces SET color = 'invalid'", [])
        .expect("corrupt current color value");
    drop(connection);

    let error = WorkspaceRepository::open(&directory.path().join("config.toml"))
        .expect_err("invalid current snapshot must fail");
    assert!(error.to_string().contains("load or migrate"));
}

#[test]
fn the_single_binding_schema_migration_preserves_binding_and_restore_state() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open legacy workspace database");
    connection
        .execute_batch(
            r#"
            CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                remote_id TEXT UNIQUE,
                name TEXT NOT NULL,
                icon TEXT NOT NULL,
                color TEXT NOT NULL,
                tint_sidebar INTEGER NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL UNIQUE,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL,
                remote TEXT,
                unavailable INTEGER NOT NULL DEFAULT 0,
                selected_session_id TEXT,
                selected_window_id TEXT
            );
            CREATE TABLE workspace_session_groups (
                id INTEGER PRIMARY KEY,
                binding_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                position INTEGER NOT NULL
            );
            CREATE TABLE workspace_sessions (
                binding_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                group_id INTEGER NOT NULL,
                position INTEGER NOT NULL
            );
            CREATE TABLE workspace_session_name_metadata (
                binding_id INTEGER NOT NULL,
                session_id TEXT NOT NULL,
                cwd TEXT NOT NULL,
                generated_name TEXT NOT NULL,
                session_name TEXT NOT NULL,
                display_name TEXT NOT NULL,
                explicit INTEGER NOT NULL
            );
            CREATE TABLE workspace_window_state (
                window_key TEXT PRIMARY KEY,
                selected_space_id INTEGER NOT NULL
            );
            INSERT INTO workspace_spaces
                (id, remote_id, name, icon, color, tint_sidebar, position)
            VALUES (7, 'remote-7', 'Legacy Space', 'star', '#010203', 1, 0);
            INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status, remote, unavailable,
                 selected_session_id, selected_window_id)
            VALUES (9, 7, 'Legacy Binding', 'tmux', 1, '{"source":"local"}', 1,
                    'session-1', 'window-1');
            INSERT INTO workspace_session_groups (id, binding_id, name, position)
            VALUES (11, 9, 'work', 0);
            INSERT INTO workspace_sessions (binding_id, name, group_id, position)
            VALUES (9, 'session-1', 11, 0);
            INSERT INTO workspace_session_name_metadata
                (binding_id, session_id, cwd, generated_name, session_name, display_name, explicit)
            VALUES (9, 'session-1', '/worktree', 'generated', 'session-1', 'Display', 1);
            INSERT INTO workspace_window_state (window_key, selected_space_id)
            VALUES ('main', 7);
            "#,
        )
        .expect("write legacy single-binding schema");
    drop(connection);

    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("migrate workspace");
    let space = &snapshot.spaces()[0];
    let binding = &space.binding();
    assert_eq!(space.id().persistence_value(), 7);
    assert_eq!(space.remote_id(), "remote-7");
    assert_eq!(snapshot.selected_space("main"), Some(space.id()));
    assert_eq!(
        binding.backend_override(),
        Some(MultiplexerBackendConfig::Tmux)
    );
    assert_eq!(binding.remote_override(), &SpaceRemoteOverride::Local);
    assert!(binding.hide_tmux_status());
    assert!(binding.unavailable());
    assert_eq!(
        binding
            .selection()
            .map(|selection| (selection.session_id(), selection.window_id(),)),
        Some(("session-1", Some("window-1")))
    );
    // Sessions come across under a provisional identity. Nothing in any multiplexer carries that
    // value, so the first successful refresh finds each one by its name and stamps a real id.
    let claimed = binding.sessions().sessions();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].identity, "legacy:9:session-1");
    assert_eq!(claimed[0].backend_name, "session-1");
    assert_eq!(claimed[0].label(), "Display");
    assert!(claimed[0].explicit);
    assert_eq!(claimed[0].cwd, "/worktree");
}

#[test]
fn a_space_update_that_fails_leaves_every_field_as_it_was() {
    let (directory, mut repository) = repository();
    let space = &repository.spaces()[0];
    let space_id = space.id();
    let original_remote_id = space.remote_id().to_owned();
    let original_position = space.position();
    let binding_scope = space.binding().mux_scope();
    let database = directory.path().join("session-order.sqlite3");
    let trigger_connection = Connection::open(&database).expect("open workspace database");
    trigger_connection
        .execute_batch(
            "CREATE TRIGGER fail_space_binding_update
             BEFORE UPDATE OF backend, remote ON workspace_spaces
             BEGIN
                 SELECT RAISE(ABORT, 'forced binding update failure');
             END;",
        )
        .expect("install binding update failure");

    let error = repository
        .update_space(
            binding_scope,
            "Updated Space",
            "star",
            [9, 8, 7],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Local,
            },
        )
        .expect_err("binding failure must reject the whole space update");
    assert!(error.to_string().contains("forced binding update failure"));

    drop(trigger_connection);
    drop(repository);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let stored_space = reopened
        .spaces()
        .iter()
        .find(|space| space.id() == space_id)
        .expect("stored space");
    assert_eq!(stored_space.remote_id(), original_remote_id.as_str());
    assert_eq!(stored_space.name(), "Default Space");
    assert_eq!(stored_space.icon(), DEFAULT_SPACE_ICON);
    assert_eq!(stored_space.color(), DEFAULT_SPACE_COLOR);
    assert!(!stored_space.tint_sidebar());
    assert_eq!(stored_space.position(), original_position);

    let stored_binding = stored_space.binding();
    assert_eq!(stored_binding.mux_scope(), space_id);
    assert_eq!(stored_binding.backend_override(), None);
    assert_eq!(
        stored_binding.remote_override(),
        &SpaceRemoteOverride::Inherit
    );
    assert!(!stored_binding.hide_tmux_status());
}

/// A Space is one connection, so the fold has to decide what a database holding two of them
/// means. It keeps the first and every session claimed through either, rather than losing a Space
/// or half its sessions to a shape no release ever produced on purpose.
#[test]
fn folding_a_space_that_held_two_connections_keeps_the_first_and_every_session() {
    let directory = tempfile::tempdir().expect("temporary workspace directory");
    let config_path = directory.path().join("config.toml");
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(
            r#"
            PRAGMA user_version = 4;
            CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                remote_id TEXT UNIQUE,
                name TEXT NOT NULL,
                icon TEXT NOT NULL,
                color TEXT NOT NULL,
                tint_sidebar INTEGER NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL,
                remote TEXT,
                unavailable INTEGER NOT NULL DEFAULT 0,
                selected_session_id TEXT,
                selected_window_id TEXT
            );
            CREATE TABLE workspace_sessions (
                identity TEXT PRIMARY KEY,
                binding_id INTEGER NOT NULL,
                backend_name TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                explicit INTEGER NOT NULL DEFAULT 0,
                cwd TEXT NOT NULL DEFAULT '',
                position INTEGER NOT NULL
            );
            CREATE TABLE workspace_window_state (
                window_key TEXT PRIMARY KEY,
                selected_space_id INTEGER NOT NULL
            );
            INSERT INTO workspace_spaces
                (id, remote_id, name, icon, color, tint_sidebar, position)
            VALUES (3, 'remote-3', 'Doubled Space', 'star', '#010203', 0, 0);
            INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status, remote)
            VALUES (5, 3, 'First', 'tmux', 1, '{"source":"local"}'),
                   (6, 3, 'Second', 'rmux', 0, NULL);
            INSERT INTO workspace_sessions
                (identity, binding_id, backend_name, position)
            VALUES ('id-1', 5, 'first', 0),
                   ('id-2', 6, 'second', 0);
            "#,
        )
        .expect("write a workspace holding two connections for one Space");
    drop(connection);

    let (_, snapshot) = WorkspaceRepository::open(&config_path).expect("migrate workspace");
    let space = &snapshot.spaces()[0];
    let binding = space.binding();
    assert_eq!(
        binding.backend_override(),
        Some(MultiplexerBackendConfig::Tmux),
        "the first connection is the one that survives"
    );
    assert!(binding.hide_tmux_status());
    assert_eq!(binding.remote_override(), &SpaceRemoteOverride::Local);
    assert_eq!(
        backend_names(binding.sessions()),
        vec!["first".to_owned(), "second".to_owned()],
        "sessions claimed through either connection are kept"
    );
}

#[test]
fn a_two_space_commit_is_atomic_when_the_second_space_fails() {
    let (directory, mut repository) = repository();
    let first_binding = repository.spaces()[0].binding().clone();
    let second_space = repository
        .create_space(
            "Second",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create second space")
        .expect("second space");
    let second_binding = second_space.binding().clone();

    let mut first = first_binding.sessions().clone();
    let mut second = second_binding.sessions().clone();
    first.claim(session("id-1", "first-old"));
    second.claim(session("id-2", "second-old"));
    repository
        .commit_binding_states(&[
            (first_binding.mux_scope(), first.clone()),
            (second_binding.mux_scope(), second.clone()),
        ])
        .expect("commit baseline binding states");

    first.claim(session("id-3", "first-new"));
    second.claim(session("id-4", "second-new"));
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_second_binding_session
             BEFORE INSERT ON workspace_sessions
             WHEN NEW.space_id = {} AND NEW.backend_name = 'second-new'
             BEGIN
                 SELECT RAISE(ABORT, 'forced second binding failure');
             END;",
            second_binding.mux_scope().persistence_value()
        ))
        .expect("install second binding failure");
    drop(connection);

    repository
        .commit_binding_states(&[
            (first_binding.mux_scope(), first),
            (second_binding.mux_scope(), second),
        ])
        .expect_err("the second binding failure must roll back the first binding");
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let stored = reopened
        .spaces()
        .iter()
        .map(|space| space.binding())
        .map(|binding| (binding.mux_scope(), backend_names(binding.sessions())))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        stored.get(&first_binding.mux_scope()),
        Some(&vec!["first-old".to_owned()])
    );
    assert_eq!(
        stored.get(&second_binding.mux_scope()),
        Some(&vec!["second-old".to_owned()])
    );
}

#[test]
fn a_remote_backend_success_is_recovered_after_its_metadata_commit_fails() {
    let (directory, mut repository) = repository();
    let binding = repository.spaces()[0].binding().clone();
    let scope = binding.mux_scope();
    let mutation = BindingMembershipMutation::Create {
        identity: "id-1".to_owned(),
        session_name: "created-name".to_owned(),
        display_name: "created-name".to_owned(),
        explicit: true,
        cwd: "/worktree".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &mutation)
        .expect("journal remote create before backend execution");

    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(
            "CREATE TRIGGER fail_remote_metadata_commit
             BEFORE INSERT ON workspace_sessions
             WHEN NEW.backend_name = 'created-name'
             BEGIN
                 SELECT RAISE(ABORT, 'forced remote metadata failure');
             END;",
        )
        .expect("install metadata failure");
    let mut sessions = binding.sessions().clone();
    repository
        .commit_binding_membership_mutation(scope, &mutation, &mut sessions)
        .expect_err("metadata failure must retain the binding operation journal");
    assert!(sessions.is_empty());
    assert_eq!(
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read pending mutations")
            .iter()
            .map(|pending| pending.mutation())
            .collect::<Vec<_>>(),
        [&mutation]
    );

    connection
        .execute("DROP TRIGGER fail_remote_metadata_commit", [])
        .expect("remove metadata failure");
    drop(connection);
    // The backend renamed the session on its way in, which the identity makes irrelevant.
    assert!(
        repository
            .reconcile_binding_membership_mutations(
                scope,
                &[membership("$4", "created-name-2", "id-1")],
                &mut sessions,
            )
            .expect("reconcile authoritative backend snapshot"),
    );
    assert_eq!(backend_names(&sessions), vec!["created-name"]);
    assert!(
        repository
            .pending_binding_membership_mutations(scope)
            .expect("read cleared mutations")
            .is_empty()
    );
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        backend_names(reopened.spaces()[0].binding().sessions()),
        vec!["created-name"]
    );
}

#[test]
fn remote_rename_and_ditch_mutations_commit_binding_membership() {
    let (_directory, mut repository) = repository();
    let binding = repository.spaces()[0].binding().clone();
    let scope = binding.mux_scope();
    let mut sessions = binding.sessions().clone();
    sessions.claim(session("id-1", "old-name"));
    repository
        .commit_binding_state(scope, &sessions)
        .expect("commit baseline membership");

    let rename = BindingMembershipMutation::Rename {
        identity: "id-1".to_owned(),
        old_name: "old-name".to_owned(),
        new_name: "new-name".to_owned(),
        display_name: "New name".to_owned(),
        explicit: true,
    };
    repository
        .begin_binding_membership_mutation(scope, &rename)
        .expect("journal rename");
    repository
        .commit_binding_membership_mutation(scope, &rename, &mut sessions)
        .expect("commit rename");
    assert_eq!(backend_names(&sessions), vec!["new-name"]);
    assert_eq!(
        sessions.get("id-1").map(|claimed| claimed.label()),
        Some("New name"),
        "the claim keeps its identity across the rename"
    );

    let ditch = BindingMembershipMutation::Ditch {
        identity: "id-1".to_owned(),
        old_name: "new-name".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &ditch)
        .expect("journal ditch");
    repository
        .commit_binding_membership_mutation(scope, &ditch, &mut sessions)
        .expect("commit ditch");
    assert!(sessions.is_empty());
}

/// A ditched session takes its name with it. Leaving the record behind is what used to make the
/// next session started in the same directory come back wearing a dead session's name.
#[test]
fn a_ditched_session_leaves_no_name_behind_for_the_next_one_in_that_directory() {
    let (_directory, mut repository) = repository();
    let binding = repository.spaces()[0].binding().clone();
    let scope = binding.mux_scope();
    let mut sessions = binding.sessions().clone();
    sessions.claim(session("id-1", "bootty"));
    sessions.set_display_name("id-1", "the name I picked", true);
    repository
        .commit_binding_state(scope, &sessions)
        .expect("commit the named session");

    let ditch = BindingMembershipMutation::Ditch {
        identity: "id-1".to_owned(),
        old_name: "bootty".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &ditch)
        .expect("journal ditch");
    repository
        .commit_binding_membership_mutation(scope, &ditch, &mut sessions)
        .expect("commit ditch");

    let replacement = BindingMembershipMutation::Create {
        identity: "id-2".to_owned(),
        session_name: "bootty".to_owned(),
        display_name: "bootty".to_owned(),
        explicit: false,
        cwd: "/worktree".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &replacement)
        .expect("journal the replacement create");
    repository
        .commit_binding_membership_mutation(scope, &replacement, &mut sessions)
        .expect("commit the replacement create");

    assert_eq!(
        sessions.get("id-2").map(|claimed| claimed.label()),
        Some("bootty"),
        "the new session is named for its directory, not for the one that was ditched"
    );
    assert!(sessions.get("id-1").is_none());
}

#[test]
fn a_pending_operation_with_forbidden_fields_is_rejected_on_reopen() {
    let (directory, repository) = repository();
    let scope = repository.spaces()[0].binding().mux_scope();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute(
            "INSERT INTO workspace_pending_binding_operations
                (space_id, operation, identity, old_name, new_name, cwd)
             VALUES (?1, 'ditch', 'id-1', 'old-name', 'forbidden', '/forbidden')",
            [scope.persistence_value()],
        )
        .expect("insert invalid pending operation");
    drop(connection);

    assert!(WorkspaceRepository::open(&directory.path().join("config.toml")).is_err());
}

#[test]
fn a_pending_operation_with_a_non_boolean_explicit_value_is_rejected() {
    let (directory, repository) = repository();
    let scope = repository.spaces()[0].binding().mux_scope();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute(
            "INSERT INTO workspace_pending_binding_operations
                (space_id, operation, identity, new_name,
                 display_name, explicit, cwd)
             VALUES (?1, 'create', 'id-1', 'backend-name',
                     'display-name', 2, '/worktree')",
            [scope.persistence_value()],
        )
        .expect("insert corrupt pending operation");
    drop(connection);

    assert!(WorkspaceRepository::open(&directory.path().join("config.toml")).is_err());
}

#[test]
fn a_fresh_repository_has_one_default_space_and_connection() {
    let (_directory, repository) = repository();

    assert_eq!(repository.spaces().len(), 1);
    assert_eq!(repository.spaces()[0].name(), "Default Space");
    let binding = repository.spaces()[0].binding();
    assert_eq!(binding.mux_scope(), repository.spaces()[0].id());
    assert_eq!(binding.backend_override(), None);
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
            false,
        )
        .expect("create space")
        .expect("valid space");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
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
        stored.binding().remote_override(),
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
                false,
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
            false,
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
            false,
        )
        .expect("create duplicate space")
        .expect("valid duplicate space");

    assert_eq!(duplicate.name(), "review 2");
}

#[test]
fn session_membership_is_binding_scoped_and_persists() {
    let (directory, mut repository) = repository();
    let first_binding = repository.default_space().expect("default Space");
    let second_space = repository
        .create_space(
            "Second",
            DEFAULT_SPACE_ICON,
            DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
            false,
        )
        .expect("create second space")
        .expect("second space");
    let second_scope = second_space.binding().mux_scope();

    let mut first = repository
        .sessions(first_binding)
        .expect("first binding membership");
    let mut second = second_space.binding().sessions().clone();
    for (identity, name) in [
        ("id-1", "arc/migrations"),
        ("id-2", "arc/readiness"),
        ("id-3", "agents"),
        ("id-4", "bootty"),
    ] {
        first.claim(session(identity, name));
    }
    second.claim(session("id-5", "other"));
    assert!(first.move_before("id-3", Some("id-1")));

    let first_scope = first_binding;
    repository
        .commit_binding_state(first_scope, &first)
        .expect("commit first binding");
    repository
        .commit_binding_state(second_scope, &second)
        .expect("commit second binding");

    repository = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        backend_names(
            &repository
                .sessions(first_binding)
                .expect("reopened first binding")
        ),
        vec!["agents", "arc/migrations", "arc/readiness", "bootty"]
    );
    assert_eq!(
        backend_names(
            &repository
                .sessions(second_scope)
                .expect("reopened second binding")
        ),
        vec!["other"],
        "one Space's sessions never leak into another's"
    );
}

/// A backend that answers with nothing has not told us the Space emptied, only that it has not
/// answered yet.
#[test]
fn an_empty_backend_refresh_does_not_erase_the_claimed_sessions() {
    let (directory, mut repository) = repository();
    let binding = repository.default_space().expect("default Space");
    let mut sessions = repository.sessions(binding).expect("binding membership");
    sessions.claim(session("id-1", "first"));
    sessions.claim(session("id-2", "second"));
    assert!(sessions.move_before("id-2", Some("id-1")));
    assert!(!sessions.retain_alive(&std::collections::HashSet::new()));
    let scope = binding;
    repository
        .commit_binding_state(scope, &sessions)
        .expect("commit membership");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        backend_names(&reopened.sessions(binding).expect("reopened binding")),
        vec!["second", "first"]
    );
}

/// The name bootty shows is stored against the identity, so a backend that had to add a uniqueness
/// suffix does not push that suffix into the sidebar.
#[test]
fn the_shown_name_survives_a_backend_name_the_server_had_to_uniquify() {
    let (directory, mut repository) = repository();
    let binding = repository.default_space().expect("default Space");
    let mut sessions = repository.sessions(binding).expect("binding membership");
    sessions.claim(session("id-1", "agents/main-2"));
    sessions.set_display_name("id-1", "agents/main", true);
    let scope = binding;
    repository
        .commit_binding_state(scope, &sessions)
        .expect("commit membership");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let sessions = reopened.sessions(binding).expect("reopened binding");
    let claimed = sessions.get("id-1").expect("the claimed session");
    assert_eq!(claimed.label(), "agents/main");
    assert_eq!(claimed.backend_name, "agents/main-2");
    assert!(claimed.explicit);
}

#[test]
fn a_failed_binding_commit_keeps_the_committed_snapshot_and_database() {
    let (directory, mut repository) = repository();
    let binding = repository.default_space().expect("default Space");
    let scope = binding;
    let mut committed = repository.sessions(binding).expect("binding membership");
    assert!(committed.claim(session("id-1", "stable")));
    repository
        .commit_binding_state(scope, &committed)
        .expect("commit baseline");

    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");

    let mut candidate = committed.clone();
    assert!(candidate.claim(session("id-2", "uncommitted")));
    let error = repository
        .commit_binding_state(scope, &candidate)
        .expect_err("locked database must reject the candidate");
    assert!(error.to_string().contains("workspace persistence error"));
    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(reopened.sessions(binding), Some(committed));
}

#[test]
fn a_stranded_mutation_does_not_block_a_change_to_another_session() {
    let (_directory, mut repository) = repository();
    let scope = repository.spaces()[0].binding().mux_scope();
    let stranded = BindingMembershipMutation::Create {
        identity: "stranded-id".to_owned(),
        session_name: "stranded-name".to_owned(),
        display_name: "stranded-name".to_owned(),
        explicit: true,
        cwd: String::new(),
    };
    repository
        .begin_binding_membership_mutation(scope, &stranded)
        .expect("journal the first mutation");

    let next = BindingMembershipMutation::Create {
        identity: "next-id".to_owned(),
        session_name: "next-name".to_owned(),
        display_name: "next-name".to_owned(),
        explicit: true,
        cwd: String::new(),
    };
    repository
        .begin_binding_membership_mutation(scope, &next)
        .expect("a stranded entry does not block the next change");

    let pending = repository
        .pending_binding_membership_mutations(scope)
        .expect("read the pending mutations");
    assert_eq!(
        pending
            .iter()
            .map(|pending| pending.mutation().clone())
            .collect::<Vec<_>>(),
        [next, stranded],
        "operations on different sessions do not collide"
    );
}

/// A second operation on the *same* session replaces the first rather than erroring: the older one
/// is exactly what reconciliation would discard.
#[test]
fn a_second_mutation_on_one_session_supersedes_the_first() {
    let (_directory, mut repository) = repository();
    let scope = repository.spaces()[0].binding().mux_scope();
    for name in ["first-name", "second-name"] {
        repository
            .begin_binding_membership_mutation(
                scope,
                &BindingMembershipMutation::Create {
                    identity: "one-session".to_owned(),
                    session_name: name.to_owned(),
                    display_name: name.to_owned(),
                    explicit: true,
                    cwd: String::new(),
                },
            )
            .expect("journal the mutation");
    }

    let pending = repository
        .pending_binding_membership_mutations(scope)
        .expect("read the pending mutations");
    assert_eq!(pending.len(), 1);
    assert!(matches!(
        pending[0].mutation(),
        BindingMembershipMutation::Create { session_name, .. } if session_name == "second-name"
    ));
}

/// A journal left over from an older shape is rebuilt, not carried.
///
/// The intent journal gained its identity key in one revision and lost its `binding_id` in another,
/// but `CREATE TABLE IF NOT EXISTS` leaves whatever is already there. A database that kept the old
/// shape still had a foreign key into `workspace_bindings`, so folding that table away turned every
/// membership change — every ditch, every rename — into
/// `no such table: main.workspace_bindings`. Opening the database has to repair it, because a user
/// cannot.
#[test]
fn a_journal_from_an_older_shape_is_rebuilt_on_open() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let config_path = directory.path().join("config.toml");
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(
            r"
            PRAGMA user_version = 4;
            CREATE TABLE workspace_spaces (
                id INTEGER PRIMARY KEY,
                remote_id TEXT UNIQUE,
                name TEXT NOT NULL,
                icon TEXT NOT NULL,
                color TEXT NOT NULL,
                tint_sidebar INTEGER NOT NULL,
                position INTEGER NOT NULL UNIQUE
            );
            CREATE TABLE workspace_bindings (
                id INTEGER PRIMARY KEY,
                space_id INTEGER NOT NULL,
                name TEXT NOT NULL,
                backend TEXT NOT NULL,
                hide_tmux_status INTEGER NOT NULL,
                remote TEXT
            );
            CREATE TABLE workspace_sessions (
                identity TEXT PRIMARY KEY,
                binding_id INTEGER NOT NULL,
                backend_name TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                explicit INTEGER NOT NULL DEFAULT 0,
                cwd TEXT NOT NULL DEFAULT '',
                position INTEGER NOT NULL
            );
            CREATE TABLE workspace_window_state (
                window_key TEXT PRIMARY KEY,
                selected_space_id INTEGER NOT NULL
            );
            -- The shape that survived: keyed by identity, but still carrying the binding it was
            -- written against.
            CREATE TABLE workspace_pending_binding_operations (
                identity TEXT PRIMARY KEY,
                space_id INTEGER NOT NULL REFERENCES workspace_spaces(id) ON DELETE CASCADE,
                binding_id INTEGER NOT NULL REFERENCES workspace_bindings(id) ON DELETE CASCADE,
                operation TEXT NOT NULL,
                old_name TEXT,
                new_name TEXT,
                display_name TEXT,
                explicit INTEGER,
                cwd TEXT
            );
            INSERT INTO workspace_spaces
                (id, remote_id, name, icon, color, tint_sidebar, position)
            VALUES (1, 'remote-1', 'Work', 'star', '#010203', 0, 0);
            INSERT INTO workspace_bindings (id, space_id, name, backend, hide_tmux_status, remote)
            VALUES (1, 1, 'Default Binding', 'tmux', 0, NULL);
            INSERT INTO workspace_sessions (identity, binding_id, backend_name, position)
            VALUES ('id-1', 1, 'work', 0);
            ",
        )
        .expect("write a workspace whose journal kept the older shape");
    drop(connection);

    let (mut repository, snapshot) =
        WorkspaceRepository::open(&config_path).expect("migrate workspace");
    let scope = snapshot.spaces()[0].binding().mux_scope();

    // A ditch journals before it calls the backend, which is where this failed.
    repository
        .begin_binding_membership_mutation(
            scope,
            &BindingMembershipMutation::Ditch {
                identity: "id-1".to_owned(),
                old_name: "work".to_owned(),
            },
        )
        .expect("journal a ditch against the migrated database");

    let connection = Connection::open(&database).expect("reopen workspace database");
    let mut statement = connection
        .prepare("SELECT name FROM pragma_table_info('workspace_pending_binding_operations')")
        .expect("read the journal columns");
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query the journal columns")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect the journal columns");
    assert!(
        !columns.iter().any(|column| column == "binding_id"),
        "the rebuilt journal drops the column that pointed at a table that is gone: {columns:?}"
    );
}
