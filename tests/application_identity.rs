use bootty_app::application_identity::ApplicationIdentity;
use bootty_app::config::default_config_path;

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
#[cfg(debug_assertions)]
fn debug_builds_use_the_development_application_identity() {
    assert_eq!(
        ApplicationIdentity::current(),
        ApplicationIdentity::Development
    );
}
