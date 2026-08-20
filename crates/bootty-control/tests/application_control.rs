#[cfg(unix)]
use std::{
    fs,
    process::Command,
    sync::{Arc, Barrier},
    thread,
};

#[cfg(unix)]
use bootty_command::{
    AppCommandReceiver, AppCommandSender, Caller, app_command_channel as command_channel,
};
#[cfg(unix)]
use bootty_control::{
    ControlCatalog, ControlPlane, ControlServer, InstanceDescriptor, invoke_instance,
};
#[cfg(unix)]
use bootty_extension::ExtensionCatalog;
#[cfg(unix)]
use bootty_identity::ApplicationIdentity;

#[cfg(unix)]
const HELPER_ENV: &str = "BOOTTY_APPLICATION_CONTROL_TEST_HELPER";

#[cfg(unix)]
fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    command_channel(capacity, Arc::new(|| {}))
}

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

    let runtime = std::path::PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").expect("isolated runtime directory"),
    );
    let descriptor_path = runtime
        .join(ApplicationIdentity::current().cli_name())
        .join("control.json");
    let spawn_server = || {
        let (sender, _receiver) = app_command_channel(1);
        ControlServer::spawn(
            "main",
            sender.for_caller(Caller::Socket),
            Arc::new(ControlCatalog::new(
                Vec::new(),
                Arc::new(ExtensionCatalog::default()),
            )),
            &ControlPlane::default(),
        )
    };

    let first = spawn_server().expect("first application claims its identity");
    let first_descriptor: InstanceDescriptor =
        serde_json::from_slice(&fs::read(&descriptor_path).expect("published first descriptor"))
            .expect("decode first descriptor");
    let described = invoke_instance(
        &first_descriptor,
        "instance.describe",
        serde_json::Value::Null,
    )
    .expect("first endpoint accepts a request");
    assert_eq!(
        described
            .result
            .and_then(|value| serde_json::from_value::<InstanceDescriptor>(value).ok()),
        Some(first_descriptor.clone())
    );

    let duplicate = spawn_server();
    assert_eq!(
        duplicate
            .err()
            .expect("a duplicate application must be rejected")
            .to_string(),
        "BoottyDev is already running"
    );

    let mut observed_stale = first_descriptor.clone();
    observed_stale.started_at_ms = observed_stale.started_at_ms.saturating_add(1000);
    fs::write(
        &descriptor_path,
        serde_json::to_vec(&observed_stale).expect("encode stale descriptor"),
    )
    .expect("install stale descriptor");
    let replacement = spawn_server().expect("a stale descriptor permits replacement");
    let replacement_descriptor: InstanceDescriptor = serde_json::from_slice(
        &fs::read(&descriptor_path).expect("published replacement descriptor"),
    )
    .expect("decode replacement descriptor");
    assert_ne!(
        replacement_descriptor.generation,
        first_descriptor.generation
    );
    assert_ne!(replacement_descriptor.endpoint, first_descriptor.endpoint);

    drop(first);
    assert_eq!(
        serde_json::from_slice::<InstanceDescriptor>(
            &fs::read(&descriptor_path).expect("replacement descriptor survives old shutdown")
        )
        .expect("decode surviving descriptor"),
        replacement_descriptor
    );
    invoke_instance(
        &replacement_descriptor,
        "instance.describe",
        serde_json::Value::Null,
    )
    .expect("replacement endpoint survives old shutdown");
    drop(replacement);

    fs::write(&descriptor_path, b"not json").expect("install malformed descriptor");
    let recovered = spawn_server().expect("a malformed descriptor is recoverable");
    drop(recovered);

    let other_identity = if ApplicationIdentity::current().cli_name() == "bootty" {
        "bootty-dev"
    } else {
        "bootty"
    };
    let other_directory = runtime.join(other_identity);
    fs::create_dir_all(&other_directory).expect("create other identity directory");
    let other_descriptor = other_directory.join("control.json");
    fs::write(&other_descriptor, b"other identity").expect("write other identity marker");
    let current = spawn_server().expect("current identity starts beside the other identity");
    assert_eq!(
        fs::read(&other_descriptor).expect("other identity marker survives"),
        b"other identity"
    );
    drop(current);

    let barrier = Arc::new(Barrier::new(3));
    let contenders = (0..2)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let (sender, _receiver) = app_command_channel(1);
                barrier.wait();
                ControlServer::spawn(
                    "main",
                    sender.for_caller(Caller::Socket),
                    Arc::new(ControlCatalog::new(
                        Vec::new(),
                        Arc::new(ExtensionCatalog::default()),
                    )),
                    &ControlPlane::default(),
                )
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let results = contenders
        .into_iter()
        .map(|contender| contender.join().expect("singleton contender"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        vec![format!(
            "{} is already running",
            ApplicationIdentity::current().display_name()
        )]
    );
}
