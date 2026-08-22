use std::{
    path::Path,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::{Result, bail};
use bootty_daemon::catalog::{Backend, CATALOG_VERSION, Catalog};
use bootty_identity::ApplicationIdentity;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    command::MuxCommand,
    provider::{MuxBackendProvider, MuxBackendRegistry},
    snapshot::{MuxPaneAnchor, MuxSession, MuxSessionTag, MuxSnapshot, session_matches},
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
        tag: MuxSessionTag::default(),
    }
}

fn tagged_session(id: &str, name: &str, space_id: &str) -> MuxSession {
    MuxSession {
        tag: MuxSessionTag {
            identity: Some(format!("{id}-identity")),
            space: Some(space_id.to_owned()),
        },
        ..session(id, name)
    }
}

/// A backend that actually applies `StampSession`, so adoption can be observed.
struct StampingBackend {
    sessions: Vec<MuxSession>,
}

impl MuxBackend for StampingBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot {
            sessions: self.sessions.clone(),
            ..MuxSnapshot::default()
        })
    }

    fn execute(&mut self, command: MuxCommand) -> Result<()> {
        if let MuxCommand::StampSession { session_id, tag } = command
            && let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == session_id)
        {
            session.tag = tag;
        }
        Ok(())
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

/// A Space's sessions are the ones carrying its tag. Nothing else is visible through it.
#[test]
fn a_snapshot_answers_with_the_sessions_that_carry_this_spaces_tag() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Tagged")?;
    let other_id = create_space(&mut catalog, "Other")?;

    let mut backend = ScriptedBackend {
        snapshot: MuxSnapshot {
            sessions: vec![
                tagged_session("$1", "mine", &space_id),
                tagged_session("$2", "theirs", &other_id),
                session("$3", "unclaimed"),
            ],
            active_session_id: Some("$2".to_owned()),
            ..MuxSnapshot::default()
        },
        execute_calls: 0,
        fail_after_apply: false,
    };

    let snapshot = catalog.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["mine"]
    );
    assert_eq!(
        snapshot.active_session_id, None,
        "a selection pointing outside this Space does not survive the filter"
    );
    Ok(())
}

#[test]
fn a_command_against_another_spaces_session_is_refused() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Mine")?;
    let other_id = create_space(&mut catalog, "Theirs")?;

    let mut backend = ScriptedBackend {
        snapshot: MuxSnapshot {
            sessions: vec![tagged_session("$2", "theirs", &other_id)],
            ..MuxSnapshot::default()
        },
        execute_calls: 0,
        fail_after_apply: false,
    };

    let error = catalog
        .execute_with_backend(
            &space_id,
            Backend::Rmux,
            MuxCommand::DitchSession {
                session_id: "$2".to_owned(),
            },
            &mut backend,
        )
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!("session does not belong to remote Space {space_id}")
    );
    assert_eq!(backend.execute_calls, 0);
    Ok(())
}

/// Membership used to live in a table of names beside the multiplexer. It is stamped onto the
/// sessions themselves once, and the table is dropped.
#[test]
fn membership_recorded_by_name_is_adopted_once_and_then_forgotten() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.sqlite");
    let mut catalog = open_catalog(&path)?;
    let space_id = create_space(&mut catalog, "Upgraded")?;
    connection(&path).execute(
        "INSERT INTO remote_space_sessions (space_id, session_name, position)
         VALUES (?1, 'recorded', 0)",
        [&space_id],
    )?;

    let mut backend = StampingBackend {
        sessions: vec![session("$1", "recorded"), session("$2", "not-recorded")],
    };
    let snapshot = catalog.snapshot_with_backend(&space_id, Backend::Rmux, &mut backend)?;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect::<Vec<_>>(),
        ["recorded"]
    );
    assert!(
        stored_sessions(&path, &space_id).is_empty(),
        "the name-keyed rows are gone once their sessions carry the tag"
    );
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
fn a_real_daemon_round_trips_a_session_tag_through_rmux() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let rmux_root = directory.path().join("rmux");
    std::fs::create_dir(&rmux_root)?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "a_real_daemon_round_trips_a_session_tag_through_rmux_helper",
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
fn a_real_daemon_round_trips_a_session_tag_through_rmux_helper() -> Result<()> {
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
    let session_id = format!("bootty-recovery-{}", std::process::id());
    let identity = format!("{session_id}-identity");
    let payload =
        bootty_remote::space_protocol::encode_command(&MuxCommand::CreateProjectSession {
            session_id: session_id.clone(),
            cwd: root.to_string_lossy().into_owned(),
            tag: MuxSessionTag {
                identity: Some(identity.clone()),
                space: Some(space_id.to_owned()),
            },
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
    assert!(
        executed.status.success(),
        "{}",
        String::from_utf8_lossy(&executed.stderr)
    );

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
    // The tag survives the whole round trip: written by the client, stored by rmux, read back
    // through the daemon's JSON snapshot.
    let snapshot: MuxSnapshot = serde_json::from_slice(&snapshot.stdout)?;
    let created = snapshot
        .sessions
        .iter()
        .find(|session| session.name == session_id)
        .expect("the created session is visible through its Space");
    assert_eq!(created.tag.identity.as_deref(), Some(identity.as_str()));
    assert_eq!(created.tag.space.as_deref(), Some(space_id));

    // rmux does not migrate its option store on rename, so a tag stored against the session name
    // would be orphaned here. Nothing compensates for that any more: the tag is keyed on the
    // session's stable id, and a rename simply does not touch it.
    let renamed_name = format!("{session_id}-renamed");
    let rename = bootty_remote::space_protocol::encode_command(&MuxCommand::RenameSession {
        session_id: session_id.clone(),
        name: renamed_name.clone(),
    })?;
    let renamed = run(&[
        "remote-space".to_owned(),
        "execute".to_owned(),
        "--id".to_owned(),
        space_id.to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
        "--payload".to_owned(),
        rename,
    ])?;
    assert!(
        renamed.status.success(),
        "{}",
        String::from_utf8_lossy(&renamed.stderr)
    );
    let snapshot = run(&[
        "remote-space".to_owned(),
        "snapshot".to_owned(),
        "--id".to_owned(),
        space_id.to_owned(),
        "--backend".to_owned(),
        "rmux".to_owned(),
    ])?;
    let snapshot: MuxSnapshot = serde_json::from_slice(&snapshot.stdout)?;
    let renamed = snapshot
        .sessions
        .iter()
        .find(|session| session.name == renamed_name)
        .expect("the renamed session still belongs to its Space");
    assert_eq!(renamed.tag.identity.as_deref(), Some(identity.as_str()));
    let session_id = renamed_name;

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
