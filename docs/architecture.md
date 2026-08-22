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
| Persistent Space and binding metadata | `WorkspaceRepository` | A failed commit leaves the prior state active. |
| The live workspace | `WorkspaceRuntime` | A replacement appears only after validation and persistence. |
| Backend processes and native topology | The selected backend binding | Bootty reports backend failure and does not invent success. |
| The installed remote Space catalog | `bootty-daemon::Catalog` | A journal precedes backend mutation. |
| PTY and child lifecycle | `TerminalSession` and `TerminalWorker` | Runtime health reports asynchronous core failure. |
| VT state | `TerminalEngine` | Consumers read published frames. |
| Accepted product configuration | `AppConfigRuntime` | Invalid candidates do not replace the accepted policy. |
| Command meaning and confirmation | `CommandCatalog` and `AppState` | All callers use one typed invocation path. |
| Local extension generations | `ExtensionHost` and `CommandCatalog` | A complete candidate replaces one complete generation. |
| Agent protocol state | The Pi and Codex extension modules | Rust provides bounded transport and no inferred agent state. |

## Process composition

`ApplicationIdentity` has two values: Production and Development.

The identity selects the config tree, state tree, control descriptor, local
daemon catalog, rmux endpoint, tmux server, and zellij socket directory.

The `bootty` executable launches the GUI only when the selected identity has no
live owner. An argumented invocation uses the owner-local control endpoint.

The installed daemon defaults to Production. Local development identity does not
change remote assets or remote backend namespaces.

## Workspace and mux

`WorkspaceRepository` owns SQLite access for Spaces and backend bindings.
It loads one validated `WorkspaceSnapshot`.

`WorkspaceRuntime` owns the live committed workspace value.
`BindingRuntime` owns one committed placement and one realized
`MuxBindingConfig`.

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

`bootty-mux-model` owns dependency-neutral backend, SSH target, binding, and
remote Space wire values.
`bootty-mux` owns backend commands, protocol adapters, SSH processes, remote
installation, and pane runtime behavior.

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

`CommandCatalog` resolves core and extension commands.
`AppState` applies target resolution and destructive confirmation.

The command palette, keybindings, CLI, local socket, and Luau host submit the
same `CommandInvocation`.

The control layer transports requests and outcomes.
It does not own command policy or workspace state.

Detached tasks and event subscriptions use opaque owner-local capability IDs.

## Extensions and agents

A local extension module is one canonical relative `.lua` or `.luau` path
under `<config>/extensions`.

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

- `bootty-app` owns product composition, the desktop host, workspace UI,
  commands, control, extensions, and agent integration assets.
- `bootty-config` owns product configuration.
- `bootty-daemon` owns the installed headless catalog and remote commands.
- `bootty-font` owns the shared OpenType feature value and grammar.
- `bootty-identity` owns the closed application identity.
- `bootty-mux-model` owns dependency-neutral mux values.
- `bootty-mux` owns backend and SSH execution.
- `bootty-render` owns paint planning and WGPU preparation.
- `bootty-runtime` owns PTY sessions and terminal workers.
- `bootty-surface` owns host-neutral geometry.
- `bootty-terminal` owns terminal semantics and frames.
- `bootty-ui` owns shared egui theme values and widgets.
- `bootty-winit` owns native host and input adapters.
- `bootty-site` owns the documentation site.
- `bootty` is the stable library facade.

## Application modules

- `app/state.rs` owns the window-independent application state machine.
- `app/host.rs` owns the eframe adapter and window host policy.
- `app/config_runtime.rs` owns accepted config and derived input policy.
- `app/dialog_runtime.rs` owns the one modal dialog value.
- `app/terminal_workspace_view.rs` owns terminal widget and renderer lifecycle.
- `app/workspace_runtime.rs` owns live Space and binding composition.
- `command_extensions.rs` and its child modules own extension generations.
- `commands.rs` owns typed command descriptions and submission.
- `control.rs` owns local transport and instance publication.
- `ui/settings/surface.rs` owns settings UI composition.

These files can remain large only while each keeps one cohesive owner and a
small interface. File size is a signal for review. It is not proof of depth.
