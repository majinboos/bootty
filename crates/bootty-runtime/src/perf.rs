//! A guard for work that must stay off the frame path.
//!
//! Forking a subprocess costs tens of milliseconds. Doing it while the frame thread is inside
//! `update_frame` stalls the window for as long as the child takes to start and answer, which reads
//! as stutter during any interaction. This has regressed twice in the session-name reconciler, so
//! it is enforced at the helpers that spawn rather than left to review.
//!
//! Tests arm the guard around the frames they want kept clean; [`record_subprocess`] then panics
//! naming the caller. Production leaves it disarmed, because a frame may legitimately fork `git` in
//! response to a click (opening the ditch popup, creating a worktree) and killing a live session
//! over that would be worse than the stall it prevents. A steady-state frame has no such excuse,
//! which is exactly the window a test can arm.

use std::cell::Cell;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// Panic on subprocess spawns from this thread until the returned guard drops.
#[must_use = "the guard disarms as soon as it drops"]
pub fn guard_frame_path() -> FramePathGuard {
    FramePathGuard(ARMED.replace(true))
}

/// Restores the previous state rather than disarming, so a nested guard cannot leave the rest of an
/// enclosing guarded scope silently permitting forks.
pub struct FramePathGuard(bool);

impl Drop for FramePathGuard {
    fn drop(&mut self) {
        ARMED.set(self.0);
    }
}

/// Refuse a subprocess spawn on a thread that is inside a guarded frame.
///
/// `what` names the caller so the panic points at the offending path rather than at this helper.
///
/// Never place a call on a path a `Drop` impl can reach: panicking while already unwinding aborts,
/// turning a named test failure into a bare SIGABRT. `BackendPaneTerminal::drop` forks `tmux`, so
/// that constraint is live if this ever extends into `bootty-mux`.
///
/// # Panics
///
/// While a [`FramePathGuard`] is alive on the calling thread.
pub fn record_subprocess(what: &str) {
    assert!(
        !ARMED.get(),
        "{what} spawned a subprocess on the frame path; move it to a worker so frames do not stall"
    );
}
