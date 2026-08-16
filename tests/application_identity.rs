use bootty_app::application_identity::ApplicationIdentity;

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
#[cfg(debug_assertions)]
fn debug_builds_use_the_development_application_identity() {
    assert_eq!(
        ApplicationIdentity::current(),
        ApplicationIdentity::Development
    );
}
