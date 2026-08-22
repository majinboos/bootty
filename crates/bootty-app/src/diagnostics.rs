use std::{fs::File, io::Write, time::Instant};

use crate::{config::BoottyConfig, strings::csv_field};

pub use bootty_runtime::latency::{start as latency_start, trace_phase, trace_slow};

pub struct StabilityTrace {
    pub started_at: Instant,
    file: File,
}

impl StabilityTrace {
    pub fn from_config(config: &BoottyConfig) -> Option<Self> {
        let path = config
            .diagnostics
            .stability_trace
            .clone()
            .or_else(|| std::env::var_os("BOOTTY_STABILITY_TRACE").map(Into::into))?;
        let mut file = File::create(path).ok()?;
        writeln!(
            file,
            "elapsed_ms,selected_session,cols,rows,pending_pty_bytes,drain_bytes,drain_elapsed_us,text_runs,last_error"
        )
        .ok()?;
        Some(Self {
            started_at: Instant::now(),
            file,
        })
    }

    pub fn record(&mut self, sample: StabilityTraceSample<'_>) {
        let _ = writeln!(
            self.file,
            "{},{},{},{},{},{},{},{},{}",
            self.started_at.elapsed().as_millis(),
            csv_field(sample.selected_session.unwrap_or("")),
            sample.cols,
            sample.rows,
            sample.pending_pty_bytes,
            sample.drain_bytes,
            sample.drain_elapsed_us,
            sample.text_runs,
            csv_field(sample.last_error.unwrap_or(""))
        );
    }
}

pub struct StabilityTraceSample<'a> {
    pub selected_session: Option<&'a str>,
    pub cols: u16,
    pub rows: u16,
    pub pending_pty_bytes: usize,
    pub drain_bytes: usize,
    pub drain_elapsed_us: u64,
    pub text_runs: usize,
    pub last_error: Option<&'a str>,
}
