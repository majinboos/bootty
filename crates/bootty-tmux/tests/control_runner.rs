#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;

use assert_fs::prelude::*;
use bootty_mux::process::CommandRunner;
use bootty_tmux::TmuxControlRunner;
use pretty_assertions::assert_eq;
#[test]
fn tmux_control_runner_reuses_queries_and_forks_mutations() {
    let directory = assert_fs::TempDir::new().expect("temporary command directory");
    let program = directory.child("tmux-fixture");
    program
        .write_str(
            r#"#!/bin/sh
if [ "$1" = "-C" ]; then
  printf '%%begin 1 1 1\n%%end 1 1 1\n'
  while IFS= read -r line; do
    case "$line" in
      *display-message*)
        printf '%%begin 1 1 1\nbootty-control-ready\n%%end 1 1 1\n'
        ;;
      *list-sessions*" ; "*list-panes*)
        printf '%%begin 1 1 1\nsession-row\n%%end 1 1 1\n'
        printf '%%begin 1 1 1\npane-row\n%%end 1 1 1\n'
        ;;
      *list-sessions*)
        printf '%%begin 1 1 1\nsession-row\n%%end 1 1 1\n'
        ;;
      *)
        printf '%%begin 1 1 1\nunsupported\n%%error 1 1 1\n'
        ;;
    esac
  done
  exit 0
fi
printf 'forked:'
printf ' <%s>' "$@"
printf '\n'
"#,
        )
        .expect("write command fixture");
    let mut permissions = std::fs::metadata(program.path()).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(program.path(), permissions).unwrap();
    let program = program.path().to_string_lossy().into_owned();
    let runner = TmuxControlRunner::default();

    let query = [
        "list-sessions",
        "-F",
        "s\u{1f}#{session_id}",
        ";",
        "list-panes",
        "-a",
        "-F",
        "p\u{1f}#{pane_id}",
    ]
    .map(str::to_owned);
    let first = runner.run(&program, &query).expect("first control query");
    let second = runner.run(&program, &query).expect("second control query");
    assert!(first.success);
    assert_eq!(first.stdout, "session-row\npane-row");
    assert_eq!(second.stdout, first.stdout);

    let mutation = ["kill-session", "-t", "build"].map(str::to_owned);
    let forked = runner.run(&program, &mutation).expect("forked mutation");
    assert!(forked.success);
    assert_eq!(forked.stdout, "forked: <kill-session> <-t> <build>\n");

    let unsafe_query = ["list-sessions", "-F", "it's"].map(str::to_owned);
    let forked = runner
        .run(&program, &unsafe_query)
        .expect("unsafe query fallback");
    assert_eq!(forked.stdout, "forked: <list-sessions> <-F> <it's>\n");
}
