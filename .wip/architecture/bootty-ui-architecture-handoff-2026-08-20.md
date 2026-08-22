# Bootty UI Architecture Completion Handoff

Date: 2026-08-20
Repository: `/Users/luan/src/bootty`
Branch: `luan/cleanups`
Commit at handoff: `9334d914f78a6f7702f6a299d87ba2c0a65d8717`
Last update: 2026-08-20, after the chrome and settings ownership cuts (working tree clean)

## Objective

Finish the application architecture recovery, not another cleanup wave.

The end state is:

```text
authoritative domain owners
  -> borrowed, frame-local presentation
  -> egui views
  -> typed feature intents
  -> owner update/effect execution
  -> next accepted state
```

`bootty-ui` remains reusable presentation mechanics. It must not own Bootty product policy, mux/workspace state, configuration authority, persistence, retries, remote processes, or async task lifetimes.

This work is complete only when that boundary is consistent across settings, chrome, dialogs, and workspace presentation. Aggregate LOC deletion alone is not completion.

## Current checkout warning

The campaign up to the architecture cut is checkpointed in `7062c10 chore(wip): checkpoint architecture cleanup`, including the user's keybinding-preset edits. Every cut since is its own commit on `luan/cleanups`; the working tree is clean. The branch will be repackaged for review later, so commit freely and do not rewrite the earlier history.

- Do not reset, clean, switch branches, or recreate the work in another worktree.
- Do not revert unrelated hunks.
- Reinspect `git status` before every broad edit.
- Treat `crates/bootty-config/src/config/defaults.rs` and `keybind_presets.rs` as user-owned work.
- No commit or PR has been requested. `7062c10` was a checkpoint; ask before committing again.

## Current measured state

The refreshed database is `.wip/architecture/bootty-rust-complexity-2026-08-19.sqlite`; the
`*_after` tables hold the current snapshot. Refresh them with:

```sh
scc --by-file --uloc --format sql --include-ext rs crates tests
```

then replace `scc_files_after` / `metadata_after`, rewriting the project column to
`bootty-all-architecture` (the `*_after` views derive everything else).

Current production metrics:

| Scope | Before | Current | Delta |
|---|---:|---:|---:|
| All production Rust LOC | 83,202 | 76,967 | -6,235 |
| All production complexity | 8,403 | 7,493 | -910 |
| `bootty-app/src/ui` LOC | 12,925 | 10,474 | -2,451 |
| `bootty-app/src/ui` complexity | 1,492 | 1,163 | -329 |

Aggregate production LOC rose by 235 since the 2026-08-20 handoff while `src/ui` fell by 363.
That is the ownership work: `chrome_frame.rs` (the owner-side prepare/apply for the chrome frame)
and the owner APIs on `ExtensionHost`, `AppConfigRuntime` and `AppState` are new code that replaced
mixed paint-time paths. Read it as a boundary move, not a deletion, and do not try to buy it back
with wrappers.

Where the UI still is:

| Remaining area | LOC | Complexity |
|---|---:|---:|
| `bootty-app/src/ui` | 10,474 | 1,163 |
| `bootty-app/src/ui/settings` | 4,694 | 454 |
| `bootty-app/src/ui/chrome` | 2,957 | 405 |

| File | LOC | Complexity | Shape |
|---|---:|---:|---|
| `ui/chrome/sidebar_panel.rs` | 1,050 | 149 | leaf view: paints from `SidebarModel`, returns one typed event |
| `ui/chrome/status_bar.rs` | 1,007 | 156 | leaf view: measurement, notch wrapping, joins, hit testing |
| `ui/settings/surface.rs` | 825 | 58 | page router plus accepted config and per-pane editor state |
| `ui/settings/surface/keybinds.rs` | 820 | 132 | the Keys pane and its recorder |
| `ui/chrome/runtime.rs` | 632 | 71 | chrome layout only (was 937/124) |

The two largest files are now leaf presentation: neither holds an owner handle, starts filesystem
or process work, or mutates anything outside its own frame. Their size is layout, measurement and
hit testing. Treat the handoff's earlier "roughly 8,000-8,500 UI LOC" target as met in spirit only
if a later reading of those two files finds real duplication; do not delete presentation logic to
reach a number.

## Architecture decisions already settled

