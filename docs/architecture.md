# Bootty architecture

Bootty is a Rust-first, cross-platform terminal application. The app shell is
`egui`/`eframe` on WGPU. Terminal content is rendered through Bootty-owned
render commands submitted as an eframe WGPU callback; egui owns chrome, layout,
focus, input capture, and repaint scheduling.

The workspace keeps one binary crate, `bootty-app` (binary name `bootty`), and
splits reusable terminal implementation into supporting library crates. These
crates are not compatibility wrappers: each owns a seam whose deletion would
push state, runtime, geometry, renderer, or host-specific complexity back into
multiple callers.

## Design constraints

- Use `libghostty-vt` for VT parsing, terminal state, colors, cursor state, and
  key/focus/mouse/paste encoders.
- Keep PTY/process I/O outside the UI layer.
- Keep terminal drawing semantics independent from egui painter APIs.
- Keep geometry and input conversion shared so renderer and input code do not
  duplicate cell, padding, or pointer-coordinate math.
- Keep renderer latency observable through lightweight status metrics and
  benchmarkable module seams.

## Crate boundaries

- `bootty-app` owns product composition: the default binary, full egui app,
  theme resolution, mux chrome, app-level metrics, examples, and
  compatibility-facing re-exports for tests and package examples.
- `bootty-config` owns the Bootty TOML schema, XDG config path resolution,
  includes, restricted theme color resolution, reload state, and round-trip
  TOML writeback. It is host-neutral so non-egui hosts can load the same
  config.
- `bootty` is the stable library facade: re-exports of the four core library
  crates for external callers.
- `bootty-ui` owns egui theme/color widgets shared by app chrome.
- `bootty-tauri` is a Tauri host adapter exposing terminal sessions over Tauri
  commands.
- `bootty-site` is the documentation website and interactive demo.
- `bootty-surface` owns terminal geometry: cell metrics, padding, viewport
  rectangles, grid sizing, PTY pixel dimensions, and pointer transforms.
- `bootty-terminal` owns the `libghostty-vt` adapter, frame snapshots, terminal
  input value types, Kitty image extraction, and inherited Ghostty adapter
  tests.
- `bootty-runtime` owns the PTY/session runtime, worker thread, bounded drain
  budgets, published frame snapshots, repaint wakeup policy, and scheduling
  guardrails.
- `bootty-render` owns renderer-independent paint planning plus WGPU resource
  preparation for backgrounds, text, color emoji, sprites, decorations, cursor,
  and Kitty image placement.
- `bootty-winit` owns host adapters that are not the product app itself: bare
  winit/WGPU hosting, direct native input, egui input conversion, key bindings,
  modifier remaps, and host boundary tests.

## Main boundaries

```text
egui/eframe app shell
  |-- app chrome and status bar
  |-- native mux sidebar, picker, and dialogs
  |-- mux backend selection and snapshots
  |-- input ownership router
  `-- TerminalWidget
      |-- TerminalSurface geometry
      |-- TerminalSession
      |   `-- TerminalWorker
      |       |-- TerminalEngine
      |       |   |-- libghostty-vt Terminal
      |       |   |-- input encoders
      |       |   `-- RenderState extraction
      |       `-- PublishedFrame
      |-- PaintPlanner
      |-- TerminalRenderFrame
      `-- terminal_wgpu callback
