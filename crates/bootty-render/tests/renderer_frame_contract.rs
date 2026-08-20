use bootty_render::{
    paint_plan::{CursorBlinkPhase, PaintPlanner},
    renderer_frame::{
        GhosttyGraphicsElement, MinimumContrastPolicy, RendererCellGraphics, RendererCursorOptions,
        RendererCursorShape, RendererCursorState, RendererFrame, RendererSelectionIntent,
        renderer_cursor_shape,
    },
    terminal_text::TerminalTextConfig,
};
use bootty_surface::{
    geometry::{CellMetrics, TerminalPadding, TerminalSurface},
    selection::{SelectionPoint, TerminalSelection},
};
use bootty_terminal::terminal_frame::{
    CellStyle, CursorSnapshot, FrameColors, FrameStats, RenderCell, RenderFrame,
};
use libghostty_vt::{
    render::{CursorVisualStyle, Dirty},
    style::{RgbColor, Underline},
};

#[test]
fn renderer_frame_preserves_rows_cells_metrics_padding_cursor_and_decor() {
    let surface = TerminalSurface::for_logical_size(
        80.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::uniform(2.0),
    );
    let frame = render_frame(vec![
        cell(0, 0, 0, 1, style_with_underline()),
        cell(1, 0, 1, 1, CellStyle::default()),
        cell(0, 1, 2, 1, CellStyle::default()),
    ]);

    let renderer_frame = RendererFrame::from_terminal(
        &frame,
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    );

    assert_eq!(renderer_frame.metrics.cell, CellMetrics::new(10.0, 20.0));
    assert_eq!(
        renderer_frame.metrics.padding,
        TerminalPadding::uniform(2.0)
    );
    assert_eq!(renderer_frame.rows.len(), 2);
    assert_eq!(renderer_frame.rows[0].cells, 0..2);
    assert_eq!(renderer_frame.rows[1].cells, 2..3);
    assert_eq!(renderer_frame.cells[0].text, "A");
    assert!(renderer_frame.cells[0].decor.underline);
    assert_eq!(
        renderer_frame.cells[0].selection,
        RendererSelectionIntent::None
    );
    assert_eq!(renderer_frame.cursor.map(|cursor| cursor.x), Some(1));
}

