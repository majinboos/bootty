use std::time::{Duration, Instant};

use super::BindingRuntime;

#[derive(Clone, Copy, Debug)]
struct RemoteReattach {
    retry_at: Instant,
    attempts: u32,
    started: bool,
}

impl RemoteReattach {
    const FIRST_DELAY: Duration = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(30);
    const STABLE_AFTER: Duration = Duration::from_secs(5);

    fn after_failure(previous: Option<Self>, attached_for: Option<Duration>, now: Instant) -> Self {
        let established = attached_for.is_some_and(|elapsed| elapsed >= Self::STABLE_AFTER);
        let attempts = match previous {
            Some(previous) if !established => previous.attempts.saturating_add(1),
            _ => 1,
        };
        Self {
            retry_at: now + Self::delay(attempts),
            attempts,
            started: false,
        }
    }

    fn due(self, now: Instant) -> bool {
        !self.started && now >= self.retry_at
    }

    fn delay(attempts: u32) -> Duration {
        Self::FIRST_DELAY
            .saturating_mul(1u32 << attempts.saturating_sub(1).min(8))
            .min(Self::MAX_DELAY)
    }
}

#[derive(Default)]
pub(super) struct BindingReconnect {
    pending: Option<RemoteReattach>,
    attach_started: Option<Instant>,
}

impl BindingRuntime {
    /// Returns `true` when the caller must close the local attach pane.
    pub(super) fn handle_attach_client_exit(&mut self, now: Instant) -> bool {
        let Some(remote) = self.multiplexer.remote.as_ref() else {
            return true;
        };
        if self
            .reconnect
            .pending
            .is_some_and(|reattach| !reattach.started)
        {
            return false;
        }
        let attached_for = self
            .reconnect
            .attach_started
            .map(|started| now.saturating_duration_since(started));
        let reattach = RemoteReattach::after_failure(self.reconnect.pending, attached_for, now);
        let error = format!(
            "lost the connection to {}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.mux.set_availability_error(Some(error));
        self.reconnect.pending = Some(reattach);
        false
    }

    pub(super) fn handle_attach_start_failure(&mut self, now: Instant, detail: &str) {
        let Some(remote) = self.multiplexer.remote.as_ref() else {
            return;
        };
        let reattach = RemoteReattach::after_failure(self.reconnect.pending, None, now);
        let error = format!(
            "could not connect to {}: {detail}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.mux.set_availability_error(Some(error));
        self.reconnect.pending = Some(reattach);
    }

    pub(super) fn resolve_attach_exit_after_refresh(&mut self, refresh_applied: bool) {
        if !refresh_applied || self.reconnect.pending.is_none() || !self.mux.sessions().is_empty() {
            return;
        }
        self.reconnect.pending = None;
        self.reconnect.attach_started = None;
        self.mux.set_availability_error(None);
    }

    pub(super) fn note_attach_client_alive(&mut self, now: Instant) {
        let established = self.reconnect.attach_started.is_some_and(|started| {
            now.saturating_duration_since(started) >= RemoteReattach::STABLE_AFTER
        });
        if established
            && self
                .reconnect
                .pending
                .is_some_and(|reattach| reattach.started)
        {
            self.reconnect.pending = None;
            self.mux.set_availability_error(None);
        }
    }

    pub(super) fn reattach_wait(&mut self, now: Instant) -> Option<Duration> {
        let mut reattach = self.reconnect.pending?;
        if !reattach.due(now) {
            return (!reattach.started).then(|| reattach.retry_at.saturating_duration_since(now));
        }
        reattach.started = true;
        self.reconnect.pending = Some(reattach);
        self.reconnect.attach_started = Some(now);
        self.terminal.discard_active_pane();
        None
    }

    pub(super) fn waiting_to_reattach(&self) -> bool {
        self.reconnect
            .pending
            .is_some_and(|reattach| !reattach.started)
    }

    pub(super) fn is_degraded_remote(&self) -> bool {
        self.reconnect.pending.is_some()
    }

    pub(super) fn restart_remote(&mut self, now: Instant) -> bool {
        let Some(remote) = self.multiplexer.remote.as_ref() else {
            return false;
        };
        self.reconnect.pending = Some(RemoteReattach {
            retry_at: now,
            attempts: 1,
            started: true,
        });
        self.reconnect.attach_started = Some(now);
        self.mux
            .set_availability_error(Some(format!("reconnecting to {}", remote.host)));
        self.terminal.discard_active_pane();
        true
    }

    pub(super) fn degraded_error(&self) -> Option<String> {
        self.mux.last_error().map(str::to_owned).or_else(|| {
            self.reconnect
                .pending
                .map(|reattach| format!("reconnecting (attempt {})", reattach.attempts))
        })
    }
}
