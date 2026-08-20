# Product composition realizes host input configuration

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config` owns modifier-remap source strings and preset generation.
`bootty-winit` owns modifier-remap parsing, finalization, and host application.
`bootty-app` owns the one conversion from loaded source strings into one finalized
`ModifierRemapSet`.

## Authority and invariants

- The app parses entries in source order without trimming them.
- The app stops at the first invalid entry and does not publish a partial set.
- The app finalizes the set only after every entry parses.
- Parser aliases, side expansion, destination rules, and final ordering stay in
  `bootty-winit`.
- Startup resolves modifier remaps before it opens `WorkspaceRuntime`.
- Live reload keeps the last good config, remap set, keybindings, and workspace
  state when modifier-remap realization fails.
- Invalid modifier-remap grammar is an app composition failure. A structurally
  valid TOML string remains valid config input.
- Config preset tests can use `bootty-winit` as a development-only consumer.

## Failure and recovery

The app reports one focused error with the rejected source entry and the unchanged
parser error. Startup returns that error. Live reload rejects the candidate and
keeps the prior committed application state.

## Rejected alternatives

- Moving the modifier grammar into `bootty-config` duplicates host-input policy.
- A config-owned host-input value reverses dependency direction.
- A generic conversion trait adds a seam with one implementation.
- Moving preset generation removes product configuration behavior from its owner.

## Migration consequences

`bootty-config` stops depending on `bootty-winit` in production. It retains a
development dependency for cross-crate preset contracts. The app startup,
reload, and startup benchmark use one resolver.
