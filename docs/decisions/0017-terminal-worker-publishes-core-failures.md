# Terminal worker publishes core failures

Status: accepted on 2026-08-13.

Implementation: complete.

`TerminalSession` reports every fallible terminal-core or PTY operation through
one existing failure route. A queued one-way command stays asynchronous. Its
worker failure becomes visible through the next session health check.

## Authority and invariants

- `TerminalEngine` owns terminal-core operations and their source errors.
- `TerminalWorker` owns asynchronous execution and operation classification.
- `TerminalSession` owns one bounded latest worker-health slot.
- One private typed failure retains the operation and source text.
- The latest asynchronous failure replaces an older unread failure.
- A health read consumes one failure.
- `send_command`, `extract_frame`, and `child_exited` check worker health.
- Public `TerminalRuntime` and `TerminalFrameSource` result interfaces do not
  change.
- Hot input operations return after enqueue. They do not wait for worker
  acknowledgement.

## Failure classification

A command-channel failure, PTY master resize failure, or response-channel
disconnect returns synchronously from its initiating call.

The worker records asynchronous failures from terminal resize, live config,
input encoding, wheel mouse-mode inspection, copy-mode entry, selection
mutation, synchronized-output inspection, frame extraction, frame publication,
and PTY output writes. A synchronized-output inspection failure uses `false` as
the safe scheduling fallback only after it records the health failure.

Format selection, copy-mode action, search, and explicit mouse-tracking queries
return their engine error through their existing command response. They do not
also poison worker health. A dropped response receiver means that the caller
left. It is not a worker failure.

No selection, no search match, disabled mouse tracking, and no reported working
directory remain valid absence values. A disconnected terminal side-effect
consumer disables forwarding. Diagnostic drain-statistics lock failures and
child cleanup failures remain best effort. A PTY reader-channel disconnect is
normal EOF after the reader exits. It is lifecycle state, not a failed PTY
operation.

## Failure and recovery

Worker health prevents a stale frame, lost PTY write, or rejected one-way
terminal operation from appearing healthy. The next session health check returns
the operation-scoped error once. A later successful operation does not erase an
unread failure.

This decision does not restart the worker or retry the operation. It does not
roll back accepted app config. A caller can rebuild or close the runtime through
its existing owner after it observes the error.

## Rejected alternatives

- Per-command acknowledgements add latency to the hot input path.
- A public error hierarchy exposes worker implementation detail.
- An error history, retry loop, fatal latch, or telemetry system adds lifecycle
  policy without a current product need.
- A broad engine trait exists only to inject test failures.
- Silent `false`, stale-frame, or empty-output fallbacks report false success.

## Compatibility

Command ordering, input latency, drain budgets, repaint timing, latest-frame
publication, public method signatures, and config acceptance from ADR 0012 stay
unchanged.