#[test]
fn renderer_frame_applies_shared_terminal_selection() {
    let surface = TerminalSurface::for_logical_size(
        40.0,
        40.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let frame = render_frame(vec![
        cell(0, 0, 0, 1, CellStyle::default()),
        cell(1, 0, 1, 1, CellStyle::default()),
        cell(0, 1, 2, 1, CellStyle::default()),
        cell(1, 1, 3, 1, CellStyle::default()),
    ])
    .with_text(vec!['A', 'B', 'C', 'D']);
    let mut renderer_frame = RendererFrame::from_terminal(
        &frame,
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    );

    renderer_frame.select_terminal_selection(TerminalSelection::new(
        SelectionPoint::new(1, 0),
        SelectionPoint::new(0, 1),
    ));

    assert_eq!(
        renderer_frame.cells[0].selection,
        RendererSelectionIntent::None
    );
    assert!(matches!(
        renderer_frame.cells[1].selection,
        RendererSelectionIntent::Selected { .. }
    ));
    assert!(matches!(
        renderer_frame.cells[2].selection,
        RendererSelectionIntent::Selected { .. }
    ));
}

#[test]
fn renderer_frame_classifies_terminal_graphics_and_skips_minimum_contrast() {
    let surface = TerminalSurface::for_logical_size(
        20.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let frame = render_frame(vec![cell(0, 0, 0, 1, CellStyle::default())]);

    let renderer_frame = RendererFrame::from_terminal(
        &frame.with_text(vec!['█']),
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    );

    assert_eq!(
        renderer_frame.cells[0].graphics,
        RendererCellGraphics::Ghostty(GhosttyGraphicsElement::Block)
    );
    assert_eq!(
        renderer_frame.cells[0].minimum_contrast_policy,
        MinimumContrastPolicy::SkipForGraphicsElement
    );
}

#[test]
fn renderer_frame_keeps_text_cells_on_minimum_contrast_policy() {
    let surface = TerminalSurface::for_logical_size(
        20.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let frame = render_frame(vec![cell(0, 0, 0, 1, CellStyle::default())]);

    let renderer_frame = RendererFrame::from_terminal(
        &frame.with_text(vec!['A']),
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    );

    assert_eq!(renderer_frame.cells[0].graphics, RendererCellGraphics::Text);
    assert_eq!(
        renderer_frame.cells[0].minimum_contrast_policy,
        MinimumContrastPolicy::EnforceForText
    );
}

#[test]
fn renderer_frame_paint_plan_preserves_cursor_text_and_wide_cell_behavior() {
    let surface = TerminalSurface::for_logical_size(
        40.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut frame = render_frame(vec![
        cell(0, 0, 0, 1, CellStyle::default()),
        cell(1, 0, 1, 1, CellStyle::default()),
    ])
    .with_text(vec!['A', '界']);
    frame.cols = 4;
    frame.rows = 1;
    frame.cursor = Some(CursorSnapshot {
        x: 0,
        y: 0,
        at_wide_tail: false,
        style: CursorVisualStyle::Block,
        blinking: false,
        color: None,
    });

    let text_config = TerminalTextConfig::with_cell_metrics(surface.cell);
    let renderer_frame = RendererFrame::from_terminal(&frame, surface, &text_config);
    let mut planner = PaintPlanner::default();
    let expected = planner
        .plan_with_cursor_blink_phase(
            surface,
            &frame,
            text_config.font_size,
            CursorBlinkPhase::visible(),
        )
        .clone();

    let plan = renderer_frame.to_paint_plan();

    assert_eq!(plan, expected);
    assert!(
        plan.cursor
            .and_then(|cursor| cursor.text_under_cursor)
            .is_some()
    );
    assert!(
        plan.text_runs
            .iter()
            .any(|run| { run.text.contains('界') && run.cells > run.text.chars().count() as u16 })
    );
}

#[test]
fn renderer_cursor_default_uses_configured_style() {
    let state = RendererCursorState {
        visual_style: RendererCursorShape::Bar,
        blinking: true,
        ..RendererCursorState::default()
    };

    assert_cursor_shapes(
        state,
        &[
            ((true, true), Some(RendererCursorShape::Bar)),
            ((false, true), Some(RendererCursorShape::HollowBlock)),
            ((false, false), Some(RendererCursorShape::HollowBlock)),
            ((true, false), None),
        ],
    );
}

#[test]
fn renderer_cursor_blinking_disabled_stays_visible() {
    let state = RendererCursorState {
        visual_style: RendererCursorShape::Bar,
        blinking: false,
        ..RendererCursorState::default()
    };

    assert_cursor_shapes(
        state,
        &[
            ((true, true), Some(RendererCursorShape::Bar)),
            ((true, false), Some(RendererCursorShape::Bar)),
            ((false, true), Some(RendererCursorShape::HollowBlock)),
            ((false, false), Some(RendererCursorShape::HollowBlock)),
        ],
    );
}

#[test]
fn renderer_cursor_explicitly_not_visible() {
    let state = RendererCursorState {
        visible: false,
        visual_style: RendererCursorShape::Bar,
        blinking: false,
        ..RendererCursorState::default()
    };

    for focused in [true, false] {
        for blink_visible in [true, false] {
            assert_eq!(
                renderer_cursor_shape(
                    state,
                    RendererCursorOptions {
                        focused,
                        blink_visible,
                        ..RendererCursorOptions::default()
                    },
                ),
                None
            );
        }
    }
}

#[test]
fn renderer_cursor_preedit_forces_block_when_cursor_is_in_viewport() {
    for focused in [true, false] {
        for blink_visible in [true, false] {
            assert_eq!(
                renderer_cursor_shape(
                    RendererCursorState::default(),
                    RendererCursorOptions {
                        preedit: true,
                        focused,
                        blink_visible,
                    },
                ),
                Some(RendererCursorShape::Block)
            );
        }
    }

    assert_eq!(
        renderer_cursor_shape(
            RendererCursorState {
                in_viewport: false,
                ..RendererCursorState::default()
            },
            RendererCursorOptions {
                preedit: true,
                focused: true,
                blink_visible: true,
            },
        ),
        None
    );
}

fn assert_cursor_shapes(
    state: RendererCursorState,
    cases: &[((bool, bool), Option<RendererCursorShape>)],
) {
    for ((focused, blink_visible), expected) in cases {
        assert_eq!(
            renderer_cursor_shape(
                state,
                RendererCursorOptions {
                    focused: *focused,
                    blink_visible: *blink_visible,
                    ..RendererCursorOptions::default()
                },
            ),
            *expected
        );
    }
}

fn render_frame(cells: Vec<RenderCell>) -> RenderFrame {
    RenderFrame {
        cols: 2,
        rows: 2,
        dirty: Dirty::Full,
        colors: FrameColors {
            background: rgb(1, 2, 3),
            foreground: rgb(220, 221, 222),
            cursor: Some(rgb(9, 10, 11)),
            ..Default::default()
        },
        cursor: Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: None,
        }),
        row_dirty: vec![true, true],
        row_wraps: vec![false, false],
        search_matches: Vec::new(),
        active_search_match: None,
        active_search_match_index: None,
        search_match_count: 0,
        search_pulse: 0,
        copy_mode: None,
        selections: Vec::new(),
        cells,
        text: vec!['A', 'B', 'C'],
        images: Default::default(),
        scrollbar: None,
        stats: FrameStats {
            cells: 3,
            chars: 3,
            dirty_rows: 2,
            ..Default::default()
        },
    }
}

trait WithText {
    fn with_text(self, text: Vec<char>) -> Self;
}

impl WithText for RenderFrame {
    fn with_text(mut self, text: Vec<char>) -> Self {
        self.text = text;
        self.stats.chars = self.text.len();
        self
    }
}

fn cell(x: u16, y: u16, text_start: usize, text_len: usize, style: CellStyle) -> RenderCell {
    RenderCell {
        x,
        y,
        text_start,
        text_len,
        fg: None,
        bg: None,
        style,
        hyperlink: None,
    }
}

fn style_with_underline() -> CellStyle {
    CellStyle {
        underline: Underline::Single,
        ..Default::default()
    }
}

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}
