# Mux binding model owns operational placement

Status: accepted on 2026-08-13.

Implementation: complete.

One dependency-neutral `bootty-mux-model` crate owns the exact values that
describe an operational backend binding. Product config, app composition, and
mux execution share these concrete values. They do not copy or translate a DTO.

## Authority and invariants

- `bootty-mux-model` is part of the mux module. It is not a generic model crate.
- `MuxBackendKind` owns the closed Native, rmux, tmux, and zellij backend kinds.
- `SshTarget` owns one resolved immutable SSH process target.
- `MuxBindingConfig` owns backend kind, tmux status policy, SSH target, and
  remote Space identity for one binding.
- `MuxBindingConfigError` owns empty-host and unsupported-remote validation.
- The neutral binding default is Native with visible tmux status and no remote
  placement.
- `bootty-config` owns TOML, defaults, partial patches, SSH profile policy, and
  config error timing. It re-exports the mux values under the current config
  names.
- `bootty-app::app::mux_config` owns one realization from product and Space
  placement policy into the exact `MuxBindingConfig` stored by `BindingRuntime`.
- `bootty-mux` owns backend selection, Windows fallback, backend construction,
  SSH argv policy, remote Space execution, controller generation equality, and
  terminal attach behavior.
- `BindingRuntime` stores one realized binding value. It does not keep separate
  schema and operational copies.

## Binding realization

Binding realization starts with the validated product multiplexer value. A
remote Profile backend overrides a binding backend override. Realization clears
the product remote Space id before it resolves placement.

- Inherit keeps product remote placement.
- Local clears remote placement.
- Profile sets the remote Space id and resolves the named SSH profile.
- A missing Profile clears remote placement and reports
  `SSH profile '{id}' is unavailable`.
- Inline installs its exact SSH target.
- A backend that cannot run remotely clears remote placement.

Profile authentication, host-key policy, identity files, proxy jumps, and
profile ids remain product and app policy. Mux receives the resolved target.

## Validation and compatibility

Config load and CLI overrides keep their current validation timing. The visible
errors remain:

- `multiplexer.remote.host must name a host`.
- `multiplexer.remote needs a backend with a client to run there, got Native`.

Local tmux resolves to Native only on Windows. Remote tmux stays tmux. Remote
Space command grammar, daemon catalog state, workspace persistence, backend
capabilities, SSH argument ordering, and stale command completion checks do not
change.

`bootty-config` re-exports the shared values as `MultiplexerConfig`,
`MultiplexerBackendConfig`, and `SshRemoteConfig`. Existing source names and TOML
tokens stay compatible.
These names are compatibility aliases. New mux execution code uses the owning
model names.

## Rejected alternatives

- A config DTO plus a mux DTO duplicates values and validation.
- A mux dependency on product config reverses ownership.
- A generic config trait creates a shallow abstraction around one value model.
- Moving SSH profile policy into mux mixes durable product policy with process
  execution.
- Changing backend behavior while moving the values hides compatibility bugs.

## Dependency consequence

`bootty-config` and `bootty-mux` depend on `bootty-mux-model`.
`bootty-mux` no longer depends on `bootty-config`. The installed daemon keeps its
config dependency for validated legacy migration.
