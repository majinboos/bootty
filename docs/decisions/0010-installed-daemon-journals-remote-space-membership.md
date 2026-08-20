# Installed daemon journals remote Space membership

Status: accepted on 2026-08-13.

Implementation: complete.

The installed `bootty-daemon` records a remote Space membership operation before
it asks the backend to create, rename, or ditch a session. The daemon never
retries an ambiguous backend command. It resolves the durable operation from the
next authoritative backend snapshot.

## Authority and invariants

- `bootty-mux::membership` owns the backend-neutral membership operation,
  validation, and authoritative-snapshot classifier.
- `Catalog` is the only SQLite authority for installed-daemon remote Spaces and
  their session membership.
- `WorkspaceRepository` remains the only SQLite authority for the local workspace.
- The local and installed-daemon journals protect different host-owned commits.
  They share the backend operation value and classifier only.
- One remote Space can have one pending membership operation. Another remote
  Space remains independent.
- A per-backend file lease serializes snapshot, reconciliation, and execution
  across daemon processes. Session identity is shared by all Spaces on that
  backend. The lease starts before the backend snapshot.
- A journal commit happens before the backend call.
- A completed catalog membership update and journal deletion use one SQLite
  transaction.
- Reconciliation happens before dead-session cleanup and snapshot filtering.
- Malformed journal state is a catalog corruption error. The daemon does not
  repair or discard it silently.

## Failure and recovery

A journal failure prevents the backend call. An ambiguous backend error keeps
the journal row and reports that recovery is pending. A catalog commit failure
after backend success also keeps the row. The next snapshot applies the
operation only when the backend state proves its effect. Otherwise, it discards
the operation. Recovery never executes the backend command again.

## Schema and compatibility

The daemon adds `remote_space_pending_membership_operations`. The Space ID is
the primary key. The row stores the operation, session ID, old name, and new
name. This is an additive internal schema change. Existing remote Space rows,
session order, command grammar, JSON shape, and catalog protocol version 3 stay
unchanged.

## Rejected alternatives

- The daemon does not copy `WorkspaceRepository` or the local workspace schema.
- The daemon does not copy the membership classifier.
- The daemon does not use a generic storage trait or public transaction closure.
- Recovery does not retry a backend mutation.
- This slice does not add idempotency tokens or change the remote protocol.

## Migration consequences

Existing catalogs open without data movement. The journal table is created with
`CREATE TABLE IF NOT EXISTS`. Existing production and development identity
namespaces remain separate. Per-backend lease files live beside the catalog in a
catalog-specific lock directory. Stale lease files hold no ownership after the
process closes them.