### Keep egui

Do not migrate to Iced or GPUI.

Borrow these patterns instead:

- Iced: feature-local state, typed intent, update, and explicit effects.
- GPUI/Zed: lifecycle-owned tasks, typed events, weak identity revalidation after async work.
- Rerun: prepared frame data and narrow borrowed contexts for egui plus custom WGPU rendering.
- Notedeck: shell-owned active/open/focus lifecycle and background update separate from visible rendering.

Do not add:

- a universal `UiMessage` or root reducer;
- a GPUI-style entity arena;
- a Zed-style heterogeneous `Item` trait;
- a global event bus;
- a service-locator context;
- schema-driven UI infrastructure without deletion proof;
- a cached workspace presentation before truthful semantic invalidation exists.

### Feature shape

Use the smallest version that deletes an existing mixed path:

```rust
struct FeatureState {
    // Durable workflow state and accepted owner state.
}

struct FeaturePresentation<'a> {
    // Borrowed values prepared for this frame.
}

enum FeatureIntent {
    // A semantic user decision crossing an owner boundary.
}

enum FeatureEffect {
    // Owner-run filesystem, network, process, persistence, or async work.
}
```

Local form fields may mutate their local draft directly. Do not emit messages for every text edit. Create an intent only when an action crosses a real owner boundary.

### Frame lifecycle

`eframe::App::logic` should:

1. drain worker results;
2. advance domain owners;
3. apply accepted completions;
4. prepare or publish immutable snapshots;
5. request repaint when visible state changes.

`eframe::App::ui` should:

1. build or borrow frame presentation;
2. render egui and terminal surfaces;
3. collect typed intents;
4. return without filesystem, network, process, or persistence work.

After rendering, the shell routes intents to their authoritative owners.

## Work already completed in the architecture cut

### Modal lifecycle

Files:

- `crates/bootty-app/src/ui/dialog_runtime.rs`
- `crates/bootty-app/src/host.rs`
- `crates/bootty-app/src/state/dialogs.rs`

Idle modals now remain boxed in `DialogRuntime`. They are no longer taken from `AppState`, returned as a synthetic `Event::None`, and reboxed every frame. Modal views emit `Option<Intent>` or the smallest equivalent. `AppState` runs only real actions.

Preserved behavior:

- rename/ditch failures retain their dialog;
- successful actions close and restore terminal focus;
- theme preview, restore, select, and close ordering;
- modal replacement and input gating.

### New-session remote workflow

Files:

- `crates/bootty-app/src/new_session.rs`
- `crates/bootty-app/src/ui/new_session_picker.rs`

The root workflow owner now owns thread spawning, channels, cancellation, the single-worker permit, and remote service calls. The picker emits typed remote effects and consumes outcomes. Dropping the dialog cancels owned work.

### Space editor

Files:

- `crates/bootty-app/src/ui/space.rs`
- `crates/bootty-app/src/remote_catalog.rs`

`SpaceDraft` separates editable durable values from transient UI state. Remote catalog work is cancellable, tagged with profile identity, and protected by a worker permit. A stale result cannot update a replacement profile. Remote creation remains successful and selected even if the following catalog refresh fails. Disconnected workers now produce an error instead of leaving the UI stuck on “loading.”

WorkspaceRuntime remains the authority for persistence, activation, terminal transition, and backend-keybinding publication.

### Settings SSH test

Files:

- `crates/bootty-app/src/ui/settings/surface/remotes.rs`
- `crates/bootty-app/src/ui/settings/surface.rs`
- `crates/bootty-app/src/host.rs`

Painting emits an SSH test request. `BoottyApp` starts the operation after rendering. The editor retains only the receiver and result presentation.

### Chrome presentation

Files:

- `crates/bootty-app/src/ui/chrome/runtime.rs`
- `crates/bootty-app/src/ui/chrome/status_bar.rs`

Chrome now has a named frame-local presentation. Status items borrow extension snapshot data rather than cloning strings, primitives, and item DTOs each frame. Leaf status actions carry the existing `ExtensionUiAction`; the owner still runs effects.

The notch regression fix is mandatory: identify the semantic windows surface with `segment.surface == "windows"`. `segment.module` is the producer identity such as `windows.luau` and must remain intact for generation/action routing.

