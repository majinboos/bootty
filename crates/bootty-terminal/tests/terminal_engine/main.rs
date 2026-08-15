pub use anyhow::{Context, Result};
pub use bootty_terminal::{
    geometry::*, terminal_engine::*, terminal_frame::*, terminal_image::*, terminal_input_model::*,
    terminal_palette::*,
};
pub use libghostty_vt::{
    Terminal, focus, key,
    kitty::graphics,
    mouse, paste,
    render::CursorVisualStyle,
    selection::gesture,
    style::{RgbColor, Underline},
    terminal::{ColorScheme, CursorStyle, Mode, Point, PointCoordinate, ScrollViewport},
};
pub use std::sync::{Arc, Mutex};

pub fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub fn terminal_engine_with_colors(
    geometry: TerminalGeometry,
    colors: TerminalColorConfig,
) -> Result<TerminalEngine> {
    TerminalEngine::new_with_scrollback(geometry, colors, DEFAULT_MAX_SCROLLBACK)
}

pub fn drain_clipboard_texts(engine: &mut TerminalEngine) -> Vec<String> {
    engine
        .drain_side_effects()
        .into_iter()
        .filter_map(|effect| match effect {
            TerminalSideEffect::ClipboardWrite(text) => Some(text),
            _ => None,
        })
        .collect()
}

mod suite;
