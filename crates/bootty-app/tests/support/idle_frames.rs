#[path = "frames.rs"]
mod frame_inputs;

pub(super) fn idle_frame(now: std::time::Instant) -> bootty_app::FrameInputs {
    frame_inputs::frame(now, Vec::new())
}
