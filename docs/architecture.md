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
| Local application identity and default process namespaces | `ApplicationIdentity` | Process composition selects one closed identity. A conflicting second initialization fails startup. |
| Live terminal processes and backend-native layout | The selected backend binding | Bootty reports the backend failure. It does not invent successful topology. |
| Persistent Space and backend-binding metadata | `WorkspaceRepository` | A failed load rejects the snapshot. A failed commit preserves the prior committed state. |
| Installed remote Space catalog membership | `bootty-daemon::Catalog` | A journal precedes backend mutation. An authoritative snapshot resolves partial completion. |
| Validated live workspace state | `WorkspaceRuntime` | A replacement becomes visible only after validation and persistence succeed. |
| PTY and process lifecycle | `TerminalSession` and `TerminalWorker` | The UI receives typed runtime outcomes. It does not own the process. |
| VT state | `TerminalEngine` | Render consumers read published frames. They do not mutate VT state. |
| Application orchestration | `AppState` | The UI boundary converts typed domain failures into user-visible effects. |
| Command meaning and validation | The command catalog and the UI-owner broker | UI, keybindings, the local CLI, the socket, and Luau use one command path. |
| Control transport state | The local control plane | The transport carries requests and outcomes. It is not a second state model. |
| Control instance ownership | The control instance lease | One application identity publishes one generation-specific endpoint. Cleanup cannot remove another generation. |
| Extension declarations and generations | **Accepted target:** the extension host | A generation publishes atomically. Rust retains topology and UI authority. |

The extension target does not describe the current manifest-package loader. That
loader does not yet implement local-module identity or atomic generation replacement.
Customized local module source persistence is already owned by
`bootty-app::extensions`. It resolves the final target symlink, serializes one
target, and publishes one same-directory replacement. See
[`0021-customized-extension-source-is-atomically-persisted.md`](decisions/0021-customized-extension-source-is-atomically-persisted.md).

## Application identity and local processes

`ApplicationIdentity` is the shared closed identity seam. It has only
`Production` and `Development`. The app selects its build identity once during
process composition. That identity selects the local rmux endpoint, the default
config tree, the local daemon catalog, and the matching legacy migration source.
The internal rmux daemon receives the resolved endpoint from its launcher. It does
not infer a local identity or open the daemon catalog. The app exports the same
closed identity through `BOOTTY_APPLICATION_IDENTITY` for that local child only.
Remote daemon commands ignore that inherited value and default to Production.

Production Bootty and BoottyDev use separate singleton namespaces. They can run
at the same time. A second process with the same identity does not become another
selectable Bootty instance. Production keeps the existing `bootty` paths and the
`bootty-wire{wire_version}` rmux endpoint. Development uses `bootty-dev` paths and
the `bootty-dev-wire{wire_version}` endpoint.

Each application identity also has one control instance lease. The lease
serializes descriptor claim, stale cleanup, publication, and release. Each
server lifetime binds one unique endpoint before it publishes its generation
descriptor. Cleanup removes only the generation it observed. See
[`0015-control-instance-lease-owns-one-generation.md`](decisions/0015-control-instance-lease-owns-one-generation.md).

An explicit `BOOTTY_DAEMON_STATE` value is the final SQLite file path. Identity
does not modify it. Automatically installed remote daemons receive no local
development identity. They default to Production and keep the existing remote
asset and command protocol. See
[`0003-application-identity-scopes-local-daemon.md`](decisions/0003-application-identity-scopes-local-daemon.md).

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

Each `BindingRuntime` owns the committed `SpaceMuxOverride` and the realized
`MuxBindingConfig` for one exact `MuxScope`. It constructs itself from one
validated `WorkspaceBinding`. Space edits and profile reload do not maintain a
second placement map or reconstruct bindings from decomposed caller state. See
[`0022-binding-runtime-owns-committed-placement.md`](decisions/0022-binding-runtime-owns-committed-placement.md).

