use std::sync::Arc;

use anyhow::Result;
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::terminal_frame::RenderFrame;

use crate::terminal_session::TerminalSession;

pub trait TerminalRenderSource {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()>;
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>>;
    fn scroll_viewport_delta(&mut self, _delta: isize) -> Result<()> {
        Ok(())
    }
}

impl TerminalRenderSource for TerminalSession {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        Self::resize(self, geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Self::extract_frame(self)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        Self::scroll_viewport_delta(self, delta)
    }
}
