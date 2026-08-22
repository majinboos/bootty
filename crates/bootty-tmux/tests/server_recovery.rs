#![cfg(unix)]

use assert_fs::prelude::*;
use bootty_tmux::{clear_dead_socket, tmux_server_exited};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case("no server running on /tmp/tmux-1/default", true)]
#[case("server exited unexpectedly", true)]
#[case("can't find session: bogus", false)]
fn server_exit_messages_are_classified_for_recovery(#[case] stderr: &str, #[case] expected: bool) {
    assert_eq!(tmux_server_exited(stderr), expected);
}

#[test]
fn a_dead_socket_is_cleared_and_a_live_one_is_left_alone() {
    let directory = assert_fs::TempDir::new().expect("temporary directory");

    let dead = directory.child("dead");
    drop(std::os::unix::net::UnixListener::bind(dead.path()).expect("bind the dead socket"));

    let alive = directory.child("alive");
    let _listener =
        std::os::unix::net::UnixListener::bind(alive.path()).expect("bind a live socket");

    let regular = directory.child("regular");
    regular
        .write_str("not a socket")
        .expect("write a regular file");
    assert_eq!(
        [
            clear_dead_socket(dead.path()),
            clear_dead_socket(alive.path()),
            clear_dead_socket(regular.path()),
        ],
        [true, false, false],
    );
    assert_eq!(
        (dead.exists(), alive.exists(), regular.exists()),
        (false, true, true),
    );
}