### Extension source ownership (2026-08-20)

Files:

- `crates/bootty-extension/src/module_sources.rs`
- `crates/bootty-extension/src/host.rs`
- `crates/bootty-app/src/ui/settings/surface/modules.rs`
- `crates/bootty-app/src/ui/settings/surface/status_bar.rs`
- `crates/bootty-app/src/ui/settings/surface.rs`
- `crates/bootty-app/src/host.rs`

`ExtensionHost` is now the single authority for editable module sources. It exposes
`module_sources() -> ModuleSources<'_>` (identities plus legacy files, both filled by the existing
500 ms scan) and `apply_module_source_request(ModuleSourceRequest) -> ModuleSourceOutcome` for
load/create/save/reset/import-legacy. Import validation uses the host's own theme facts, so the
editor no longer recomputes theme tokens for it.

The settings editor keeps only editor state: the loaded draft, the selection, the create field, and
the preview. Painting pushes typed requests; `BoottyApp` runs them after `show` and applies the
outcome, then requests a repaint. Deleted from painting:

- `module_identities` + `legacy_extension_modules` directory walks on every frame, in both the
  Extensions pane and the status-segment page;
- `editable_module_source` / `save_module_source` / `reset_module_source` /
  `import_legacy_extension_module` calls inside paint branches;
- the second theme-directory scanner (`available_themes` in `settings/surface/appearance.rs` and
  `theme_picker::available_themes`), replaced by `bootty_config::config::available_theme_names`,
  which owns the built-in catalog plus `themes/*.toml` and collapses case-duplicates.

A module whose Luau fails to load stays listed, so it can be edited back into shape. Selecting a
module shows "Loading module source…" for exactly one frame before the owner answers. A create is
handed back once, to the page whose button asked for it, and dropped if settings closed in between.

### Accepted-config revision (2026-08-20)

Files:

- `crates/bootty-app/src/config_runtime.rs`
- `crates/bootty-app/src/state.rs`
- `crates/bootty-app/src/ui/settings/surface.rs`
- `crates/bootty-app/src/host.rs`

`AppConfigRuntime` carries a `revision` bumped by every accepted or live config change; the only
mutable path to `current` is `current_mut()`, which records the change. `SettingsSurface` remembers
the revision it synced, so the settings frame no longer clones the whole `BoottyConfig` and
`ConfigDocument` on every frame. A dirty draft still blocks the sync and retries after acceptance.

This is the first of the narrow semantic revisions the workspace-presentation cache will need.

Tests:

- `crates/bootty-extension/tests/extension_ui_contracts.rs::editor_requests_run_against_the_host_extension_root`
- `crates/bootty-extension/tests/extension_ui_contracts.rs::legacy_module_stays_in_place_until_explicit_validated_import` (now drives the host request path)
- `crates/bootty-app/tests/live_config_reload_contracts.rs::every_accepted_config_change_advances_the_revision`
- `crates/bootty-config/tests/config.rs::theme_catalog_combines_builtin_and_user_themes` (moved from `bootty-app/tests/dialog_contracts.rs`)

### Chrome ownership (2026-08-20)

Files:

- `crates/bootty-app/src/chrome_frame.rs` (new: the owner side of the chrome frame)
- `crates/bootty-app/src/ui/chrome/runtime.rs`
- `crates/bootty-app/src/host.rs`
- `crates/bootty-app/src/state.rs`

The chrome frame is now prepare -> paint -> apply:

```text
host.logic: sample window/notch facts (after this frame's input and config reload)
host.ui:    drain extension session reorders -> chrome_frame::prepare -> extensions.update_mux
            -> ChromeRuntime::show(&AppState, &ExtensionHost, tab_context, cell_height)
            -> chrome_frame::apply(swipe, sidebar, spaces, resize, status bars, in that order)
            -> shell installs the frame's chrome handles -> shell paints the terminal
```

What that deleted from painting: `handle_sidebar_event` and `handle_status_bar_event` (moved
verbatim to the owner), the inline space-switcher match, the swipe activation, the live and
persisted sidebar-width writes, `ChromeView::prepare_frame`, the reorder drain and `update_mux`
publication, five per-frame AppKit calls, the keepawake guard, `reset_chrome_handles` /
`register_chrome_handle`, and three whole-catalog surface clones per frame.

