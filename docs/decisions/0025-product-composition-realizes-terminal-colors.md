# Product composition realizes terminal colors

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config` owns parsed neutral RGBA values. `bootty-app` owns their
realization as terminal RGB values.

## Authority and invariants

- Config parsing accepts the same `#RRGGBB` and `#RRGGBBAA` values.
- The app maps every terminal color slot through one private conversion.
- The terminal conversion preserves red, green, and blue.
- The terminal conversion intentionally drops alpha.
- An absent configured color keeps the terminal fallback.
- An empty palette keeps the terminal fallback palette.
- A configured palette keeps input order and the 256-entry limit.
- Palette generation flags do not change.
- Reload and preview ordering do not change.

## Simplification

`bootty-config` no longer depends on `libghostty-vt`. It does not implement a
conversion into a terminal-owned type. The existing app composition module owns
the complete terminal policy conversion.

## Rejected alternatives

- A shared color DTO would duplicate `Color`.
- A conversion trait would add one implementation and hide the ownership seam.
- Moving parsing into the terminal crate would mix product schema with terminal
  execution.
