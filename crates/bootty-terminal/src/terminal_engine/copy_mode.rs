use anyhow::Result;
use libghostty_vt::{
    render::CursorVisualStyle,
    selection::Selection,
    terminal::{Point, PointCoordinate},
};

use crate::terminal_frame::{CursorSnapshot, FrameCopyMode, RenderFrame};

use super::logical_search::{
    CopyModeSearchMatch, copy_mode_logical_search_matches, normalize_search_char,
    normalized_search_query,
};
use super::{TerminalEngine, TerminalSelectionFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSearchDirection {
    Previous,
    Current,
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCopyModeAction {
    Cancel,
    CancelOrClearSelection,
    ClearSelection,
    BeginSelection,
    ToggleSelection,
    SelectLine,
    ToggleSelectionEnd,
    ToggleRectangle,
    CopySelectionAndCancel,
    CopyEndOfLineAndCancel,
    Search {
        query: String,
        direction: TerminalSearchDirection,
    },
    SearchWord(TerminalSearchDirection),
    Move(TerminalCopyModeMotion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCopyModeMotion {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    HistoryTop,
    HistoryBottom,
    StartOfLine,
    EndOfLine,
    BackToIndentation,
    TopLine,
    MiddleLine,
    BottomLine,
    NextWord,
    PreviousWord,
    NextWordEnd,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalCopyModeOutcome {
    pub copied: Option<Vec<u8>>,
    pub search: Option<TerminalCopyModeSearchOutcome>,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCopyModeSearchOutcome {
    pub query: String,
    pub found: bool,
}

#[derive(Debug)]
pub(super) struct CopyModeState {
    cursor: PointCoordinate,
    anchor: Option<PointCoordinate>,
    rectangle: bool,
    linewise: bool,
    desired_col: u16,
}

impl CopyModeState {
    fn selecting(&self) -> bool {
        self.anchor.is_some()
    }
}

impl TerminalEngine {
    pub fn enter_copy_mode(&mut self) -> Result<()> {
        let cursor = self.copy_mode_entry_point()?;
        self.copy_mode = Some(CopyModeState {
            cursor,
            anchor: None,
            rectangle: false,
            linewise: false,
            desired_col: cursor.x,
        });
        self.terminal.set_selection(None)?;
        self.ensure_copy_mode_cursor_visible()?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn copy_mode_active(&self) -> bool {
        self.copy_mode.is_some()
    }

    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        if self.copy_mode.is_none() {
            return Ok(TerminalCopyModeOutcome::default());
        }

        let mut copied = None;
        let mut search = None;
        match action {
            TerminalCopyModeAction::Cancel => self.cancel_copy_mode()?,
            TerminalCopyModeAction::CancelOrClearSelection => {
                if self
                    .copy_mode
                    .as_ref()
                    .is_some_and(CopyModeState::selecting)
                {
                    self.clear_copy_mode_selection()?;
                } else {
                    self.cancel_copy_mode()?;
                }
            }
            TerminalCopyModeAction::ClearSelection => self.clear_copy_mode_selection()?,
            TerminalCopyModeAction::BeginSelection => self.begin_copy_mode_selection(false)?,
            TerminalCopyModeAction::ToggleSelection => self.toggle_copy_mode_selection()?,
            TerminalCopyModeAction::SelectLine => self.select_copy_mode_line()?,
            TerminalCopyModeAction::ToggleSelectionEnd => self.toggle_copy_mode_selection_end()?,
            TerminalCopyModeAction::ToggleRectangle => self.toggle_copy_mode_rectangle()?,
            TerminalCopyModeAction::Search { query, direction } => {
                let found = self.search_copy_mode_query(&query, direction)?;
                search = Some(TerminalCopyModeSearchOutcome { query, found });
            }
            TerminalCopyModeAction::SearchWord(direction) => {
                if let Some(query) = self.copy_mode_word_under_cursor()? {
                    let found = self.search_copy_mode_query(&query, direction)?;
                    search = Some(TerminalCopyModeSearchOutcome { query, found });
                }
            }
            TerminalCopyModeAction::CopySelectionAndCancel => {
                copied = self.copy_mode_selection_and_cancel()?;
            }
            TerminalCopyModeAction::CopyEndOfLineAndCancel => {
                if !self
                    .copy_mode
                    .as_ref()
                    .is_some_and(CopyModeState::selecting)
                {
                    self.begin_copy_mode_selection(false)?;
                }
                self.move_copy_mode_cursor(TerminalCopyModeMotion::EndOfLine)?;
                copied = self.copy_mode_selection_and_cancel()?;
            }
            TerminalCopyModeAction::Move(motion) => self.move_copy_mode_cursor(motion)?,
        }

        Ok(TerminalCopyModeOutcome {
            copied,
            search,
            active: self.copy_mode.is_some(),
        })
    }

    fn copy_mode_entry_point(&mut self) -> Result<PointCoordinate> {
        let viewport_top = self.viewport_top_screen_row()?;
        let max_y = (self.terminal.total_rows()?.max(1) as u32).saturating_sub(1);
        let max_x = self.geometry.cols.saturating_sub(1);
        let last_row = u32::from(self.geometry.rows.max(1).saturating_sub(1));
        let cursor = self.extract_frame()?.cursor;
        if let Some(cursor) = cursor {
            return Ok(PointCoordinate {
                x: cursor.x.min(max_x),
                y: viewport_top
                    .saturating_add(u32::from(
                        cursor.y.min(self.geometry.rows.saturating_sub(1)),
                    ))
                    .min(max_y),
            });
        }

        Ok(PointCoordinate {
            x: 0,
            y: viewport_top.saturating_add(last_row).min(max_y),
        })
    }

    fn copy_mode_screen_point(&self) -> Option<PointCoordinate> {
        self.copy_mode.as_ref().map(|state| state.cursor)
    }

    fn begin_copy_mode_selection(&mut self, rectangle: bool) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        if let Some(state) = &mut self.copy_mode {
            state.anchor = Some(point);
            state.rectangle = rectangle;
            state.linewise = false;
        }
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_selection(&mut self) -> Result<()> {
        if self
            .copy_mode
            .as_ref()
            .is_some_and(CopyModeState::selecting)
        {
            self.clear_copy_mode_selection()
        } else {
            self.begin_copy_mode_selection(false)
        }
    }

    fn clear_copy_mode_selection(&mut self) -> Result<()> {
        if let Some(state) = &mut self.copy_mode {
            state.anchor = None;
            state.rectangle = false;
            state.linewise = false;
        }
        self.terminal.set_selection(None)?;
        self.mark_content_changed();
        Ok(())
    }

    fn cancel_copy_mode(&mut self) -> Result<()> {
        self.copy_mode = None;
        self.terminal.set_selection(None)?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_rectangle(&mut self) -> Result<()> {
        if !self
            .copy_mode
            .as_ref()
            .is_some_and(CopyModeState::selecting)
        {
            self.begin_copy_mode_selection(true)?;
            return Ok(());
        }
        if let Some(state) = &mut self.copy_mode {
            state.rectangle = !state.rectangle;
            state.linewise = false;
        }
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn select_copy_mode_line(&mut self) -> Result<()> {
        let Some(mut point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let start = PointCoordinate { x: 0, y: point.y };
        point.x = self.screen_row_end_col(point.y)?;
        if let Some(state) = &mut self.copy_mode {
            state.anchor = Some(start);
            state.rectangle = false;
            state.linewise = true;
            state.cursor = point;
            state.desired_col = point.x;
        }
        self.ensure_copy_mode_cursor_visible()?;
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_selection_end(&mut self) -> Result<()> {
        if let Some(state) = &mut self.copy_mode
            && let Some(anchor) = &mut state.anchor
        {
            std::mem::swap(anchor, &mut state.cursor);
            state.desired_col = state.cursor.x;
            self.ensure_copy_mode_cursor_visible()?;
            self.sync_copy_mode_selection()?;
            self.mark_content_changed();
        }
        Ok(())
    }

    fn copy_mode_selection_and_cancel(&mut self) -> Result<Option<Vec<u8>>> {
        self.sync_copy_mode_selection()?;
        let copied = self.format_selection(TerminalSelectionFormat::PlainText)?;
        self.cancel_copy_mode()?;
        Ok(copied)
    }

    fn move_copy_mode_cursor(&mut self, motion: TerminalCopyModeMotion) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let desired_col = self
            .copy_mode
            .as_ref()
            .map_or(point.x, |state| state.desired_col);
        let (next, update_desired_col) =
            self.copy_mode_motion_target(point, desired_col, motion)?;
        self.set_copy_mode_cursor(next, update_desired_col)
    }

    fn copy_mode_motion_target(
        &mut self,
        point: PointCoordinate,
        desired_col: u16,
        motion: TerminalCopyModeMotion,
    ) -> Result<(PointCoordinate, bool)> {
        let total_rows = self.terminal.total_rows()?.max(1) as u32;
        let max_y = total_rows.saturating_sub(1);
        let max_x = self.geometry.cols.saturating_sub(1);
        let page = u32::from(self.geometry.rows.max(1));
        let half_page = (page / 2).max(1);
        let mut next = point;
        let mut update_desired_col = true;

        match motion {
            TerminalCopyModeMotion::Left => {
                if next.x > 0 {
                    next.x -= 1;
                } else if next.y > 0 {
                    next.y -= 1;
                    next.x = self.screen_row_end_col(next.y)?;
                }
            }
            TerminalCopyModeMotion::Right => {
                if next.x < max_x {
                    next.x += 1;
                } else if next.y < max_y {
                    next.y += 1;
                    next.x = 0;
                }
            }
            TerminalCopyModeMotion::Up
            | TerminalCopyModeMotion::PageUp
            | TerminalCopyModeMotion::HalfPageUp => {
                let distance = match motion {
                    TerminalCopyModeMotion::Up => 1,
                    TerminalCopyModeMotion::PageUp => page,
                    _ => half_page,
                };
                next.y = next.y.saturating_sub(distance);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::Down
            | TerminalCopyModeMotion::PageDown
            | TerminalCopyModeMotion::HalfPageDown => {
                let distance = match motion {
                    TerminalCopyModeMotion::Down => 1,
                    TerminalCopyModeMotion::PageDown => page,
                    _ => half_page,
                };
                next.y = next.y.saturating_add(distance).min(max_y);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::HistoryTop => {
                next.y = 0;
                next.x = 0;
            }
            TerminalCopyModeMotion::HistoryBottom => {
                next.y = max_y;
                next.x = self.screen_row_end_col(next.y)?;
            }
            TerminalCopyModeMotion::StartOfLine => next.x = 0,
            TerminalCopyModeMotion::EndOfLine => next.x = self.screen_row_end_col(next.y)?,
            TerminalCopyModeMotion::BackToIndentation => {
                next.x = self.screen_row_first_nonblank_col(next.y)?;
            }
            TerminalCopyModeMotion::TopLine => next.y = self.viewport_top_screen_row()?,
            TerminalCopyModeMotion::MiddleLine => {
                next.y = self
                    .viewport_top_screen_row()?
                    .saturating_add(page / 2)
                    .min(max_y);
            }
            TerminalCopyModeMotion::BottomLine => {
                next.y = self
                    .viewport_top_screen_row()?
                    .saturating_add(page.saturating_sub(1))
                    .min(max_y);
            }
            TerminalCopyModeMotion::NextWord => next = self.next_word_start(point)?,
            TerminalCopyModeMotion::PreviousWord => next = self.previous_word_start(point)?,
            TerminalCopyModeMotion::NextWordEnd => next = self.next_word_end(point)?,
            TerminalCopyModeMotion::ScrollUp | TerminalCopyModeMotion::ScrollDown => {
                let before_top = self.viewport_top_screen_row()?;
                let delta = if motion == TerminalCopyModeMotion::ScrollUp {
                    -1
                } else {
                    1
                };
                self.scroll_viewport_delta(delta);
                let after_top = self.viewport_top_screen_row()?;
                next.y = Self::shifted_screen_row(next.y, before_top, after_top, max_y);
                update_desired_col = false;
            }
        }

        next.x = next.x.min(max_x);
        next.y = next.y.min(max_y);
        Ok((next, update_desired_col))
    }
    fn shifted_screen_row(row: u32, before_top: u32, after_top: u32, max_y: u32) -> u32 {
        let delta = i128::from(after_top) - i128::from(before_top);
        (i128::from(row) + delta).clamp(0, i128::from(max_y)) as u32
    }

    fn set_copy_mode_cursor(
        &mut self,
        point: PointCoordinate,
        update_desired_col: bool,
    ) -> Result<()> {
        if let Some(state) = &mut self.copy_mode {
            state.cursor = point;
            if update_desired_col {
                state.desired_col = point.x;
            }
        }
        self.ensure_copy_mode_cursor_visible()?;
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn search_copy_mode_query(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(false);
        }
        if self.search_query != query {
            self.search_query = query.to_owned();
            self.search_active_index = 0;
            self.mark_content_changed();
        }

        let Some(cursor) = self.copy_mode_screen_point() else {
            return Ok(false);
        };
        let matches = self.copy_mode_search_matches(query)?;
        let Some(found) = Self::copy_mode_search_target(&matches, cursor, direction) else {
            self.bump_search_pulse();
            let _ = self.extract_frame()?;
            return Ok(false);
        };

        self.set_copy_mode_cursor(found.start, true)?;
        self.align_copy_mode_active_search_match()?;
        self.bump_search_pulse();
        Ok(true)
    }

    fn copy_mode_word_under_cursor(&self) -> Result<Option<String>> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(None);
        };
        let chars = self.screen_row_chars(point.y)?;
        let Some(ch) = chars.get(usize::from(point.x)).copied() else {
            return Ok(None);
        };
        if ch.is_whitespace() {
            return Ok(None);
        }

        let mut start = usize::from(point.x);
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let mut end = usize::from(point.x);
        while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
            end += 1;
        }

        let word: String = chars[start..=end].iter().collect();
        Ok((!word.is_empty()).then_some(word))
    }

    fn copy_mode_search_matches(&self, query: &str) -> Result<Vec<CopyModeSearchMatch>> {
        let query = normalized_search_query(query);
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let total_rows = self.terminal.total_rows()?.max(1) as u32;
        let mut matches = Vec::new();
        let mut logical = Vec::new();
        let mut positions = Vec::new();
        for y in 0..total_rows {
            for x in 0..self.geometry.cols {
                let point = PointCoordinate { x, y };
                logical.push(normalize_search_char(self.screen_char(point)?));
                positions.push(point);
            }
            if !self.screen_row_wrapped(y)? {
                matches.extend(copy_mode_logical_search_matches(
                    &logical, &positions, &query,
                ));
                logical.clear();
                positions.clear();
            }
        }
        matches.extend(copy_mode_logical_search_matches(
            &logical, &positions, &query,
        ));
        Ok(matches)
    }

    fn copy_mode_search_target(
        matches: &[CopyModeSearchMatch],
        cursor: PointCoordinate,
        direction: TerminalSearchDirection,
    ) -> Option<CopyModeSearchMatch> {
        match direction {
            TerminalSearchDirection::Current => matches
                .iter()
                .copied()
                .find(|candidate| Self::copy_mode_search_match_contains(*candidate, cursor))
                .or_else(|| {
                    matches
                        .iter()
                        .copied()
                        .find(|candidate| Self::point_after(candidate.start, cursor))
                })
                .or_else(|| matches.first().copied()),
            TerminalSearchDirection::Next => matches
                .iter()
                .copied()
                .find(|candidate| {
                    !Self::copy_mode_search_match_contains(*candidate, cursor)
                        && Self::point_after(candidate.start, cursor)
                })
                .or_else(|| {
                    matches.iter().copied().find(|candidate| {
                        !Self::copy_mode_search_match_contains(*candidate, cursor)
                    })
                })
                .or_else(|| matches.first().copied()),
            TerminalSearchDirection::Previous => matches
                .iter()
                .rev()
                .copied()
                .find(|candidate| {
                    !Self::copy_mode_search_match_contains(*candidate, cursor)
                        && Self::point_before(candidate.start, cursor)
                })
                .or_else(|| {
                    matches.iter().rev().copied().find(|candidate| {
                        !Self::copy_mode_search_match_contains(*candidate, cursor)
                    })
                })
                .or_else(|| matches.last().copied()),
        }
    }

    fn align_copy_mode_active_search_match(&mut self) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let viewport_top = self.viewport_top_screen_row()?;
        let Some(row) = point.y.checked_sub(viewport_top) else {
            return Ok(());
        };
        let Ok(row) = u16::try_from(row) else {
            return Ok(());
        };
        let index = {
            let frame = self.extract_frame()?;
            frame.search_matches.iter().position(|selection| {
                selection.row == row
                    && selection.start_col <= point.x
                    && point.x <= selection.end_col
            })
        };
        if let Some(index) = index {
            self.search_active_index = index;
            self.mark_content_changed();
            let _ = self.extract_frame()?;
        }
        Ok(())
    }

    fn copy_mode_search_match_contains(
        candidate: CopyModeSearchMatch,
        point: PointCoordinate,
    ) -> bool {
        !Self::point_before(point, candidate.start) && !Self::point_after(point, candidate.end)
    }

    fn point_before(point: PointCoordinate, cursor: PointCoordinate) -> bool {
        (point.y, point.x) < (cursor.y, cursor.x)
    }

    fn point_after(point: PointCoordinate, cursor: PointCoordinate) -> bool {
        (point.y, point.x) > (cursor.y, cursor.x)
    }

    fn sync_copy_mode_selection(&mut self) -> Result<()> {
        let Some(state) = &self.copy_mode else {
            self.terminal.set_selection(None)?;
            return Ok(());
        };
        let Some(anchor) = state.anchor else {
            self.terminal.set_selection(None)?;
            return Ok(());
        };
        let (start_point, end_point) = if state.linewise {
            let start_y = anchor.y.min(state.cursor.y);
            let end_y = anchor.y.max(state.cursor.y);
            (
                PointCoordinate { x: 0, y: start_y },
                PointCoordinate {
                    x: self.screen_row_end_col(end_y)?,
                    y: end_y,
                },
            )
        } else {
            (anchor, state.cursor)
        };
        let start = self.terminal.grid_ref(Point::Screen(start_point))?;
        let end = self.terminal.grid_ref(Point::Screen(end_point))?;
        let selection = Selection::new(start, end, state.rectangle);
        self.terminal.set_selection(Some(&selection))?;
        Ok(())
    }

    fn ensure_copy_mode_cursor_visible(&mut self) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let top = self.viewport_top_screen_row()?;
        let bottom = top.saturating_add(u32::from(self.geometry.rows.max(1)).saturating_sub(1));
        let delta = if point.y < top {
            i128::from(point.y) - i128::from(top)
        } else if point.y > bottom {
            i128::from(point.y) - i128::from(bottom)
        } else {
            0
        };
        if delta != 0 {
            self.scroll_viewport_delta(delta.clamp(isize::MIN as i128, isize::MAX as i128) as isize);
        }
        Ok(())
    }

    pub(super) fn viewport_top_screen_row(&self) -> Result<u32> {
        Ok(self.terminal.scrollbar()?.offset as u32)
    }

    fn screen_row_chars(&self, row: u32) -> Result<Vec<char>> {
        (0..self.geometry.cols)
            .map(|x| self.screen_char(PointCoordinate { x, y: row }))
            .collect()
    }

    fn screen_row_end_col(&self, row: u32) -> Result<u16> {
        for x in (0..self.geometry.cols).rev() {
            if !self
                .screen_char(PointCoordinate { x, y: row })?
                .is_whitespace()
            {
                return Ok(x);
            }
        }
        Ok(0)
    }

    fn screen_row_first_nonblank_col(&self, row: u32) -> Result<u16> {
        for x in 0..self.geometry.cols {
            if !self
                .screen_char(PointCoordinate { x, y: row })?
                .is_whitespace()
            {
                return Ok(x);
            }
        }
        Ok(0)
    }

    fn screen_char(&self, point: PointCoordinate) -> Result<char> {
        let cell = self.terminal.grid_ref(Point::Screen(point))?.cell()?;
        if cell.has_text()? {
            Ok(char::from_u32(cell.codepoint()?).unwrap_or(' '))
        } else {
            Ok(' ')
        }
    }

    fn screen_row_wrapped(&self, row: u32) -> Result<bool> {
        Ok(self
            .terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y: row }))?
            .row()?
            .is_wrapped()
            .unwrap_or(false))
    }

    fn next_screen_point(&self, point: PointCoordinate, max_y: u32) -> Option<PointCoordinate> {
        let max_x = self.geometry.cols.saturating_sub(1);
        if point.x < max_x {
            Some(PointCoordinate {
                x: point.x + 1,
                y: point.y,
            })
        } else if point.y < max_y {
            Some(PointCoordinate {
                x: 0,
                y: point.y + 1,
            })
        } else {
            None
        }
    }

    fn previous_screen_point(&self, point: PointCoordinate) -> Option<PointCoordinate> {
        if point.x > 0 {
            Some(PointCoordinate {
                x: point.x - 1,
                y: point.y,
            })
        } else if point.y > 0 {
            Some(PointCoordinate {
                x: self.geometry.cols.saturating_sub(1),
                y: point.y - 1,
            })
        } else {
            None
        }
    }

    fn next_word_start(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        let mut left_word = self.screen_char(current)?.is_whitespace();
        let max_y = (self.terminal.total_rows()?.max(1) as u32).saturating_sub(1);
        while let Some(next) = self.next_screen_point(current, max_y) {
            current = next;
            let is_word = !self.screen_char(current)?.is_whitespace();
            if left_word && is_word {
                return Ok(current);
            }
            if !is_word {
                left_word = true;
            }
        }
        Ok(current)
    }

    fn previous_word_start(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        while let Some(previous) = self.previous_screen_point(current) {
            current = previous;
            if !self.screen_char(current)?.is_whitespace() {
                break;
            }
        }
        while let Some(previous) = self.previous_screen_point(current) {
            if self.screen_char(previous)?.is_whitespace() {
                break;
            }
            current = previous;
        }
        Ok(current)
    }

    fn next_word_end(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        let max_y = (self.terminal.total_rows()?.max(1) as u32).saturating_sub(1);
        while let Some(next) = self.next_screen_point(current, max_y) {
            current = next;
            if !self.screen_char(current)?.is_whitespace() {
                break;
            }
        }
        while let Some(next) = self.next_screen_point(current, max_y) {
            if self.screen_char(next)?.is_whitespace() {
                break;
            }
            current = next;
        }
        Ok(current)
    }
    pub(super) fn copy_mode_frame_state(
        copy_mode: Option<&CopyModeState>,
    ) -> Option<FrameCopyMode> {
        copy_mode.map(|state| FrameCopyMode {
            selecting: state.selecting(),
            rectangle: state.rectangle,
        })
    }

    pub(super) fn apply_copy_mode_frame_cursor(
        frame: &mut RenderFrame,
        copy_mode: Option<&CopyModeState>,
        viewport_top: u32,
    ) {
        let Some(point) = copy_mode.map(|state| state.cursor) else {
            return;
        };
        let Some(row) = point.y.checked_sub(viewport_top) else {
            return;
        };
        let Ok(y) = u16::try_from(row) else {
            return;
        };
        if point.x >= frame.cols || y >= frame.rows {
            return;
        }
        frame.cursor = Some(CursorSnapshot {
            x: point.x,
            y,
            at_wide_tail: false,
            style: CursorVisualStyle::BlockHollow,
            blinking: false,
            color: frame.colors.cursor.or(Some(frame.colors.foreground)),
        });
    }
}
