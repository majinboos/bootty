use std::{
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Result, bail};
use bootty_daemon::catalog::{Backend, CATALOG_VERSION, Catalog};
use bootty_identity::ApplicationIdentity;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    command::MuxCommand,
    provider::{MuxBackendProvider, MuxBackendRegistry},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSnapshot, session_matches},
};
#[cfg(unix)]
use bootty_rmux::endpoint_path_for;
use rusqlite::Connection;
#[cfg(unix)]
use std::sync::OnceLock;
#[cfg(unix)]
use tokio::runtime::Builder;

#[cfg(unix)]
fn start_embedded_rmux_daemon_for_tests() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    STARTED
        .get_or_init(|| {
            let socket = endpoint_path_for(ApplicationIdentity::Production)
                .map_err(|error| error.to_string())?;
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            thread::spawn(move || {
                let started_tx = ready_tx.clone();
                let result = (|| -> Result<()> {
                    let runtime = Builder::new_multi_thread().enable_all().build()?;
                    runtime.block_on(async {
                        let daemon =
                            rmux_server::ServerDaemon::new(rmux_server::DaemonConfig::new(socket))
                                .bind()
                                .await?;
                        let _ = started_tx.send(Ok(()));
                        daemon.wait().await
                    })?;
                    Ok(())
                })();
                if let Err(error) = result {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            });
            ready_rx.recv().map_err(|error| error.to_string())?
        })
        .clone()
        .map_err(anyhow::Error::msg)
}

struct MarkerBackend;

impl MuxBackend for MarkerBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        Ok(())
    }
}

struct MarkerProvider;

impl MuxBackendProvider for MarkerProvider {
    fn command_dispatch(&self) -> bootty_mux::provider::MuxCommandDispatch {
        bootty_mux::provider::MuxCommandDispatch::WorkerThread
    }

    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Tmux
    }

    fn build_backend(
        &self,
        _config: &MuxBindingConfig,
        _workspace: Option<&Path>,
    ) -> Box<dyn MuxBackend> {
        Box::new(MarkerBackend)
    }
}

#[cfg(unix)]
const REAL_DAEMON_HELPER_ENV: &str = "BOOTTY_DAEMON_CATALOG_RECOVERY_HELPER";

#[derive(Default)]
struct ScriptedBackend {
    snapshot: MuxSnapshot,
    execute_calls: usize,
    fail_after_apply: bool,
    name_keyed: bool,
}

struct PausingBackend {
    snapshot: Arc<Mutex<MuxSnapshot>>,
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}

impl MuxBackend for PausingBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.snapshot.lock().expect("snapshot lock").clone())
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.entered.send(())?;
        self.resume.recv()?;
        let MuxCommand::CreateProjectSession { session_id, .. } = command else {
            bail!("pausing backend expected a create command")
        };
        self.snapshot
            .lock()
            .expect("snapshot lock")
            .sessions
            .push(session(&session_id, &session_id));
        Ok(())
    }
}

struct SharedSnapshotBackend(Arc<Mutex<MuxSnapshot>>);

impl MuxBackend for SharedSnapshotBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.0.lock().expect("snapshot lock").clone())
    }

    fn execute(&mut self, _command: MuxCommand) -> Result<()> {
        bail!("snapshot backend does not execute commands")
    }
}

impl MuxBackend for ScriptedBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(self.snapshot.clone())
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        self.execute_calls += 1;
        match command {
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                self.snapshot
                    .sessions
                    .push(session(&session_id, &session_id));
            }
            MuxCommand::RenameSession { session_id, name } => {
                let session = self
                    .snapshot
                    .sessions
                    .iter_mut()
                    .find(|session| session_matches(session, &session_id))
                    .expect("scripted renamed session");
                if self.name_keyed {
                    session.id.clone_from(&name);
                }
                session.name = name;
            }
            MuxCommand::DitchSession { session_id } => self
                .snapshot
                .sessions
                .retain(|session| !session_matches(session, &session_id)),
            _ => {}
        }
        if self.fail_after_apply {
            bail!("scripted transport disconnected after apply")
        }
        Ok(())
    }
}

fn session(id: &str, name: &str) -> MuxSession {
    MuxSession {
        id: id.to_owned(),
        name: name.to_owned(),
        active: false,
        anchor: MuxPaneAnchor::default(),
        active_window_id: None,
        windows: Vec::new(),
    }
}

