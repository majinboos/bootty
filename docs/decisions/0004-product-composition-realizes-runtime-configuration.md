# Product composition realizes runtime configuration

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config` owns the Bootty TOML schema, includes, defaults, path selection,
writeback, and schema validation. It does not construct runtime-owned aggregate
values.

`bootty-app` owns the conversion from one loaded `BoottyConfig` into one complete
`TerminalSessionConfig`. The existing workspace composition seam owns shell launch,
working-directory fallback, environment, terminal identity, active appearance
colors, cursor policy, terminal features, scrollback, Option-as-Alt, and the
terminal side-effect sender.

## Authority and invariants

- `bootty-config` stores product configuration values.
- `bootty-runtime` owns `SessionLaunchConfig` and `TerminalSessionConfig`.
- `bootty-app` realizes product configuration as runtime state.
- Initial binding creation, inactive bindings, profile rebuild, and config reload
  use one app-owned conversion.
- A configured working directory wins over the platform home fallback.
- The conversion preserves empty launch arguments and environment removals.
- Initial composition preserves no pane-specific side-effect identity and no
  benchmark trace.
- Config syntax, defaults, validation messages, launch behavior, and live reload
  behavior do not change.

## Failure and recovery

Schema failures remain config load failures. Runtime construction failures remain
app composition failures. Live reload keeps the last good config and runtime state
when either step fails.

## Rejected alternatives

- A generic conversion trait hides the product composition owner.
- A runtime DTO duplicates `SessionLaunchConfig` and can drift from it.
- Making `bootty-runtime` parse Bootty TOML reverses the dependency direction.
- Combining font and renderer ownership into this slice requires a separate
  `FontFeature` ownership decision.

## Migration consequences

`bootty-config` stops depending on `bootty-runtime`. Runtime mapping tests move to
the app composition boundary. Schema parsing and default-value tests stay with
`bootty-config`.