The chrome view now takes `&AppState` and `&ExtensionHost`. Mutating owner state from the chrome
paint is a compile error, which is the real guarantee this cut bought.

Ordering preserved exactly: swipe, then sidebar event, then space switcher, then resize, then the
status bars top-before-bottom. Events are applied before the terminal is painted, so a session or
Space switch still shows its own terminal in the same frame.

One measured behavior difference, accepted deliberately: the sidebar's `focused`,
`hovered_session` and unfocused-dim overlay are read during the paint, and the events that mutate
them now run after it. A Space swipe (which clears the hover and moves focus to the terminal) and a
context action that opens a dialog therefore paint one frame with the pre-event values; the repaint
`apply` requests corrects it on the next frame. Removing that frame would mean applying owner
mutations mid-paint again, which is what this cut removed.

Also landed with it:

- `ExtensionCatalog::surfaces_for` / `has_surfaces` filter inside the lock; the duplicated
  surface-name matcher became `PublishedSurfaceSnapshot::matches_name`.
- The window-tab surface identity and the `activate-window:` action encoding each have one
  implementation (`chrome::is_windows_surface`, `chrome::activate_window_target`), so the semantic
  surface identity can no longer drift from the producer module identity.
- `SpaceSwitcherItem` deleted; the switcher borrows the owner's `SpaceSummary`.
- Per-pane terminal facts are keyed by pane id instead of the enclosing window, deleting an
  O(panes^2) topology scan per frame and keeping a moved pane's progress.

### Settings ownership (2026-08-20)

Files:

- `crates/bootty-app/src/ui/settings/surface.rs`
- `crates/bootty-app/src/ui/settings/surface/keybinds.rs` (+ `model.rs`, `trigger_edit.rs`)
- `crates/bootty-app/src/host.rs`

- The nine loose keybind fields became `keybinds::EditorState`, matching the modules and remotes
  panes. `cancel_capture`, `reload_scope` and `focus_action` replaced five open-coded capture
  clears and six loaded-scope pokes.
- The keybind model takes the draft document and the input config, not the whole surface.
- Modifier sides are rewritten by parsing with `BindingTrigger` and re-formatting, deleting the
  editor's private left/right token table. Unparseable text passes through, so a half-typed row
  stays editable.
- The font database scan and the themes-directory read happen in `open_settings`, not on a page's
  first paint. `bootty_render::font_database::installed_family_names` and
  `bootty_config::config::available_theme_names` own those lists; the second theme scanner is gone.

## Remaining architecture work

### 1. Finish settings ownership

Done: extension source editing, the Keys pane editor state, the model de-forwarding, the catalog
scans, and the revision-gated accepted config. `SettingsSurface` is now accepted config + document
+ page + per-pane editor state.

Left, in order:

1. One draft-survival rule — mostly done, one step deliberately left. The rule is "seed the editor
   buffer once per editing key; only an explicit key change reloads it". Two of the three
   mechanisms now follow it directly: the keybind loaded-scope gate always did, and session env is
   now its own lazy draft (`session_env`) instead of a dirty flag plus two accepted-config stashes,
   so the surface's accepted config no longer deliberately holds invalid pairs. `modifier_rows` is
   the same rule with a degenerate key and can be folded in when convenient.

   **Never key a draft on `config_revision`.** An accepted rebind bumps the revision, and
   `write_scope` strips incomplete rows before the document is written, so a revision-keyed draft
   would reseed from a document missing exactly the half-typed row the invariant protects.
   `synced_revision` is a cache token for the accepted-config copy only.

   The `Draft<K, T>` generic proposed for all three was not taken: it costs about 30 lines to save
   about 12, and its highest-churn step (folding the keybind rows into it) has no test safety net
   in a 939-line file. Fold `modifier_rows` into the existing lazy-Option idiom instead.

   Fixed on the way: `commit_draft` cleared the keybind dirty flag when it wrote rows into the
   draft document, before the owner accepted anything, so a refused write (an unwritable
   `config.toml`) hid Apply while its failure notice was still on screen, with no way to retry.
   `SettingsSurface::reject_submission` re-arms it. A validation rejection was already
   self-healing, because fixing the row re-sets `changed`.

