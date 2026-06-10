mod ghostty_ffi_compat;
mod terminal_png_decoder;

pub mod terminal_engine;
pub mod terminal_frame;
pub mod terminal_image;
pub mod terminal_input_model;
pub mod terminal_palette;

pub mod geometry {
    pub use bootty_surface::geometry::*;
}

pub mod terminal {
    pub use crate::terminal_engine::{TERMINAL_BACKGROUND, TERMINAL_FOREGROUND, TerminalEngine};
    pub use crate::terminal_frame::{
        CellStyle, CursorSnapshot, FrameColors, FrameScrollbar, FrameStats, RenderCell, RenderFrame,
    };
    pub use crate::terminal_input_model::{
        KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseEncoderSize,
        MouseInput, TerminalKey,
    };
}
