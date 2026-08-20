# Shared font feature grammar

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-font` owns the OpenType feature value, parser, and canonical format.
`bootty-config` owns strict product-policy validation and resolved product
defaults. `bootty-app` realizes that policy as renderer state. `bootty-render`
owns text configuration, shaping, and independent renderer fallbacks.

## Authority and invariants

- One `FontFeature` type and one grammar serve config, CLI overrides, settings,
  and rendering.
- Config validates each TOML feature during load. It stops at the first invalid
  value with `invalid font feature: {feature}`.
- Config preserves the product order: default features, `[font].features`, then
  the legacy top-level `font-feature` list. It preserves duplicates.
- CLI overrides use the same strict parser and error text.
- Settings use the same parser but keep their permissive editor behavior. They
  discard invalid comma-separated entries, remove duplicates, and write the
  canonical format.
- Config owns explicit product font defaults. Renderer owns independent fallback
  defaults. An app compatibility contract pins their current equality.
- `app/terminal_config.rs` is the only Bootty product conversion from
  `FontConfig` to `TerminalTextConfig`.

## Failure and recovery

Invalid TOML remains a `ConfigLoadError`. Startup fails before it creates a
window or opens a workspace. Reload keeps the last good config and emits no font
effect. Font composition after config validation is infallible.

## Renderer boundary

This decision moves grammar and composition ownership. It does not define the
WGPU policy boundary. ADR 0008 defines how semantic text commands carry the
configured policy through shaping and cache reuse.

## Rejected alternatives

- Keeping `FontFeature` in render preserves the wrong config-to-render edge.
- Moving `FontFeature` into config reverses renderer dependency direction.
- Keeping raw strings after config validation requires duplicate parsing or
  permits invalid resolved state.
- Copying the value or parser creates drift.
- Deferring TOML validation to app composition changes startup and reload errors.
- Fixing WGPU shaping in this dependency slice mixes an output change with an
  ownership change and omits the required cache decision.

## Migration consequences

`bootty-config` stops depending on `bootty-render`. `bootty-render::terminal_text`
keeps a compatibility re-export for existing callers. New production code imports
the authoritative `bootty-font` value directly when practical.

This removes the public `FontConfig::terminal_text_config` workspace API. Keeping
that method would keep the forbidden config-to-render dependency. Product hosts
must realize `FontConfig` at their composition boundary.