2. Narrow each settings page to the draft/editor fields it uses, page by page, only where it
   deletes a mixed path. Do not wrap `SettingsWriteback` again; it is already a tight draft owner,
   and its `dirty` (held until acceptance) and `submit` (consumed each frame) are not duplicates.
3. Do not add a `SettingsIntent` enum. Nothing in these pages starts side effects during paint any
   more, so an intent enum would be a forwarding layer with no old path deleted.

### 2. Finish chrome ownership

The vertical flow is done (see the completed section). What is left is inside the leaf views and
their models, and none of it is ownership:

1. **One sidebar row model — deferred, and it is not a deduplication.** `ui/sidebar.rs` builds two
   row models: a published-items merge, and a Rust re-implementation of the Luau grouping in
   `crates/bootty-extension/src/extension_ui.luau`, used only when `binding_count() > 1`. Deleting
   the Rust builder (~190 LOC) is blocked on a data-model change, not on Luau work:

   `MuxView` is single-binding by construction — one flat `sessions: Vec<SessionView>` filled from
   the active binding, one `scope_key`, and neither `SessionView` nor `ModuleItem` carries a scope.
   So `build_sidebar_items_from_published_items` stamps one scope onto every row. Merging the two
   models before the fact model grows a binding axis breaks four load-bearing behaviors:

   - **Cross-binding reorder becomes possible.** `sidebar.rs` nulls `reorder_anchor` on every
     binding-group row, which is what disables drag and MoveUp/MoveDown; Luau's `ui.session_items`
     always sets an anchor. The handler is scope-blind (`reorder_active_session_before`), so a drag
     on a non-active binding's row would silently reorder the active binding using a foreign
     session name. That is a trust-boundary defect, not a cosmetic one.
   - **Selection collides.** `presentation_values.rs` locks that two bindings legitimately share a
     backend session id (tmux ids are per-server). Flattened, both rows read as current, and the
     row's persistent egui Id `(scope, id, kind)` stops being unique.
   - **`context_position` changes meaning**: `(index, len)` per binding today versus one global
     map, and those two numbers gate MoveUp/MoveDown/SwitchSession.
   - **`can_return_to_last_session` and the accent colors are per binding**, and the header count
     sums across bindings.

   Preconditions, in order: give `MuxView` a per-binding structure (or put `scope_key` on
   `SessionView`/`ModuleItem`); resolve each row's scope from that field; make
   `reorder_session_before`/`move_session_from_ui` take a `MuxScope` and reject a foreign one;
   restore a per-scope `context_position`; only then publish binding group rows from
   `sessions.luau` and delete the Rust builder.

   No test drives a multi-binding sidebar through the panel — the multi-binding tests exercise the
   builders as pure functions, which cannot catch a mis-scoped click, drag or context menu. A
   second binding needs a Space with two `WorkspaceBinding`s, in practice a live remote host.
   Reopen this when that can be exercised manually, or when an injectable mux backend can fake a
   second binding in-process.
2. Everything else the area maps proposed for these files was rejected on the stop rules: caching
   composed sidebar rows or the surface list across frames, unifying the three "is this session
   selected" predicates (the third is a deliberate live override), unifying `context_position`
   with `sidebar_blocks` (different clipping semantics on purpose), turning the native window drag
   into an event, and applying `macos_set_window_shadow` on change instead of per frame.

### 3. Add truthful workspace presentation invalidation only at real owners

Still the precondition for any retained chrome snapshot. One revision now exists:
`AppState::config_revision`, bumped by `AppConfigRuntime::current_mut`, and used by the settings
surface to skip a per-frame clone of the accepted config and document. Copy that shape.

Inputs that still mutate with no revision or typed dirty cause:

- mux topology (per-resource generations exist in `bootty-mux/src/controller.rs` but nothing
  aggregates them for a presentation reader);
- selection (`activate_target` persists then publishes, with no revision bump);
- session display names and order;
- terminal titles, progress and ports (`BindingTerminalFacts` is plain maps);
- Space metadata, selection and reconnect state;
- extension published surfaces and their generations.

Until every displayed input participates, keep rebuilding `chrome_frame::prepare` each frame.
`facts.update_mux` compares the whole projection structurally; that equality check is the only
invalidation that exists today.

