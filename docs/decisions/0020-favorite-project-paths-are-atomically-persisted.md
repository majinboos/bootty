# Favorite project paths are atomically persisted

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-mux::project::favorite_paths` owns persistence for the favorite project
path file. The public toggle API keeps its existing `io::Result<bool>` contract.

## Authority and invariants

- The writer resolves the final symlink before it opens the lock or reads data.
- A process mutex and a sibling file lock serialize writers for the resolved
  target. The writer rereads the target while it owns both locks.
- The replacement file is exclusive and lives in the target directory.
- Unix replacement preserves the existing mode. A new target uses mode `0600`.
- The complete replacement is flushed and synchronized before publication.
- Unix uses one same-filesystem rename. Windows uses `ReplaceFileW` for an
  existing target and `MoveFileExW` with `MOVEFILE_WRITE_THROUGH` for a new one.
- Unix synchronizes the parent directory after replacement.
- A parent-directory sync failure occurs after the replacement is committed.
  The toggle therefore returns the committed boolean result.

## Failure and recovery

A failure before replacement leaves the previous target active. Temporary-file
cleanup is best effort. A replacement failure returns the original `io::Error`.

The writer does not expose a generic persistence abstraction. Other persistence
owners have separate write contracts.

## Rejected alternatives

- Direct `fs::write` can truncate the only valid file and loses concurrent updates.
- Reading before the lock permits one Bootty writer to overwrite another writer's
  change.
- A temporary file outside the target directory cannot be atomically renamed on
  every filesystem.
- Delete-then-rename creates a crash window with no favorites file.

## Migration consequences

Existing favorite files keep their bytes except for the requested toggle and keep
their Unix permission bits. New files are private by default. The Windows branch
has a compile proof; runtime replacement coverage belongs on a Windows runner.
