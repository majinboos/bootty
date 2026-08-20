#[cfg(unix)]
use std::{process::Command, sync::Arc};

#[cfg(unix)]
use bootty_app::{
    commands::{Caller, CommandCatalog, app_command_channel},
    control::{ControlPlane, ControlServer},
};

#[cfg(unix)]
const HELPER_ENV: &str = "BOOTTY_APPLICATION_CONTROL_TEST_HELPER";

#[cfg(unix)]
#[test]
fn one_process_owns_each_application_identity() {
    let runtime = tempfile::tempdir().expect("temporary runtime directory");
    let status = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", "singleton_helper"])
        .env(HELPER_ENV, "1")
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("RMUX_TMPDIR", runtime.path())
        .status()
        .expect("run isolated singleton check");

    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn singleton_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }

    let (sender, _receiver) = app_command_channel(1);
    let catalog = Arc::new(CommandCatalog::default());
    let first = ControlServer::spawn(
        "main".to_owned(),
        sender.for_caller(Caller::Socket),
        Arc::clone(&catalog),
        ControlPlane::default(),
    )
    .expect("first application claims its identity");

    let duplicate = ControlServer::spawn(
        "other-window".to_owned(),
        sender.for_caller(Caller::Socket),
        Arc::clone(&catalog),
        ControlPlane::default(),
    );
    assert_eq!(
        duplicate
            .err()
            .expect("a duplicate application must be rejected")
            .to_string(),
        "BoottyDev is already running"
    );

    drop(first);
    ControlServer::spawn(
        "main".to_owned(),
        sender.for_caller(Caller::Socket),
        catalog,
        ControlPlane::default(),
    )
    .expect("the identity is released when the application stops");
}
