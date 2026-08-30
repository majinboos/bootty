# Bootty architecture

Bootty is a terminal workspace and local control host.

The repository contains one desktop application, one installed daemon, and
library crates with explicit owners.

The vault owns product language, plans, rationale, and durable decisions.
This file describes the current production structure.

## Architecture rules

- Each durable fact and live mutation has one owner.
- A module has a small interface and hides substantial behavior.
- Callers do not reconstruct invariants or synchronize duplicate state.
- Persistence commits before live publication.
- Backends own live processes and backend-native topology.
- Bootty owns cross-backend identity, presentation, and command safety.
- The UI thread does not block on terminal, extension, or agent work.
- Production and Development use separate local namespaces.

## Authority

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
daemon catalog, rmux endpoint, and tmux server.

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
`bootty-herdr`, `bootty-native`, `bootty-rmux`, and `bootty-tmux` own concrete control and pane
policies.
`bootty-remote` owns SSH commands, remote daemon installation, remote command
framing, and remote Space transport.
Herdr owns authoritative workspace, tab, pane, layout, process, and agent state.
Bootty uses Herdr's public API for snapshots and mutations, then attaches the
stock Herdr client to render its chrome and terminal surfaces inside Bootty. A
remote binding starts the named headless server, forwards its public sockets,
and launches that same stock client over SSH.
The `bootty` executable links all four providers.
The daemon links rmux and tmux for catalog-backed remote Spaces. Herdr remote
bindings use its public client commands instead.

## Terminal path

```text
TerminalWidget
  -> TerminalSurface
  -> TerminalSession
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
- `bootty-herdr`, `bootty-native`, `bootty-rmux`, and `bootty-tmux` own concrete provider policies.
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