```

`TerminalEngine` is the UI-free terminal core used by tests and benchmarks.
`TerminalSession` is the concrete app-facing runtime for `portable-pty`, worker
commands, bounded PTY drain scheduling, published drain stats, and published
render frames.

## Module map

- `app.rs` owns app-level orchestration, status metrics, repaint/runtime
  lifecycle, and terminal command application.
- `bootty-config::config` owns the Bootty TOML schema, XDG config path
  resolution, includes, restricted theme resolution, reload state, and
  round-trip TOML writeback. `bootty-config::config_reload` owns hot-reload
  polling state.
- `mux/` owns backend selection, backend-neutral commands, snapshots, and mux
  backend contracts.
- `input/` owns app input focus and event routing before terminal input
  conversion.
- `ui/` owns native sidebar, picker, and dialog state/rendering.
- `bootty-surface::geometry` owns terminal surface geometry: rect, padding,
  renderer-owned cell metrics, grid sizing, PTY pixel dimensions, and pointer
  transforms.
- `bootty-winit::input` converts egui events plus `TerminalSurface` into UI-free
  `TerminalInputCommand` values.
- `bootty-terminal::terminal_input_model` owns the terminal key, modifier, mouse
  action, and mouse-size value types shared by egui input, direct native input,
  bindings, and the Ghostty encoder adapter.
- `bootty-runtime::scheduler` converts runtime activity signals into repaint
  recommendations.
- `terminal.rs` is the public terminal API facade for callers that need stable
  Bootty terminal types without coupling to implementation module layout.
- `bootty-terminal::terminal_engine` owns `TerminalEngine`, Ghostty encoder
  application, default terminal colors, terminal image decoding, and render
  frame extraction. Its tests cover the libghostty adapter and terminal feature
  matrix.
- `bootty-terminal::terminal_frame` owns the immutable frame snapshot value types
  consumed by paint planning and renderer tests.
- `bootty-runtime::terminal_session` owns the PTY/process runtime, terminal
  worker, bounded drain budgets, command delivery, repaint wakeups, published
  render frames, and published drain stats. Deleting it would spread worker
  scheduling, PTY I/O, and frame publication back through the app host and
  terminal core.
- `bootty-render::paint_plan` converts `RenderFrame` plus `TerminalSurface` into
  renderer-ready backgrounds, text runs, decorations, and cursor primitives.
- `bootty-render::terminal_render` converts `TerminalPaintPlan` plus
  `TerminalTextContract` into terminal render commands. This boundary must not
  expose egui painter, text, or mesh types.
- `bootty-render::terminal_text` defines terminal text configuration, font
  fallback policy, native-symbol fragmentation, and sprite/text routing.
- `bootty-render::terminal_text_atlas` owns glyph atlas packing, text
  rasterization, cached atlas uploads, and macOS CoreText color emoji
  rasterization for clusters such as `🥟`.
- `bootty-render::terminal_sprite` owns sprite classification and
  renderer-independent sprite draw commands for terminal glyphs that have
  deterministic geometry.
- `bootty-render::terminal_wgpu` owns the WGPU callback backend for terminal
  fills, text, color glyphs, sprites, decorations, images, and cursors.
- `bootty-winit::bare_host` owns the minimal non-egui window path and its surface
  format selection guardrail so terminal palette colors are not gamma-shifted.
- `bootty::mux` owns backend-neutral session snapshots, lifecycle commands,
  Bootty-native mux state, and rmux/tmux/zellij adapter command translation.
  `app.rs` consumes this contract and must not invoke backend command surfaces
  directly.

## Runtime flow

PTY output is processed off the UI thread:

```text
PTY reader thread
  -> mpsc<Vec<u8>>
  -> TerminalWorker pending PTY queue
  -> bounded drain into TerminalEngine::write_vt
  -> TerminalEngine::extract_frame
  -> PublishedFrame
```

The UI thread consumes the latest published snapshot:

```text
TerminalWidget
  -> TerminalSession::extract_frame
  -> PaintPlanner
  -> TerminalPaintPlan
  -> TerminalRenderFrame
  -> terminal_wgpu eframe WGPU callback
```

`TerminalSession::drain_pty` returns worker-published drain statistics for the
status bar; it does not itself write PTY bytes into the terminal engine.

## Input flow

```text
egui::Event + TerminalSurface
  -> input module
  -> TerminalInputCommand
  -> TerminalSession command channel
  -> TerminalWorker
  -> TerminalEngine encoders or raw PTY write
  -> PTY writer
```

Keyboard, focus, mouse, and paste commands use Ghostty-compatible encoders where
available. Printable text writes UTF-8 bytes directly. Bracketed paste, terminal
mouse modes, focus reporting, and application cursor/keypad modes are driven by
terminal state held inside `libghostty-vt`.

## Renderer contract

Terminal content must cross this path:

```text
RenderFrame -> PaintPlanner -> TerminalRenderFrame -> terminal_wgpu
```

Do not reintroduce terminal-cell drawing through `egui::Painter`, egui text
layout, ad hoc meshes, or a parallel screenshot/offline renderer. egui may host
the callback shape and draw non-terminal chrome only.

## Known limitations

- Text shaping, font fallback, bold/italic face selection, combining marks, and
  ligatures are not terminal-perfect.
- Color emoji support currently relies on macOS CoreText rasterization for
  emoji clusters and the RGBA text atlas path. Other platforms still need an
  equivalent color glyph rasterizer.
- Sprite coverage is intentionally narrow; unclaimed glyphs remain in the text
  path unless `terminal_sprite` has deterministic geometry for them.
- Dirty-row state is extracted and counted, but full-frame extraction and paint
  planning still run across the visible grid.
- PTY read chunks allocate `Vec<u8>` before entering the worker queue.
- Selection, scrollback UI, search, hyperlink handling, and richer IME
  composition UI are outside the current terminal renderer contract.
- Kitty image protocol coverage is part of the renderer contract, but remains
  partial and is tracked through the Ghostty parity matrix rather than by ad hoc
  renderer tests in `terminal.rs`.
