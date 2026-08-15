# Render source is frame-only

Status: accepted on 2026-08-13.

Implementation: complete.

`TerminalFrameSource` owns the four operations required to construct a terminal
frame. `bootty-mux::TerminalRuntime` owns pane lifecycle, input, viewport,
selection, search, copy mode, and terminal configuration.

## Authority and invariants

- `TerminalFrameSource` requires display scale, render cell metrics, resize, and
  frame extraction. It supplies no successful default behavior.
- `TerminalWidget` accepts only `dyn TerminalFrameSource`.
- `TerminalRuntime` requires every user interaction operation. A new runtime
  cannot compile while silently dropping input or selection behavior.
- `TerminalSession`, `RmuxNativeTerminal`, `StartingNativeTerminal`, and
  `BackendPaneTerminal` implement their interaction behavior explicitly.
- `StartingNativeTerminal` keeps the current startup contract. It queues scroll,
  copy-mode entry, and selection mutations. Queries return the current startup
  result until the terminal is ready.
- `IdleTerminalRuntime` states every intentional no-op explicitly.
- App orchestration calls pane interaction through `TerminalRuntime`. It does not
  import an interaction interface from the renderer module.

## Failure and recovery

Existing errors cross the same runtime boundary. No interaction error becomes a
successful no-op. Startup keeps its current queue and replay order.

## Rejected alternatives

- One broad render source gives the renderer false ownership of pane mutation.
- Successful trait defaults let incomplete runtimes compile.
- Capability traits create many shallow interfaces before another capability
  model exists.
- A second session DTO duplicates the live pane runtime.

## Migration consequences

Render-only fakes implement four frame operations. Runtime fakes must state every
interaction. The mux runtime becomes the one polymorphic pane behavior boundary.
