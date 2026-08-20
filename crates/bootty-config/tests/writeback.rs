use std::fs;

use bootty_config::config::{load_config_from_path, update_config_document};

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
    assert!(
        (load_config_from_path(&path)
            .expect("reopen config")
            .font
            .size
            - 15.0)
            .abs()
            < f32::EPSILON
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
fn a_pre_replacement_error_keeps_the_existing_file() {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let path = directory.path().join("config.toml");
    let original = "[window]\ntitle = \"old\"\n";
    fs::write(&path, original).expect("write initial config");

    let error = update_config_document(&path, |document| {
        document.set_item(&[], toml_edit::value("reject candidate"))
    })
    .expect_err("reject mutation");

    assert_eq!(error.to_string(), "config writeback path cannot be empty");
    assert_eq!(fs::read_to_string(&path).expect("read config"), original);
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
        document.set_item(&["window", "title"], toml_edit::value("new"))
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
