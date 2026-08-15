use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::app::AppState;
use bootty_config::config::{
    MultiplexerBackendConfig, load_config_from_path, update_config_document,
};

mod support;

#[test]
fn atomic_writeback_preserves_structure_and_unix_mode() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let source = "# user comment\ninclude = [\"?local.toml\"]\n\n[window]\ntitle = \"Keep\"\n\n[chrome]\nsidebar = true\n";
    fs::write(&path, source).expect("write initial config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("set initial mode");
    }

    update_config_document(&path, |document| {
        document.set_item(&["font", "size"], toml_edit::value(15.0))
    })
    .expect("replace config");

    let written = fs::read_to_string(&path).expect("read replaced config");
    assert!(written.contains("# user comment"));
    assert!(written.contains("include = [\"?local.toml\"]"));
    assert!(written.contains("title = \"Keep\""));
    assert!(written.contains("sidebar = true"));
    assert!(written.contains("size = 15.0"));
    assert_eq!(
        load_config_from_path(&path)
            .expect("reopen config")
            .font
            .size,
        15.0
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path)
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o640
        );
    }
}

#[test]
fn a_new_config_is_private_and_loadable() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("nested/config.toml");

    update_config_document(&path, |document| {
        document.set_item(&["window", "title"], toml_edit::value("Private"))
    })
    .expect("create config");

    assert_eq!(
        load_config_from_path(&path)
            .expect("load new config")
            .window
            .title,
        "Private"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path)
                .expect("config metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o600
        );
    }
}

#[cfg(unix)]
#[test]
fn writeback_keeps_a_relative_symlink_and_replaces_its_target() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary config directory");
    let target = directory.path().join("target.toml");
    let link = directory.path().join("config.toml");
    fs::write(&target, "[window]\ntitle = \"old\"\n").expect("write target");
    symlink("target.toml", &link).expect("create relative symlink");

    update_config_document(&link, |document| {
        document.set_item(&["window", "title"], toml_edit::value("new"))
    })
    .expect("replace symlink target");

    assert!(
        fs::symlink_metadata(&link)
            .expect("link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(
        fs::read_to_string(target)
            .expect("read target")
            .contains("title = \"new\"")
    );
}

#[cfg(unix)]
#[test]
fn writeback_creates_a_dangling_symlink_target_without_replacing_the_link() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary config directory");
    let target = directory.path().join("created.toml");
    let link = directory.path().join("config.toml");
    symlink("created.toml", &link).expect("create dangling symlink");

    update_config_document(&link, |document| {
        document.set_item(
            &["window", "title"],
            toml_edit::value("Created through link"),
        )
    })
    .expect("create symlink target");

    assert!(
        fs::symlink_metadata(&link)
            .expect("link metadata")
            .file_type()
            .is_symlink()
    );
    assert!(target.is_file());
}

#[cfg(unix)]
#[test]
fn a_config_symlink_cycle_is_rejected() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary config directory");
    let first = directory.path().join("first.toml");
    let second = directory.path().join("second.toml");
    symlink("second.toml", &first).expect("create first link");
    symlink("first.toml", &second).expect("create second link");

    let error = update_config_document(&first, |_| Ok(())).expect_err("reject symlink cycle");

    assert!(error.to_string().contains("config symlink cycle detected"));
}

#[test]
fn a_failed_app_write_keeps_the_error_visible() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, "[chrome]\nsidebar-width = 320\n").expect("write initial config");
    let mut config = load_config_from_path(&path).expect("load initial config");
    config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
    let mut state = AppState::new(config, support::backends(), Arc::new(|| {}), None, None)
        .expect("start app state");
    fs::remove_file(&path).expect("remove config file");
    fs::create_dir(&path).expect("replace config with directory");

    state.set_sidebar_width_live(444.0);
    state.persist_sidebar_width(444.0);

    assert_eq!(state.config().chrome.sidebar_width, 444.0);
    assert!(
        state
            .last_error()
            .is_some_and(|error| error.contains("config file"))
    );
    assert!(path.is_dir());
}

#[test]
fn bootty_processes_serialize_config_updates() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(&path, "[window]\ntitle = \"base\"\n").expect("write initial config");
    let entered = directory.path().join("entered");
    let resume = directory.path().join("resume");
    let finished = directory.path().join("finished");
    let executable = std::env::current_exe().expect("current test executable");

    let mut first =
        config_writer_process(&executable, &path, "first", &entered, &resume, &finished);
    wait_for_path(&entered);
    let mut second =
        config_writer_process(&executable, &path, "second", &entered, &resume, &finished);

    thread::sleep(Duration::from_millis(50));
    assert!(!finished.exists());
    fs::write(&resume, "resume").expect("release first writer");
    assert!(first.wait().expect("wait for first writer").success());
    assert!(second.wait().expect("wait for second writer").success());
    assert!(finished.exists());

    let written = fs::read_to_string(path).expect("read combined config");
    assert!(written.contains("size = 14.0"));
    assert!(written.contains("sidebar = false"));
}

#[test]
fn config_writer_process_helper() {
    let Some(path) = std::env::var_os("BOOTTY_TEST_CONFIG_WRITE_PATH") else {
        return;
    };
    let role = std::env::var("BOOTTY_TEST_CONFIG_WRITE_ROLE").expect("writer role");
    let entered = PathBuf::from(
        std::env::var_os("BOOTTY_TEST_CONFIG_WRITE_ENTERED").expect("entered marker"),
    );
    let resume =
        PathBuf::from(std::env::var_os("BOOTTY_TEST_CONFIG_WRITE_RESUME").expect("resume marker"));
    let finished = PathBuf::from(
        std::env::var_os("BOOTTY_TEST_CONFIG_WRITE_FINISHED").expect("finished marker"),
    );
    if role == "first" {
        update_config_document(path, |document| {
            fs::write(&entered, "entered").expect("publish entered marker");
            wait_for_path(&resume);
            document.set_item(&["font", "size"], toml_edit::value(14.0))
        })
        .expect("first config write");
    } else {
        update_config_document(path, |document| {
            document.set_item(&["chrome", "sidebar"], toml_edit::value(false))
        })
        .expect("second config write");
        fs::write(finished, "finished").expect("publish finished marker");
    }
}

fn config_writer_process(
    executable: &Path,
    config: &Path,
    role: &str,
    entered: &Path,
    resume: &Path,
    finished: &Path,
) -> std::process::Child {
    Command::new(executable)
        .args(["--exact", "config_writer_process_helper"])
        .env("BOOTTY_TEST_CONFIG_WRITE_PATH", config)
        .env("BOOTTY_TEST_CONFIG_WRITE_ROLE", role)
        .env("BOOTTY_TEST_CONFIG_WRITE_ENTERED", entered)
        .env("BOOTTY_TEST_CONFIG_WRITE_RESUME", resume)
        .env("BOOTTY_TEST_CONFIG_WRITE_FINISHED", finished)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start config writer process")
}

fn wait_for_path(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(5));
    }
}
