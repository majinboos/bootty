# The control instance lease owns one generation

Status: accepted on 2026-08-13.

Implementation: complete.

Each Bootty application identity has one discoverable control server. One
short-lived cross-process lease serializes descriptor inspection, stale cleanup,
claim, publication, and release. Each server lifetime has its own generation and
endpoint.

## Authority and invariants

- `ApplicationIdentity` selects the Production or Development control namespace.
- The private control instance lease owns `control.json`, its claim lock, stale
  cleanup, endpoint naming, descriptor publication, and release.
- One server generation owns one unique endpoint.
- A descriptor identifies one exact generation, process start time, and endpoint.
- The server binds the endpoint and applies owner-only permissions before it
  publishes the descriptor.
- Discovery validates the protocol, identity, endpoint namespace, process, and
  process start time while it holds the lease.
- Cleanup removes only the endpoint named by the observed descriptor.
- Cleanup removes `control.json` only when its current bytes still identify that
  same generation.
- `ControlServer` releases only its own generation.
- The cross-process lease is not held for the server lifetime.
- Production and Development use separate descriptor, lock, and endpoint
  namespaces.

## Failure and recovery

A live descriptor rejects a second server for the same application identity. A
malformed or dead descriptor is stale. The next lease holder removes that exact
stale generation before it claims a new generation.

A startup failure removes only the candidate endpoint and any descriptor that
still identifies that candidate. A crashed server can leave one descriptor and
one generation endpoint. The next lease holder can remove them after it proves
that descriptor stale. An old server shutdown cannot unlink a replacement
generation.

## Rejected alternatives

- One shared endpoint lets stale cleanup unlink a newer live listener.
- PID-only ownership is unsafe after PID reuse.
- A server-lifetime lock prevents normal discovery and crash recovery.
- A heartbeat adds another liveness model when process identity and start time
  already exist.
- A multi-instance registry conflicts with one singleton per application
  identity.
- A generic lock interface adds no second implementation.

## Compatibility and proof

The public descriptor fields, JSON-RPC payloads, command semantics, same-user
checks, request limits, and CLI exit behavior remain compatible. Endpoint paths
are private owner-local state.

Public process contracts prove stale cleanup cannot remove a replacement
generation, old-server shutdown cannot remove a replacement, malformed state
recovers, a live duplicate is rejected, and Production cleanup cannot touch
Development state.
