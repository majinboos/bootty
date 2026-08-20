# TerminalSession owns construction and delivered state

Status: accepted on 2026-08-13.

Implementation: complete.

`TerminalSession` owns a spawned PTY child from the first successful process
spawn. It publishes cached runtime state only after synchronous delivery
succeeds.

## Authority and invariants

- A private pending-child owner takes the child immediately after
  `spawn_command` succeeds.
- The pending owner covers PTY reader setup, PTY writer setup, benchmark trace
  creation, and terminal worker startup.
- A successful constructor transfers the child into `TerminalSession`.
- Any earlier return kills the child and starts its reap.
- `TerminalSession` remains the process owner after construction.
- Geometry, display scale, and render cell metrics describe delivered state.
  They do not describe requested state.
- A setter updates its cached value only after every synchronous delivery step
  succeeds.

## Failure and retry

A failed resize can occur after the worker receives the terminal geometry but
before the PTY master accepts the size. The session keeps the prior delivered
geometry. An identical retry sends the worker command again and retries the PTY
resize.

A failed display-scale or render-cell command keeps the prior delivered value.
An identical retry sends the command again. The session does not report success
from a cache value that no live worker accepted.

Queued input and asynchronous worker-health behavior do not change. ADR 0017
still owns asynchronous terminal-core and PTY failure reporting.

## Rejected alternatives

- Relying on PTY handle drop to stop a child makes process ownership implicit.
- Updating a cache before delivery turns a failed request into false success on
  retry.
- Automatic retries hide failure timing and can duplicate input or process
  operations.
- A generic PTY or engine trait would exist only for failure injection.
- A public construction hook would expose test control in the runtime API.
