//! Bootty is the reusable terminal embedding layer extracted from Majin.
//!
//! Use `bootty-runtime` to own PTY-backed terminal sessions, `bootty-terminal`
//! for frame data, `bootty-render` for paint plans and WGPU rendering, and the
//! frontend examples for concrete Tauri/WebGL2 and Winit/WGPU hosts.

pub mod geometry {
    pub use bootty_surface::geometry::*;
}

pub mod paint_plan {
    pub use bootty_render::paint_plan::*;
}

pub mod renderer_frame {
    pub use bootty_render::renderer_frame::*;
}

pub mod runtime {
    pub use bootty_runtime::*;
}

pub mod terminal {
    pub use bootty_terminal::terminal::*;
}

pub mod terminal_image {
    pub use bootty_terminal::terminal_image::*;
}

pub mod terminal_render {
    pub use bootty_render::terminal_render::*;
}

pub mod terminal_text {
    pub use bootty_render::terminal_text::*;
}

pub mod terminal_wgpu {
    pub use bootty_render::terminal_wgpu::*;
}