fn open_catalog(path: &Path) -> Result<Catalog> {
    bootty_rmux::link();
    bootty_tmux::link();
    let backends = bootty_mux::provider::MuxBackendRegistry::collect([
        bootty_mux::MuxBackendKind::Rmux,
        bootty_mux::MuxBackendKind::Tmux,
    ])?;
    Catalog::open(
        path,
        ApplicationIdentity::Development,
        std::sync::Arc::new(backends),
    )
}

#[test]
fn daemon_uses_the_stored_backend_provider_without_desktop_fallback() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let provider: Arc<dyn MuxBackendProvider> = Arc::new(MarkerProvider);
    let backends = Arc::new(MuxBackendRegistry::from_core_providers(
        [provider],
        [MuxBackendKind::Tmux],
    )?);
    let mut catalog = Catalog::open(
        &directory.path().join("catalog.sqlite"),
        ApplicationIdentity::Development,
        backends,
    )?;
    let space = catalog.create("Stored tmux", Backend::Tmux)?;

    assert_eq!(
        catalog.snapshot(&space.id, Backend::Tmux)?,
        MuxSnapshot::default()
    );
    Ok(())
}

fn create_space(catalog: &mut Catalog, name: &str) -> Result<String> {
    Ok(catalog.create(name, Backend::Rmux)?.id)
}

fn connection(path: &Path) -> Connection {
    Connection::open(path).expect("catalog database")
}

fn pending_count(path: &Path, space_id: &str) -> i64 {
    connection(path)
        .query_row(
            "SELECT COUNT(*) FROM remote_space_pending_membership_operations
             WHERE space_id = ?1",
            [space_id],
            |row| row.get(0),
        )
        .expect("pending operation count")
}

fn stored_sessions(path: &Path, space_id: &str) -> Vec<(String, i64)> {
    let connection = connection(path);
    let mut statement = connection
        .prepare(
            "SELECT session_name, position FROM remote_space_sessions
             WHERE space_id = ?1 ORDER BY position",
        )
        .expect("session query");
    statement
        .query_map([space_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("session rows")
        .collect::<rusqlite::Result<_>>()
        .expect("stored sessions")
}

fn create_command(name: &str) -> MuxCommand {
    MuxCommand::CreateProjectSession {
        session_id: name.to_owned(),
        cwd: "/tmp".to_owned(),
    }
}

#[test]
fn a_journal_failure_prevents_the_backend_call() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Journal failure")?;
    connection(&path).execute_batch(
        "CREATE TRIGGER fail_pending_insert
         BEFORE INSERT ON remote_space_pending_membership_operations
         BEGIN SELECT RAISE(FAIL, 'forced journal failure'); END;",
    )?;
    let mut backend = ScriptedBackend::default();

    let error = catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            create_command("blocked"),
            &mut backend,
        )
        .unwrap_err();

    assert!(error.to_string().contains("forced journal failure"));
    assert_eq!(backend.execute_calls, 0);
    assert_eq!(pending_count(&path, &space_id), 0);
    Ok(())
}

#[test]
fn a_completed_create_recovers_after_the_catalog_commit_fails() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Create recovery")?;
    connection(&path).execute_batch(
        "CREATE TRIGGER fail_session_insert
         BEFORE INSERT ON remote_space_sessions
         BEGIN SELECT RAISE(FAIL, 'forced membership failure'); END;",
    )?;
    let mut backend = ScriptedBackend::default();

    let error = catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            create_command("created"),
            &mut backend,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "remote Space membership completed but catalog commit failed: forced membership failure; recovery is pending"
    );
    assert_eq!(pending_count(&path, &space_id), 1);
    assert_eq!(stored_sessions(&path, &space_id), Vec::new());

    connection(&path).execute("DROP TRIGGER fail_session_insert", [])?;
    drop(catalog);
    let mut reopened = open_catalog(&path)?;
    let snapshot = reopened.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;

    assert_eq!(snapshot.sessions[0].name, "created");
    assert_eq!(
        stored_sessions(&path, &space_id),
        vec![("created".to_owned(), 0)]
    );
    assert_eq!(pending_count(&path, &space_id), 0);
    Ok(())
}

