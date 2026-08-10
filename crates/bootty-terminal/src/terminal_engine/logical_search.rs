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

    let rows = frame.text_rows();
    let mut matches = Vec::new();
    let mut logical = Vec::new();
    let mut positions = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        for (col_index, ch) in row.chars().enumerate() {
            logical.push(search_char(ch));
            positions.push((row_index as u16, col_index as u16));
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
        if next_row == row && next_col == end_col.saturating_add(1) {
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