The owner-local headless remote Space projection has no GUI runtime. Its focused
`RemoteSpaceRuntime` loads the same snapshot and commits through the same
`WorkspaceRepository`. It is a composition path for one binding. It is not a
second persistence authority.

The installed remote daemon owns a separate remote Space catalog on that host.
Its catalog journals the same backend membership operation before it mutates the
backend. It applies remote catalog membership and clears the journal in one
transaction. A per-backend file lease serializes this path across daemon processes.
It does not open or copy the local workspace repository. See
[`0010-installed-daemon-journals-remote-space-membership.md`](decisions/0010-installed-daemon-journals-remote-space-membership.md).

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
Bootty never invents backend topology. GUI and owner-local headless flows use the
workspace journal. The installed daemon uses its catalog journal. Both use the
same backend membership operation and authoritative-snapshot classifier.

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

Each terminal widget carries its configured font features into semantic text
commands. WGPU keeps one shared atlas and isolates prepared renderer state by
stable widget identity. A live policy change misses every shaping-dependent
cache before prepared-frame reuse. Closed widgets release their renderer entries
on the next callback preparation. See
[`0008-per-widget-wgpu-font-feature-policy.md`](decisions/0008-per-widget-wgpu-font-feature-policy.md).

## Crate boundaries

- `bootty-app` owns product composition: the default binary, full egui app,
  theme resolution, mux chrome, app-level metrics, examples, and
  compatibility-facing re-exports for tests and package examples.
- `bootty-identity` owns the closed Production or Development product identity,
  its stable namespace, and platform-neutral default path resolution.
- `bootty-daemon` owns the small headless executable used as the app's local
  rmux daemon and as the automatically installed remote Space server. It owns
  command dispatch, remote Space catalog persistence, and remote project and
  worktree operations. It journals remote Space membership before backend
  mutation and resolves ambiguous completion from an authoritative backend
  snapshot. Protocol, backend behavior, and the project discovery
  heuristics stay in `bootty-mux`.
  Local macOS installs cross-build every supported daemon target and keep the
  target-named binaries under `Bootty.app/Contents/Resources/daemons`.
- `bootty-config` owns the Bootty TOML schema, XDG config path resolution,
  includes, restricted theme color resolution, reload state, round-trip TOML
  writeback, resolved product font policy, and neutral RGBA color values. It is
  currently a composition schema rather than a low-level leaf. Product composition
  in `bootty-app` constructs runtime-owned, terminal-owned, and renderer-owned
  aggregate values. It also realizes modifier-remap source strings as host-input
  state. See
  [`0004-product-composition-realizes-runtime-configuration.md`](decisions/0004-product-composition-realizes-runtime-configuration.md)
  and
  [`0005-product-composition-realizes-host-input-configuration.md`](decisions/0005-product-composition-realizes-host-input-configuration.md).
  Config does not construct `bootty-terminal` aggregate values. See
  [`0006-product-composition-realizes-terminal-configuration.md`](decisions/0006-product-composition-realizes-terminal-configuration.md).
  The app realizes neutral config colors as terminal RGB values. See
  [`0025-product-composition-realizes-terminal-colors.md`](decisions/0025-product-composition-realizes-terminal-colors.md).
- `bootty-font` owns the dependency-neutral OpenType feature value, parser, and
  canonical format shared by config, app surfaces, and rendering. See
  [`0007-shared-font-feature-grammar.md`](decisions/0007-shared-font-feature-grammar.md).
- `bootty-mux-model` owns the closed backend kind, resolved SSH target,
  operational binding configuration, and remote Space summary wire value shared
  by config, app composition, daemon transport, and mux execution. It is the
  dependency-neutral value layer of the mux module. See
  [`0018-mux-binding-model-owns-operational-placement.md`](decisions/0018-mux-binding-model-owns-operational-placement.md).
  See
  [`0024-remote-space-summary-has-one-wire-owner.md`](decisions/0024-remote-space-summary-has-one-wire-owner.md)
  for the remote catalog wire contract.
