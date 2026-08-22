#![allow(clippy::redundant_closure_for_method_calls)]

use bootty_config::config::{MultiplexerBackendConfig, SshRemoteConfig};
use bootty_mux::{
    controller::{BindingId, MuxScope},
    membership::BackendMembership,
};
use bootty_workspace::{
    BindingMembershipMutation, DEFAULT_SPACE_COLOR, DEFAULT_SPACE_ICON, SessionNameStore,
    SessionOrderStore, SpaceMuxOverride, SpaceRemoteOverride, WorkspaceRepository,
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

    fn default_binding_id(&self) -> Option<BindingId> {
        self.spaces()
            .first()?
            .bindings()
            .first()
            .map(|binding| binding.mux_scope().binding_id())
    }

    fn session_order(&self, binding_id: BindingId) -> Option<SessionOrderStore> {
        self.spaces()
            .iter()
            .flat_map(|space| space.bindings())
            .find(|binding| binding.mux_scope().binding_id() == binding_id)
            .map(|binding| binding.session_order().clone())
    }

    fn session_names(&self, binding_id: BindingId) -> Option<SessionNameStore> {
        self.spaces()
            .iter()
            .flat_map(|space| space.bindings())
            .find(|binding| binding.mux_scope().binding_id() == binding_id)
            .map(|binding| binding.session_names().clone())
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

fn binding_scope(repository: &LoadedRepository, binding_id: BindingId) -> MuxScope {
    repository
        .spaces()
        .iter()
        .flat_map(|space| space.bindings())
        .find(|binding| binding.mux_scope().binding_id() == binding_id)
        .expect("binding scope")
        .mux_scope()
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
    let binding = &space.bindings()[0];
    assert_eq!(space.id().persistence_value(), 7);
    assert_eq!(space.remote_id(), "remote-7");
    assert_eq!(snapshot.selected_space("main"), Some(space.id()));
    assert_eq!(binding.mux_scope().binding_id().persistence_value(), 9);
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
    assert_eq!(binding.session_order().session_names(), vec!["session-1"]);
    let record = binding
        .session_names()
        .record("session-1")
        .expect("session name metadata");
    assert!(record.explicit);
    assert_eq!(record.cwd, "/worktree");
    assert_eq!(record.generated_name, "generated");
    assert_eq!(record.display_name, "Display");
}

#[test]
fn a_space_update_is_atomic_when_the_binding_update_fails() {
    let (directory, mut repository) = repository();
    let space = &repository.spaces()[0];
    let space_id = space.id();
    let original_remote_id = space.remote_id().to_owned();
    let original_position = space.position();
    let binding_id = space.bindings()[0].mux_scope().binding_id();
    let binding_scope = space.bindings()[0].mux_scope();
    let database = directory.path().join("session-order.sqlite3");
    let trigger_connection = Connection::open(&database).expect("open workspace database");
    trigger_connection
        .execute_batch(
            "CREATE TRIGGER fail_space_binding_update
             BEFORE UPDATE OF backend, remote ON workspace_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'forced binding update failure');
             END;",
        )
        .expect("install binding update failure");

    let error = repository
        .update_space_and_binding(
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

    let stored_binding = stored_space
        .bindings()
        .iter()
        .find(|binding| binding.mux_scope().binding_id() == binding_id)
        .expect("stored binding");
    assert_eq!(stored_binding.name(), "Default Binding");
    assert_eq!(stored_binding.backend_override(), None);
    assert_eq!(
        stored_binding.remote_override(),
        &SpaceRemoteOverride::Inherit
    );
    assert!(!stored_binding.hide_tmux_status());
}

#[test]
fn a_space_update_changes_the_selected_binding_instead_of_the_first_binding() {
    let (directory, repository) = repository();
    let space_id = repository.spaces()[0].id();
    let first_scope = repository.spaces()[0].bindings()[0].mux_scope();
    drop(repository);

    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute(
            "INSERT INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status, remote, unavailable)
             VALUES (42, ?1, 'Second Binding', 'tmux', 0, NULL, 0)",
            [space_id.persistence_value()],
        )
        .expect("insert second binding");
    drop(connection);

    let mut repository = LoadedRepository::open(&directory.path().join("config.toml"));
    let second_scope = repository.spaces()[0]
        .bindings()
        .iter()
        .find(|binding| binding.name() == "Second Binding")
        .expect("second binding")
        .mux_scope();
    repository
        .update_space_and_binding(
            second_scope,
            "Updated Space",
            "star",
            [9, 8, 7],
            true,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Rmux),
                remote: SpaceRemoteOverride::Local,
            },
        )
        .expect("update selected binding");
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let space = &reopened.spaces()[0];
    let first = space
        .bindings()
        .iter()
        .find(|binding| binding.mux_scope() == first_scope)
        .expect("first binding");
    let second = space
        .bindings()
        .iter()
        .find(|binding| binding.mux_scope() == second_scope)
        .expect("second binding");
    assert_eq!(first.backend_override(), None);
    assert_eq!(first.remote_override(), &SpaceRemoteOverride::Inherit);
    assert_eq!(
        second.backend_override(),
        Some(MultiplexerBackendConfig::Rmux)
    );
    assert_eq!(second.remote_override(), &SpaceRemoteOverride::Local);
}