#[test]
fn ambiguous_backend_failure_keeps_intent_and_other_spaces_continue() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let first_space = create_space(&mut catalog, "Ambiguous")?;
    let second_space = create_space(&mut catalog, "Independent")?;
    let mut ambiguous = ScriptedBackend {
        fail_after_apply: true,
        ..ScriptedBackend::default()
    };

    let error = catalog
        .execute_with_backend(
            &first_space,
            Backend::Rmux,
            create_command("ambiguous"),
            &mut ambiguous,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "remote backend result is ambiguous: scripted transport disconnected after apply; remote Space membership recovery is pending"
    );
    assert_eq!(pending_count(&path, &first_space), 1);

    let mut independent = ScriptedBackend::default();
    catalog.execute_with_backend(
        &second_space,
        Backend::Rmux,
        create_command("independent"),
        &mut independent,
    )?;
    assert_eq!(stored_sessions(&path, &second_space)[0].0, "independent");

    ambiguous.fail_after_apply = false;
    catalog.snapshot_with_backend(&first_space, Backend::Rmux, &mut ambiguous)?;
    assert_eq!(stored_sessions(&path, &first_space)[0].0, "ambiguous");
    assert_eq!(pending_count(&path, &first_space), 0);
    Ok(())
}

#[test]
fn a_concurrent_snapshot_cannot_discard_an_in_flight_operation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Concurrent")?;
    drop(catalog);

    let shared_snapshot = Arc::new(Mutex::new(MuxSnapshot::default()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let first_path = path.clone();
    let first_space = space_id.clone();
    let first_snapshot = Arc::clone(&shared_snapshot);
    let first = thread::spawn(move || -> Result<()> {
        let mut catalog = open_catalog(&first_path)?;
        let mut backend = PausingBackend {
            snapshot: first_snapshot,
            entered: entered_tx,
            resume: resume_rx,
        };
        catalog.execute_with_backend(
            &first_space,
            Backend::Rmux,
            create_command("in-flight"),
            &mut backend,
        )
    });

    entered_rx.recv()?;
    assert_eq!(pending_count(&path, &space_id), 1);

    let second_path = path.clone();
    let second_space = space_id.clone();
    let second_snapshot = Arc::clone(&shared_snapshot);
    let (finished_tx, finished_rx) = mpsc::channel();
    let second = thread::spawn(move || -> Result<()> {
        let mut catalog = open_catalog(&second_path)?;
        let mut backend = SharedSnapshotBackend(second_snapshot);
        catalog.snapshot_with_backend(&second_space, Backend::Rmux, &mut backend)?;
        finished_tx.send(())?;
        Ok(())
    });

    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert_eq!(pending_count(&path, &space_id), 1);

    resume_tx.send(())?;
    first.join().expect("first catalog thread")?;
    finished_rx.recv_timeout(Duration::from_secs(2))?;
    second.join().expect("second catalog thread")?;

    assert_eq!(pending_count(&path, &space_id), 0);
    assert_eq!(
        stored_sessions(&path, &space_id),
        vec![("in-flight".to_owned(), 0)]
    );
    Ok(())
}

#[test]
fn two_spaces_cannot_claim_the_same_backend_session_concurrently() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let first_space = create_space(&mut catalog, "First claim")?;
    let second_space = create_space(&mut catalog, "Second claim")?;
    drop(catalog);

    let shared_snapshot = Arc::new(Mutex::new(MuxSnapshot::default()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let first_path = path.clone();
    let first_thread_space = first_space.clone();
    let first_snapshot = Arc::clone(&shared_snapshot);
    let first = thread::spawn(move || -> Result<()> {
        let mut catalog = open_catalog(&first_path)?;
        let mut backend = PausingBackend {
            snapshot: first_snapshot,
            entered: entered_tx,
            resume: resume_rx,
        };
        catalog.execute_with_backend(
            &first_thread_space,
            Backend::Rmux,
            create_command("shared-name"),
            &mut backend,
        )
    });

    entered_rx.recv()?;
    let second_path = path.clone();
    let second_thread_space = second_space.clone();
    let second_snapshot = Arc::clone(&shared_snapshot);
    let (finished_tx, finished_rx) = mpsc::channel();
    let second = thread::spawn(move || -> Result<()> {
        let mut catalog = open_catalog(&second_path)?;
        let mut backend = SharedSnapshotBackend(second_snapshot);
        let error = catalog
            .execute_with_backend(
                &second_thread_space,
                Backend::Rmux,
                create_command("shared-name"),
                &mut backend,
            )
            .unwrap_err();
        finished_tx.send(error.to_string())?;
        Ok(())
    });

    assert!(matches!(
        finished_rx.recv_timeout(Duration::from_millis(50)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    resume_tx.send(())?;
    first.join().expect("first Space claim thread")?;
    assert_eq!(
        finished_rx.recv_timeout(Duration::from_secs(2))?,
        "session already belongs to another remote Space"
    );
    second.join().expect("second Space claim thread")?;

    assert_eq!(
        stored_sessions(&path, &first_space),
        vec![("shared-name".to_owned(), 0)]
    );
    assert_eq!(stored_sessions(&path, &second_space), Vec::new());
    assert_eq!(pending_count(&path, &first_space), 0);
    assert_eq!(pending_count(&path, &second_space), 0);
    Ok(())
}

fn assert_rename_recovery(name_keyed: bool) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Rename recovery")?;
    let mut backend = ScriptedBackend {
        name_keyed,
        ..ScriptedBackend::default()
    };
    catalog.execute_with_backend(
        &space_id,
        Backend::Rmux,
        create_command("before"),
        &mut backend,
    )?;
    connection(&path).execute_batch(
        "CREATE TRIGGER fail_session_rename
         BEFORE UPDATE ON remote_space_sessions
         BEGIN SELECT RAISE(FAIL, 'forced rename failure'); END;",
    )?;

    catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            MuxCommand::RenameSession {
                session_id: "before".to_owned(),
                name: "after".to_owned(),
            },
            &mut backend,
        )
        .unwrap_err();
    assert_eq!(pending_count(&path, &space_id), 1);
    assert_eq!(
        stored_sessions(&path, &space_id),
        vec![("before".to_owned(), 0)]
    );

    connection(&path).execute("DROP TRIGGER fail_session_rename", [])?;
    drop(catalog);
    let mut reopened = open_catalog(&path)?;
    reopened.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(
        stored_sessions(&path, &space_id),
        vec![("after".to_owned(), 0)]
    );
    assert_eq!(pending_count(&path, &space_id), 0);
    Ok(())
}

#[test]
fn stable_id_and_name_keyed_renames_recover_without_reordering() -> Result<()> {
    assert_rename_recovery(false)?;
    assert_rename_recovery(true)
}

#[test]
fn ditch_recovery_waits_for_authoritative_absence() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Ditch recovery")?;
    let mut backend = ScriptedBackend::default();
    catalog.execute_with_backend(
        &space_id,
        Backend::Rmux,
        create_command("ditched"),
        &mut backend,
    )?;
    connection(&path).execute_batch(
        "CREATE TRIGGER fail_session_delete
         BEFORE DELETE ON remote_space_sessions
         BEGIN SELECT RAISE(FAIL, 'forced ditch failure'); END;",
    )?;

    catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            MuxCommand::DitchSession {
                session_id: "ditched".to_owned(),
            },
            &mut backend,
        )
        .unwrap_err();
    assert_eq!(stored_sessions(&path, &space_id)[0].0, "ditched");
    assert_eq!(pending_count(&path, &space_id), 1);

    connection(&path).execute("DROP TRIGGER fail_session_delete", [])?;
    drop(catalog);
    let mut reopened = open_catalog(&path)?;
    reopened.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(stored_sessions(&path, &space_id), Vec::new());
    assert_eq!(pending_count(&path, &space_id), 0);
    Ok(())
}

#[test]
fn malformed_pending_state_stops_before_backend_mutation() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Corrupt journal")?;
    let connection = connection(&path);
    connection.execute_batch("PRAGMA ignore_check_constraints = ON;")?;
    connection.execute(
        "INSERT INTO remote_space_pending_membership_operations
         (space_id, operation, session_id, old_name, new_name)
         VALUES (?1, 'create', 'broken', 'forbidden', NULL)",
        [&space_id],
    )?;
    let mut backend = ScriptedBackend::default();

    let error = catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            create_command("not-run"),
            &mut backend,
        )
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "pending membership operation has an invalid shape"
    );
    assert_eq!(backend.execute_calls, 0);
    assert_eq!(pending_count(&path, &space_id), 1);
    Ok(())
}