- `bootty-mux` owns backend-neutral session snapshots, lifecycle commands,
  Bootty-native mux state, rmux/tmux/zellij adapter command translation, and
  the tmux control-mode protocol parser. Its project module owns project and
  worktree discovery plus the favorite-project-path persistence file. Favorite
  path updates resolve the final symlink, lock the resolved target, reread under
  the lock, and atomically publish a same-directory replacement. Its membership
  module owns the shared backend membership operation and authoritative-snapshot
  classifier. Its terminal runtime owns pane input, selection, search, copy mode,
  viewport mutation, and runtime configuration. It is egui-free: the controller
  signals repaints through a `RepaintHandle` callback supplied by the host.
  Its remote installer validates a Unix daemon candidate before it atomically
  replaces the exact versioned path. See
  [`0016-remote-daemon-publication-is-verified.md`](decisions/0016-remote-daemon-publication-is-verified.md).
  See [`0020-favorite-project-paths-are-atomically-persisted.md`](decisions/0020-favorite-project-paths-are-atomically-persisted.md)
  for the favorite-path write contract.
- `bootty` is the stable library facade: re-exports of the four core library
  crates for external callers.
- `bootty-ui` owns egui theme/color widgets shared by app chrome.
- `bootty-site` is the documentation website and interactive demo.
- `bootty-surface` owns terminal geometry: cell metrics, padding, viewport
  rectangles, grid sizing, PTY pixel dimensions, and pointer transforms. Its
  values are numeric and host-neutral. App and winit adapters convert egui
  positions and rectangles at their boundaries. See
  [`0023-terminal-surface-geometry-is-host-neutral.md`](decisions/0023-terminal-surface-geometry-is-host-neutral.md).
- `bootty-terminal` owns the `libghostty-vt` adapter, frame snapshots, terminal
  input value types, Kitty image extraction, and inherited Ghostty adapter
  tests.
- `bootty-runtime` owns the PTY/session runtime, worker thread, bounded drain
  budgets, published frame snapshots, repaint wakeup policy, scheduling
  guardrails, host-neutral runtime diagnostics, and the four-operation terminal
  frame source shared across crates.
- `bootty-app::extensions` owns durable customized Lua and Luau source bytes.
  It publishes saves with a locked same-directory atomic replacement. Runtime
  generation activation remains a Goal 4 target. See
  [`0021-customized-extension-source-is-atomically-persisted.md`](decisions/0021-customized-extension-source-is-atomically-persisted.md).
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
render frames. It owns a spawned child from the first successful process spawn.
It caches geometry, display scale, and render cell metrics only after synchronous
delivery succeeds. See
[`0019-terminal-session-owns-construction-and-delivered-state.md`](decisions/0019-terminal-session-owns-construction-and-delivered-state.md).
The worker publishes asynchronous terminal-core and PTY failures through one
bounded session-health slot. See
[`0017-terminal-worker-publishes-core-failures.md`](decisions/0017-terminal-worker-publishes-core-failures.md).

Config reload has two ordered phases. The app fully realizes and accepts one
valid product config. It then publishes one terminal-owned live config aggregate
to existing runtimes. A dead runtime reports a scoped warning after acceptance.
It cannot roll back the accepted product config. See
[`0012-config-reload-accepts-before-terminal-publication.md`](decisions/0012-config-reload-accepts-before-terminal-publication.md).

Config loading and include dependency discovery use one traversal. A failed
candidate keeps its discovered dependency snapshot. Fixing only the bad included
file can trigger recovery. See
[`0013-config-include-graph-survives-load-failure.md`](decisions/0013-config-include-graph-survives-load-failure.md).

The installed daemon validates one same-identity legacy import before it writes
destination rows. Imported rows and the migration marker commit together. A bad
source stays retryable. See
[`0014-daemon-legacy-migration-fails-closed.md`](decisions/0014-daemon-legacy-migration-fails-closed.md).

## Module map

