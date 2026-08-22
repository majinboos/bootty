# Bootty architecture

Bootty is a terminal workspace and local control host.

The repository contains one desktop application, one installed daemon, and
library crates with explicit owners.

Unmarked sections describe the current production architecture. Sections labeled
**Accepted target** describe accepted decisions that production code does not yet
fully implement. Known violations remain listed until the target lands.

## Design constraints

## Architecture rules

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

| Fact or mutation | Owner | Failure rule |
| --- | --- | --- |
| Application identity and local namespace | `bootty-identity` | A conflicting process identity fails startup. |
| The discoverable application process | The control instance lease | One identity publishes one generation endpoint. |
| Persistent Space and binding metadata | `bootty-workspace::WorkspaceRepository` | A failed commit leaves the prior state active. |
| The live workspace | `WorkspaceRuntime` | A replacement appears only after validation and persistence. |
| Backend processes and native topology | The selected backend binding | Bootty reports backend failure and does not invent success. |
| The installed remote Space catalog | `bootty-daemon::Catalog` | A journal precedes backend mutation. |
| PTY and child lifecycle | `TerminalSession` and `TerminalWorker` | Runtime health reports asynchronous core failure. |
| VT state | `TerminalEngine` | Consumers read published frames. |
| Accepted product configuration | `AppConfigRuntime` | Invalid candidates do not replace the accepted policy. |
| Command values and transport | `bootty-command` | All callers use one typed invocation path. |
| Command resolution, execution, and policy | `bootty-app` | The app owns target resolution and destructive confirmation. |
| Local control transport and instance ownership | `bootty-control` | The singleton lease publishes one owner-local endpoint. |
| Local extension generations | `bootty-extension` | A complete candidate replaces one complete generation. |
| Agent integration state and assets | `bootty-extension` | Rust provides bounded transport and no inferred agent state. |

## Process composition

`ApplicationIdentity` has two values: Production and Development.

The identity selects the config tree, state tree, control descriptor, local
daemon catalog, rmux endpoint, tmux server, and zellij socket directory.

The `bootty` executable launches the GUI only when the selected identity has no
live owner. An argumented invocation uses the owner-local control endpoint.

The installed daemon defaults to Production. Local development identity does not
change remote assets or remote backend namespaces.

## Workspace and mux

`bootty-workspace::WorkspaceRepository` owns SQLite access for Spaces and backend bindings.
It loads one validated `WorkspaceSnapshot`.

`WorkspaceRuntime` owns the live committed workspace value.
`BindingRuntime` owns one committed placement and one realized
`MuxBindingConfig`.
It also owns binding-scoped pane layouts, terminal titles, progress, and ports.
It reconciles backend pane snapshots and owns pane focus, split ratios, and
per-pane terminal attachment.
It resolves and applies binding-scoped session and window actions from its own
mux snapshot and realized config.
It owns remote attach recovery and backoff for its binding.
`RemoteReconnectRuntime` detects network changes and coordinates reconnects
across the workspace.
`WorkspaceRuntime` owns project-session creation, generated display-name
reconciliation, and the durable metadata commit around those backend actions.
It projects binding session groups for the sidebar and cross-Space session
finder. The host does not traverse active and inactive binding storage.

A durable workspace mutation uses this order:

```text
intent
  -> candidate
  -> validation
  -> one SQLite transaction
  -> live replacement
  -> typed outcome
```

Backend membership cannot share the SQLite transaction.
Bootty writes a binding-scoped journal before create, rename, or ditch.
The next authoritative backend snapshot resolves partial completion.

`bootty-mux-model` owns neutral mux values.
`bootty-mux` owns the core provider contract and registry.
Its application facet owns a separately validated pane-policy and capability
registry.
It also owns generic commands, snapshots, controller, process, project, and
pane orchestration.
`bootty-native`, `bootty-rmux`, `bootty-tmux`, and `bootty-zellij` own concrete
control and pane policies.
`bootty-remote` owns SSH commands, remote daemon installation, remote command
framing, and remote Space transport.
The `bootty` executable links all four providers.
The daemon links rmux, tmux, and Zellij.

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
  -> TerminalEngine
  -> PublishedFrame
  -> PaintPlanner
  -> terminal_wgpu callback
