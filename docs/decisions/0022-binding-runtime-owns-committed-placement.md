# BindingRuntime owns committed placement

Status: accepted on 2026-08-13.

Implementation: complete.

One `BindingRuntime` owns the committed placement policy and the realized mux
operation for one backend binding.

## Authority and invariants

- `WorkspaceRepository` remains the durable authority for binding placement.
- `WorkspaceRuntime` owns the live collection of bindings and coordinates
  commit-before-publish workspace operations.
- `BindingRuntime` stores the committed `SpaceMuxOverride` and the realized
  `MuxBindingConfig` for its exact `MuxScope`.
- `BindingRuntime` constructs itself from one validated `WorkspaceBinding`.
  Construction owns placement realization, availability, restore selection,
  session metadata, and persisted-session restore.
- A profile reload re-realizes the binding from its committed placement.
- A rebuild preserves session ordering, session-name metadata, and pending
  generated names.
- A Space placement edit changes the live binding only after the repository
  commit succeeds.
- A pending membership journal still prevents a placement change.

## Simplification

`WorkspaceRuntime` does not keep a second placement map. Callers do not
decompose a binding into placement, label, session stores, availability, and
selection before reconstruction.

The implementation reuses `SpaceMuxOverride`. It adds no placement DTO, storage
trait, runtime factory, or generic rebuild interface.

## Failure and recovery

A repository failure leaves the prior placement and realized mux operation
active. A missing SSH profile remains an availability error on the exact
binding. Profile reload stays deferred while membership recovery is pending.

The backend remains authoritative for live processes and backend-native layout.
This decision changes host-owned binding policy only.

## Rejected alternatives

- A separate `HashMap<MuxScope, BindingPlacement>` duplicates live binding
  policy and requires manual synchronization after open, insert, delete, and
  update.
- Caller-managed reconstruction repeats ordering rules and can drop binding
  state during Space edits or profile reload.
- A new placement value would duplicate `SpaceMuxOverride`.
- Merging persistence candidates with mux execution would mix durable metadata
  authority with backend authority.