#[test]
fn a_multi_binding_commit_is_atomic_when_the_second_binding_fails() {
    let (directory, mut repository) = repository();
    let first_binding = repository.spaces()[0].bindings()[0].clone();
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
    let second_binding = second_space.bindings()[0].clone();

    let mut first_order = first_binding.session_order().clone();
    let mut second_order = second_binding.session_order().clone();
    first_order.add_session("first-old");
    second_order.add_session("second-old");
    repository
        .commit_binding_states(&[
            (
                first_binding.mux_scope(),
                first_order.clone(),
                first_binding.session_names().clone(),
            ),
            (
                second_binding.mux_scope(),
                second_order.clone(),
                second_binding.session_names().clone(),
            ),
        ])
        .expect("commit baseline binding states");

    first_order.add_session("first-new");
    second_order.add_session("second-new");
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(&database).expect("open workspace database");
    connection
        .execute_batch(&format!(
            "CREATE TRIGGER fail_second_binding_session
             BEFORE INSERT ON workspace_sessions
             WHEN NEW.binding_id = {} AND NEW.name = 'second-new'
             BEGIN
                 SELECT RAISE(ABORT, 'forced second binding failure');
             END;",
            second_binding.mux_scope().binding_id().persistence_value()
        ))
        .expect("install second binding failure");
    drop(connection);

    repository
        .commit_binding_states(&[
            (
                first_binding.mux_scope(),
                first_order,
                first_binding.session_names().clone(),
            ),
            (
                second_binding.mux_scope(),
                second_order,
                second_binding.session_names().clone(),
            ),
        ])
        .expect_err("the second binding failure must roll back the first binding");
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let stored_orders = reopened
        .spaces()
        .iter()
        .flat_map(|space| space.bindings())
        .map(|binding| (binding.mux_scope(), binding.session_order().session_names()))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        stored_orders.get(&first_binding.mux_scope()),
        Some(&vec!["first-old".to_owned()])
    );
    assert_eq!(
        stored_orders.get(&second_binding.mux_scope()),
        Some(&vec!["second-old".to_owned()])
    );
}

