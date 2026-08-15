use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use bootty_config::color::Color;
use bootty_config::config::{
    SegmentAlign, SshAuthenticationConfig, SshHostKeyPolicyConfig, SshProfileConfig, StatusSegment,
    load_config_from_path, update_config_document,
};

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

    update_config_document(&path, |document| document.set_f32(&["font", "size"], 15.0))
        .expect("replace config");

    let written = fs::read_to_string(&path).expect("read replaced config");
    assert!(written.contains("# user comment"));
    assert!(written.contains("include = [\"?local.toml\"]"));
    assert!(written.contains("title = \"Keep\""));
    assert!(written.contains("sidebar = true"));
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
        document.set_str(&["window", "title"], "Private")
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

#[test]
fn ssh_profile_writeback_replaces_and_removes_only_the_named_profile() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        "# preserve this comment\n\n[window]\n# preserve this window comment\ntitle = \"Keep\"\n",
    )
    .expect("write initial config");

    let full = SshProfileConfig {
        name: "Full Profile".to_owned(),
        host: "full.example.test".to_owned(),
        user: Some("luan".to_owned()),
        port: Some(2222),
        authentication: SshAuthenticationConfig::KeyFile,
        host_key_policy: SshHostKeyPolicyConfig::AcceptNew,
        identity_file: Some(PathBuf::from("/tmp/full-key")),
        proxy_jump: Some("gateway".to_owned()),
        program: "ssh-wrapper".to_owned(),
        args: vec!["-v".to_owned(), "--flag".to_owned()],
    };
    update_config_document(&path, |document| document.set_ssh_profile("lab", &full))
        .expect("write full SSH profile");
    assert_eq!(
        load_config_from_path(&path)
            .expect("load full SSH profile")
            .ssh_profiles
            .get("lab"),
        Some(&full)
    );

    let replacement = SshProfileConfig {
        name: "Replacement".to_owned(),
        host: "replacement.example.test".to_owned(),
        user: None,
        port: None,
        authentication: SshAuthenticationConfig::Auto,
        host_key_policy: SshHostKeyPolicyConfig::Strict,
        identity_file: None,
        proxy_jump: None,
        program: "ssh".to_owned(),
        args: Vec::new(),
    };
    update_config_document(&path, |document| {
        document.set_ssh_profile("lab", &replacement)
    })
    .expect("replace SSH profile");
    let written = fs::read_to_string(&path).expect("read replaced config");
    assert!(written.contains("# preserve this comment"));
    assert!(written.contains("# preserve this window comment"));
    assert!(written.contains("title = \"Keep\""));
    assert!(!written.contains("user = \"luan\""));
    assert!(!written.contains("port = 2222"));
    assert!(!written.contains("identity-file = \"/tmp/full-key\""));
    assert!(!written.contains("proxy-jump = \"gateway\""));
    assert!(!written.contains("args ="));
    assert_eq!(
        load_config_from_path(&path)
            .expect("load replacement SSH profile")
            .ssh_profiles
            .get("lab"),
        Some(&replacement)
    );

    update_config_document(&path, |document| document.remove_ssh_profile("lab"))
        .expect("remove SSH profile");
    let written = fs::read_to_string(&path).expect("read deleted config");
    assert!(!written.contains("[ssh-profiles.lab]"));
    assert!(written.contains("# preserve this comment"));
    assert!(written.contains("# preserve this window comment"));
    assert!(written.contains("title = \"Keep\""));
}

#[test]
fn a_pre_replacement_error_keeps_the_existing_file() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let original = "[window]\ntitle = \"old\"\n";
    fs::write(&path, original).expect("write initial config");

    let error = update_config_document(&path, |document| document.set_str(&[], "reject candidate"))
        .expect_err("reject mutation");

    assert_eq!(error.to_string(), "config writeback path cannot be empty");
    assert_eq!(fs::read_to_string(&path).expect("read config"), original);
}