### 4. Finish shell/dialog consistency

Done, and deliberately left alone. Modal closes funnel through one `AppState::dismiss_modal_dialog`,
`KeybindHelpDialog::show` returns a plain `bool` because that is the smallest truthful result, and
terminal-find keeps its own owner path. Do not add a dialog trait or a modal event bus.

### 5. Re-evaluate crate boundaries after ownership moves

`bootty-ui` is clean: its only dependencies are `eframe` and `iconflow`, and it holds no Bootty
product type. The reverse move — lifting `settings_pane` and `module_selector_row` from
`ui/settings/surface/modules.rs` into `bootty_ui::settings` — was considered and not done: both
have exactly two callers inside settings, so the move would improve the `src/ui` number without
changing ownership, which is the stop rule.

## Correctness invariants

Any implementation must preserve all of these.

### Persistence and publication

- Config becomes live only after `AppConfigRuntime` validates and atomically commits the accepted document.
- Space creation persists before runtime insertion/publication.
- Space update persists before mutating live runtime.
- Space activation persists restore state and selected Space before swapping the active runtime.
- Mux selection uses persist-before-publish when required.
- Remote mux mutation journals before backend execution and commits persistence afterward.

### Async and task lifetime

- Dropping a workflow owner cancels replaceable work.
- Results are tagged or otherwise checked against the current owner/profile/generation after await.
- A stale result never mutates a replacement dialog or profile.
- Exactly-once mux commands are not represented as cancel-on-drop UI tasks.
- A successful destructive or remote mutation is never reported as wholly failed because a later refresh failed.

### Trust boundaries

- External `CommandTarget` retains exact opaque handle plus generation equality checks.
- UI uses typed internal targets; it never decodes or fabricates external handles.
- UI intents and external serialized commands may share a lower planner, not transport/admission semantics.

### Rendering

- Glyph/cluster/shaped/atlas caches remain unless benchmark evidence proves redundancy.
- Terminal rendering remains in the existing WGPU paint/atlas/texture pipeline.
- UI presentation must not change draw order, notch layout, input hit testing, accessibility, or focus semantics.

## Stop rules

Revert a slice when any of these occurs:

- It adds a state bag, context, trait, event enum, or forwarding layer without deleting the old owner path.
- UI still starts the same filesystem/network/process work after the refactor.
- A “presentation” type contains `AppState`, `WorkspaceRuntime`, `ExtensionHost`, callbacks into them, or other owner handles.
- The change only moves code out of `src/ui` without changing ownership.
- Production LOC grows for a purely architectural rename or wrapper.
- Settings, extension editing, retries, accessibility, or user-visible features are removed to satisfy metrics.
- Persistence-before-publication ordering changes.
- Async results can apply to a stale owner.
- A workspace cache is introduced without complete semantic invalidation.
- Focused test time grows materially without protecting a new behavior boundary.

A small positive LOC delta is acceptable only for a demonstrated correctness fix, such as cancellable stale-safe remote mutation. Record that explicitly and offset it in the same vertical flow when safe.

## Definition of done

Status as of 2026-08-20, after the chrome and settings cuts:

1. **Met.** No egui page or view starts filesystem, network, process, persistence, or
   authoritative mux work. One documented exception remains: `same_dir` in
   `ui/new_session_picker.rs` canonicalizes two paths on a step transition, with the upgrade
   condition written at the call site.
2. **Met.** Settings, chrome, new-session, Space and dialogs render prepared state and emit their
   smallest existing typed event. No universal intent enum was added.
3. **Met.** AppConfigRuntime, WorkspaceRuntime, ExtensionHost, terminal workers and the mux
   controller are the sole authorities; the chrome view holds `&AppState` and `&ExtensionHost`.
4. **Met** by the earlier new-session, Space and settings-SSH cuts; nothing added since is async.
5. **Met** in the safe direction: chrome consumes one frame-local projection and retains no cache,
   because the semantic revisions are not complete (see remaining item 3).
6. **Met.** `bootty-ui` depends only on `eframe` and `iconflow` and holds no product type.
7. **Met** for every path these cuts touched.
8. **Met** — see Validation.
9. **Met** — `scc_files_after`, `metadata_after`, `candidate_targets` and `static_analysis_runs`
   are refreshed.
