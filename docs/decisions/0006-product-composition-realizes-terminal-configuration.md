# Product composition realizes terminal configuration

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config` owns parsed Bootty policy and its resolved product defaults.
`bootty-app` owns every conversion from that policy into `bootty-terminal`
aggregate values. `bootty-terminal` keeps independent engine fallback and capacity
constants.

## Authority and invariants

- `SessionConfig::default` resolves the Bootty product defaults for `TERM`,
  `COLORTERM`, scrollback capacity, and the glyph protocol.
- The app realizes colors, cursor policy, terminal features, and macOS
  Option-as-Alt policy.
- One app composition module constructs the complete `TerminalSessionConfig`.
- Startup, inactive bindings, profile rebuild, theme preview, appearance changes,
  and live reload use that module.
- Color realization starts from the terminal default palette. An empty configured
  palette keeps it. A nonempty palette replaces it in source order and stops at
  256 colors.
- Live reload keeps its existing ordered failure behavior. Fallible terminal
  updates complete before the candidate config becomes active.
- An app compatibility contract pins current Bootty product defaults to the
  independent terminal fallbacks.

## Failure and recovery

The conversions are infallible. Terminal update failures remain typed runtime
failures. Live reload rejects the candidate before it publishes the new config.

## Rejected alternatives

- Making config fields optional would defer product-default resolution and widen
  every caller interface.
- A config-owned terminal aggregate reverses dependency direction.
- A second terminal DTO duplicates the terminal-owned value types.
- A generic conversion trait adds a seam with one implementation.
- A new constants crate adds an owner with no independent behavior.

## Migration consequences

`bootty-config` stops depending on `bootty-terminal`. It keeps its existing schema
and resolved values. `bootty-terminal` keeps its standalone fallbacks. The app
owns the mapping and the compatibility proof.
