# Handoff: SSH remote multiplexer bindings

Working notes for continuing this branch. **Delete this file before merging the PR.**

## What is done

A binding can name an SSH host, and its multiplexer client runs there while bootty renders it here.

- `crates/bootty-mux/src/ssh.rs` — builds the SSH argv: destination, port, configured flags first
  (SSH keeps the first value per option, so a host that needs different timings can say so),
  keepalives, `ControlMaster` on unix, and the remote command quoted for whatever login shell is on
  the other side.
- `tmux` / `zellij` — snapshots and mutations run as remote invocations; the pane is a PTY running
  the attach client over `ssh -t`. The tmux control-mode client *is* the long-lived SSH process, so a
  remote poll costs what a local one does.
- `rmux` — driven through the remote host's own rmux command line
  (`crates/bootty-mux/src/rmux_remote.rs`): `stream-pane --raw` for output, `send-keys -H` for input,
  `resize-window` for geometry, `collect-pane-output` for history. Control rides the tmux adapter,
  since rmux answers the tmux command surface.
- Per space: `workspace_bindings.remote` (JSON) plus an SSH host field in the space editor, so a
  remote space and a local one sit in the same window. `[multiplexer.remote]` is the default a space
  inherits; `--ssh-remote HOST` overrides for one run.
- A dropped connection reconnects rather than closing the pane, because the sessions live on the
  other host and closing sends the backend a kill.

## What is verified, and what is not

Verified against a real remote host, by sending the exact commands bootty builds:

- the tmux control-mode handshake over SSH, and the snapshot query it answers;
- `TERM` selection on the remote (bootty's own terminfo where that host has bootty, the universal
  fallback where it does not);
- rmux `stream-pane --raw` following live output, `send-keys -H` delivering exact bytes, and
  `set-option window-size manual` + `resize-window` actually resizing.

**Not verified:** anything through the running app. No session has been attached, rendered, typed
into, or reconnected from inside bootty. That is the first thing to do.

## How to exercise it

Any machine with `sshd` can be its own remote, which is enough for every path here:

```sh
# one-time: key auth, because the snapshot poll runs BatchMode=yes and cannot answer a prompt
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N ""
cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys; chmod 600 ~/.ssh/authorized_keys
ssh -o BatchMode=yes localhost tmux -V   # must print a version with no prompt

# seed something to attach, then run bootty against it
tmux new-session -d -s trial; tmux split-window -t trial
cargo run --release -p bootty-app -- --defaults --backend tmux --ssh-remote localhost
```

`--defaults` isolates config *and* the workspace database, so an existing setup is untouched.

For the per-space path, drop `--ssh-remote` and set the host in the space editor next to the
backend. Three spaces — local `native`, remote `tmux`, remote `zellij` — exercise the isolation
between bindings.

For rmux, the remote needs bootty installed (the binding addresses bootty's own daemon by label,
`-L bootty-wire<N>`), and that daemon has to be running there.

Checks worth making:

- `pkill -f "tmux' '-T'"` kills only the pane's attach client: the pane should return in ~500ms and
  the remote session should still have both panes.
- Dropping the link (turn off wifi ~20s) exercises the keepalive path, ~15s to notice, then backoff.
- `ls /tmp/bootty-ssh-*` shows the `ControlMaster` socket being shared.

## What is left

- **Remote rmux end to end.** The pane I/O is written but never run through the app. Expect the
  first bugs here: chunk sizing, the restore arriving before live output, and whether the input loop
  keeps up under a fast typist.
- **Wrong-daemon detection.** The rmux label carries this side's wire version. If the remote's bootty
  speaks a different one, the label names a socket that does not exist there and rmux will start an
  empty daemon on it — a working-looking space with no sessions. It should say so instead.
- **Sidebar session facts.** `sidebar_session_facts.luau` shells out to `tmux capture-pane` through
  the local extension host, so previews for a remote pane query the wrong machine.
- **New sessions on a remote space.** The project catalog and `git.rs` are local, so creating a
  project or worktree session hands the remote host paths from this one.
- **Starting a remote daemon.** A remote rmux space currently expects the daemon to be running. It
  does not start one, on purpose — that would launch a process on someone's machine unasked.
- Pre-existing test fixtures elsewhere in the repo carry a real username; worth a sweep now that the
  repo is public.
