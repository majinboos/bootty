use bootty_identity::{APPLICATION_IDENTITY_ENV, ApplicationIdentity};
use pretty_assertions::assert_eq;
#[test]
fn local_rmux_setup_exports_the_identity_for_child_processes() {
    let identity = ApplicationIdentity::Development;
    bootty_rmux::prepare_local_rmux_daemon(identity).expect("prepare local daemon");

    assert_eq!(
        (
            ApplicationIdentity::for_process(),
            std::env::var(APPLICATION_IDENTITY_ENV),
        ),
        (identity, Ok("bootty-dev".to_owned())),
    );
}
