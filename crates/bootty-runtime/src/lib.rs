pub mod benchmark_trace;
pub mod latency;
pub mod perf;
pub mod render_source;
pub mod scheduler;
pub mod terminal_session;
pub mod terminfo;

pub use benchmark_trace::{BenchmarkTrace, TraceValue};
pub use terminal_session::{
    DrainStats, PtyBacklog, SessionLaunchConfig, TerminalOutputObservation,
    TerminalRuntimeObservations, TerminalSession, TerminalSessionConfig, drain_pty_backlog,
};

pub mod geometry {
    pub use bootty_surface::geometry::*;
}

pub mod terminal {
    pub use crate::terminal_session::{DrainStats, TerminalSession};
    pub use bootty_terminal::terminal::*;
}
