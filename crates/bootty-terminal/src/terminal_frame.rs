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

    /// Returns visible terminal rows without allocating a `String` per cell.
    ///
    /// Each row is assembled from the cell spans in one pass. Combining
    /// scalars stay in their cell's span, so the serialized terminal text
    /// remains identical to the terminal's cell view.
    pub fn text_rows(&self) -> Vec<String> {
        (0..self.rows)
            .map(|row| {
                let (slots, last_col) = self.text_row_slots(row);
                let Some(last_col) = last_col else {
                    return String::new();
                };
                let mut text = String::new();
                for slot in slots.into_iter().take(last_col + 1) {
                    if let Some(cell_text) = slot {
                        text.extend(cell_text.iter().copied());
                    } else {
                        text.push(' ');
                    }
                }
                text
            })
            .collect()
    }

    /// Returns visible terminal rows with one terminal-cell column per scalar.
    ///
    /// A grapheme containing multiple Unicode scalars contributes repeated
    /// entries for the cell column that owns it. Consumers such as logical
    /// search must use these columns rather than indexing the rendered string.
    pub fn text_rows_with_columns(&self) -> Vec<FrameTextRow> {
        (0..self.rows)
            .map(|row| {
                let (slots, last_col) = self.text_row_slots(row);
                let Some(last_col) = last_col else {
                    return FrameTextRow::default();
                };
                let mut text = String::new();
                let mut columns = Vec::new();
                for (column, slot) in slots.into_iter().enumerate().take(last_col + 1) {
                    if let Some(cell_text) = slot {
                        text.extend(cell_text.iter().copied());
                        columns.extend(std::iter::repeat_n(column as u16, cell_text.len()));
                    } else {
                        text.push(' ');
                        columns.push(column as u16);
                    }
                }
                FrameTextRow { text, columns }
            })
            .collect()
    }

    fn text_row_slots(&self, row: u16) -> (Vec<Option<&[char]>>, Option<usize>) {
        let mut slots = vec![None; usize::from(self.cols)];
        let mut last_col = None;
        for cell in self
            .cells
            .iter()
            .filter(|cell| cell.y == row && cell.text_len > 0 && !cell.style.invisible)
        {
            let column = usize::from(cell.x);
            if let Some(slot) = slots.get_mut(column) {
                *slot = Some(self.cell_text(cell));
                last_col = Some(last_col.map_or(column, |last: usize| last.max(column)));
            }
        }
        (slots, last_col)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameTextRow {
    pub text: String,
    pub columns: Vec<u16>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_rows_preserve_cell_positions_and_trim_padding() {
        let frame = RenderFrame {
            cols: 5,
            rows: 2,
            cells: vec![
                RenderCell {
                    x: 1,
                    y: 0,
                    text_start: 0,
                    text_len: 2,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
                RenderCell {
                    x: 0,
                    y: 1,
                    text_start: 2,
                    text_len: 1,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
            ],
            text: vec!['a', 'b', 'c'],
            ..RenderFrame::default()
        };

        assert_eq!(frame.text_rows(), [" ab", "c"]);
    }

    #[test]
    fn text_rows_preserve_combining_characters_in_one_cell() {
        let frame = RenderFrame {
            cols: 2,
            rows: 1,
            cells: vec![
                RenderCell {
                    x: 0,
                    y: 0,
                    text_start: 0,
                    text_len: 2,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
                RenderCell {
                    x: 1,
                    y: 0,
                    text_start: 2,
                    text_len: 1,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
            ],
            text: vec!['e', '\u{301}', 'x'],
            ..RenderFrame::default()
        };

        assert_eq!(frame.text_rows(), ["e\u{301}x"]);
    }

    #[test]
    fn text_rows_with_columns_repeat_the_owner_cell_for_combining_scalars() {
        let frame = RenderFrame {
            cols: 2,
            rows: 1,
            cells: vec![
                RenderCell {
                    x: 0,
                    y: 0,
                    text_start: 0,
                    text_len: 2,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
                RenderCell {
                    x: 1,
                    y: 0,
                    text_start: 2,
                    text_len: 1,
                    fg: None,
                    bg: None,
                    style: CellStyle::default(),
                    hyperlink: None,
                },
            ],
            text: vec!['e', '\u{301}', 'x'],
            ..RenderFrame::default()
        };

        assert_eq!(
            frame.text_rows_with_columns(),
            vec![FrameTextRow {
                text: "e\u{301}x".to_owned(),
                columns: vec![0, 0, 1],
            }]
        );
    }

    #[test]
    fn text_rows_hide_concealed_cells() {
        let frame = RenderFrame {
            cols: 2,
            rows: 1,
            cells: vec![RenderCell {
                x: 0,
                y: 0,
                text_start: 0,
                text_len: 1,
                fg: None,
                bg: None,
                style: CellStyle {
                    invisible: true,
                    ..CellStyle::default()
                },
                hyperlink: None,
            }],
            text: vec!['x'],
            ..RenderFrame::default()
        };

        assert_eq!(frame.text_rows(), [""]);
    }
}
