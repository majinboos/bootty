use std::{fs, time::Instant};

use bootty_config::{
    config::load_config_from_path,
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload},
};

#[test]
fn fixing_a_failed_include_triggers_recovery_without_touching_its_parent() {
    let sandbox = tempfile::tempdir().expect("create config directory");
    let root = sandbox.path().join("config.toml");
    let child = sandbox.path().join("local.toml");
    fs::write(&root, "[window]\ntitle = \"last good\"\n").expect("write initial config");
    let current = load_config_from_path(&root).expect("load initial config");
    let mut reload = ConfigHotReload::new(&root);
    let first_check = Instant::now() + CONFIG_HOT_RELOAD_INTERVAL;

    fs::write(&child, "[window\ntitle = \"broken\"\n").expect("write invalid include");
    fs::write(
        &root,
        "include = [\"local.toml\"]\n\n[window]\ntitle = \"parent\"\n",
    )
    .expect("add include");
    assert!(reload.changed(first_check));

    let error = reload
        .reload_config()
        .expect_err("invalid include must reject the candidate");
    assert_eq!(current.window.title, "last good");
    assert!(error.to_string().contains("local.toml"));

    fs::write(&child, "[window]\ntitle = \"recovered from child\"\n").expect("fix included config");
    assert!(reload.changed(first_check + CONFIG_HOT_RELOAD_INTERVAL));

    let recovered = reload.reload_config().expect("reload fixed include");
    assert_eq!(recovered.window.title, "recovered from child");
}

#[test]
fn creating_an_optional_include_triggers_reload() {
    let sandbox = tempfile::tempdir().expect("create config directory");
    let root = sandbox.path().join("config.toml");
    let child = sandbox.path().join("optional.toml");
    fs::write(
        &root,
        "include = [\"?optional.toml\"]\n\n[window]\ntitle = \"parent\"\n",
    )
    .expect("write config with optional include");
    let mut reload = ConfigHotReload::new(&root);
    let check = Instant::now() + CONFIG_HOT_RELOAD_INTERVAL;

    fs::write(&child, "[window]\ntitle = \"optional child\"\n").expect("create optional include");

    assert!(reload.changed(check));
    let config = reload.reload_config().expect("load optional include");
    assert_eq!(config.window.title, "optional child");
}

#[test]
fn creating_a_missing_required_include_recovers_the_failed_candidate() {
    let sandbox = tempfile::tempdir().expect("create config directory");
    let root = sandbox.path().join("config.toml");
    let child = sandbox.path().join("required.toml");
    fs::write(&root, "[window]\ntitle = \"last good\"\n").expect("write initial config");
    let mut reload = ConfigHotReload::new(&root);
    let first_check = Instant::now() + CONFIG_HOT_RELOAD_INTERVAL;

    fs::write(
        &root,
        "include = [\"required.toml\"]\n\n[window]\ntitle = \"parent\"\n",
    )
    .expect("add required include");
    assert!(reload.changed(first_check));
    let error = reload
        .reload_config()
        .expect_err("missing required include must fail");
    assert_eq!(
        error.to_string(),
        format!("config file not found: {}", child.display())
    );

    fs::write(&child, "[window]\ntitle = \"required child\"\n").expect("create required include");
    assert!(reload.changed(first_check + CONFIG_HOT_RELOAD_INTERVAL));
    let config = reload.reload_config().expect("load required include");
    assert_eq!(config.window.title, "required child");
}
