pub mod protocol;

mod backend;
#[cfg(feature = "app")]
mod control;
mod provider;

#[cfg(unix)]
pub use backend::clear_dead_socket;
pub use backend::{DefaultTmuxRunner, TmuxBackend, tmux_server_exited};
#[cfg(feature = "app")]
pub use backend::{TmuxPanePolicy, local_server_args, tmux_capabilities};
#[cfg(feature = "app")]
pub use control::TmuxControlRunner;
pub use protocol::{
    TmuxClientSessionChangedNotification, TmuxControlNotification, TmuxControlParser,
    TmuxIdNameNotification, TmuxLayoutChangeNotification, TmuxOutputNotification, TmuxParseError,
    TmuxSessionChangedNotification, TmuxWindowPaneChangedNotification,
};
pub use provider::link;
