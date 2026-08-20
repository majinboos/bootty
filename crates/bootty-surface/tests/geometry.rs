use bootty_surface::geometry::*;
use proptest::prelude::*;

#[test]
fn surface_geometry_includes_rounded_cell_size() {
    let surface = TerminalSurface::for_logical_size(
        1000.0,
        672.0,
        CellMetrics::default(),
        TerminalPadding::default(),
    );
    assert_eq!(
        surface.geometry(),
        TerminalGeometry {
            cols: 100,
            rows: 30,
            cell_width: 10,
            cell_height: 22,
        }
    );
}

#[test]
fn relative_position_is_rect_local() {
    let rect = SurfaceRect::from_min_size(20.0, 40.0, 200.0, 100.0);
    let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0));

    assert_eq!(
        surface.relative_position(SurfacePoint { x: 35.0, y: 70.0 }),
        Some(SurfacePoint { x: 15.0, y: 30.0 })
    );
    assert_eq!(
        surface.relative_position(SurfacePoint { x: 10.0, y: 70.0 }),
        None
    );
}

#[test]
fn surface_rect_contains_every_edge_like_egui_rect() {
    let rect = SurfaceRect::from_min_size(20.0, 40.0, 200.0, 100.0);

    assert!(rect.contains(SurfacePoint { x: 20.0, y: 40.0 }));
    assert!(rect.contains(SurfacePoint { x: 220.0, y: 100.0 }));
    assert!(rect.contains(SurfacePoint { x: 100.0, y: 140.0 }));
    assert!(!rect.contains(SurfacePoint {
        x: 220.001,
        y: 100.0
    }));
    assert!(!rect.contains(SurfacePoint {
        x: 100.0,
        y: 140.001
    }));
}

#[test]
fn grid_rect_matches_rendered_frame_cell_extent() {
    let surface = TerminalSurface::for_logical_size(
        400.0,
        300.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::uniform(5.0),
    );

    assert_eq!(
        surface.grid_rect(12, 7),
        SurfaceRect::from_min_size(5.0, 5.0, 120.0, 140.0)
    );
}

#[test]
fn fitted_cell_height_distributes_vertical_remainder_across_rows() {
    let base_cell = CellMetrics::new(10.0, 22.0);
    let fitted_cell =
        fit_cell_height_to_available_space(1159.0, base_cell, TerminalPadding::default());

    let base_geometry = geometry_for_pixels(1000.0, 1159.0, base_cell, TerminalPadding::default());
    let fitted_geometry =
        geometry_for_pixels(1000.0, 1159.0, fitted_cell, TerminalPadding::default());

    assert_eq!(base_geometry.rows, 52);
    assert_eq!(fitted_geometry.rows, 52);
    assert_eq!(fitted_cell.width, 10.0);
    assert!((fitted_cell.height - 22.288_462).abs() < 0.001);
    assert!((fitted_cell.height * 52.0 - 1159.0).abs() < 0.001);
}

#[test]
fn fitted_cell_width_distributes_horizontal_remainder_across_columns() {
    let base_cell = CellMetrics::new(10.0, 22.0);
    let fitted_cell =
        fit_cell_width_to_available_space(1007.0, base_cell, TerminalPadding::default());

    let base_geometry = geometry_for_pixels(1007.0, 800.0, base_cell, TerminalPadding::default());
    let fitted_geometry =
        geometry_for_pixels(1007.0, 800.0, fitted_cell, TerminalPadding::default());

    // Column count is preserved; the width stretches to fill the leftover 7px with no gap.
    assert_eq!(base_geometry.cols, 100);
    assert_eq!(fitted_geometry.cols, 100);
    assert_eq!(fitted_cell.height, 22.0);
    assert!((fitted_cell.width - 10.07).abs() < 0.001);
    assert!((fitted_cell.width * 100.0 - 1007.0).abs() < 0.001);
}

#[test]
fn renderer_size_balanced_padding_equal_distributes_whitespace() {
    let surface = TerminalSurface::for_logical_size(
        1050.0,
        850.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );

    let padding = surface.balanced_padding(TerminalPadding::uniform(4.0), PaddingBalance::Equal);

    assert_eq!(padding.left, padding.right);
    assert_eq!(padding.top, padding.bottom);
    assert!(padding.top > 0);
    assert_eq!(
        padding,
        RoundedPadding {
            top: 5,
            right: 5,
            bottom: 5,
            left: 5,
        }
    );
}

#[test]
fn renderer_size_balanced_padding_capped_top_shifts_excess_to_bottom() {
    let surface = TerminalSurface::for_logical_size(
        1090.0,
        1070.0,
        CellMetrics::new(20.0, 40.0),
        TerminalPadding::default(),
    );

    let padding = surface.balanced_padding(TerminalPadding::default(), PaddingBalance::CappedTop);

    assert_eq!(padding.left, padding.right);
    assert!(padding.top < padding.bottom);
    assert_eq!(padding.top, 10);
    assert_eq!(padding.bottom, 20);
}

#[test]
fn renderer_padding_balanced_on_zero_screen_is_zero() {
    let padding = RoundedPadding::balanced(
        0,
        0,
        GridDimensions {
            cols: 100,
            rows: 37,
        },
        RoundedCellMetrics {
            width: 10,
            height: 20,
        },
    );

    assert_eq!(
        padding,
        RoundedPadding {
            top: 0,
            right: 0,
            bottom: 0,
            left: 0,
        }
    );
}

