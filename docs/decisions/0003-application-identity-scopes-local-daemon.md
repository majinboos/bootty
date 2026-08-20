# Application identity scopes local daemon state

Status: accepted on 2026-08-13.

Implementation: complete.

`ApplicationIdentity` is one closed product value with two cases: `Production`
and `Development`. The app, local rmux endpoint, and local daemon use the same
identity. The app exports the selected identity to its internal rmux child through
`BOOTTY_APPLICATION_IDENTITY`. Only the internal rmux invocation accepts that
inherited value. A daemon CLI value is explicit. Every other omitted daemon
identity means `Production`. Automatically installed remote daemons omit the local
identity and remain `Production`.

## Path contract

| Resource | Production | Development |
| --- | --- | --- |
| Unix daemon state | `$XDG_STATE_HOME/bootty/daemon.sqlite`, or `$HOME/.local/state/bootty/daemon.sqlite` | The same root with `bootty-dev/daemon.sqlite` |
| Windows daemon state | `%LOCALAPPDATA%/bootty/daemon.sqlite`, with `%APPDATA%` fallback | The same root with `bootty-dev/daemon.sqlite` |
| Legacy config | The current config root plus `bootty/config.toml` | The same root plus `bootty-dev/config.toml` |
| Legacy workspace database | `session-order.sqlite3` beside that identity's config | `session-order.sqlite3` beside that identity's config |
| Local rmux endpoint | `bootty-wire{wire_version}` | `bootty-dev-wire{wire_version}` |

`BOOTTY_DAEMON_STATE` is an explicit whole-file override. It wins over identity
selection. Bootty does not add a suffix to it.

## Authority and invariants

- `ApplicationIdentity` owns the stable product namespace.
- One process has one immutable identity. A conflicting second initialization is
  a typed startup failure.
- The app initializes its identity before it resolves or starts a local rmux
  endpoint. The internal daemon receives that resolved endpoint explicitly and
  inherits the same closed identity value.
- The daemon receives the identity from process composition. It does not infer it
  from its build profile.
- Remote commands ignore an inherited local identity. User SSH environment policy
  cannot turn a remote daemon into Development.
- The daemon catalog owns its rows. Identity selects only its default database path
  and the matching legacy source.
- Legacy migration never reads, copies, writes, or marks the other identity's state.
- Production default paths remain byte-for-byte compatible.
- Local identity does not change backend-native identity or remote Space identity.

## Failure and recovery

A missing matching legacy source means no import. A corrupt matching source follows
the existing migration failure policy. Bootty never falls back to the other
identity. A missing state or config root returns the existing typed startup error.

## Rejected alternatives

- Build-profile inference inside `bootty-daemon` can give a packaged sidecar the
  wrong identity.
- One database with identity columns adds tenant semantics that Bootty does not have.
- Development import from Production violates state isolation.
- Identity suffixing on `BOOTTY_DAEMON_STATE` changes an explicit caller choice.
- Remote daemon asset renaming applies a local development concern to another host.
- Adding the identity to remote SSH commands would leak a local development choice
  into another host.

## Migration consequences

Existing Production state stays in place. Development starts from its own empty
catalog unless a Development legacy catalog exists. Bootty does not copy Production
state into Development.
