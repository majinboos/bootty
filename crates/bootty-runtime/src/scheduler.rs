use std::time::Duration;

const INPUT_REFRESH_INTERVAL: Duration = Duration::ZERO;
const BUSY_REFRESH_INTERVAL: Duration = Duration::ZERO;
/// Cadence of the cursor's fade animation, and with it the app's idle frame rate: a repaint
/// rebuilds the whole window, so this constant sets what an idle focused window costs. 20 Hz keeps
/// the fade smooth at half the frames 30 Hz asked for; `cursor.blink = false` opts out entirely.
pub const CURSOR_BLINK_REFRESH_INTERVAL: Duration = Duration::from_millis(50);
const CHROME_REFRESH_INTERVAL: Duration = Duration::from_millis(900);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaintSignal {
    pub drained_bytes: usize,
    pub drain_elapsed_us: u64,
    pub pending_bytes: usize,
    pub dirty_rows: usize,
    pub cursor_blinking: bool,
    pub input_commands: usize,
}

impl RepaintSignal {
    fn has_input(self) -> bool {
        self.input_commands > 0
    }

    fn has_backlog_or_expensive_drain(self) -> bool {
        self.pending_bytes > 0 || self.drain_elapsed_us >= 1_000
    }

    fn has_blinking_cursor(self) -> bool {
        self.cursor_blinking
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepaintScheduler {
    input_after: Duration,
    busy_after: Duration,
    chrome_after: Duration,
}

impl Default for RepaintScheduler {
    fn default() -> Self {
        Self {
            input_after: INPUT_REFRESH_INTERVAL,
            busy_after: BUSY_REFRESH_INTERVAL,
            // Terminal output publishes wake egui directly. Periodic repainting
            // is only a chrome/session-refresh safety net while idle.
            chrome_after: CHROME_REFRESH_INTERVAL,
        }
    }
}

impl RepaintScheduler {
    pub fn recommend(self, signal: RepaintSignal) -> Duration {
        if signal.has_input() {
            self.input_after
        } else if signal.has_backlog_or_expensive_drain() {
            self.busy_after
        } else if signal.has_blinking_cursor() {
            CURSOR_BLINK_REFRESH_INTERVAL
        } else {
            self.chrome_after
        }
    }
}
