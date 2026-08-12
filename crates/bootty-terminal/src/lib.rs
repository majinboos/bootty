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

pub mod selection {
    pub use bootty_surface::selection::*;
}

pub mod terminal {
    pub use crate::terminal_engine::{
        TERMINAL_BACKGROUND, TERMINAL_FOREGROUND, TerminalCursorConfig, TerminalCursorStyle,
        TerminalEngine, TerminalSearchDirection, TerminalSelectionFormat,
    };
    pub use crate::terminal_frame::{
        CellStyle, CursorSnapshot, FrameColors, FrameScrollbar, FrameSelection, FrameStats,
        FrameTextRow, RenderCell, RenderFrame,
    };
    pub use crate::terminal_input_model::{
        KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseEncoderSize,
        MouseInput, TerminalKey,
    };
}
