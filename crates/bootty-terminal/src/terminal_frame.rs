use libghostty_vt::{
    render::{CursorVisualStyle, Dirty},
    style::{RgbColor, Underline},
};

use crate::terminal_image::KittyImageFrame;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCopyMode {
    pub selecting: bool,
    pub rectangle: bool,
}

#[derive(Clone, Debug)]
pub struct RenderFrame {
    pub cols: u16,
    pub rows: u16,
    pub dirty: Dirty,
    pub colors: FrameColors,
    pub cursor: Option<CursorSnapshot>,
    pub row_dirty: Vec<bool>,
    pub row_wraps: Vec<bool>,
    pub search_matches: Vec<FrameSelection>,
    pub active_search_match: Option<FrameSelection>,
    pub active_search_match_index: Option<usize>,
    pub search_match_count: usize,
    pub search_pulse: u64,
    pub copy_mode: Option<FrameCopyMode>,
    pub mouse_tracking: bool,
    pub selections: Vec<FrameSelection>,
    pub cells: Vec<RenderCell>,
    pub text: Vec<char>,
    pub images: KittyImageFrame,
    pub scrollbar: Option<FrameScrollbar>,
    pub stats: FrameStats,
}

impl Default for RenderFrame {
    fn default() -> Self {
        Self {
            cols: 0,
            rows: 0,
            dirty: Dirty::Full,
            colors: FrameColors::default(),
            cursor: None,
            row_dirty: Vec::new(),
            row_wraps: Vec::new(),
            search_matches: Vec::new(),
            active_search_match: None,
            active_search_match_index: None,
            search_match_count: 0,
            search_pulse: 0,
            copy_mode: None,
            mouse_tracking: false,
            selections: Vec::new(),
            cells: Vec::new(),
            text: Vec::new(),
            images: KittyImageFrame::default(),
            scrollbar: None,
            stats: FrameStats::default(),
        }
    }
}

impl RenderFrame {
    pub fn cell_text(&self, cell: &RenderCell) -> &[char] {
        &self.text[cell.text_start..cell.text_start + cell.text_len]
    }

    pub fn text_rows(&self) -> Vec<String> {
        let mut rows =
            vec![vec![String::from(" "); usize::from(self.cols)]; usize::from(self.rows)];
        for cell in self
            .cells
            .iter()
            .filter(|cell| cell.text_len > 0 && !cell.style.invisible)
        {
            let Some(row) = rows.get_mut(usize::from(cell.y)) else {
                continue;
            };
            if let Some(slot) = row.get_mut(usize::from(cell.x)) {
                *slot = self.cell_text(cell).iter().collect();
            }
        }
        rows.into_iter()
            .map(|row| row.concat().trim_end().to_owned())
            .collect()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameScrollbar {
    pub total: u64,
    pub offset: u64,
    pub len: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSelection {
    pub row: u16,
    pub start_col: u16,
    pub end_col: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    pub render_state_update_us: u64,
    pub extraction_us: u64,
    pub cells: usize,
    pub chars: usize,
    pub dirty_rows: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FrameColors {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub cursor: Option<RgbColor>,
    pub cursor_text: Option<RgbColor>,
    pub selection_background: Option<RgbColor>,
    pub selection_foreground: Option<RgbColor>,
}

#[derive(Clone, Copy, Debug)]
#[allow(
    dead_code,
    reason = "renderer snapshot preserves Ghostty cursor metadata for upcoming renderer work"
)]
pub struct CursorSnapshot {
    pub x: u16,
    pub y: u16,
    pub at_wide_tail: bool,
    pub style: CursorVisualStyle,
    pub blinking: bool,
    pub color: Option<RgbColor>,
}

#[derive(Clone, Debug)]
pub struct RenderCell {
    pub x: u16,
    pub y: u16,
    pub text_start: usize,
    pub text_len: usize,
    pub fg: Option<RgbColor>,
    pub bg: Option<RgbColor>,
    pub style: CellStyle,
    pub hyperlink: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "renderer snapshot preserves full style flags for upcoming renderer work"
)]
pub struct CellStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: Underline,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            faint: false,
            blink: false,
            inverse: false,
            invisible: false,
            strikethrough: false,
            overline: false,
            underline: Underline::None,
        }
    }
}
