# Customized extension source is atomically persisted

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-app::extensions` owns durable customized Lua and Luau source bytes. A
save publishes one complete file through the existing `save_module` interface.

## Authority and invariants

- Module name validation and target selection stay unchanged.
- An existing `<name>.luau` wins over `<name>.lua`. An existing Lua file keeps
  its suffix. A new module uses `<name>.luau`.
- The writer resolves the final symlink before it locks or writes.
- A process mutex and a sibling file lock serialize Bootty writers.
- The replacement file is exclusive and lives in the resolved target directory.
- The writer copies the exact source bytes. It does not parse or normalize Lua.
- An existing Unix target keeps its permission bits. A new Unix target uses
  mode `0666` subject to the process umask, which matches normal file creation.
- The writer flushes and synchronizes the complete replacement before it closes
  the temporary handle and publishes the file.
- Unix uses one same-filesystem rename. Windows uses `ReplaceFileW` for an
  existing file and `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` for a new file.
- Unix synchronizes the parent directory after replacement.

## Failure and recovery

A failure before replacement leaves the previous bytes and permission bits
active. Temporary-file cleanup is best effort.

A parent-directory sync failure happens after replacement. `save_module`
therefore returns the committed path. Returning an error would falsely tell the
editor that its source was not saved.

The writer serializes Bootty saves. It uses last-writer-wins behavior. External
editors do not use the lease. Atomic replacement still prevents torn reads.

## Goal boundary

This decision protects durable user source in Goal 1. It does not define local
module identity, source validation, extension manifests, runtime generations,
worker replacement, or atomic runtime publication. Goal 4 owns those decisions.
`reset_module` behavior also stays unchanged.

## Rejected alternatives

- Direct `fs::write` can truncate the only customized source.
- A generic atomic-file module would mix unrelated formats and failure rules.
- Parsing Lua before persistence would move runtime validation into storage.
- A generation transaction is broader than durable source safety.
