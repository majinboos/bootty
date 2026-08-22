pub mod benchmark_trace;
pub mod frame_source;
pub mod latency;
pub mod perf;
mod pty_backlog;
pub mod scheduler;
pub mod terminal_launch;
pub mod terminal_session;
pub mod terminfo;

pub use benchmark_trace::{BenchmarkTrace, TraceValue};
pub use pty_backlog::{
    OutputBacklog, PtyBacklog, drain_output_backlog, drain_output_backlog_with_limits,
    drain_pty_backlog,
};
pub use terminal_session::{
    DrainStats, SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
};

pub mod geometry {
    pub use bootty_surface::geometry::*;
}

pub mod terminal {
    pub use crate::terminal_session::{DrainStats, TerminalSession};
    pub use bootty_terminal::terminal::*;
}
