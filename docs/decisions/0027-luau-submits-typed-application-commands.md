# Luau submits typed application commands

Status: accepted on 2026-08-13.

Implementation: complete.

The bounded Luau host submits `CommandInvocation` values through the existing
application command channel. It does not execute application commands or mutate
topology directly.

## Authority and invariants

- `AppState` remains the command policy and execution owner.
- `bootty.commands.invoke` accepts the serialized `CommandInvocation` shape
  without a caller field.
- The host binds every nested invocation to `Caller::Luau`.
- A nested invocation inherits the exact deadline and cancellation token from
  the active extension invocation.
- The extension worker waits only until that deadline.
- The result is the existing serialized `CommandOutcome`.
- The control plane keeps transport and event state only.

## Failure behavior

- Queue overload and application shutdown return typed failed outcomes.
- Cancellation returns a typed `cancelled` outcome.
- Deadline expiry returns a typed `deadline_exceeded` outcome and cancels the
  inherited token.
- A closed response channel returns a typed `shutdown` outcome.
- Calling `bootty.commands.invoke` outside an extension command handler is a
  Luau runtime error.

## Rejected alternatives

- A second Luau command registry would duplicate command meaning.
- Routing Luau through the local socket would add transport work inside the
  owner process.
- Putting the application command sender in `ControlPlane` would mix execution
  with transport state.
- A fresh deadline or cancellation token would let nested work outlive its
  parent invocation.
