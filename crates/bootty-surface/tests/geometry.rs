use bootty_surface::geometry::*;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn point(x: f32, y: f32) -> SurfacePoint {
    SurfacePoint { x, y }
}

fn rounded_cell(width: u32, height: u32) -> RoundedCellMetrics {
    RoundedCellMetrics { width, height }
}

fn view(zoom: f32) -> ViewTransform {
    ViewTransform {
        zoom,
        pan_x: 0.0,
        pan_y: 0.0,
    }
}

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

#[rstest]
#[case::retina(CellMetrics::new(10.25, 22.5), 2.0, (21, 45))]
#[case::fractional_scale(CellMetrics::new(8.0, 23.0), 1.5, (12, 35))]
#[case::invalid_scale(CellMetrics::new(8.0, 23.0), f32::NAN, (8, 23))]
fn physical_cell_size_uses_display_scale(
    #[case] cell: CellMetrics,
    #[case] display_scale: f32,
    #[case] expected: (u32, u32),
) {
    assert_eq!(cell.physical_size(display_scale), expected);
}

#[test]
fn relative_position_is_rect_local() {
    let rect = SurfaceRect::from_min_size(20.0, 40.0, 200.0, 100.0);
    let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0));

    assert_eq!(
        surface.relative_position(point(35.0, 70.0)),
        Some(point(15.0, 30.0))
    );
    assert_eq!(surface.relative_position(point(10.0, 70.0)), None);
}

#[test]
fn surface_rect_contains_every_edge_like_egui_rect() {
    let rect = SurfaceRect::from_min_size(20.0, 40.0, 200.0, 100.0);

    assert!(rect.contains(point(20.0, 40.0)));
    assert!(rect.contains(point(220.0, 100.0)));
    assert!(rect.contains(point(100.0, 140.0)));
    assert!(!rect.contains(point(220.001, 100.0)));
    assert!(!rect.contains(point(100.0, 140.001)));
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
fn fitted_cell_dimensions_distribute_remainders_without_changing_the_grid() {
    let base_cell = CellMetrics::new(10.0, 22.0);
    let fitted_height =
        fit_cell_height_to_available_space(1159.0, base_cell, TerminalPadding::default());

    let base_geometry = geometry_for_pixels(1000.0, 1159.0, base_cell, TerminalPadding::default());
    let fitted_geometry =
        geometry_for_pixels(1000.0, 1159.0, fitted_height, TerminalPadding::default());

    assert_eq!(base_geometry.rows, 52);
    assert_eq!(fitted_geometry.rows, 52);
    assert_eq!(fitted_height.width, 10.0);
    assert!((fitted_height.height - 22.288_462).abs() < 0.001);
    assert!((fitted_height.height * 52.0 - 1159.0).abs() < 0.001);

    let fitted_width =
        fit_cell_width_to_available_space(1007.0, base_cell, TerminalPadding::default());

    let base_geometry = geometry_for_pixels(1007.0, 800.0, base_cell, TerminalPadding::default());
    let fitted_geometry =
        geometry_for_pixels(1007.0, 800.0, fitted_width, TerminalPadding::default());

    // Column count is preserved; the width stretches to fill the leftover 7px with no gap.
    assert_eq!(base_geometry.cols, 100);
    assert_eq!(fitted_geometry.cols, 100);
    assert_eq!(fitted_width.height, 22.0);
    assert!((fitted_width.width - 10.07).abs() < 0.001);
    assert!((fitted_width.width * 100.0 - 1007.0).abs() < 0.001);
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
        rounded_cell(10, 20),
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

#[rstest]
#[case(100, 40, rounded_cell(5, 10), GridDimensions { cols: 20, rows: 4 })]
#[case(20, 40, rounded_cell(6, 15), GridDimensions { cols: 3, rows: 2 })]
fn grid_dimensions_floor_to_whole_cells_with_minimum_size(
    #[case] width: u32,
    #[case] height: u32,
    #[case] cell: RoundedCellMetrics,
    #[case] expected: GridDimensions,
) {
    assert_eq!(GridDimensions::for_pixels(width, height, cell), expected);
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
        (GridPoint { x: 0, y: 0 }, point(0.0, 0.0)),
        (GridPoint { x: 1, y: 0 }, point(6.0, 0.0)),
        (GridPoint { x: 1, y: 1 }, point(6.0, 10.0)),
        (GridPoint { x: 0, y: 0 }, point(-10.0, -10.0)),
        (
            GridPoint {
                x: grid.cols - 1,
                y: grid.rows - 1,
            },
            point(100_000.0, 100_000.0),
        ),
    ];

    for (expected, actual) in cases {
        assert_eq!(surface.surface_to_grid(actual), expected);
    }
}

proptest! {
    /// Property: geometry preserves the rounded cell metrics and enforces terminal minima.
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

#[rstest]
#[case(
    ViewTransform::IDENTITY,
    SurfaceRect::from_min_size(10.0, 20.0, 800.0, 600.0),
    SurfaceRect::from_min_size(10.0, 20.0, 800.0, 600.0)
)]
#[case(
    view(2.0),
    SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0),
    SurfaceRect::from_min_size(0.0, 0.0, 400.0, 300.0)
)]
fn view_transform_projects_surface_by_zoom(
    #[case] view: ViewTransform,
    #[case] surface: SurfaceRect,
    #[case] expected: SurfaceRect,
) {
    assert_eq!(view.applied_to(surface), expected);
}

#[test]
fn pinch_keeps_the_surface_point_under_the_cursor_anchored() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let focal = point(200.0, 150.0);
    let before = ViewTransform::IDENTITY;
    let under_cursor = before.inverse_point(focal);
    let after = before.pinched(2.0, focal, surface);
    let redisplayed = point(
        under_cursor.x * after.zoom + after.pan_x,
        under_cursor.y * after.zoom + after.pan_y,
    );
    assert!((redisplayed.x - focal.x).abs() < 1e-3);
    assert!((redisplayed.y - focal.y).abs() < 1e-3);
}

#[test]
fn pan_clamps_so_magnified_content_keeps_covering_the_viewport() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let zoomed = view(2.0);
    let forward = zoomed.panned(10_000.0, 10_000.0, surface);
    assert_eq!((forward.pan_x, forward.pan_y), (0.0, 0.0));
    let backward = zoomed.panned(-10_000.0, -10_000.0, surface);
    assert_eq!((backward.pan_x, backward.pan_y), (-800.0, -600.0));
}

#[test]
fn raster_supersample_is_quantized_and_capped() {
    assert_eq!(ViewTransform::IDENTITY.raster_supersample(), 1.0);
    let zoomed = view(1.2);
    assert_eq!(zoomed.raster_supersample(), 2.0);
    let extreme = view(5.0);
    assert_eq!(extreme.raster_supersample(), ViewTransform::MAX_SUPERSAMPLE);
}

#[test]
fn pinching_back_to_1x_recenters_the_view() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
    let maximum = ViewTransform::IDENTITY.pinched(100.0, point(400.0, 300.0), surface);
    assert_eq!(maximum.zoom, ViewTransform::MAX_ZOOM);
    let zoomed = ViewTransform::IDENTITY.pinched(3.0, point(600.0, 400.0), surface);
    assert!(zoomed.is_zoomed());
    let reset = zoomed.pinched(0.01, point(600.0, 400.0), surface);
    assert_eq!(reset.zoom, 1.0);
    assert_eq!((reset.pan_x, reset.pan_y), (0.0, 0.0));
}
