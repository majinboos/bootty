# Command catalog owns one resolution policy

Status: accepted on 2026-08-13.

Implementation: complete.

The command catalog resolves built-in and extension commands into one typed
invocation. `AppState` applies target and confirmation policy once before it
dispatches the selected executor.

## Authority and invariants

- `CommandCatalog` owns command lookup and argument validation.
- One resolved command contains its descriptor, invocation, and executor.
- `AppState` resolves the current target once for every command.
- CLI, socket, and Luau callers must confirm destructive commands.
- Command palette and keybinding callers keep direct user-action behavior.
- Built-in mux preflight remains specific to the built-in executor.
- Extension execution keeps its existing deadline and cancellation values.
- The control plane binds every socket request to `Caller::Socket`.

## Simplification

The catalog has no separate extension-resolution API. The runtime has no
parallel target or confirmation branch for extension commands.

## Rejected alternatives

- A generic command executor trait would add one implementation boundary for a
  closed two-case dispatch.
- A second extension policy layer would keep the current drift risk.
- Moving target resolution into extensions would give extensions application
  topology authority.
