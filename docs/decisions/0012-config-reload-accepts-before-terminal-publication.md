# Config reload accepts before terminal publication

Status: accepted on 2026-08-13.

Implementation: complete.

`ConfigState` accepts one fully loaded and app-realized config before Bootty
publishes that config to live terminal runtimes. A dead terminal reports a
scoped publication warning. It does not reject an otherwise valid config.

## Authority and invariants

- `bootty-config` loads and validates the product config.
- `bootty-app` realizes modifier remaps, key bindings, renderer values, and
  binding-specific terminal session values before it publishes the candidate.
- `ConfigState` owns the accepted product config.
- `bootty-terminal::TerminalLiveConfig` owns the colors, cursor, and terminal
  features that can change in a live terminal engine.
- `TerminalRuntime` exposes one required live-config operation. It does not
  expose separate color, cursor, and feature setters.
- Native and rmux workers receive one aggregate command. The worker applies
  colors, then cursor policy, then terminal features.
- Each `BindingRuntime` stores the complete accepted `TerminalSessionConfig`
  before it sends the live aggregate to existing pane runtimes. New panes use
  that stored accepted value.
- Reload accepts the candidate once. It then publishes app state, app effects,
  and terminal policy once.
- Parked native, active binding, inactive binding, and inactive Space runtimes
  receive the same accepted live policy for their binding-specific config.

## Failure and recovery

A load, modifier-remap, or key-binding failure rejects the candidate before any
live terminal command or app effect is published. The previous config remains
authoritative.

A terminal delivery failure happens after acceptance. Bootty keeps the accepted
config and accepted session values. It continues delivery to other runtimes and
reports every failed binding scope or parked native owner. A later pane starts
with the accepted config.

The worker applies one aggregate command. This prevents interleaving and partial
queue delivery inside one runtime. Engine errors remain runtime-health failures.
This slice does not add a cross-worker prepare protocol, rollback transaction,
or worker restart. See
[`0017-terminal-worker-publishes-core-failures.md`](0017-terminal-worker-publishes-core-failures.md).

## Rejected alternatives

- Rejecting the candidate after one terminal changed creates mixed live policy
  with no authoritative config.
- Three independent terminal commands permit partial delivery and interleaving.
- Cross-worker rollback cannot make independent PTY workers transactional.
- A generic publisher trait adds a shallow abstraction around one concrete
  runtime boundary.

## Migration consequences

Manual reload, hot reload, settings-triggered reload, and command-triggered
reload use the same acceptance and publication order. Hot reload refreshes its
baseline after acceptance, including an accepted reload with delivery warnings.
