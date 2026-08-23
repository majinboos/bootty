use std::sync::Arc;

use anyhow::Result;
use bootty_app::renderer::{CursorEmphasis, RendererMetrics, TerminalFrameSource, TerminalWidget};
use bootty_render::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface, ViewTransform},
    terminal_text::TerminalTextConfig,
};
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_frame::RenderFrame,
    terminal_image::{KittyImageLayer, placement_destination},
};
use eframe::{
    egui::{self, CursorIcon, Event, Modifiers, Pos2, RawInput, Rect, Vec2},
    wgpu,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

struct CursorTerminal {
    engine: TerminalEngine,
}

impl TerminalFrameSource for CursorTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.engine.set_display_scale(display_scale);
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.engine.set_render_cell_metrics(cell);
        Ok(())
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.engine.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(self.engine.extract_frame()?.clone()))
    }
}

fn terminal_cursor_icon(mouse_tracking: bool, shift: bool, configured: CursorIcon) -> CursorIcon {
    let mut terminal = CursorTerminal {
        engine: TerminalEngine::new(TerminalWidget::initial_geometry()).expect("terminal engine"),
    };
    if mouse_tracking {
        terminal.engine.write_vt(b"\x1b[?1003h\x1b[?1006h");
    }
    let mut widget = TerminalWidget::new(Some(wgpu::TextureFormat::Bgra8Unorm))
        .with_text_config(TerminalTextConfig::default());
    widget.set_terminal_cursor_icon(configured);
    let ctx = egui::Context::default();
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
    let mut cursor_icon = CursorIcon::Default;
    for x in [399.0, 400.0] {
        let mut output = ctx.run_ui(
            RawInput {
                screen_rect: Some(rect),
                events: vec![
                    Event::ModifiersChanged(Modifiers {
                        shift,
                        ..Modifiers::default()
                    }),
                    Event::PointerMoved(Pos2::new(x, 300.0)),
                ],
                ..RawInput::default()
            },
            |ui| {
                widget
                    .show_at_rect(ui, ui.max_rect(), "cursor-test", &mut terminal)
                    .expect("terminal widget");
            },
        );
        cursor_icon = output.platform_output.cursor_icon;
        output.textures_delta.clear();
    }
    cursor_icon
}

#[test]
fn renderer_starts_with_stable_geometry_and_empty_runtime_state() {
    let widget = TerminalWidget::new(None);

    assert_eq!(
        TerminalWidget::initial_geometry(),
        TerminalGeometry {
            cols: 100,
            rows: 30,
            cell_width: 10,
            cell_height: 22,
        }
    );
    assert_eq!(widget.view_transform(), ViewTransform::IDENTITY);
    assert!(!widget.is_zoomed());
    assert_eq!(widget.metrics(), RendererMetrics::default());
}

#[rstest]
#[case::shell(false, false, CursorIcon::Text, CursorIcon::Text)]
#[case::tui(true, false, CursorIcon::Text, CursorIcon::Default)]
#[case::tui_selection_override(true, true, CursorIcon::Text, CursorIcon::Text)]
#[case::hidden_while_typing(true, false, CursorIcon::None, CursorIcon::None)]
fn terminal_cursor_matches_mouse_event_ownership(
    #[case] mouse_tracking: bool,
    #[case] shift: bool,
    #[case] configured: CursorIcon,
    #[case] expected: CursorIcon,
) {
    assert_eq!(
        terminal_cursor_icon(mouse_tracking, shift, configured),
        expected
    );
}

#[rstest]
#[case::focused(CursorEmphasis::Normal, true)]
#[case::inactive(CursorEmphasis::Inactive, false)]
fn inactive_pane_cursor_does_not_animate(
    #[case] emphasis: CursorEmphasis,
    #[case] expected_blinking: bool,
) {
    let mut terminal = CursorTerminal {
        engine: TerminalEngine::new(TerminalWidget::initial_geometry()).expect("terminal engine"),
    };
    terminal.engine.write_vt(b"\x1b[5 q");
    let mut widget = TerminalWidget::new(Some(wgpu::TextureFormat::Bgra8Unorm))
        .with_text_config(TerminalTextConfig::default());
    let ctx = egui::Context::default();
    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

    let mut output = ctx.run_ui(
        RawInput {
            screen_rect: Some(rect),
            ..RawInput::default()
        },
        |ui| {
            widget
                .show_at_rect_with_cursor_emphasis(
                    ui,
                    ui.max_rect(),
                    "cursor-emphasis-test",
                    &mut terminal,
                    emphasis,
                )
                .expect("terminal widget");
        },
    );
    output.textures_delta.clear();

    assert_eq!(widget.metrics().cursor_blinking, expected_blinking);
}