#[test]
fn grid_dimensions_floor_to_whole_cells_with_minimum_size() {
    for (width, height, cell, expected) in [
        (
            100,
            40,
            RoundedCellMetrics {
                width: 5,
                height: 10,
            },
            GridDimensions { cols: 20, rows: 4 },
        ),
        (
            20,
            40,
            RoundedCellMetrics {
                width: 6,
                height: 15,
            },
            GridDimensions { cols: 3, rows: 2 },
        ),
    ] {
        assert_eq!(GridDimensions::for_pixels(width, height, cell), expected);
    }
}

#[test]
fn surface_to_grid_clamps_to_the_terminal_grid() {
    let surface = TerminalSurface::for_logical_size(
        100.0,
        100.0,
        CellMetrics::new(5.0, 10.0),
        TerminalPadding::default(),
    );
    let grid = surface.raw_grid_size();
    let cases = [
        (GridPoint { x: 0, y: 0 }, SurfacePoint { x: 0.0, y: 0.0 }),
        (GridPoint { x: 1, y: 0 }, SurfacePoint { x: 6.0, y: 0.0 }),
        (GridPoint { x: 1, y: 1 }, SurfacePoint { x: 6.0, y: 10.0 }),
        (
            GridPoint { x: 0, y: 0 },
            SurfacePoint { x: -10.0, y: -10.0 },
        ),
        (
            GridPoint {
                x: grid.cols - 1,
                y: grid.rows - 1,
            },
            SurfacePoint {
                x: 100_000.0,
                y: 100_000.0,
            },
        ),
    ];

    for (expected, actual) in cases {
        assert_eq!(surface.surface_to_grid(actual), expected);
    }
}

proptest! {
    #[test]
    fn property_geometry_never_drops_below_terminal_minimums(
        width in 0_u32..5000,
        height in 0_u32..5000,
        cell_width in 1_u32..80,
        cell_height in 1_u32..80,
        padding in 0_u32..80,
    ) {
        let surface = TerminalSurface::for_logical_size(
            width as f32,
            height as f32,
            CellMetrics::new(cell_width as f32, cell_height as f32),
            TerminalPadding::uniform(padding as f32),
        );
        let geometry = surface.geometry();

        prop_assert!(geometry.cols >= MIN_COLS);
        prop_assert!(geometry.rows >= MIN_ROWS);
        prop_assert_eq!(geometry.cell_width, cell_width);
        prop_assert_eq!(geometry.cell_height, cell_height);
    }
}

#[test]
fn view_transform_projects_surface_by_zoom() {
    for (view, surface, expected) in [
        (
            ViewTransform::IDENTITY,
            SurfaceRect::from_min_size(10.0, 20.0, 800.0, 600.0),
            SurfaceRect::from_min_size(10.0, 20.0, 800.0, 600.0),
        ),
        (
            ViewTransform {
                zoom: 2.0,
                pan_x: 0.0,
                pan_y: 0.0,
            },
            SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0),
            SurfaceRect::from_min_size(0.0, 0.0, 400.0, 300.0),
        ),
    ] {
        assert_eq!(view.applied_to(surface), expected);
    }
}

#[test]
fn pinch_keeps_the_surface_point_under_the_cursor_anchored() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let focal = SurfacePoint { x: 200.0, y: 150.0 };
    let before = ViewTransform::IDENTITY;
    let under_cursor = before.inverse_point(focal);
    let after = before.pinched(2.0, focal, surface);
    let redisplayed = SurfacePoint {
        x: under_cursor.x * after.zoom + after.pan_x,
        y: under_cursor.y * after.zoom + after.pan_y,
    };
    assert!((redisplayed.x - focal.x).abs() < 1e-3);
    assert!((redisplayed.y - focal.y).abs() < 1e-3);
}

#[test]
fn pinch_clamps_zoom_to_the_maximum() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let view = ViewTransform::IDENTITY.pinched(100.0, SurfacePoint { x: 400.0, y: 300.0 }, surface);
    assert_eq!(view.zoom, ViewTransform::MAX_ZOOM);
}

#[test]
fn pan_clamps_so_magnified_content_keeps_covering_the_viewport() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let zoomed = ViewTransform {
        zoom: 2.0,
        pan_x: 0.0,
        pan_y: 0.0,
    };
    let forward = zoomed.panned(10_000.0, 10_000.0, surface);
    assert_eq!((forward.pan_x, forward.pan_y), (0.0, 0.0));
    let backward = zoomed.panned(-10_000.0, -10_000.0, surface);
    assert_eq!((backward.pan_x, backward.pan_y), (-800.0, -600.0));
}

#[test]
fn raster_supersample_is_quantized_and_capped() {
    assert_eq!(ViewTransform::IDENTITY.raster_supersample(), 1.0);
    let zoomed = ViewTransform {
        zoom: 1.2,
        pan_x: 0.0,
        pan_y: 0.0,
    };
    assert_eq!(zoomed.raster_supersample(), 2.0);
    let extreme = ViewTransform {
        zoom: 5.0,
        pan_x: 0.0,
        pan_y: 0.0,
    };
    assert_eq!(extreme.raster_supersample(), ViewTransform::MAX_SUPERSAMPLE);
}

#[test]
fn pinching_back_to_1x_recenters_the_view() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let zoomed = ViewTransform::IDENTITY.pinched(3.0, SurfacePoint { x: 600.0, y: 400.0 }, surface);
    assert!(zoomed.is_zoomed());
    let reset = zoomed.pinched(0.01, SurfacePoint { x: 600.0, y: 400.0 }, surface);
    assert_eq!(reset.zoom, 1.0);
    assert_eq!((reset.pan_x, reset.pan_y), (0.0, 0.0));
}
