# Bootty architecture

Bootty is a Rust-first, cross-platform terminal application. The app shell is
`egui`/`eframe` on WGPU. Terminal content is rendered through Bootty-owned
render commands submitted as an eframe WGPU callback; egui owns chrome, layout,
focus, input capture, and repaint scheduling.

The workspace ships the full `bootty` app and a separate `bootty-daemon`
headless binary. Supporting library crates keep terminal and multiplexer logic
shared without pulling egui or WGPU into the daemon interface. These crates are
not compatibility wrappers. Each owns a seam whose deletion would push state,
runtime, geometry, renderer, or host-specific complexity back into multiple
callers.

Unmarked sections describe the current production architecture. Sections labeled
**Accepted target** describe accepted decisions that production code does not yet
fully implement. Known violations remain listed until the target lands.

## Design constraints

- Use `libghostty-vt` for VT parsing, terminal state, colors, cursor state, and
  key/focus/mouse/paste encoders.
- Keep PTY/process I/O outside the UI layer.
- Keep terminal drawing semantics independent from egui painter APIs.
- Keep geometry and input conversion shared so renderer and input code do not
  duplicate cell, padding, or pointer-coordinate math.
- Keep renderer latency observable through lightweight status metrics and
  benchmarkable module seams.

## Authority map

Each durable fact and live mutation has one authority.

| Fact or mutation | Authority | Failure rule |
| --- | --- | --- |
| Live terminal processes and backend-native layout | The selected backend binding | Bootty reports the backend failure. It does not invent successful topology. |
| Persistent Space and backend-binding metadata | `WorkspaceRepository` | A failed load rejects the snapshot. A failed commit preserves the prior committed state. |
| Validated live workspace state | `WorkspaceRuntime` | A replacement becomes visible only after validation and persistence succeed. |
| PTY and process lifecycle | `TerminalSession` and `TerminalWorker` | The UI receives typed runtime outcomes. It does not own the process. |
| VT state | `TerminalEngine` | Render consumers read published frames. They do not mutate VT state. |
| Application orchestration | `AppState` | The UI boundary converts typed domain failures into user-visible effects. |
| Command meaning and validation | The command catalog and the UI-owner broker | UI, keybindings, the local CLI, the socket, and Luau use one command path. |
| Control transport state | The local control plane | The transport carries requests and outcomes. It is not a second state model. |
| Extension declarations and generations | **Accepted target:** the extension host | A generation publishes atomically. Rust retains topology and UI authority. |

The extension target does not describe the current manifest-package loader. That
loader does not yet implement local-module identity or atomic generation replacement.

## Workspace persistence

`WorkspaceRepository` is the only SQLite authority for persistent Space and
backend-binding state. It migrates the schema and loads one validated
`WorkspaceSnapshot`. The snapshot includes Space metadata, binding configuration,
restore selection, session ordering, group membership, and session-name metadata.
It retains persistence access state after load. It does not retain a second mutable
workspace model.

`WorkspaceRuntime` owns the committed live snapshot. Focused session-order and
session-name types are in-memory domain values. They do not open SQLite or save
themselves. `AppState` requests workspace operations. It does not construct
persistence stores or derive database paths.

The headless remote Space catalog has no GUI runtime. Its focused
`RemoteSpaceRuntime` loads the same snapshot and commits through the same
`WorkspaceRepository`. It is a composition path for one binding. It is not a
second persistence authority.

Every durable mutation uses commit-before-publish ordering:

```text
workspace intent
  -> build and validate a candidate snapshot
  -> commit one private SQLite transaction
  -> replace the committed WorkspaceRuntime snapshot
  -> publish the typed outcome
```

Space appearance is Space-scoped. Backend and remote placement are binding-scoped.
The Space editor commits placement for the selected binding through its exact
`MuxScope`. It never guesses the first binding in the Space.

A backend operation cannot share a SQLite transaction. Bootty records a durable,
binding-scoped membership intent before create, rename, or ditch. It then requires
backend confirmation. One transaction applies the metadata and clears the intent.
A create or rename intent includes the backend name, the Bootty display name, and
whether the user chose that name.
A metadata failure after backend completion is a typed partial-completion failure.
The next authoritative backend snapshot applies or discards the retained intent.
Bootty never invents backend topology. GUI and headless catalog flows use the same
journal and repository rules.

A binding keeps its backend identity until its pending journal entry resolves.
The Space editor rejects a backend or remote placement change during recovery.
SSH profile reload defers the affected binding rebuild and retries it after the
next authoritative snapshot resolves the journal.

