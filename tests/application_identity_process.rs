use bootty_app::application_identity::{APPLICATION_IDENTITY_ENV, ApplicationIdentity};

#[test]
fn development_identity_is_exported_for_local_child_processes() {
    bootty_rmux::prepare_local_rmux_daemon(ApplicationIdentity::Development)
        .expect("prepare Development local daemon");

    assert_eq!(
        ApplicationIdentity::for_process(),
        ApplicationIdentity::Development
    );
    assert_eq!(
        std::env::var(APPLICATION_IDENTITY_ENV).as_deref(),
        Ok("bootty-dev")
    );
}
