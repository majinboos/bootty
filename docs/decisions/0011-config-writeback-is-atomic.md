# Config writeback is atomic

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config::ConfigDocument` owns one locked update workflow for Bootty
preference writeback. The workflow reads the latest document, applies one
mutation, validates the result, syncs a complete replacement, and atomically
publishes it.

## Authority and invariants

- `ConfigDocument` owns round-trip TOML mutation and replacement.
- App callers own live UI state, reload ordering, and error presentation only.
- A sibling advisory lease serializes Bootty writers for one resolved config
  target. The lease starts before the source read and ends after replacement.
- An existing final-path symlink remains a symlink. The workflow resolves
  relative links against their containing directory and rejects cycles.
- A dangling final link can create its target. A missing ordinary target can
  also be created.
- The replacement temporary file is exclusive and lives in the target
  directory.
- The complete candidate is parsed as TOML before replacement. Includes remain
  source declarations. The workflow does not write a merged config.
- The temporary file is flushed and synchronized before replacement.
- An existing Unix target keeps its permission bits. A new Unix target uses
  mode `0600` because the config can contain SSH connection metadata.
- An existing Windows target is replaced with `ReplaceFileW`. A new Windows
  target is published with `MoveFileExW` and `MOVEFILE_WRITE_THROUGH`.
- Unix replacement uses one same-filesystem rename. The parent directory is
  synchronized after the rename.

## Failure and recovery

A failure before replacement leaves the previous target byte-for-byte active.
Temporary-file cleanup is best effort and does not replace the primary error.
The error keeps the prefix `failed to write config file {path}:` and names the
failed phase.

A parent-directory sync failure happens after replacement. The write result
therefore reports a committed config with uncertain crash durability. App
callers refresh the hot-reload baseline, reload the committed config when that
flow requires it, and show the durability warning.

## Concurrency

Every Bootty writer uses one `update_config_document` workflow. The workflow
loads the source while it owns the lease. A later Bootty writer therefore sees
and preserves the earlier write.

External editors do not use this lease. Atomic replacement prevents a torn
config. It does not provide compare-and-swap behavior across other programs.
An external symlink retarget during a write is outside the lease contract.

## Rejected alternatives

- Direct `fs::write` can truncate the only valid config.
- Loading outside the lease can lose another Bootty writer's update.
- Delete-then-rename creates a crash window with no config.
- A backup history, config daemon, generic filesystem trait, and include
  writeback are outside this slice.
- A successful replacement is not reported as an ordinary failed write only
  because the later directory sync failed.

## Migration consequences

All settings, sidebar, appearance, theme, and font-size writes use the locked
workflow. Existing comments, ordering, includes, unrelated tables, symlinks,
and Unix permission bits remain intact. New Unix config files become private by
default.

The Windows branch has a compile proof. Its existing-file replacement needs a
runtime contract on the Windows repository runner because this macOS workspace
cannot execute `ReplaceFileW`.
