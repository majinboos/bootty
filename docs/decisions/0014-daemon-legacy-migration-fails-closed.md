# Daemon legacy migration fails closed before marking completion

Status: accepted on 2026-08-13.

Implementation: complete.

The installed daemon validates one complete same-identity legacy import before
it writes destination rows. It commits imported rows and the migration marker in
one destination transaction.

## Authority and invariants

- `bootty-config` owns product TOML, include semantics, defaults, and validation.
- `bootty-identity` selects the matching Production or Development legacy path.
- `Catalog` owns the destination SQLite schema and migration marker.
- One private daemon importer owns legacy SQLite compatibility and produces one
  complete in-memory import plan.
- The importer never falls back across application identities.
- `BOOTTY_DAEMON_STATE` changes only the destination path.
- An inherited backend comes from the resolved `BoottyConfig`.
- A missing or `native` effective backend does not produce an installed-daemon
  Space.
- A local binding has an explicit local placement, or it inherits a config with
  no remote placement.
- An unknown backend, malformed placement, ambiguous local binding, malformed
  schema, or invalid row is a migration failure.

## Failure and recovery

The destination state is checked before legacy inputs are read. An existing
migration marker does nothing. Existing destination rows remain authoritative;
Bootty writes the marker and ignores stale legacy inputs. A missing matching
legacy database writes the marker and imports nothing.

When an empty destination has a matching legacy database, every config, include,
SQLite, schema, row, and session-order error is fatal. No destination row or
marker is written. A corrected source retries on the next open.

The complete import plan and marker commit in one destination transaction. A
row or marker failure rolls back the whole import. Concurrent opens recheck the
marker and destination state inside the transaction.

## Rejected alternatives

- A fallback `remote = true` converts corruption into a false migration policy.
- A daemon-owned TOML loader duplicates `bootty-config` semantics.
- Selecting the first binding by database ID gives storage order product meaning.
- Marking an incompatible source migrated prevents repair and retry.
- A generic repository trait adds a second storage model without another owner.

## Migration consequences

Production paths, Development paths, destination schema, catalog version, CLI
JSON, and remote protocol do not change. Missing legacy state keeps the current
one-time migration behavior. Present invalid legacy state now reports an error
and remains retryable.
