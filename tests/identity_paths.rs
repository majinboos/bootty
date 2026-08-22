use std::path::{Path, PathBuf};

use bootty_config::config::default_config_path;
use bootty_identity::{
    ApplicationIdentity, legacy_config_path_from_env, unix_daemon_state_path,
    windows_daemon_state_path,
};
use bootty_rmux::{endpoint_path_for, socket_name};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

#[rstest]
#[case(ApplicationIdentity::Production, "Bootty", "bootty", true)]
#[case(ApplicationIdentity::Development, "BoottyDev", "bootty-dev", false)]
fn identity_metadata_drives_user_visible_names_and_updates(
    #[case] identity: ApplicationIdentity,
    #[case] display_name: &str,
    #[case] cli_name: &str,
    #[case] automatic_updates: bool,
) {
    assert_eq!(
        (
            identity.display_name(),
            identity.cli_name(),
            identity.automatic_updates_enabled(),
        ),
        (display_name, cli_name, automatic_updates),
    );
}

#[test]
fn identities_use_separate_default_config_trees() {
    let production = ApplicationIdentity::Production.default_config_path();
    let development = ApplicationIdentity::Development.default_config_path();

    assert_eq!(production, default_config_path());
    assert_eq!(production.file_name(), Some("config.toml".as_ref()));
    assert_eq!(development.file_name(), Some("config.toml".as_ref()));
    assert_eq!(
        production.parent().and_then(Path::file_name),
        Some("bootty".as_ref())
    );
    assert_eq!(
        development.parent().and_then(Path::file_name),
        Some("bootty-dev".as_ref()),
    );
    assert_ne!(production, development);
}

#[test]
fn identities_use_separate_local_rmux_endpoint_names() {
    let production =
        endpoint_path_for(ApplicationIdentity::Production).expect("production rmux endpoint");
    let development =
        endpoint_path_for(ApplicationIdentity::Development).expect("development rmux endpoint");

    assert_eq!(
        (
            socket_name(ApplicationIdentity::Production, 8),
            socket_name(ApplicationIdentity::Development, 8),
        ),
        ("bootty-wire8".to_owned(), "bootty-dev-wire8".to_owned()),
    );
    assert_eq!(production.parent(), development.parent());
    assert_ne!(production, development);
}

#[test]
fn an_uninitialized_process_uses_production_identity() {
    assert_eq!(
        ApplicationIdentity::for_process(),
        ApplicationIdentity::Production
    );
}

fn absolute(component: &str) -> PathBuf {
    Path::new("/").join(component)
}

#[derive(Debug, Arbitrary)]
struct PathInputs {
    #[proptest(regex = "[a-z][a-z0-9-]{0,15}")]
    state: String,
    #[proptest(regex = "[a-z][a-z0-9-]{0,15}")]
    config: String,
    #[proptest(regex = "[a-z][a-z0-9-]{0,15}")]
    home: String,
    #[proptest(regex = "[a-z][a-z0-9-]{0,15}\\.sqlite")]
    explicit: String,
}

proptest! {
    /// Property: every derived daemon/config path stays below the caller-provided base and changes
    /// only its identity directory; an explicit state file always wins over fallback directories.
    #[test]
    fn state_and_legacy_paths_preserve_bases_and_isolate_identities(inputs in any::<PathInputs>()) {
        let state = absolute(&inputs.state);
        let config = absolute(&inputs.config);
        let home = absolute(&inputs.home);
        let explicit = absolute(&inputs.explicit);

        for (identity, namespace) in [
            (ApplicationIdentity::Production, "bootty"),
            (ApplicationIdentity::Development, "bootty-dev"),
        ] {
            let daemon = |base: &Path| Some(base.join(namespace).join("daemon.sqlite"));
            prop_assert_eq!(
                (
                    unix_daemon_state_path(identity, None, Some(&state), Some(&home)),
                    unix_daemon_state_path(identity, None, None, Some(&home)),
                    windows_daemon_state_path(identity, None, Some(&state), Some(&config)),
                    windows_daemon_state_path(identity, None, None, Some(&config)),
                    unix_daemon_state_path(identity, Some(&explicit), Some(&state), Some(&home)),
                    windows_daemon_state_path(identity, Some(&explicit), Some(&state), Some(&config)),
                    legacy_config_path_from_env(identity, Some(&config), Some(&home)),
                ),
                (
                    daemon(&state),
                    daemon(&home.join(".local/state")),
                    daemon(&state),
                    daemon(&config),
                    Some(explicit.clone()),
                    Some(explicit.clone()),
                    Some(config.join(namespace).join("config.toml")),
                ),
            );
        }
    }
}

#[cfg(debug_assertions)]
#[test]
fn debug_builds_use_the_development_application_identity() {
    assert_eq!(
        ApplicationIdentity::current(),
        ApplicationIdentity::Development
    );
}
