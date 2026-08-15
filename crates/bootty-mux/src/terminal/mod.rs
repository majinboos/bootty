mod attach;
mod pane;
mod startup;

pub use pane::{
    BackendPanePolicy, BackendPaneTerminal as ActiveTerminal, MuxPaneTarget,
    PaneLayoutResizeRequest, PaneStartRequest, ScopedMuxPaneTarget, TerminalRuntime,
    decode_scoped_pane_id, encode_scoped_pane_id,
};

pub use attach::{AttachLaunch, resolve_launch_program, start_attach_terminal};
pub use startup::StartingNativeTerminal;