#[test]
fn a_pre_journal_catalog_reopens_without_protocol_or_order_changes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let connection = connection(&path);
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         CREATE TABLE daemon_metadata (key TEXT PRIMARY KEY);
         CREATE TABLE remote_spaces (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL UNIQUE,
             backend TEXT NOT NULL,
             position INTEGER NOT NULL
         );
         CREATE TABLE remote_space_sessions (
             space_id TEXT NOT NULL REFERENCES remote_spaces(id) ON DELETE CASCADE,
             session_name TEXT NOT NULL,
             position INTEGER NOT NULL,
             PRIMARY KEY (space_id, session_name)
         );
         INSERT INTO daemon_metadata (key) VALUES ('legacy_catalog_migrated');
         INSERT INTO remote_spaces (id, name, backend, position)
         VALUES ('second', 'Second', 'rmux', 1), ('first', 'First', 'rmux', 0);
         INSERT INTO remote_space_sessions (space_id, session_name, position)
         VALUES ('first', 'one', 0), ('first', 'two', 1);",
    )?;
    drop(connection);

    let catalog = open_catalog(&path)?;
    let spaces = catalog.list()?;

    assert_eq!(
        spaces
            .iter()
            .map(|space| space.id.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(
        spaces
            .iter()
            .all(|space| space.catalog_version == CATALOG_VERSION)
    );
    assert_eq!(
        stored_sessions(&path, "first"),
        vec![("one".to_owned(), 0), ("two".to_owned(), 1)]
    );
    assert_eq!(CATALOG_VERSION, 3);
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_daemon_recovers_rmux_success_after_catalog_failure() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let rmux_root = directory.path().join("rmux");
    std::fs::create_dir(&rmux_root)?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "real_daemon_recovers_rmux_success_after_catalog_failure_helper",
        ])
        .env(REAL_DAEMON_HELPER_ENV, "1")
        .env("RMUX_TMPDIR", rmux_root)
        .env("BOOTTY_DAEMON_RECOVERY_ROOT", directory.path())
        .status()?;

    assert!(status.success());
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_daemon_recovers_rmux_success_after_catalog_failure_helper() -> Result<()> {
    if std::env::var_os(REAL_DAEMON_HELPER_ENV).is_none() {
        return Ok(());
    }
    start_embedded_rmux_daemon_for_tests()?;
    let root = std::path::PathBuf::from(
        std::env::var_os("BOOTTY_DAEMON_RECOVERY_ROOT").expect("recovery root"),
    );
    let state = root.join("daemon.sqlite");
    let empty_path = root.join("empty-path");
    std::fs::create_dir_all(&empty_path)?;
    let daemon = env!("CARGO_BIN_EXE_bootty-daemon");
    let run = |args: &[String]| {
        std::process::Command::new(daemon)
            .env("BOOTTY_DAEMON_STATE", &state)
            .env("PATH", &empty_path)
            .env("SHELL", "/bin/sh")
            .env("BOOTTY_SHELL", "/bin/sh")
            .args(args)
            .output()
    };

    let created = run(&[
        "remote-space".to_owned(),
        "create".to_owned(),
        "--name".to_owned(),
        "Recovery".to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
    ])?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let space: serde_json::Value = serde_json::from_slice(&created.stdout)?;
    let space_id = space["id"].as_str().expect("Space id");
    connection(&state).execute_batch(
        "CREATE TRIGGER fail_real_session_insert
         BEFORE INSERT ON remote_space_sessions
         BEGIN SELECT RAISE(FAIL, 'forced real membership failure'); END;",
    )?;
    let session_id = format!("bootty-recovery-{}", std::process::id());
    let payload =
        bootty_remote::space_protocol::encode_command(&MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: root.to_string_lossy().into_owned(),
        })?;

    let executed = run(&[
        "remote-space".to_owned(),
        "execute".to_owned(),
        "--id".to_owned(),
        space_id.to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
        "--payload".to_owned(),
        payload,
    ])?;
    assert!(!executed.status.success());
    assert!(
        String::from_utf8_lossy(&executed.stderr)
            .contains("remote Space membership completed but catalog commit failed")
    );
    assert_eq!(pending_count(&state, space_id), 1);

    connection(&state).execute("DROP TRIGGER fail_real_session_insert", [])?;
    let snapshot = run(&[
        "remote-space".to_owned(),
        "snapshot".to_owned(),
        "--id".to_owned(),
        space_id.to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
    ])?;
    assert!(
        snapshot.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    let snapshot: MuxSnapshot = serde_json::from_slice(&snapshot.stdout)?;
    assert!(
        snapshot
            .sessions
            .iter()
            .any(|session| session.name == session_id)
    );
    assert_eq!(pending_count(&state, space_id), 0);

    let ditch = bootty_remote::space_protocol::encode_command(&MuxCommand::DitchSession {
        session_id: session_id.clone(),
    })?;
    let cleaned = run(&[
        "remote-space".to_owned(),
        "execute".to_owned(),
        "--id".to_owned(),
        space_id.to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
        "--payload".to_owned(),
        ditch,
    ])?;
    assert!(
        cleaned.status.success(),
        "{}",
        String::from_utf8_lossy(&cleaned.stderr)
    );
    Ok(())
}
