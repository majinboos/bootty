use bootty_app::{
    bare_host::renderer_parity_gallery_frame,
    geometry::{CellMetrics, TerminalPadding, TerminalSurface},
    paint_plan::PlanColor,
    renderer_frame::{
        MinimumContrastPolicy, RendererFrame, RendererRepaintDecision, RendererSelectionIntent,
    },
    terminal::{CellStyle, CursorSnapshot, FrameColors, FrameStats, RenderCell, RenderFrame},
    terminal_render::{FillRole, TerminalRenderCommand},
    terminal_text::TerminalTextConfig,
};
use libghostty_vt::{
    render::{CursorVisualStyle, Dirty},
    style::RgbColor,
};

#[test]
fn renderer_frame_applies_selection_intent_to_cells() {
    let mut renderer_frame = RendererFrame::from_terminal(
        &frame_with_cells(vec![cell(0, 0, 0, 1, CellStyle::default())]),
        surface(),
        &TerminalTextConfig::with_cell_metrics(surface().cell),
    );
    renderer_frame.select_cells(0, 0..1);

    assert_eq!(
        renderer_frame.cells[0].selection,
        RendererSelectionIntent::Selected {
            foreground: color(1, 2, 3),
            background: color(4, 5, 6),
        }
    );
    let render_frame = renderer_frame
        .to_terminal_render_frame(&TerminalTextConfig::with_cell_metrics(surface().cell));
    assert!(render_frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::CellBackground && fill.color == color(4, 5, 6)
    )));
    assert!(render_frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.attrs.fg == color(1, 2, 3)
    )));
}

#[test]
fn minimum_contrast_adjusts_text_but_skips_graphics_elements() {
    let text = MinimumContrastPolicy::EnforceForText
        .resolve_foreground(color(10, 10, 10), color(12, 12, 12));
    let graphic = MinimumContrastPolicy::SkipForGraphicsElement
        .resolve_foreground(color(10, 10, 10), color(12, 12, 12));

    assert_ne!(text, color(10, 10, 10));
    assert_eq!(graphic, color(10, 10, 10));

    let mut low_contrast = CellStyle::default();
    let mut frame = frame_with_cells(vec![RenderCell {
        fg: Some(rgb(10, 10, 10)),
        bg: Some(rgb(12, 12, 12)),
        ..cell(0, 0, 0, 1, CellStyle::default())
    }]);
    low_contrast.invisible = false;
    frame.cells[0].style = low_contrast;
    let render_frame = RendererFrame::from_terminal(
        &frame,
        surface(),
        &TerminalTextConfig::with_cell_metrics(surface().cell),
    )
    .to_terminal_render_frame(&TerminalTextConfig::with_cell_metrics(surface().cell));
    assert!(render_frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.attrs.fg != color(10, 10, 10)
    )));
}

#[test]
fn renderer_repaint_decision_tracks_dirty_and_blinking_cursor() {
    let mut frame = frame_with_cells(vec![cell(0, 0, 0, 1, CellStyle::default())]);
    frame.dirty = Dirty::Clean;
    frame.cursor = Some(CursorSnapshot {
        x: 0,
        y: 0,
        at_wide_tail: false,
        style: CursorVisualStyle::Block,
        blinking: true,
        color: None,
    });

    let renderer_frame = RendererFrame::from_terminal(
        &frame,
        surface(),
        &TerminalTextConfig::with_cell_metrics(surface().cell),
    );

    assert_eq!(
        renderer_frame.repaint_decision(),
        RendererRepaintDecision::ScheduleBlink
    );
}

#[test]
fn bare_host_parity_gallery_contains_text_sprite_cursor_and_decor() {
    let gallery = renderer_parity_gallery_frame();

    assert!(gallery.cells.iter().any(|cell| cell.text.contains('A')));
    assert!(gallery.cells.iter().any(|cell| !cell.graphics.is_text()));
    assert!(gallery.cursor.is_some());
    assert!(gallery.cells.iter().any(|cell| cell.decor.underline));
    assert!(gallery.cells.iter().any(|cell| cell.decor.overline));
    assert!(
        gallery
            .cells
            .iter()
            .any(|cell| matches!(cell.selection, RendererSelectionIntent::Selected { .. }))
    );
    assert_eq!(gallery.images.placements.len(), 3);
    assert!(gallery.cells.iter().any(|cell| {
        cell.minimum_contrast_policy == MinimumContrastPolicy::EnforceForText
            && cell.foreground == Some(color(10, 10, 10))
            && cell.background == Some(color(12, 12, 12))
    }));
}

fn surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        40.0,
        20.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    )
}

fn frame_with_cells(cells: Vec<RenderCell>) -> RenderFrame {
    RenderFrame {
        cols: 4,
        rows: 1,
        dirty: Dirty::Full,
        colors: FrameColors {
            background: rgb(1, 2, 3),
            foreground: rgb(220, 221, 222),
            cursor: Some(rgb(9, 10, 11)),
            selection_foreground: Some(rgb(1, 2, 3)),
            selection_background: Some(rgb(4, 5, 6)),
            ..Default::default()
        },
        cursor: None,
        row_dirty: vec![true],
        cells,
        text: vec!['A', '█', 'B', 'C'],
        images: Default::default(),
        scrollbar: None,
        stats: FrameStats {
            cells: 4,
            chars: 4,
            dirty_rows: 1,
            ..Default::default()
        },
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

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}