10. **Met** — the measured-state section reports `bootty-app/src/ui` separately.

Open: remaining items 1 (one draft-survival rule) and 2 (one sidebar row model) in the section
above. Both are behavior-bearing rather than ownership work.

Metrics are a guardrail, not the definition. `src/ui` is 10,474 LOC / 1,163 complexity against the
earlier 8,000-8,500 aspiration; the gap is `sidebar_panel.rs` and `status_bar.rs`, which the area
maps confirmed hold no owner handle and start no side effects. That is the "irreducible
presentation logic" exception, with one exception of its own: the duplicated sidebar row model in
remaining item 2 is genuinely deletable once the Luau side publishes binding groups.

## Validation

Run focused tests during each slice. Do not run independent Cargo commands concurrently.

Required final gate:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --tests
cargo test -p bootty-app --bench paint_plan --no-run
```

The control-plane tests require permission to create/use their local owner endpoint. A sandboxed `Operation not permitted` is an environment failure; rerun the same suite with the required permission.

Current validation is green (2026-08-20, after the chrome and settings cuts):

- `cargo fmt --check` passed;
- workspace all-target Clippy passed with warnings denied (logged as
  `static_analysis_runs.profile = 'required-ownership-wave'`);
- `cargo test --workspace --lib --tests`: 803 passed, 0 failed, across 133 suites;
- paint-plan benchmark compile passed.

Tests added or moved by these cuts:

- `bootty-extension/tests/extension_ui_contracts.rs::editor_requests_run_against_the_host_extension_root`
- `bootty-extension/tests/extension_ui_contracts.rs::legacy_module_stays_in_place_until_explicit_validated_import`
  now drives the host request path
- `bootty-app/tests/live_config_reload_contracts.rs::every_accepted_config_change_advances_the_revision`
- `bootty-config/tests/config.rs::theme_catalog_combines_builtin_and_user_themes`, moved from
  `bootty-app/tests/dialog_contracts.rs` to the crate that now owns the catalog

Behavior with no test coverage, needing a manual pass on macOS:

- same-frame chrome event ordering (a swipe plus a switcher click in one frame);
- entering and leaving fullscreen with tabs-in-notch on and off, on a notched display;
- a sidebar session switch, watched for a stale terminal frame;
- a pane moving between windows while reporting progress.

## SQLite queries for the next session

```sql
-- Current production totals.
SELECT
  SUM(CASE WHEN scope IN ('production','build') THEN code ELSE 0 END) AS code,
  SUM(CASE WHEN scope IN ('production','build') THEN complexity ELSE 0 END) AS complexity
FROM rust_files_after;

-- Current UI directory.
SELECT folder, production_code, production_complexity
FROM folder_metrics_after
WHERE folder = 'crates/bootty-app/src/ui';

-- Highest current modules.
SELECT rank, module, code, complexity
FROM production_module_ranking_after
LIMIT 20;

-- Highest current files.
SELECT rank, path, code, complexity
FROM production_file_ranking_after
LIMIT 30;

-- UI file deltas from the original baseline.
SELECT path, before_code, after_code, code_delta,
       before_complexity, after_complexity, complexity_delta
FROM scc_file_delta
WHERE path LIKE 'crates/bootty-app/src/ui/%'
ORDER BY complexity_delta, code_delta;
```

## Recommended session start

1. Read this handoff.
2. Run `git status --short --branch` and `git log --oneline 7062c10..HEAD`.
3. Query the SQLite metrics above; the `*_after` tables and `candidate_targets` were refreshed on
   2026-08-20 after the ownership wave.
4. Inspect current diffs in settings/chrome before editing.
5. Start with the one draft-survival rule in settings (remaining item 1). The sidebar row model
   (remaining item 2) needs a manual multi-binding pass, so schedule it when you can run the app.
6. Use disjoint implementation owners.
7. Fan out read-only mapping and adversarial review to subagents; keep implementation in one
   sequential thread. The area maps for the four hot files, and the ranked cut list they produced,
   are what this wave was implemented from: `runtime.rs` alone was touched by nine of the fourteen
   cuts, so parallel implementers would have collided.
8. Measure and validate each vertical slice. Keep correctness fixes even when slightly additive; revert architectural ceremony.
