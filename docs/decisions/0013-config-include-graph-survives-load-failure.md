# Config include graph survives load failure

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-config` uses one include traversal for config loading and dependency
discovery. The traversal records each resolved file before it reads or parses
that file. A load failure keeps the discovered dependency snapshot.

## Authority and invariants

- `bootty-config` owns include parsing, relative path resolution, cycle
  detection, merge order, optional-file behavior, and dependency discovery.
- One traversal produces the merged TOML document and the dependency snapshot.
- The root file and every reached include stay in the snapshot after a read,
  parse, include-shape, or cycle failure.
- A missing optional include stays in the snapshot. Creating it can trigger a
  reload.
- A missing required include stays in the snapshot and remains a load error.
- A failed reload keeps the last good product config and its visible error.
- Every reload attempt refreshes the dependency snapshot. Success is not
  required.
- Polling known files stays metadata-only. Bootty rebuilds the dependency graph
  only after a known file changes or an explicit reload attempt.

## Failure and recovery

A bad root or included file rejects the candidate config. The prior config
remains authoritative. The watcher keeps the partial dependency graph from the
failed candidate. A later edit to any discovered file triggers another reload.
Bootty does not require a parent-file edit to recover.

If parsing fails before an include declaration can be read, Bootty can retain
only the path that failed and the ancestors already discovered. This is enough
to observe the edit that can make further discovery possible.

## Rejected alternatives

- Falling back to the root path loses the failing child and can prevent
  recovery.
- Separate loader and watcher traversals duplicate include semantics and drift.
- Watching a directory tree adds unrelated events and does not define include
  ownership.
- Retrying a failed load on every frame wastes work and hides the dependency
  defect.

## Migration consequences

Config syntax, merge order, error text, and the 250 ms polling interval do not
change. Manual reload, hot reload, settings writeback, and command reload use
the same dependency refresh rule.