#[test]
fn a_remote_backend_success_is_recovered_after_its_metadata_commit_fails() {
    let (directory, mut repository) = repository();
    let binding = repository.spaces()[0].bindings()[0].clone();
    let scope = binding.mux_scope();
    let mutation = BindingMembershipMutation::Create {
        session_id: "created-id".to_owned(),
        session_name: "created-name".to_owned(),
        display_name: "created-name".to_owned(),
        explicit: true,
        cwd: Some("/worktree".to_owned()),
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
             WHEN NEW.name = 'created-name'
             BEGIN
                 SELECT RAISE(ABORT, 'forced remote metadata failure');
             END;",
        )
        .expect("install metadata failure");
    let mut order = binding.session_order().clone();
    let mut names = binding.session_names().clone();
    repository
        .commit_binding_membership_mutation(scope, &mutation, &mut order, &mut names)
        .expect_err("metadata failure must retain the binding operation journal");
    assert!(order.session_names().is_empty());
    assert_eq!(
        repository
            .pending_binding_membership_mutation(scope)
            .expect("read pending mutation")
            .as_ref()
            .map(|pending| pending.mutation()),
        Some(&mutation)
    );

    connection
        .execute("DROP TRIGGER fail_remote_metadata_commit", [])
        .expect("remove metadata failure");
    drop(connection);
    assert!(
        repository
            .reconcile_binding_membership_mutation(
                scope,
                &[BackendMembership {
                    id: "created-id".to_owned(),
                    name: "created-name".to_owned(),
                }],
                &mut order,
                &mut names,
            )
            .expect("reconcile authoritative backend snapshot"),
    );
    assert_eq!(order.session_names(), vec!["created-name"]);
    assert!(
        repository
            .pending_binding_membership_mutation(scope)
            .expect("read cleared mutation")
            .is_none()
    );
    drop(repository);

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        reopened.spaces()[0].bindings()[0]
            .session_order()
            .session_names(),
        vec!["created-name"]
    );
}

#[test]
fn remote_rename_and_ditch_mutations_commit_binding_membership() {
    let (_directory, mut repository) = repository();
    let binding = repository.spaces()[0].bindings()[0].clone();
    let scope = binding.mux_scope();
    let mut order = binding.session_order().clone();
    let mut names = binding.session_names().clone();
    order.add_session("old-name");
    names.mark_explicit("session-id", "old-name", "Old name", "/worktree");
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit baseline membership");

    let rename = BindingMembershipMutation::Rename {
        session_id: "session-id".to_owned(),
        old_name: "old-name".to_owned(),
        new_name: "new-name".to_owned(),
        display_name: "New name".to_owned(),
        explicit: true,
        cwd: Some("/worktree".to_owned()),
    };
    repository
        .begin_binding_membership_mutation(scope, &rename)
        .expect("journal rename");
    repository
        .commit_binding_membership_mutation(scope, &rename, &mut order, &mut names)
        .expect("commit rename");
    assert_eq!(order.session_names(), vec!["new-name"]);

    let ditch = BindingMembershipMutation::Ditch {
        session_id: "session-id".to_owned(),
        old_name: "new-name".to_owned(),
    };
    repository
        .begin_binding_membership_mutation(scope, &ditch)
        .expect("journal ditch");
    repository
        .commit_binding_membership_mutation(scope, &ditch, &mut order, &mut names)
        .expect("commit ditch");
    assert!(order.session_names().is_empty());
}

#[test]
fn a_name_keyed_backend_rename_is_recovered_from_its_new_identity() {
    let (_directory, mut repository) = repository();
    let binding = repository.spaces()[0].bindings()[0].clone();
    let scope = binding.mux_scope();
    let mut order = binding.session_order().clone();
    let mut names = binding.session_names().clone();
    order.add_session("old-name");
    names.mark_explicit("old-name", "old-name", "Old display", "");
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit baseline membership");
    let rename = BindingMembershipMutation::Rename {
        session_id: "old-name".to_owned(),
        old_name: "old-name".to_owned(),
        new_name: "new-name".to_owned(),
        display_name: "New display".to_owned(),
        explicit: true,
        cwd: None,
    };
    repository
        .begin_binding_membership_mutation(scope, &rename)
        .expect("journal name-keyed rename");

    assert!(
        repository
            .reconcile_binding_membership_mutation(
                scope,
                &[BackendMembership {
                    id: "new-name".to_owned(),
                    name: "new-name".to_owned(),
                }],
                &mut order,
                &mut names,
            )
            .expect("reconcile name-keyed backend snapshot"),
    );
    assert_eq!(order.session_names(), vec!["new-name"]);
    assert!(names.record("old-name").is_none());
    let record = names
        .record("new-name")
        .expect("name-keyed metadata follows the new identity");
    assert_eq!(record.display_name, "New display");
    assert!(record.explicit);
}

