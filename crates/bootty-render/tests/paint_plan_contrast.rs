use bootty_render::paint_plan::{PaintPlanner, PlanColor, TerminalPaintPlan};
use bootty_surface::geometry::{CellMetrics, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_frame::{
    CellStyle, FrameColors, FrameSelection, FrameStats, RenderCell, RenderFrame,
};
use libghostty_vt::style::RgbColor;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

static_assertions::assert_impl_all!(PlanColor: Copy, Eq, Send, Sync);

#[derive(Arbitrary, Clone, Copy, Debug)]
struct TestRgb {
    r: u8,
    g: u8,
    b: u8,
}

impl From<TestRgb> for RgbColor {
    fn from(color: TestRgb) -> Self {
        rgb(color.r, color.g, color.b)
    }
}

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
    make_plan(frame, false)
}

fn minimum_contrast_plan(frame: &RenderFrame) -> TerminalPaintPlan {
    make_plan(frame, true)
}

fn make_plan(frame: &RenderFrame, minimum_contrast: bool) -> TerminalPaintPlan {
    let cell = CellMetrics::new(10.0, 20.0);
    let surface = TerminalSurface::for_logical_size(
        cell.width,
        cell.height * f32::from(frame.rows),
        cell,
        TerminalPadding::default(),
    );
    let mut planner = PaintPlanner::default();
    if minimum_contrast {
        planner.plan_with_minimum_contrast(surface, frame, 16.0)
    } else {
        planner.plan(surface, frame, 16.0)
    }
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
fn minimum_contrast_is_opt_in() {
    let source = frame(&['a']);

    assert_eq!(foreground(&plan(&source), "a"), color(10, 10, 10));
    assert_eq!(
        foreground(&minimum_contrast_plan(&source), "a"),
        color(255, 255, 255)
    );
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

#[test]
fn overlay_background_order_and_foreground_precedence_remain_exact() {
    let mut frame = frame(&['s', 'a', 'x']);
    frame.colors.selection_background = Some(rgb(4, 5, 6));
    frame.search_matches = (0..3)
        .map(|row| FrameSelection {
            row,
            start_col: 0,
            end_col: 0,
        })
        .collect();
    frame.active_search_match = Some(FrameSelection {
        row: 1,
        start_col: 0,
        end_col: 0,
    });
    frame.selections.push(FrameSelection {
        row: 2,
        start_col: 0,
        end_col: 0,
    });

    let plan = plan(&frame);

    assert_eq!(
        plan.backgrounds
            .iter()
            .map(|background| background.color)
            .collect::<Vec<_>>(),
        vec![
            PlanColor {
                r: 245,
                g: 194,
                b: 66,
                a: 210,
            };
            3
        ]
        .into_iter()
        .chain([color(255, 235, 120), color(4, 5, 6)])
        .collect::<Vec<_>>()
    );
    assert_eq!(foreground(&plan, "s"), color(20, 20, 20));
    assert_eq!(foreground(&plan, "a"), color(0, 0, 0));
    assert_eq!(foreground(&plan, "x"), color(1, 2, 3));
}

#[test]
fn overlay_backgrounds_keep_unclipped_ranges_while_the_text_mask_is_clipped() {
    let mut frame = frame(&['a']);
    frame.search_matches = vec![
        FrameSelection {
            row: 0,
            start_col: 0,
            end_col: 3,
        },
        FrameSelection {
            row: 0,
            start_col: 1,
            end_col: 0,
        },
        FrameSelection {
            row: 2,
            start_col: 0,
            end_col: 1,
        },
    ];

    let plan = plan(&frame);

    assert_eq!(plan.backgrounds.len(), 2);
    assert_eq!(plan.backgrounds[0].rect.width(), 40.0);
    assert_eq!(plan.backgrounds[1].rect.width(), 20.0);
    assert_eq!(plan.backgrounds[1].rect.min_y, 40.0);
    assert_eq!(foreground(&plan, "a"), color(20, 20, 20));
}

proptest! {
    /// Property: minimum contrast never recolors glyphs rendered as native terminal graphics.
    #[test]
    fn native_graphics_preserve_their_source_foreground(
        glyph in prop::sample::select(vec!['█', '■']),
        foreground_color in any::<TestRgb>(),
        background in any::<TestRgb>(),
    ) {
        let mut source = frame(&[glyph]);
        source.colors.background = background.into();
        source.cells[0].fg = Some(foreground_color.into());

        let actual = foreground(&minimum_contrast_plan(&source), &glyph.to_string());

        prop_assert_eq!(
            actual,
            color(foreground_color.r, foreground_color.g, foreground_color.b)
        );
    }
}
