# UI regression recovery

The branch's deletion wave (`7062c10 chore(wip): checkpoint architecture cleanup`, plus the
composition work after it) removed working UI. This tracks the reconstruction. Each item is
rebuilt in the current architecture — reusable painting in `bootty-ui`, meaning/routing in
`bootty-app` — not restored by revert.

Method: `crates/bootty-app/tests/settings_page_snapshots.rs` renders every settings page and
compares its painted text against a committed baseline. The same test was run at the merge base
(782dbd6) in a scratch worktree to produce the "before" side, which is how these were found.

## High

- [x] H1 Text: the font-features picker card (10 preset chips with labels+descriptions, `Clear`,
      an Advanced raw field) became one plain text row.
- [x] H2 Appearance: `color_hex` dropped alpha, so Focus border opacity was silently discarded.
- [x] H3 Status Bar: the live preview strip (`status_preview`/`status_preview_bar`) is gone.
- [x] H4 Sidebar: the SIDEBAR and SESSION module lists are gone — no enable, disable or reorder,
      and `+ New module` no longer activates what it creates. `sidebar.modules` /
      `sidebar.session_modules` still drive the real sidebar, so the config path is live and
      unreachable.
- [x] H5 Module editor: Luau syntax highlighting, the line-number gutter and autocomplete are gone
      (`egui_code_editor` dropped from the workspace).
- [x] H6 Module preview: a real chrome render became one plain label per item.
- [x] H7 Keys: `Resolved shortcuts` lost its search, keycaps, responsive grid, human action titles
      and trigger-flag tags — now a raw `trigger=action` dump.
- [~] H8 Keys: binding edits need an `Apply keybindings` click. Kept: the draft model is the
      architecture, and `commit_draft` runs on scope switch and on close, so no edit is lost. H9 was
      the real defect.
- [x] H9 Keys: `Edit sidebar shortcuts` (and `focus_keybinding`) discard uncommitted drafts.
      New bug, not a restoration.
- [x] H10 Keys: the trigger cell lost keycap rendering, the record dot, the recording pulse and its
      pointer cursor.
- [x] H11 Keys: the Prefix row took the same downgrade.

## Medium

- [x] M1 Status Bar: segment rows show raw module ids (`sysinfo · Right`), not display names.
- [x] M2 Status Bar: the module picker is no longer scoped to status placement, and nested
      identities collapse onto their file stem.
- [x] M3 Sidebar: module rows show the raw file identity including its extension.
- [x] M4 Window: Decoration and Fullscreen mode lost their per-option descriptions.
- [x] M5 Window: Top offset read only the draft, so an `include`d value showed as "Auto".
- [x] M6 Module editor: Cmd+/ comment toggle gone; fixed height, no horizontal scroll, page
      scrolls under the pointer.
- [x] M7 Keys: the conflicts notice panel became one plain text line.
- [x] M8 Keys: the per-row options button lost its active-state tint.
- [x] M9 Keys: a valid row's green check became the word "valid".

## Low

- [x] L1 Appearance: `Status bar background` moved to the end of SIDEBAR COLORS.
- [~] L2 Remotes: the test-connection result overwrites the save message. Kept — one transient
      notice slot where the newest result wins is the behaviour a reader expects.
- [x] L3 Module editor: trailing-newline handling and the module path label.
- [x] L4 New module: client-side validation messages gone, placeholder now demands an extension.
- [x] L5 Keys: `→` moved to the UI font (tofu risk); the flags editor lost its frame and margins.
- [x] L6 Extensions: number fields size from an unbounded range; section headers are raw namespaces.
- [~] L7 Deleted tests. Replaced for every behaviour that actually regressed — action titles, the
      chord recorder's list, `hover_selects`, run-end rounding, the sweep clock, nav icons, the
      module preview, the editor gutter and comment toggle — plus a rendered-text snapshot per
      settings page. The wider inline-test loss in the chrome files is not backfilled.
- [x] L8 Sidebar: empty state reworded.

## Second wave, from the non-settings audit

- [x] The whole Luau floating-window surface: a bare `egui::Window` titled with a raw surface id
      became the native overlay again, and a declaration can name its title, icon and hint.
- [x] A declared floating surface permanently disabled terminal keyboard input; it now depends on a
      surface actually showing items.
- [x] The per-session agent row the default config still asked for, agent detection through a
      wrapper process, the pulsing activity line, and the process row naming the agent.
- [x] `agents.codex` / `agents.pi` were registered but never painted.
- [x] Built-in session rows were layered over a user-owned `sessions` module, duplicating every row.
- [x] The hover overlay squared off a rounded tab; a clipped tab run was rounded as if whole; a
      click could overwrite a context-menu choice from the same pass. The last two were introduced
      by this campaign's own status-strip move.
- [x] Sweeping the pointer over the Ditch dialog re-aimed Enter at a destructive action.
- [x] The window-tab progress bar stepped at its module's 0.2s interval; the painter drives it now.
- [x] The find bar's buttons, status items bleeding across tab rows, a drop indicator over
      non-droppable rows, and an empty state disagreeing with the rows on screen.

## Intentional deletions, for the record (not regressions)

- The Zellij backend was removed from the whole workspace in `7062c10`, which is why the Keys
  scope selector and the General backend picker each lost an option.
- `keybind_presets.rs` moved the pane navigation defaults from `alt+<key>` to `alt+shift+<key>`.
