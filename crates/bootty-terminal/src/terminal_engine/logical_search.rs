use crate::terminal_frame::{FrameSelection, RenderFrame};

use libghostty_vt::terminal::PointCoordinate;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CopyModeSearchMatch {
    pub(super) start: PointCoordinate,
    pub(super) end: PointCoordinate,
}

pub(super) fn normalized_search_query(query: &str) -> Vec<char> {
    query.chars().map(search_char).collect()
}

pub(super) fn normalize_search_char(ch: char) -> char {
    search_char(ch)
}

pub(super) fn frame_search_matches(frame: &RenderFrame, query: &str) -> Vec<FrameSelection> {
    let query = normalized_search_query(query);
    if query.is_empty() {
        return Vec::new();
    }

    let rows = frame.text_rows_with_columns();
    let mut matches = Vec::new();
    let mut logical = Vec::new();
    let mut positions = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for (char_index, ch) in row.text.chars().enumerate() {
            logical.push(search_char(ch));
            positions.push((
                row_index as u16,
                row.columns.get(char_index).copied().unwrap_or_default(),
            ));
        }
        if !frame.row_wraps.get(row_index).copied().unwrap_or(false) {
            push_frame_matches(&mut matches, &logical, &positions, &query);
            logical.clear();
            positions.clear();
        }
    }
    push_frame_matches(&mut matches, &logical, &positions, &query);
    matches
}

pub(super) fn copy_mode_logical_search_matches(
    logical: &[char],
    positions: &[PointCoordinate],
    query: &[char],
) -> Vec<CopyModeSearchMatch> {
    logical_search_ranges(logical, query)
        .into_iter()
        .map(|range| CopyModeSearchMatch {
            start: positions[range.start],
            end: positions[range.end - 1],
        })
        .collect()
}

fn push_frame_matches(
    matches: &mut Vec<FrameSelection>,
    logical: &[char],
    positions: &[(u16, u16)],
    query: &[char],
) {
    for range in logical_search_ranges(logical, query) {
        push_position_range(matches, &positions[range]);
    }
}

fn logical_search_ranges(logical: &[char], query: &[char]) -> Vec<std::ops::Range<usize>> {
    if query.len() > logical.len() {
        return Vec::new();
    }
    (0..=logical.len() - query.len())
        .filter_map(|start| {
            (logical[start..start + query.len()] == *query).then_some(start..start + query.len())
        })
        .collect()
}

fn push_position_range(matches: &mut Vec<FrameSelection>, positions: &[(u16, u16)]) {
    let Some(&(mut row, mut start_col)) = positions.first() else {
        return;
    };
    let mut end_col = start_col;
    for &(next_row, next_col) in &positions[1..] {
        if next_row == row && (next_col == end_col || next_col == end_col.saturating_add(1)) {
            end_col = next_col;
            continue;
        }
        matches.push(FrameSelection {
            row,
            start_col,
            end_col,
        });
        row = next_row;
        start_col = next_col;
        end_col = next_col;
    }
    matches.push(FrameSelection {
        row,
        start_col,
        end_col,
    });
}

fn search_char(ch: char) -> char {
    ch.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_frame::{CellStyle, RenderCell};

    fn combining_frame() -> RenderFrame {
        RenderFrame {
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
        }
    }

    #[test]
    fn combining_scalars_map_to_their_terminal_cell_column() {
        let frame = combining_frame();

        assert_eq!(
            frame_search_matches(&frame, "e\u{301}"),
            vec![FrameSelection {
                row: 0,
                start_col: 0,
                end_col: 0,
            }]
        );
        assert_eq!(
            frame_search_matches(&frame, "e\u{301}x"),
            vec![FrameSelection {
                row: 0,
                start_col: 0,
                end_col: 1,
            }]
        );
    }
}