#[test]
fn multiple_name_keyed_records_survive_a_generic_binding_commit() {
    let (directory, mut repository) = repository();
    let binding = repository.spaces()[0].bindings()[0].clone();
    let scope = binding.mux_scope();
    let mut order = binding.session_order().clone();
    let mut names = binding.session_names().clone();
    for name in ["first", "second"] {
        order.add_session(name);
        names.mark_explicit(name, name, name, "");
    }
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit name-keyed records without worktree paths");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let names = reopened.spaces()[0].bindings()[0].session_names();
    assert!(names.record("first").is_some());
    assert!(names.record("second").is_some());
}

#[test]
fn a_stable_id_backend_rename_is_recovered_from_its_new_name() {
    let (_directory, mut repository) = repository();
    let binding = repository.spaces()[0].bindings()[0].clone();
    let scope = binding.mux_scope();
    let mut order = binding.session_order().clone();
    let mut names = binding.session_names().clone();
    order.add_session("old-name");
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit baseline membership");
    let rename = BindingMembershipMutation::Rename {
        session_id: "stable-id".to_owned(),
        old_name: "old-name".to_owned(),
        new_name: "new-name".to_owned(),
        display_name: "new-name".to_owned(),
        explicit: true,
        cwd: None,
    };
    repository
        .begin_binding_membership_mutation(scope, &rename)
        .expect("journal stable-id rename");

    assert!(
        repository
            .reconcile_binding_membership_mutation(
                scope,
                &[BackendMembership {
                    id: "stable-id".to_owned(),
                    name: "new-name".to_owned(),
                }],
                &mut order,
                &mut names,
            )
            .expect("reconcile stable-id backend snapshot"),
    );
    assert_eq!(order.session_names(), vec!["new-name"]);
}

#[test]
fn a_pending_operation_with_forbidden_fields_is_rejected_on_reopen() {
    let (directory, repository) = repository();
    let scope = repository.spaces()[0].bindings()[0].mux_scope();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute(
            "INSERT INTO workspace_pending_binding_operations
                (space_id, binding_id, operation, session_id, old_name, new_name, cwd)
             VALUES (?1, ?2, 'ditch', 'session-id', 'old-name', 'forbidden', '/forbidden')",
            [
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value(),
            ],
        )
        .expect("insert invalid pending operation");
    drop(connection);

    assert!(WorkspaceRepository::open(&directory.path().join("config.toml")).is_err());
}