#[test]
fn configured_cell_metrics_drive_public_geometry_and_survive_reset() {
    let config = TerminalTextConfig {
        cell_width: Some(8.5),
        cell_height: Some(19.25),
        fit_cell_height: false,
        fit_cell_width: false,
        ..TerminalTextConfig::default()
    };
    let mut widget = TerminalWidget::new(None).with_text_config(config);

    assert_eq!(widget.cell_dimensions(), (8.5, 19.25));
    assert_eq!(widget.cell_size(), (9, 20));
    assert_eq!(
        widget.geometry_for_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(850.0, 385.0))),
        TerminalGeometry {
            cols: 100,
            rows: 20,
            cell_width: 9,
            cell_height: 20,
        }
    );

    widget.reset();

    assert_eq!(widget.cell_dimensions(), (8.5, 19.25));
}

#[rstest]
#[case::height(true, false, Vec2::new(1000.0, 1159.0), TerminalGeometry { cols: 100, rows: 52, cell_width: 10, cell_height: 23 })]
#[case::width(false, true, Vec2::new(1007.0, 800.0), TerminalGeometry { cols: 100, rows: 36, cell_width: 11, cell_height: 22 })]
fn fitting_fills_the_enabled_axis_without_changing_the_other(
    #[case] fit_cell_height: bool,
    #[case] fit_cell_width: bool,
    #[case] size: Vec2,
    #[case] expected: TerminalGeometry,
) {
    let widget = TerminalWidget::new(None).with_text_config(TerminalTextConfig {
        cell_width: Some(10.0),
        cell_height: Some(22.0),
        fit_cell_height,
        fit_cell_width,
        ..TerminalTextConfig::default()
    });

    assert_eq!(
        widget.geometry_for_rect(Rect::from_min_size(Pos2::ZERO, size)),
        expected
    );
}

#[test]
fn gestures_wait_for_a_rendered_surface() {
    let mut widget = TerminalWidget::new(None);

    widget.apply_pinch(2.0, Some(Pos2::new(100.0, 100.0)));
    widget.apply_pan(Vec2::new(40.0, -20.0));

    assert_eq!(widget.view_transform(), ViewTransform::IDENTITY);
    assert!(!widget.is_zoomed());
}

#[test]
fn kitty_image_layers_match_terminal_render_order() {
    assert_eq!(
        KittyImageLayer::ordered(),
        [
            KittyImageLayer::BelowBackground,
            KittyImageLayer::BelowText,
            KittyImageLayer::AboveText
        ]
    );
}

fn kitty_placement(
    scale_factor: f32,
    offset_x: u32,
    offset_y: u32,
    columns: u32,
    rows: u32,
) -> bootty_render::geometry::SurfaceRect {
    placement_destination(
        TerminalSurface::for_logical_size(
            120.0,
            80.0,
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::uniform(2.0),
        ),
        libghostty_vt::kitty::graphics::PlacementRenderInfo {
            size: std::mem::size_of::<libghostty_vt::kitty::graphics::PlacementRenderInfo>(),
            pixel_width: 30,
            pixel_height: 40,
            grid_cols: 3,
            grid_rows: 2,
            viewport_col: 4,
            viewport_row: 1,
            viewport_visible: true,
            source_x: 0,
            source_y: 0,
            source_width: 30,
            source_height: 40,
        },
        scale_factor,
        offset_x,
        offset_y,
        columns,
        rows,
    )
}

#[rstest]
#[case::cell_offsets(1.0, 3, 5, 3, 2, (45.0, 27.0, 75.0, 67.0))]
#[case::logical_pixels(2.0, 4, 6, 0, 0, (44.0, 25.0, 59.0, 45.0))]
fn kitty_placement_uses_cells_offsets_and_logical_scale(
    #[case] scale_factor: f32,
    #[case] offset_x: u32,
    #[case] offset_y: u32,
    #[case] columns: u32,
    #[case] rows: u32,
    #[case] expected: (f32, f32, f32, f32),
) {
    let rect = kitty_placement(scale_factor, offset_x, offset_y, columns, rows);
    assert_eq!((rect.min_x, rect.min_y, rect.max_x, rect.max_y), expected);
}
