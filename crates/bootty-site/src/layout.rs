//! Cell-space scrolling helpers.

pub(crate) fn max_scroll(line_count: usize, area_height: u16) -> u16 {
    let content_height = area_height as usize;
    line_count.saturating_sub(content_height) as u16
}
