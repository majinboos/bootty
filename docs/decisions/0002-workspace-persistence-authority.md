# One authority owns workspace persistence

Status: accepted on 2026-08-13.

Implementation: complete.

`WorkspaceRepository` owns all SQLite access for persistent Space and
backend-binding state. `WorkspaceRuntime` owns one complete validated live
`WorkspaceSnapshot`. This removes independent SQLite writers and prevents a
persistence failure from appearing as an empty workspace.

## Authority and invariants

- `WorkspaceRepository` loads and commits persistent Space and binding facts.
- `WorkspaceRuntime` owns the committed live snapshot.
- The owner-local headless `RemoteSpaceRuntime` owns one loaded binding value for
  one remote catalog request. It commits only through `WorkspaceRepository`.
- `WorkspaceSnapshot` is a transfer and candidate value. `WorkspaceRepository`
  retains persistence access state after load. It does not retain a mutable
  workspace model.
- A backend binding remains authoritative for live processes and backend-native
  topology.
- A backend-native session ID is meaningful only within its backend binding.
- Worktree session identity remains the canonical worktree directory path.
- A persisted restore selection is an opaque backend reference. Offline backend
  state does not make it corrupt.
- Session order, group membership, restore references, and name metadata must
  reference a binding in the same Space.
- A fresh database creates the current valid default Space and binding.
- An existing database must produce one complete valid snapshot.
- Focused session-order and session-name values contain no database path,
  connection, transaction, or save operation.
- `AppState` cannot construct a persistence store or derive the SQLite path.
- Space appearance is Space-scoped. Backend and remote placement are binding-scoped.
  A placement update names one exact `MuxScope` and never selects the first binding.

## Failure and recovery

Loads reject corruption, invalid values, broken relationships, incomplete state,
and unsupported revisions with a typed workspace persistence error.

Writes use commit-before-publish ordering. The repository validates a candidate
and commits one private SQLite transaction. `WorkspaceRuntime` replaces its live
snapshot only after the commit succeeds. A failed commit keeps the prior runtime
snapshot and database state active.

Backend mutations cannot join a SQLite transaction. Binding membership operations
first record one durable binding-scoped intent. An ambiguous backend failure keeps
the intent. Backend success commits the metadata and clears the intent in one
transaction. A later authoritative snapshot resolves an intent left by a crash,
backend error, or metadata failure. Bootty reports a typed partial-completion
failure until recovery. GUI and owner-local headless flows use the same workspace
journal. The installed remote daemon uses its own catalog journal under the same
journal-before-backend rule. See
[`0010-installed-daemon-journals-remote-space-membership.md`](0010-installed-daemon-journals-remote-space-membership.md).
Create and rename intents retain the backend name, the Bootty display name, and
the explicit-name state.

The binding backend identity used for the operation remains active until recovery
finishes. The Space editor rejects a placement change during recovery. SSH profile
reload defers and later retries the affected binding rebuild.

GUI membership commands keep speculative metadata in memory. Their worker returns
an authoritative snapshot after create, rename, and ditch. Authoritative command
success is published only after the derived workspace metadata commits.

## Rejected alternatives

- Independent `SessionOrderStore` and `SessionNameStore` writers split authority
  and can hide database failures.
- A generic storage trait adds an abstraction without a second storage system.
- A public transaction closure leaks persistence workflow to callers.
- A broad `WorkspaceRuntime` forwarding facade is shallow unless each operation
  owns validation, commit ordering, and reconciliation.
- Repairing unexplained invalid data into defaults can destroy user state.

## Migration consequences

The current SQLite schema remains compatible.
Schema revision 2 adds the pending binding membership-operation journal.
Schema revision 3 adds durable display-name and explicit-name intent to that journal.
Session ordering and naming become in-memory domain values.
Their SQL moves behind `WorkspaceRepository`.
GUI durable mutations pass through `WorkspaceRuntime`. The owner-local headless
projection uses its focused `RemoteSpaceRuntime` and the same repository operations.
Callers handle typed failures at the UI boundary.

## Proof

Fast public contracts must prove valid reopen, invalid snapshot rejection,
binding-scope isolation, commit followed by reopen, and a real write failure that
preserves the prior committed live snapshot. The full repository gate must pass.
