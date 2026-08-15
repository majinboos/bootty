use bootty_config::config::default_config_path;
use bootty_identity::{
    ApplicationIdentity, legacy_config_path_from_env, unix_daemon_state_path,
    windows_daemon_state_path,
};
use bootty_rmux::{
    endpoint_path_for as bootty_rmux_endpoint_path_for, socket_name as bootty_rmux_socket_name,
};

#[test]
fn production_and_development_are_distinct_application_singletons() {
    let production = ApplicationIdentity::Production;
    let development = ApplicationIdentity::Development;

    assert_eq!(production.display_name(), "Bootty");
    assert_eq!(production.cli_name(), "bootty");
    assert!(production.automatic_updates_enabled());

    assert_eq!(development.display_name(), "BoottyDev");
    assert_eq!(development.cli_name(), "bootty-dev");
    assert!(!development.automatic_updates_enabled());

    assert_ne!(production, development);
}

#[test]
fn production_and_development_use_separate_default_config_trees() {
    let production = ApplicationIdentity::Production.default_config_path();
    let development = ApplicationIdentity::Development.default_config_path();

    assert_eq!(production, default_config_path());
    assert_eq!(production.file_name(), Some("config.toml".as_ref()));
    assert_eq!(development.file_name(), Some("config.toml".as_ref()));
    assert_eq!(
        production.parent().and_then(|path| path.file_name()),
        Some("bootty".as_ref())
    );
    assert_eq!(
        development.parent().and_then(|path| path.file_name()),
        Some("bootty-dev".as_ref())
    );
    assert_ne!(production, development);
}

#[test]
fn production_and_development_use_separate_local_rmux_endpoints() {
    let production = bootty_rmux_endpoint_path_for(ApplicationIdentity::Production)
        .expect("production rmux endpoint");
    let development = bootty_rmux_endpoint_path_for(ApplicationIdentity::Development)
        .expect("development rmux endpoint");

    assert_eq!(
        bootty_rmux_socket_name(ApplicationIdentity::Production, 8),
        "bootty-wire8"
    );
    assert_eq!(
        bootty_rmux_socket_name(ApplicationIdentity::Development, 8),
        "bootty-dev-wire8"
    );
    assert_eq!(
        production.parent(),
        development.parent(),
        "identity changes only the local endpoint name"
    );
    assert_ne!(production, development);
}

#[test]
fn an_uninitialized_process_uses_production_identity() {
    assert_eq!(
        ApplicationIdentity::for_process(),
        ApplicationIdentity::Production
    );
}

#[test]
fn daemon_and_legacy_paths_preserve_production_and_isolate_development() {
    let state = std::path::Path::new("/state");
    let config = std::path::Path::new("/config");
    let home = std::path::Path::new("/home/user");
    let local_app_data = std::path::Path::new(r"C:\Users\user\AppData\Local");
    let app_data = std::path::Path::new(r"C:\Users\user\AppData\Roaming");
    let explicit = std::path::Path::new("/exact/file.sqlite");

    assert_eq!(
        unix_daemon_state_path(
            ApplicationIdentity::Production,
            None,
            Some(state),
            Some(home)
        ),
        Some(state.join("bootty/daemon.sqlite"))
    );
    assert_eq!(
        unix_daemon_state_path(
            ApplicationIdentity::Development,
            None,
            Some(state),
            Some(home)
        ),
        Some(state.join("bootty-dev/daemon.sqlite"))
    );
    assert_eq!(
        unix_daemon_state_path(ApplicationIdentity::Production, None, None, Some(home)),
        Some(home.join(".local/state/bootty/daemon.sqlite"))
    );
    assert_eq!(
        windows_daemon_state_path(
            ApplicationIdentity::Production,
            None,
            Some(local_app_data),
            Some(app_data),
        ),
        Some(local_app_data.join("bootty/daemon.sqlite"))
    );
    assert_eq!(
        windows_daemon_state_path(ApplicationIdentity::Development, None, None, Some(app_data),),
        Some(app_data.join("bootty-dev/daemon.sqlite"))
    );
    for identity in [
        ApplicationIdentity::Production,
        ApplicationIdentity::Development,
    ] {
        assert_eq!(
            unix_daemon_state_path(identity, Some(explicit), Some(state), Some(home)),
            Some(explicit.to_path_buf())
        );
        assert_eq!(
            windows_daemon_state_path(
                identity,
                Some(explicit),
                Some(local_app_data),
                Some(app_data),
            ),
            Some(explicit.to_path_buf())
        );
    }
    assert_eq!(
        legacy_config_path_from_env(ApplicationIdentity::Production, Some(config), Some(home)),
        Some(config.join("bootty/config.toml"))
    );
    assert_eq!(
        legacy_config_path_from_env(ApplicationIdentity::Development, Some(config), Some(home)),
        Some(config.join("bootty-dev/config.toml"))
    );
}

#[test]
#[cfg(debug_assertions)]
fn debug_builds_use_the_development_application_identity() {
    assert_eq!(
        ApplicationIdentity::current(),
        ApplicationIdentity::Development
    );
}
