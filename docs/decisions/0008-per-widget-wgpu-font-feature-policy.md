# Per-widget WGPU font feature policy

Status: accepted on 2026-08-13.

Implementation: complete.

`TerminalWidget` owns one stable renderer identity and its effective ordered font
feature policy. `TerminalTextContract` carries that policy into each text command.
The WGPU callback carries stable widget identity. Callback resources keep one
shared text atlas and separate prepared renderer state per widget.

## Authority and invariants

- A renderer identity stays with its `TerminalWidget` through pane focus swaps,
  moves, and resizes.
- A renderer cache entry holds only a weak widget lifetime. The next callback
  preparation removes entries for closed widgets.
- The public compatibility callback keeps its prior surface-based identity.
- The callback cache key uses renderer identity and target format. Surface
  geometry is render input. It is not widget identity.
- Each text command carries one shared immutable ordered feature value.
- Shaped-run keys, prepared-text equality, prepared-frame equality, and shaped
  atlas keys include the policy.
- Shaping reads command policy. It does not mutate global shaper policy.
- `TerminalTextShaper` is a policy-free grid segmenter. OpenType feature policy
  exists only on semantic text commands and the font-library shaping path.
- Widgets share feature-independent atlas, font, face, glyph, and sprite state.
- Prepared GPU renderer state remains isolated by stable widget identity.
- Feature equality is exact. Order and duplicates remain behavior.
- The callback applies feature policy before it can reuse a prepared frame.
- A feature change misses every shaping-dependent cache through semantic
  equality. It does not discard feature-independent atlas state.
- The default direct renderer path keeps `+liga`. An empty effective list remains
  empty. WGPU does not add a hidden default.

## Failure and recovery

Font feature realization is infallible after config validation. The next callback
after a live config reload carries commands with the new policy. It cannot reuse a
prepared frame or prepared text from the previous policy.

## Rejected alternatives

- A mutable global shaper policy leaks behavior between widgets.
- A feature-bearing `TerminalTextShaper` is false authority because the
  segmenter does not perform OpenType shaping.
- One builder per widget duplicates the atlas, font caches, glyph caches, and GPU
  texture memory.
- A builder cache keyed only by feature policy couples atlas lifetime to policy
  equality and still hides policy outside the semantic text command.
- Putting policy on every draw primitive or on a separate global frame field
  widens unrelated renderer values. A text command is the semantic shaping
  boundary and carries the policy directly.
- A shaped atlas key that contains only glyph IDs is unsafe because rasterization
  also consumes cluster data. The key must include feature policy as the bounded
  cross-policy shield.
- A generic renderer-policy trait adds an abstraction with one real policy.

## Migration consequences

The Bootty WGPU callback cache replaces geometry identity with stable widget
identity. It retains one shared text atlas. Closed widget entries are evicted on
the next callback preparation. The public compatibility callback retains its
prior geometry identity for callers that do not own a stable widget.