GUI membership commands do not persist speculative metadata. The command worker
returns an authoritative snapshot for create, rename, and ditch. Bootty commits the
derived metadata before it publishes an authoritative command success. Generated
name state used for live reconciliation stays in memory while an asynchronous UI
command is pending. The durable journal retains the naming intent across a crash.

A corrupt or incomplete database returns a typed persistence failure. It never
becomes an empty workspace. A failed write leaves the prior committed runtime
snapshot and database state active. See
[`0002-workspace-persistence-authority.md`](decisions/0002-workspace-persistence-authority.md).

## Architecture program order

Architecture work follows this dependency order:

1. Establish authoritative ownership, persistence safety, and dependency direction.
2. Deepen module interfaces and remove shallow or duplicate code.
3. Implement one semantic command path for the CLI, local socket, and Luau.
4. Implement the generic extension lifecycle on that command path.
5. Add agent integrations through the approved local extension boundary.

Tests, deleted lines, file moves, and `AppState` size are evidence only. They are
not architecture outcomes. See
[`0001-architecture-program-order.md`](decisions/0001-architecture-program-order.md).

## Crate boundaries

- `bootty-app` owns product composition: the default binary, full egui app,
  theme resolution, mux chrome, app-level metrics, examples, and
  compatibility-facing re-exports for tests and package examples.
- `bootty-daemon` owns the small headless executable used as the app's local
  rmux daemon and as the automatically installed remote Space server. It owns
  command dispatch, remote Space catalog persistence, and remote project and
  worktree operations. Protocol, backend behavior, and the project discovery
  heuristics stay in `bootty-mux`.
  Local macOS installs cross-build every supported daemon target and keep the
  target-named binaries under `Bootty.app/Contents/Resources/daemons`.
- `bootty-config` owns the Bootty TOML schema, XDG config path resolution,
  includes, restricted theme color resolution, reload state, round-trip TOML
  writeback, and conversion into terminal, runtime, renderer, and input value
  types. It is currently a composition schema rather than a low-level leaf.
- `bootty-mux` owns backend-neutral session snapshots, lifecycle commands,
  Bootty-native mux state, rmux/tmux/zellij adapter command translation, and
  the tmux control-mode protocol parser. It is egui-free: the controller
  signals repaints through a `RepaintHandle` callback supplied by the host.
- `bootty` is the stable library facade: re-exports of the four core library
  crates for external callers.
- `bootty-ui` owns egui theme/color widgets shared by app chrome.
- `bootty-site` is the documentation website and interactive demo.
- `bootty-surface` owns terminal geometry: cell metrics, padding, viewport
  rectangles, grid sizing, PTY pixel dimensions, and pointer transforms.
- `bootty-terminal` owns the `libghostty-vt` adapter, frame snapshots, terminal
  input value types, Kitty image extraction, and inherited Ghostty adapter
  tests.
- `bootty-runtime` owns the PTY/session runtime, worker thread, bounded drain
  budgets, published frame snapshots, repaint wakeup policy, scheduling
  guardrails, and host-neutral runtime diagnostics shared across crates.
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

- `app/state.rs` owns `AppState`, the egui-Context-free application state
  machine: per-frame orchestration (`update_frame(FrameInputs) -> Vec<AppEffect>`),
  status metrics, input application, config reload, and terminal command
  application. It is unit-testable without a window.
- `commands.rs` owns the typed core command registry, argument schemas, exact
  current-context targets, safety gates, and the bounded request/response channel.
  Palette, keybinding, and external callers share this contract; `AppState` alone
  resolves targets and executes requests on the UI thread.
- `app/mod.rs` owns the eframe adapter `BoottyApp`, egui input snapshots,
  `AppEffect` application, WGPU callback preparation, and substantial chrome
  composition and host policy.
- `bootty-config::config` owns the Bootty TOML schema, XDG config path
  resolution, includes, restricted theme resolution, reload state, and
  round-trip TOML writeback. `bootty-config::config_reload` owns hot-reload
  polling state.
- `bootty-mux` owns backend selection, backend-neutral commands, snapshots,
  and mux backend contracts.
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
- `bootty-mux` exposes the mux contract consumed by `bootty-app`. The app may
  submit `MuxCommand` values through `BindingMuxController`. UI code does not
  call concrete native, rmux, tmux, or zellij adapters. The crate boundary keeps
  mux logic free of egui and app types.

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

Application commands preserve the same ownership boundary:

```text
palette / keybinding -> direct UI-owner dispatch ┐
                                                 ├-> CommandRegistry validation
bound external sender -> bounded channel --------┘
  -> AppState target, capability, and confirmation gates
  -> typed CommandOutcome
```

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
