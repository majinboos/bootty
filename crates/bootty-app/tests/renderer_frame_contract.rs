use bootty_app::{
    geometry::{CellMetrics, TerminalPadding, TerminalSurface},
    paint_plan::{CursorBlinkPhase, PaintPlanner},
    renderer_frame::{
        GhosttyGraphicsElement, MinimumContrastPolicy, RendererCellGraphics, RendererCursorOptions,
        RendererCursorShape, RendererCursorState, RendererFrame, RendererLinkHighlight,
        RendererLinkMods, RendererLinkPattern, RendererPreedit, RendererPreeditCodepoint,
        RendererPreeditRange, RendererSelectionIntent, renderer_cell_constraint_width,
        renderer_cursor_shape,
    },
    terminal::{CellStyle, CursorSnapshot, FrameColors, FrameStats, RenderCell, RenderFrame},
    terminal_text::TerminalTextConfig,
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
fn renderer_state_preedit_range_covers_exact_cell_width() {
    let ascii = RendererPreedit {
        codepoints: vec![RendererPreeditCodepoint {
            codepoint: 'a',
            wide: false,
        }],
    };
    let hangul = RendererPreedit {
        codepoints: vec![RendererPreeditCodepoint {
            codepoint: '\u{AC00}',
            wide: true,
        }],
    };

    assert_eq!(
        ascii.range(2, 9),
        RendererPreeditRange {
            start: 2,
            end: 2,
            codepoint_offset: 0,
        }
    );
    assert_eq!(
        hangul.range(2, 9),
        RendererPreeditRange {
            start: 2,
            end: 3,
            codepoint_offset: 0,
        }
    );
    assert_eq!(ascii.width(), 1);
    assert_eq!(hangul.width(), 2);
}

#[test]
fn renderer_state_preedit_range_shifts_left_at_right_edge() {
    let preedit = RendererPreedit {
        codepoints: vec![RendererPreeditCodepoint {
            codepoint: '\u{AC00}',
            wide: true,
        }],
    };

    assert_eq!(
        preedit.range(9, 9),
        RendererPreeditRange {
            start: 8,
            end: 9,
            codepoint_offset: 0,
        }
    );
}

#[test]
fn renderer_cell_constraint_widths_match_upstream_symbol_cases() {
    let cases = [
        ("symbol->nothing", "\u{E8EF}", 0, 2),
        ("symbol->character", "\u{E8EF}z", 0, 1),
        ("symbol->space", "\u{E8EF} z", 0, 2),
        ("symbol->no-break space", "\u{E8EF}\u{00A0}z", 0, 1),
        ("symbol->end of row", "   \u{E8EF}", 3, 1),
        ("character->symbol", "z\u{E8EF}", 1, 2),
        ("symbol->symbol first", "\u{E8EF}\u{E8EF}", 0, 1),
        ("symbol->symbol second", "\u{E8EF}\u{E8EF}", 1, 1),
        ("symbol->space->symbol first", "\u{E8EF} \u{E8EF}", 0, 2),
        ("symbol->space->symbol second", "\u{E8EF} \u{E8EF}", 2, 2),
        ("symbol->powerline", "\u{E8EF}\u{E0B0}", 0, 1),
        ("powerline->symbol", "\u{E0B2}\u{E8EF}", 1, 2),
        ("powerline->nothing", "\u{E0B2}", 0, 2),
        ("powerline->space", "\u{E0B2} z", 0, 2),
    ];

    for (name, text, index, expected) in cases {
        assert_eq!(constraint_width_for_text(text, index), expected, "{name}");
    }
}

#[test]
fn renderer_link_cell_map_ports_always_hover_and_modifier_cases() {
    let frame = renderer_link_frame("1ABCD2EFGH3IJKL");
    let links = [
        RendererLinkPattern {
            pattern: "AB".to_owned(),
            highlight: RendererLinkHighlight::Always,
        },
        RendererLinkPattern {
            pattern: "EF".to_owned(),
            highlight: RendererLinkHighlight::Always,
        },
    ];

    let map = frame.link_cell_map(&links, None, RendererLinkMods::default());
    assert!(!map.contains(0, 0));
    assert!(map.contains(1, 0));
    assert!(map.contains(2, 0));
    assert!(!map.contains(3, 0));
    assert!(map.contains(1, 1));
    assert!(!map.contains(1, 2));

    let links = [
        RendererLinkPattern {
            pattern: "AB".to_owned(),
            highlight: RendererLinkHighlight::Hover,
        },
        RendererLinkPattern {
            pattern: "EF".to_owned(),
            highlight: RendererLinkHighlight::Always,
        },
    ];
    let not_hovering = frame.link_cell_map(&links, None, RendererLinkMods::default());
    assert!(!not_hovering.contains(1, 0));
    assert!(!not_hovering.contains(2, 0));
    assert!(not_hovering.contains(1, 1));

    let hovering = frame.link_cell_map(
        &links,
        Some(bootty_app::renderer_frame::RendererCellPoint { x: 1, y: 0 }),
        RendererLinkMods::default(),
    );
    assert!(hovering.contains(1, 0));
    assert!(hovering.contains(2, 0));
    assert!(hovering.contains(1, 1));

    let links = [
        RendererLinkPattern {
            pattern: "AB".to_owned(),
            highlight: RendererLinkHighlight::Always,
        },
        RendererLinkPattern {
            pattern: "EF".to_owned(),
            highlight: RendererLinkHighlight::AlwaysWithMods(RendererLinkMods {
                ctrl: true,
                ..RendererLinkMods::default()
            }),
        },
    ];
    let without_ctrl = frame.link_cell_map(&links, None, RendererLinkMods::default());
    assert!(without_ctrl.contains(1, 0));
    assert!(without_ctrl.contains(2, 0));
    assert!(!without_ctrl.contains(1, 1));
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

fn constraint_width_for_text(text: &str, index: usize) -> u16 {
    const COLS: u16 = 4;
    let mut frame_text = Vec::new();
    let mut cells = Vec::new();
    for (x, ch) in text.chars().enumerate() {
        let text_start = frame_text.len();
        frame_text.push(ch);
        cells.push(cell(x as u16, 0, text_start, 1, CellStyle::default()));
    }
    for x in text.chars().count()..usize::from(COLS) {
        cells.push(cell(x as u16, 0, frame_text.len(), 0, CellStyle::default()));
    }
    let mut frame = render_frame(cells).with_text(frame_text);
    frame.cols = COLS;
    frame.rows = 1;
    frame.row_dirty = vec![true];

    let surface = TerminalSurface::for_logical_size(
        f32::from(COLS) * 10.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let renderer_frame = RendererFrame::from_terminal(
        &frame,
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    );

    renderer_cell_constraint_width(&renderer_frame.cells, index, usize::from(COLS))
}

fn renderer_link_frame(text: &str) -> RendererFrame {
    const COLS: u16 = 5;
    const ROWS: u16 = 3;
    let mut frame_text = Vec::new();
    let mut cells = Vec::new();
    for (index, ch) in text.chars().enumerate() {
        let text_start = frame_text.len();
        frame_text.push(ch);
        cells.push(cell(
            (index % usize::from(COLS)) as u16,
            (index / usize::from(COLS)) as u16,
            text_start,
            1,
            CellStyle::default(),
        ));
    }
    let mut frame = render_frame(cells).with_text(frame_text);
    frame.cols = COLS;
    frame.rows = ROWS;
    frame.cursor = None;
    frame.row_dirty = vec![true; usize::from(ROWS)];
    let surface = TerminalSurface::for_logical_size(
        f32::from(COLS) * 10.0,
        f32::from(ROWS) * 20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    RendererFrame::from_terminal(
        &frame,
        surface,
        &TerminalTextConfig::with_cell_metrics(surface.cell),
    )
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