#[test]
fn status_bar_writeback_preserves_segments_legacy_cleanup_comments_and_order() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        "# keep this comment\ninclude = [\"?local.toml\"]\n\n[window]\n# keep window comment\ntitle = \"Keep\"\n\n[chrome]\nstatus-bar = false\nstatus-segment = [{ module = \"legacy\" }]\ntop-segment = [{ module = \"old-top\" }]\nbottom-segment = [{ module = \"old-bottom\" }]\nsidebar = true\n",
    )
    .expect("write initial config");

    let top_segments = vec![
        StatusSegment {
            align: SegmentAlign::Left,
            module: "left".to_owned(),
            fg: Some(Color {
                r: 0xab,
                g: 0xcd,
                b: 0xef,
                a: 0xff,
            }),
            bg: None,
            icon: Some(String::new()),
        },
        StatusSegment {
            align: SegmentAlign::Center,
            module: "center".to_owned(),
            fg: None,
            bg: Some(Color {
                r: 0x01,
                g: 0x02,
                b: 0x03,
                a: 0x80,
            }),
            icon: Some("◆".to_owned()),
        },
        StatusSegment {
            align: SegmentAlign::Right,
            module: "right".to_owned(),
            ..StatusSegment::default()
        },
    ];
    let bottom_segments = vec![StatusSegment {
        align: SegmentAlign::Right,
        module: "bottom".to_owned(),
        ..StatusSegment::default()
    }];

    update_config_document(&path, |document| {
        document.set_bottom_status_segments(&bottom_segments)
    })
    .expect("write bottom status segments");
    let after_bottom = fs::read_to_string(&path).expect("read bottom status segments");
    assert!(after_bottom.contains("status-bar = false"));
    assert!(after_bottom.contains("status-segment = [{ module = \"legacy\" }]"));

    update_config_document(&path, |document| {
        document.set_top_bar_enabled(true)?;
        document.set_top_status_segments(&top_segments)
    })
    .expect("write top status segments");

    let written = fs::read_to_string(&path).expect("read status segments");
    assert!(written.contains("# keep this comment"));
    assert!(written.contains("# keep window comment"));
    assert!(written.contains("sidebar = true"));
    assert!(!written.contains("status-bar ="));
    assert!(!written.contains("status-segment ="));
    assert!(written.contains("top-bar = true"));
    assert!(written.contains("align = \"left\""));
    assert!(written.contains("align = \"center\""));
    assert!(written.contains("align = \"right\""));
    assert!(written.contains("fg = \"#abcdef\""));
    assert!(written.contains("bg = \"#01020380\""));
    assert!(!written.contains("icon = \"\""));
    assert!(written.find("top-segment").unwrap() < written.find("bottom-segment").unwrap());
    assert!(written.find("include").unwrap() < written.find("[window]").unwrap());
    assert!(written.find("[window]").unwrap() < written.find("[chrome]").unwrap());

    let config = load_config_from_path(&path).expect("load status segments");
    let mut expected_top_segments = top_segments.clone();
    expected_top_segments[0].icon = None;
    assert_eq!(config.chrome.top_segments, expected_top_segments);
    assert_eq!(config.chrome.bottom_segments, bottom_segments);
}

#[cfg(unix)]
#[test]
fn writeback_preserves_a_relative_final_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("temporary config directory");
    let target = directory.path().join("target.toml");
    let link = directory.path().join("config.toml");
    fs::write(&target, "[window]\ntitle = \"old\"\n").expect("write target");
    symlink("target.toml", &link).expect("create symlink");

    update_config_document(&link, |document| {
        document.set_str(&["window", "title"], "new")
    })
    .expect("replace target");

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
        document.set_str(&["window", "title"], "Created through link")
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
            document.set_f32(&["font", "size"], 14.0)
        })
        .expect("first config write");
    } else {
        update_config_document(path, |document| {
            document.set_bool(&["chrome", "sidebar"], false)
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
