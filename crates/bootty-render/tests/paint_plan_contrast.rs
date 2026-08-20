use bootty_render::paint_plan::{PaintPlanner, PlanColor, TerminalPaintPlan};
use bootty_surface::geometry::{CellMetrics, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_frame::{
    CellStyle, FrameColors, FrameStats, RenderCell, RenderFrame,
};
use libghostty_vt::style::RgbColor;

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}

fn frame(text: &[char]) -> RenderFrame {
    RenderFrame {
        cols: 1,
        rows: text.len() as u16,
        colors: FrameColors {
            background: rgb(12, 12, 12),
            foreground: rgb(10, 10, 10),
            selection_foreground: Some(rgb(1, 2, 3)),
            ..Default::default()
        },
        row_dirty: vec![true; text.len()],
        cells: text
            .iter()
            .enumerate()
            .map(|(row, _)| RenderCell {
                x: 0,
                y: row as u16,
                text_start: row,
                text_len: 1,
                fg: Some(rgb(10, 10, 10)),
                bg: Some(rgb(12, 12, 12)),
                style: CellStyle::default(),
                hyperlink: None,
            })
            .collect(),
        text: text.to_vec(),
        stats: FrameStats {
            cells: text.len(),
            chars: text.len(),
            dirty_rows: text.len(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn frame_with_text(text: &[char]) -> RenderFrame {
    let mut frame = frame(&['a']);
    frame.cells[0].text_len = text.len();
    frame.text = text.to_vec();
    frame.stats.chars = text.len();
    frame
}

fn plan(frame: &RenderFrame) -> TerminalPaintPlan {
    let cell = CellMetrics::new(10.0, 20.0);
    let surface = TerminalSurface::for_logical_size(
        cell.width,
        cell.height * f32::from(frame.rows),
        cell,
        TerminalPadding::default(),
    );
    PaintPlanner::default().plan(surface, frame, 16.0).clone()
}

fn minimum_contrast_plan(frame: &RenderFrame) -> TerminalPaintPlan {
    let cell = CellMetrics::new(10.0, 20.0);
    let surface = TerminalSurface::for_logical_size(
        cell.width,
        cell.height * f32::from(frame.rows),
        cell,
        TerminalPadding::default(),
    );
    PaintPlanner::default()
        .plan_with_minimum_contrast(surface, frame, 16.0)
        .clone()
}

fn foreground(plan: &TerminalPaintPlan, text: &str) -> PlanColor {
    plan.text_runs
        .iter()
        .find(|run| run.text == text)
        .unwrap_or_else(|| panic!("missing text run {text:?}"))
        .attrs
        .fg
}

#[test]
fn ordinary_planner_keeps_low_contrast_text_color() {
    let plan = plan(&frame(&['a']));

    assert_eq!(foreground(&plan, "a"), color(10, 10, 10));
}

#[test]
fn minimum_contrast_adjusts_low_contrast_text() {
    let plan = minimum_contrast_plan(&frame(&['a']));

    assert_eq!(foreground(&plan, "a"), color(255, 255, 255));
}

#[test]
fn minimum_contrast_keeps_old_native_graphics_source_colors() {
    let block = minimum_contrast_plan(&frame(&['█']));
    let geometric = minimum_contrast_plan(&frame(&['■']));

    assert_eq!(foreground(&block, "█"), color(10, 10, 10));
    assert_eq!(foreground(&geometric, "■"), color(10, 10, 10));
}

#[test]
fn minimum_contrast_adjusts_old_text_and_multi_character_content() {
    let text = minimum_contrast_plan(&frame(&['❯']));
    let multi_character = minimum_contrast_plan(&frame_with_text(&['a', 'b']));

    assert_eq!(foreground(&text, "❯"), color(255, 255, 255));
    assert_eq!(foreground(&multi_character, "ab"), color(255, 255, 255));
}

#[test]
fn selection_and_search_foregrounds_remain_exact() {
    let mut frame = frame(&['a', 's']);
    frame
        .selections
        .push(bootty_terminal::terminal_frame::FrameSelection {
            row: 0,
            start_col: 0,
            end_col: 0,
        });
    frame
        .search_matches
        .push(bootty_terminal::terminal_frame::FrameSelection {
            row: 1,
            start_col: 0,
            end_col: 0,
        });
    let plan = minimum_contrast_plan(&frame);

    assert_eq!(foreground(&plan, "a"), color(1, 2, 3));
    assert_eq!(foreground(&plan, "s"), color(20, 20, 20));
}