#[test]
fn a_pending_operation_with_a_non_boolean_explicit_value_is_rejected() {
    let (directory, repository) = repository();
    let scope = repository.spaces()[0].bindings()[0].mux_scope();
    drop(repository);
    let database = directory.path().join("session-order.sqlite3");
    let connection = Connection::open(database).expect("open workspace database");
    connection
        .execute(
            "INSERT INTO workspace_pending_binding_operations
                (space_id, binding_id, operation, session_id, new_name,
                 display_name, explicit, cwd)
             VALUES (?1, ?2, 'create', 'session-id', 'backend-name',
                     'display-name', 2, '/worktree')",
            [
                scope.space_id().persistence_value(),
                scope.binding_id().persistence_value(),
            ],
        )
        .expect("insert corrupt pending operation");
    drop(connection);

    assert!(WorkspaceRepository::open(&directory.path().join("config.toml")).is_err());
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
fn session_order_is_binding_scoped_and_persists() {
    let (directory, mut repository) = repository();
    let first_binding = repository.default_binding_id().expect("default binding");
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
    let second_binding = second_space.bindings()[0].mux_scope().binding_id();
    let second_scope = second_space.bindings()[0].mux_scope();

    let mut first = repository
        .session_order(first_binding)
        .expect("first binding order");
    let first_names = repository
        .session_names(first_binding)
        .expect("first binding names");
    let mut second = second_space.bindings()[0].session_order().clone();
    let second_names = second_space.bindings()[0].session_names().clone();
    first.sync_sessions(["arc/migrations", "arc/readiness", "agents", "bootty"]);
    second.add_session("other");
    assert!(first.move_session_before(
        "agents",
        Some("arc/migrations"),
        ["arc/migrations", "arc/readiness", "agents", "bootty"],
    ));
    let first_scope = binding_scope(&repository, first_binding);
    repository
        .commit_binding_state(first_scope, &first, &first_names)
        .expect("commit first binding");
    repository
        .commit_binding_state(second_scope, &second, &second_names)
        .expect("commit second binding");

    repository = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        repository
            .session_order(first_binding)
            .expect("reopened first binding order")
            .sync_sessions([
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
            .expect("reopened second binding order")
            .sync_sessions(["arc/migrations", "other"]),
        vec!["other"]
    );
}

#[test]
fn an_empty_backend_refresh_does_not_erase_session_order() {
    let (directory, mut repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let mut order = repository.session_order(binding).expect("binding order");
    let names = repository.session_names(binding).expect("binding names");
    order.sync_sessions(["first", "second"]);
    assert!(order.move_session_before("second", Some("first"), ["first", "second"]));
    assert!(order.sync_sessions(std::iter::empty()).is_empty());
    let scope = binding_scope(&repository, binding);
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit order");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(
        reopened
            .session_order(binding)
            .expect("reopened binding order")
            .sync_sessions(["first", "second"]),
        vec!["second", "first"]
    );
}

#[test]
fn generated_names_survive_backend_id_discovery_and_explicit_renames() {
    let (directory, mut repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let order = repository.session_order(binding).expect("binding order");
    let mut names = repository.session_names(binding).expect("binding names");
    names.remember_generated("bootty/main", "/repo", "bootty/main", "bootty/main");

    let discovered = names
        .observe_session("$1", "bootty/main", "/repo")
        .expect("stored generated name");
    assert_eq!(discovered.session_id, "$1");
    names.mark_explicit("$1", "release", "release", "/repo");
    names.remember_generated("$1", "/repo", "project/feature", "project/feature");
    let scope = binding_scope(&repository, binding);
    repository
        .commit_binding_state(scope, &order, &names)
        .expect("commit session names");

    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    let record = reopened
        .session_names(binding)
        .expect("reopened binding names")
        .observe_session("$1", "release", "/repo")
        .expect("stored explicit name");
    assert!(record.explicit);
    assert_eq!(record.generated_name, "bootty/main");
    assert_eq!(record.display_name, "release");
}

#[test]
fn a_failed_binding_commit_keeps_the_committed_snapshot_and_database() {
    let (directory, mut repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let scope = binding_scope(&repository, binding);
    let mut committed_order = repository.session_order(binding).expect("binding order");
    let mut committed_names = repository.session_names(binding).expect("binding names");
    assert!(committed_order.add_session("stable"));
    assert!(committed_names.remember_generated("stable", "/workspace/stable", "stable", "stable",));
    repository
        .commit_binding_state(scope, &committed_order, &committed_names)
        .expect("commit baseline");

    let database = directory.path().join("session-order.sqlite3");
    let lock = Connection::open(&database).expect("open lock connection");
    lock.execute_batch("BEGIN IMMEDIATE")
        .expect("hold workspace write lock");

    let mut candidate_order = committed_order.clone();
    let mut candidate_names = committed_names.clone();
    assert!(candidate_order.add_session("uncommitted"));
    assert!(candidate_names.remember_generated(
        "uncommitted",
        "/workspace/uncommitted",
        "uncommitted",
        "uncommitted",
    ));
    let error = repository
        .commit_binding_state(scope, &candidate_order, &candidate_names)
        .expect_err("locked database must reject the candidate");
    assert!(error.to_string().contains("workspace persistence error"));
    lock.execute_batch("ROLLBACK").expect("release write lock");
    drop(lock);
    let reopened = LoadedRepository::open(&directory.path().join("config.toml"));
    assert_eq!(reopened.session_order(binding), Some(committed_order));
    assert_eq!(reopened.session_names(binding), Some(committed_names));
}

#[test]
fn a_reused_backend_id_does_not_transfer_name_metadata_between_directories() {
    let (_directory, repository) = repository();
    let binding = repository.default_binding_id().expect("default binding");
    let mut names = repository.session_names(binding).expect("binding names");
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
