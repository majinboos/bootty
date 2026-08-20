# Remote Space summary has one wire owner

Status: accepted on 2026-08-13.

Implementation: complete.

`bootty-mux-model::RemoteSpaceSummary` is the one serialized remote Space
summary used by the app and the installed daemon.

## Authority and invariants

- `bootty-mux-model` owns the exact JSON field names and backend token values.
- The wire fields remain `catalog_version`, `id`, `name`, and `backend`.
- The catalog version remains `3`.
- Backend tokens remain `rmux`, `tmux`, and `zellij`.
- `bootty-daemon::Catalog` owns catalog persistence and backend execution.
- `bootty-app::remote_catalog` owns client version validation and SSH transport.
- The hidden daemon command grammar and error text do not change.

## Simplification

The app and daemon do not define parallel summary structs. The daemon keeps its
local `Backend` operation type because it owns parsing and backend execution.
Only the serialized summary is shared.

## Rejected alternatives

- A second app DTO can drift from daemon JSON.
- Moving backend execution into the value crate would reverse the dependency
  direction.
- A new remote-only backend DTO would add conversion code without changing the
  existing wire contract.
- A protocol version change is unnecessary because the serialized bytes do not
  change.