- `app/state.rs` owns `AppState`, the egui-Context-free application state
  machine: per-frame orchestration (`update_frame(FrameInputs) -> Vec<AppEffect>`),
  status metrics, input application, config reload, and terminal command
  application. It is unit-testable without a window.
- `commands.rs` owns the typed command catalog, argument schemas, and the bounded
  request/response channel. Built-in and extension commands resolve to one typed
  invocation. Palette, keybinding, and external callers share one target and
  confirmation policy; `AppState` alone resolves targets and executes requests
  on the UI thread. See
  [`0026-command-catalog-owns-one-resolution-policy.md`](decisions/0026-command-catalog-owns-one-resolution-policy.md).
- `command_extensions.rs` owns the bounded Luau command worker. Its
  `bootty.commands.invoke` function submits the existing typed invocation to the
  UI-owner channel with the parent deadline and cancellation token. See
  [`0027-luau-submits-typed-application-commands.md`](decisions/0027-luau-submits-typed-application-commands.md).
- `app/mod.rs` owns the eframe adapter `BoottyApp`, egui input snapshots,
  `AppEffect` application, WGPU callback preparation, and substantial chrome
  composition and host policy.
- `app/terminal_config.rs` owns complete terminal session configuration and the
  focused realization of terminal and renderer text values.
- `app/mux_config.rs` resolves product and Space placement policy into one
  `MuxBindingConfig`. `BindingRuntime` stores that exact value for controller,
  backend, pane, and generation matching behavior. It also stores the committed
  `SpaceMuxOverride`, so profile reload can re-realize one binding without a
  parallel placement cache.
- `bootty-config::config` owns the Bootty TOML schema, XDG config path
  resolution, includes, restricted theme resolution, reload state, and
  round-trip TOML writeback. Its locked update workflow reads the latest
  document, validates and synchronizes a complete temporary file, and atomically
  replaces the resolved config target. See
  [`0011-config-writeback-is-atomic.md`](decisions/0011-config-writeback-is-atomic.md).
  `bootty-config::config_reload` owns hot-reload polling state.
- `bootty-config` owns mux TOML patches and SSH profile policy. It re-exports the
  exact values from `bootty-mux-model` under its compatibility names.
- `bootty-mux` owns backend selection, backend-neutral commands, snapshots, SSH
  process behavior, and mux backend contracts. It does not depend on
  `bootty-config`.
- `input/` owns app input composition, focus, and event routing before terminal
  input conversion. It realizes configured modifier-remap strings through the
  `bootty-winit` parser.
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
  render frames, published drain stats, bounded worker health, construction-time
  child cleanup, and delivered runtime state. Deleting it would spread worker
  scheduling, PTY I/O, process cleanup, failure classification, and frame
  publication back through the app host and terminal core.
- `bootty-render::paint_plan` converts `RenderFrame` plus `TerminalSurface` into
  renderer-ready backgrounds, text runs, decorations, and cursor primitives.
- `bootty-render::terminal_render` converts `TerminalPaintPlan` plus
  `TerminalTextContract` into terminal render commands. This boundary must not
  expose egui painter, text, or mesh types.
- `bootty-render::terminal_text` defines terminal text configuration, font
  fallback policy, native-symbol fragmentation, and sprite/text routing. It
  re-exports the `bootty-font` feature value for source compatibility.
- `bootty-render::terminal_text_atlas` owns glyph atlas packing, text
  shaping from each text command's ordered font policy, rasterization, cached
  atlas uploads, and macOS CoreText color emoji rasterization for clusters such
  as `🥟`.
- `bootty-render::terminal_sprite` owns sprite classification and
  renderer-independent sprite draw commands for terminal glyphs that have
  deterministic geometry.
- `bootty-render::terminal_wgpu` owns the WGPU callback backend for terminal
  fills, text, color glyphs, sprites, decorations, images, and cursors. It keys
  prepared renderer state by stable terminal widget identity and target format.
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
                                                 ├-> CommandCatalog validation
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