```

`bootty-surface` owns host-neutral terminal geometry.
App and winit adapters convert host coordinates at their seams.

`bootty-terminal` owns the Ghostty VT adapter, input encoding, terminal
effects, images, and immutable terminal frames.

`bootty-runtime` owns shell selection, environment construction, PTY creation,
child cleanup, worker scheduling, command delivery, frame publication, and
runtime health.

`bootty-render` owns semantic paint planning and WGPU resource preparation.
Text commands carry their ordered font-feature policy.

## Configuration

`bootty-config` owns TOML schema, defaults, includes, paths, reload dependency
tracking, validation, and atomic round-trip writeback.

`bootty-app` realizes accepted product policy as terminal, renderer, mux, and
host-input values.

`AppConfigRuntime` validates every derived input policy before workspace
construction or reload publication.

Live terminal publication happens after config acceptance.
A dead runtime produces a scoped warning.
It does not roll back the accepted config.

## Commands and control

`bootty-command` owns command descriptors, invocations, outcomes, cancellation,
and the bounded app command mailbox.

`bootty-app` resolves core and extension commands.
It owns target resolution, execution, destructive confirmation, and command
policy.

The command palette, keybindings, CLI, local socket, and Luau host submit the
same `CommandInvocation`.

`bootty-control` owns the local transport, read-only `ControlCatalog` metadata,
detached task and subscription state, and the singleton lease.
The app keeps command resolution, execution, and policy.

Detached tasks and event subscriptions use opaque owner-local capability IDs.

## Extensions and agents

A local extension module is one canonical relative `.lua` or `.luau` path
under `<config>/extensions`.

`bootty-extension` owns extension lifecycle, generation publication, bundled and
user assets, facts, storage, managed processes, and agent integrations.

One generation contains its token, worker, commands, topics, surfaces, storage,
actions, managed processes, and cancellation state.

A candidate validates and renders initial output before publication.
Replacement publishes the complete generation under one catalog lock.
Retired work cannot publish later mutations.

Rust owns surface rendering, focus, topology, and input routing.
Luau owns extension content and handlers.

Pi uses native JSONL RPC and native extension events.
Codex uses app-server JSONL and command-hook events.
Bootty does not infer agent state from process names, terminal output, screen
contents, or transcripts.

## Crates

- `bootty` owns executable composition, CLI dispatch, and native packaging.
- `bootty-app` owns the desktop host, workspace UI, command resolution,
  execution, policy, and presentation adapters.
- `bootty-cli` owns CLI grammar, config overrides, release updates, and
  login-shell environment hydration.
- `bootty-command` owns command descriptors, invocations, outcomes,
  cancellation, and the bounded app command mailbox.
- `bootty-control` owns the local transport, read-only control metadata,
  detached task and subscription state, and the singleton lease.
- `bootty-config` owns product configuration.
- `bootty-daemon` owns the installed headless catalog and remote commands.
- `bootty-extension` owns extension lifecycle, assets, facts, storage,
  managed processes, and agent integrations.
- `bootty-font` owns the shared OpenType feature value and grammar.
- `bootty-identity` owns the closed application identity.
- `bootty-mux-model` owns dependency-neutral mux values.
- `bootty-mux` owns the core provider contract, the validated core and app
  registries, and generic mux orchestration.
- `bootty-native`, `bootty-rmux`, `bootty-tmux`, and `bootty-zellij` own concrete provider policies.
- `bootty-remote` owns SSH commands, remote installation, command framing, and Space transport.
- `bootty-render` owns paint planning and WGPU preparation.
- `bootty-runtime` owns PTY sessions and terminal workers.
- `bootty-surface` owns host-neutral geometry.
- `bootty-terminal` owns terminal semantics and frames.
- `bootty-ui` owns shared egui theme values and widgets.
- `bootty-winit` owns native host and input adapters.
- `bootty-workspace` owns persisted Space and binding values, SQLite migrations,
  membership journals, session names, and session order.
- `bootty-write` owns atomic write targets, locking, and commit outcomes.
- `bootty-site` owns the documentation site.

## Application modules

- `state.rs` owns the window-independent application state machine.
- `host.rs` owns the eframe lifecycle adapter.
- `ui/chrome/runtime.rs` owns product chrome state, projection, layout, and event routing.
- `config_runtime.rs` owns accepted config and derived input policy.
- `ui/dialog_runtime.rs` owns the one modal dialog value.
- `renderer/workspace_view.rs` owns terminal widget and renderer lifecycle.
- `workspace_runtime.rs` owns live Space and binding composition.
- `bootty-workspace/src/repository.rs` owns workspace values and the
  `WorkspaceRepository` interface. Its private `repository/` modules own schema
  migration, snapshot hydration, and legacy import.
- `commands.rs` and `commands/runtime.rs` own app command resolution, execution mapping, and policy.
- `ui/settings/surface.rs` owns settings navigation and editor state.
- `bootty-ui/src/settings.rs` owns the shared settings widget grammar.

These files can remain large only while each keeps one cohesive owner and a
small interface. File size is a signal for review. It is not proof of depth.
