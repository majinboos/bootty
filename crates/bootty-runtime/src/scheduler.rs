use std::time::Duration;

pub const INPUT_REFRESH_INTERVAL: Duration = Duration::ZERO;
pub const BUSY_REFRESH_INTERVAL: Duration = Duration::ZERO;
pub const CURSOR_BLINK_REFRESH_INTERVAL: Duration = Duration::from_millis(33);
pub const CHROME_REFRESH_INTERVAL: Duration = Duration::from_millis(900);

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
pub struct RepaintRecommendation {
    pub after: Duration,
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
    pub fn recommend(self, signal: RepaintSignal) -> RepaintRecommendation {
        let after = if signal.has_input() {
            self.input_after
        } else if signal.has_backlog_or_expensive_drain() {
            self.busy_after
        } else if signal.has_blinking_cursor() {
            CURSOR_BLINK_REFRESH_INTERVAL
        } else {
            self.chrome_after
        };

        RepaintRecommendation { after }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_terminal_repaints_sooner_than_idle_terminal() {
        let scheduler = RepaintScheduler::default();
        let idle = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        });
        let active = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 1,
        });

        assert!(matches!(
            (active.after, idle.after),
            (active_after, idle_after) if active_after < idle_after
        ));
    }

    #[test]
    fn input_activity_counts_as_active() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 1,
        });

        assert_eq!(recommendation.after, Duration::ZERO);
    }

    #[test]
    fn chrome_refresh_interval_documents_session_refresh_cadence() {
        assert_eq!(CHROME_REFRESH_INTERVAL, Duration::from_millis(900));
    }

    #[test]
    fn stale_dirty_rows_do_not_force_active_repaint_rate() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 42,
            cursor_blinking: false,
            input_commands: 0,
        });

        assert_eq!(recommendation.after, CHROME_REFRESH_INTERVAL);
    }

    #[test]
    fn blinking_cursor_uses_bounded_refresh_interval() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: true,
            input_commands: 0,
        });

        assert_eq!(recommendation.after, CURSOR_BLINK_REFRESH_INTERVAL);
    }

    #[test]
    fn idle_terminal_does_not_poll_at_sixty_fps() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        });

        assert!(matches!(
            recommendation.after,
            after if after >= CHROME_REFRESH_INTERVAL
        ));
    }

    #[test]
    fn terminal_output_relies_on_worker_wakeup_not_active_polling() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 1024,
            drain_elapsed_us: 50,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        });

        assert_eq!(recommendation.after, CHROME_REFRESH_INTERVAL);
    }

    #[test]
    fn pending_pty_backlog_keeps_interactive_cadence() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 4096,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        });

        assert_eq!(recommendation.after, Duration::ZERO);
    }

    #[test]
    fn expensive_pty_parse_keeps_interactive_cadence() {
        let scheduler = RepaintScheduler::default();
        let recommendation = scheduler.recommend(RepaintSignal {
            drained_bytes: 64,
            drain_elapsed_us: 16_000,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        });

        assert_eq!(recommendation.after, Duration::ZERO);
    }
}
