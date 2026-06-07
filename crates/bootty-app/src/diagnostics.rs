use std::{
    fs::File,
    io::Write,
    time::{Duration, Instant},
};

use crate::{
    config::BoottyConfig, renderer::RendererMetrics, strings::csv_field, terminal::DrainStats,
};

pub const STATUS_METRICS_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default)]
pub struct StatusMetrics {
    pub drain: DrainStats,
    pub renderer: RendererMetrics,
    pub cols: u16,
    pub rows: u16,
}

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
            sample.elapsed_ms,
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
    pub elapsed_ms: u128,
    pub selected_session: Option<&'a str>,
    pub cols: u16,
    pub rows: u16,
    pub pending_pty_bytes: usize,
    pub drain_bytes: usize,
    pub drain_elapsed_us: u64,
    pub text_runs: usize,
    pub last_error: Option<&'a str>,
}

pub fn should_sample_status_metrics(elapsed: Duration) -> bool {
    elapsed >= STATUS_METRICS_SAMPLE_INTERVAL
}

pub fn us_to_ms(us: u64) -> f32 {
    us as f32 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_metrics_sample_at_four_hz() {
        assert!(!should_sample_status_metrics(
            STATUS_METRICS_SAMPLE_INTERVAL - Duration::from_millis(1)
        ));
        assert!(should_sample_status_metrics(STATUS_METRICS_SAMPLE_INTERVAL));
    }
}
