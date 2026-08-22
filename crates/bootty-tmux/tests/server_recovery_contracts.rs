#![cfg(unix)]

use bootty_tmux::{clear_dead_socket, tmux_server_exited};

/// The two ways tmux says its server is not there. Matching only the first turned a crashed server
/// into a fatal bootty error instead of something to recover from.
#[test]
fn a_crashed_server_reads_as_gone_the_same_as_one_never_started() {
    assert!(tmux_server_exited(
        "no server running on /tmp/tmux-1/default"
    ));
    assert!(tmux_server_exited("server exited unexpectedly"));
    assert!(!tmux_server_exited("can't find session: bogus"));
}

/// Only a socket nothing answers is removed. A live server answers `connect`, and deleting its
/// socket would strand every client that has not connected yet.
#[test]
fn a_dead_socket_is_cleared_and_a_live_one_is_left_alone() {
    let directory = tempfile::tempdir().expect("temporary directory");

    let dead = directory.path().join("dead");
    drop(std::os::unix::net::UnixListener::bind(&dead).expect("bind the dead socket"));
    assert!(
        clear_dead_socket(&dead),
        "a socket nobody answers is cleared"
    );
    assert!(!dead.exists());

    let alive = directory.path().join("alive");
    let _listener = std::os::unix::net::UnixListener::bind(&alive).expect("bind a live socket");
    assert!(
        !clear_dead_socket(&alive),
        "a server that answers keeps its socket"
    );
    assert!(alive.exists());

    let regular = directory.path().join("regular");
    std::fs::write(&regular, "not a socket").expect("write a regular file");
    assert!(
        !clear_dead_socket(&regular),
        "only a socket is ever removed"
    );
    assert!(regular.exists());
}
