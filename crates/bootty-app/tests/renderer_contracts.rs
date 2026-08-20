use bootty_app::{
    geometry::{TerminalGeometry, ViewTransform},
    renderer::{RendererMetrics, TerminalWidget},
    terminal_text::TerminalTextConfig,
};
use eframe::egui::{Pos2, Rect, Vec2};

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

#[test]
fn height_fitting_fills_the_available_rows_without_changing_columns() {
    let widget = TerminalWidget::new(None).with_text_config(TerminalTextConfig {
        cell_width: Some(10.0),
        cell_height: Some(22.0),
        fit_cell_height: true,
        fit_cell_width: false,
        ..TerminalTextConfig::default()
    });

    assert_eq!(
        widget.geometry_for_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 1159.0))),
        TerminalGeometry {
            cols: 100,
            rows: 52,
            cell_width: 10,
            cell_height: 23,
        }
    );
}

#[test]
fn width_fitting_fills_the_available_columns_without_changing_rows() {
    let widget = TerminalWidget::new(None).with_text_config(TerminalTextConfig {
        cell_width: Some(10.0),
        cell_height: Some(22.0),
        fit_cell_height: false,
        fit_cell_width: true,
        ..TerminalTextConfig::default()
    });

    assert_eq!(
        widget.geometry_for_rect(Rect::from_min_size(Pos2::ZERO, Vec2::new(1007.0, 800.0))),
        TerminalGeometry {
            cols: 100,
            rows: 36,
            cell_width: 11,
            cell_height: 22,
        }
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
