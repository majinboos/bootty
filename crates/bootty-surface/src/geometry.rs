use eframe::egui::{Pos2, Rect, Vec2};

pub const COMPARISON_GHOSTTY_FONT_POINTS_MACOS: f32 = 11.75;
pub const DEFAULT_FONT_DPI: f32 = 96.0;
pub const DEFAULT_FONT_SIZE: f32 = COMPARISON_GHOSTTY_FONT_POINTS_MACOS * DEFAULT_FONT_DPI / 72.0;
pub const DEFAULT_CELL_WIDTH: f32 = 10.0;
pub const DEFAULT_LINE_HEIGHT: f32 = 22.0;
pub const DEFAULT_PADDING: f32 = 0.0;
pub const MIN_COLS: u16 = 20;
pub const MIN_ROWS: u16 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalGeometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl TerminalGeometry {
    pub fn pixel_width(self) -> u16 {
        self.cols
            .saturating_mul(self.cell_width.min(u32::from(u16::MAX)) as u16)
    }

    pub fn pixel_height(self) -> u16 {
        self.rows
            .saturating_mul(self.cell_height.min(u32::from(u16::MAX)) as u16)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
}

impl CellMetrics {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: width.max(1.0),
            height: height.max(1.0),
        }
    }

    pub fn rounded_size(self) -> (u32, u32) {
        (
            self.width.ceil().max(1.0) as u32,
            self.height.ceil().max(1.0) as u32,
        )
    }
}

impl Default for CellMetrics {
    fn default() -> Self {
        Self::new(DEFAULT_CELL_WIDTH, DEFAULT_LINE_HEIGHT)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalPadding {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl TerminalPadding {
    pub fn uniform(value: f32) -> Self {
        let value = value.max(0.0);
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }

    pub fn rounded(self) -> RoundedPadding {
        RoundedPadding {
            top: self.top.round().max(0.0) as u32,
            right: self.right.round().max(0.0) as u32,
            bottom: self.bottom.round().max(0.0) as u32,
            left: self.left.round().max(0.0) as u32,
        }
    }
}

impl Default for TerminalPadding {
    fn default() -> Self {
        Self::uniform(DEFAULT_PADDING)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundedPadding {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

impl RoundedPadding {
    pub fn balanced(
        width: u32,
        height: u32,
        grid: GridDimensions,
        cell: RoundedCellMetrics,
    ) -> Self {
        let grid_width = u32::from(grid.cols).saturating_mul(cell.width);
        let grid_height = u32::from(grid.rows).saturating_mul(cell.height);
        let horizontal = width.saturating_sub(grid_width) / 2;
        let vertical = height.saturating_sub(grid_height) / 2;

        Self {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundedCellMetrics {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridDimensions {
    pub cols: u16,
    pub rows: u16,
}

impl GridDimensions {
    pub fn for_pixels(width: u32, height: u32, cell: RoundedCellMetrics) -> Self {
        Self {
            cols: ((width / cell.width.max(1)).max(1)).min(u32::from(u16::MAX)) as u16,
            rows: ((height / cell.height.max(1)).max(1)).min(u32::from(u16::MAX)) as u16,
        }
    }

    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaddingBalance {
    Equal,
    CappedTop,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfacePoint {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SurfaceRect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl SurfaceRect {
    pub fn from_egui(rect: Rect) -> Self {
        Self {
            min_x: rect.min.x,
            min_y: rect.min.y,
            max_x: rect.max.x,
            max_y: rect.max.y,
        }
    }

    pub fn from_min_size(min_x: f32, min_y: f32, width: f32, height: f32) -> Self {
        Self {
            min_x,
            min_y,
            max_x: min_x + width,
            max_y: min_y + height,
        }
    }

    pub fn width(self) -> f32 {
        self.max_x - self.min_x
    }

    pub fn height(self) -> f32 {
        self.max_y - self.min_y
    }
}

/// Render-level magnification for pinch-to-zoom; scales geometry without reflowing the grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewTransform {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl ViewTransform {
    pub const IDENTITY: Self = Self {
        zoom: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
    };
    pub const MAX_ZOOM: f32 = 5.0;
    pub const MAX_SUPERSAMPLE: f32 = 3.0;

    pub fn is_zoomed(self) -> bool {
        self.zoom > 1.0 + f32::EPSILON
    }

    // Quantized to whole steps so the glyph atlas re-rasterizes only at integer zoom crossings,
    // not every frame of a pinch.
    pub fn raster_supersample(self) -> f32 {
        self.zoom.ceil().clamp(1.0, Self::MAX_SUPERSAMPLE)
    }

    pub fn applied_to(self, surface: SurfaceRect) -> SurfaceRect {
        if !self.is_zoomed() && self.pan_x == 0.0 && self.pan_y == 0.0 {
            return surface;
        }
        let inv = 1.0 / self.zoom;
        SurfaceRect::from_min_size(
            (surface.min_x - self.pan_x) * inv,
            (surface.min_y - self.pan_y) * inv,
            surface.width() * inv,
            surface.height() * inv,
        )
    }

    pub fn pinched(self, factor: f32, focal: Pos2, surface: SurfaceRect) -> Self {
        let new_zoom = (self.zoom * factor).clamp(1.0, Self::MAX_ZOOM);
        if new_zoom == self.zoom {
            return self;
        }
        let ratio = new_zoom / self.zoom;
        Self {
            zoom: new_zoom,
            pan_x: focal.x - (focal.x - self.pan_x) * ratio,
            pan_y: focal.y - (focal.y - self.pan_y) * ratio,
        }
        .clamped(surface)
    }

    pub fn panned(self, delta: Vec2, surface: SurfaceRect) -> Self {
        Self {
            zoom: self.zoom,
            pan_x: self.pan_x + delta.x,
            pan_y: self.pan_y + delta.y,
        }
        .clamped(surface)
    }

    pub fn inverse_point(self, point: Pos2) -> Pos2 {
        Pos2::new(
            (point.x - self.pan_x) / self.zoom,
            (point.y - self.pan_y) / self.zoom,
        )
    }

    fn clamped(self, surface: SurfaceRect) -> Self {
        let span = 1.0 - self.zoom;
        let (lo_x, hi_x) = (surface.max_x * span, surface.min_x * span);
        let (lo_y, hi_y) = (surface.max_y * span, surface.min_y * span);
        Self {
            zoom: self.zoom,
            pan_x: self.pan_x.clamp(lo_x.min(hi_x), lo_x.max(hi_x)),
            pan_y: self.pan_y.clamp(lo_y.min(hi_y), lo_y.max(hi_y)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseSurfaceMetrics {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding: RoundedPadding,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalSurface {
    pub rect: Rect,
    pub padding: TerminalPadding,
    pub cell: CellMetrics,
}

impl TerminalSurface {
    pub fn new(rect: Rect, cell: CellMetrics, padding: TerminalPadding) -> Self {
        Self {
            rect,
            cell,
            padding,
        }
    }

    pub fn for_rect(rect: Rect, cell: CellMetrics) -> Self {
        Self::new(rect, cell, TerminalPadding::default())
    }

    pub fn for_size(size: Vec2, cell: CellMetrics, padding: TerminalPadding) -> Self {
        Self::new(Rect::from_min_size(Pos2::ZERO, size), cell, padding)
    }

    pub fn for_logical_size(
        width: f32,
        height: f32,
        cell: CellMetrics,
        padding: TerminalPadding,
    ) -> Self {
        Self::for_size(Vec2::new(width, height), cell, padding)
    }

    pub fn default_for_size(size: Vec2) -> Self {
        Self::for_size(size, CellMetrics::default(), TerminalPadding::default())
    }

    pub fn geometry(self) -> TerminalGeometry {
        geometry_for_size(self.rect.size(), self.cell, self.padding)
    }

    pub fn cell_size(self) -> (u32, u32) {
        self.cell.rounded_size()
    }

    pub fn rounded_cell(self) -> RoundedCellMetrics {
        let (width, height) = self.cell_size();
        RoundedCellMetrics { width, height }
    }

    pub fn content_origin(self) -> SurfacePoint {
        SurfacePoint {
            x: self.rect.min.x + self.padding.left,
            y: self.rect.min.y + self.padding.top,
        }
    }

    pub fn surface_rect(self) -> SurfaceRect {
        SurfaceRect::from_egui(self.rect)
    }

    pub fn grid_rect(self, cols: u16, rows: u16) -> SurfaceRect {
        let origin = self.content_origin();
        SurfaceRect::from_min_size(
            origin.x,
            origin.y,
            f32::from(cols) * self.cell.width,
            f32::from(rows) * self.cell.height,
        )
    }

    pub fn raw_grid_size(self) -> GridDimensions {
        let geometry = self.geometry();
        GridDimensions::new(geometry.cols, geometry.rows)
    }

    pub fn balanced_padding(
        self,
        explicit: TerminalPadding,
        mode: PaddingBalance,
    ) -> RoundedPadding {
        let width = self.rect.width().max(0.0).round() as u32;
        let height = self.rect.height().max(0.0).round() as u32;
        let cell = self.rounded_cell();
        let explicit = explicit.rounded();
        let explicit_horizontal = explicit.left.saturating_add(explicit.right);
        let explicit_vertical = explicit.top.saturating_add(explicit.bottom);
        let grid = GridDimensions::for_pixels(
            width.saturating_sub(explicit_horizontal),
            height.saturating_sub(explicit_vertical),
            cell,
        );
        let mut padding = RoundedPadding::balanced(width, height, grid, cell);

        if mode == PaddingBalance::CappedTop {
            let max_top = explicit_horizontal.saturating_add(cell.width) / 2;
            let shift = padding.top.saturating_sub(max_top);
            padding.top -= shift;
            padding.bottom += shift;
        }

        padding
    }

    pub fn surface_to_grid(self, point: SurfacePoint) -> GridPoint {
        let origin = self.content_origin();
        let grid = self.raw_grid_size();
        let x = ((point.x - origin.x).max(0.0) / self.cell.width).floor();
        let y = ((point.y - origin.y).max(0.0) / self.cell.height).floor();
        GridPoint {
            x: (x as u16).min(grid.cols.saturating_sub(1)),
            y: (y as u16).min(grid.rows.saturating_sub(1)),
        }
    }

    pub fn cell_rect(self, col: u16, row: u16) -> SurfaceRect {
        let origin = self.content_origin();
        SurfaceRect::from_min_size(
            origin.x + f32::from(col) * self.cell.width,
            origin.y + f32::from(row) * self.cell.height,
            self.cell.width,
            self.cell.height,
        )
    }

    pub fn run_rect(self, start_col: u16, row: u16, cells: u16) -> SurfaceRect {
        let origin = self.content_origin();
        SurfaceRect::from_min_size(
            origin.x + f32::from(start_col) * self.cell.width,
            origin.y + f32::from(row) * self.cell.height,
            f32::from(cells) * self.cell.width,
            self.cell.height,
        )
    }

    pub fn relative_position(self, pos: Pos2) -> Option<SurfacePoint> {
        if !self.rect.contains(pos) {
            return None;
        }

        Some(SurfacePoint {
            x: pos.x - self.rect.min.x,
            y: pos.y - self.rect.min.y,
        })
    }

    pub fn mouse_position(self, pos: Pos2) -> Option<SurfacePoint> {
        let position = self.relative_position(pos)?;
        let rounded_cell = self.rounded_cell();
        let padding = self.padding.rounded();
        Some(SurfacePoint {
            x: mouse_axis_position(
                position.x,
                self.padding.left,
                padding.left,
                self.cell.width,
                rounded_cell.width,
            ),
            y: mouse_axis_position(
                position.y,
                self.padding.top,
                padding.top,
                self.cell.height,
                rounded_cell.height,
            ),
        })
    }

    pub fn mouse_metrics(self) -> MouseSurfaceMetrics {
        let geometry = self.geometry();
        let padding = self.padding.rounded();
        MouseSurfaceMetrics {
            screen_width: u32::from(geometry.cols)
                .saturating_mul(geometry.cell_width)
                .saturating_add(padding.left)
                .saturating_add(padding.right),
            screen_height: u32::from(geometry.rows)
                .saturating_mul(geometry.cell_height)
                .saturating_add(padding.top)
                .saturating_add(padding.bottom),
            cell_width: geometry.cell_width,
            cell_height: geometry.cell_height,
            padding,
        }
    }
}

fn mouse_axis_position(
    position: f32,
    rendered_padding: f32,
    rounded_padding: u32,
    rendered_cell: f32,
    rounded_cell: u32,
) -> f32 {
    let rounded_padding = rounded_padding as f32;
    let content = position - rendered_padding;
    if content <= 0.0 {
        return if rendered_padding > 0.0 {
            position * (rounded_padding / rendered_padding)
        } else {
            position
        };
    }

    rounded_padding + content * (rounded_cell as f32 / rendered_cell.max(1.0))
}

pub fn geometry_for_size(
    size: Vec2,
    cell: CellMetrics,
    padding: TerminalPadding,
) -> TerminalGeometry {
    geometry_for_pixels(size.x, size.y, cell, padding)
}

pub fn geometry_for_pixels(
    width: f32,
    height: f32,
    cell: CellMetrics,
    padding: TerminalPadding,
) -> TerminalGeometry {
    let cols = ((width - padding.horizontal()) / cell.width)
        .floor()
        .max(f32::from(MIN_COLS)) as u16;
    let rows = ((height - padding.vertical()) / cell.height)
        .floor()
        .max(f32::from(MIN_ROWS)) as u16;
    let (cell_width, cell_height) = cell.rounded_size();

    TerminalGeometry {
        cols,
        rows,
        cell_width,
        cell_height,
    }
}

pub fn fit_cell_height_to_available_space(
    height: f32,
    cell: CellMetrics,
    padding: TerminalPadding,
) -> CellMetrics {
    let available_height = (height - padding.vertical()).max(0.0);
    if !available_height.is_finite() || available_height <= 0.0 {
        return cell;
    }

    let rows = f32::from(geometry_for_pixels(0.0, height, cell, padding).rows);
    CellMetrics::new(cell.width, available_height / rows)
}

/// Stretch the cell width so the whole-number column count exactly fills the available width,
/// distributing the trailing remainder across columns instead of leaving a dead strip on the right.
/// This matters most with split panes, where arbitrary widths rarely divide evenly.
pub fn fit_cell_width_to_available_space(
    width: f32,
    cell: CellMetrics,
    padding: TerminalPadding,
) -> CellMetrics {
    let available_width = (width - padding.horizontal()).max(0.0);
    if !available_width.is_finite() || available_width <= 0.0 {
        return cell;
    }

    let cols = f32::from(geometry_for_pixels(width, 0.0, cell, padding).cols);
    CellMetrics::new(available_width / cols, cell.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn surface_geometry_includes_rounded_cell_size() {
        let surface = TerminalSurface::default_for_size(Vec2::new(1000.0, 672.0));
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
        let rect = Rect::from_min_max(Pos2::new(20.0, 40.0), Pos2::new(220.0, 140.0));
        let surface = TerminalSurface::for_rect(rect, CellMetrics::new(9.0, 22.0));

        assert_eq!(
            surface.relative_position(Pos2::new(35.0, 70.0)),
            Some(SurfacePoint { x: 15.0, y: 30.0 })
        );
        assert_eq!(surface.relative_position(Pos2::new(10.0, 70.0)), None);
    }

    #[test]
    fn grid_rect_matches_rendered_frame_cell_extent() {
        let surface = TerminalSurface::for_size(
            Vec2::new(400.0, 300.0),
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

        let base_geometry =
            geometry_for_pixels(1000.0, 1159.0, base_cell, TerminalPadding::default());
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

        let base_geometry =
            geometry_for_pixels(1007.0, 800.0, base_cell, TerminalPadding::default());
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
        let surface = TerminalSurface::for_size(
            Vec2::new(1050.0, 850.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::default(),
        );

        let padding =
            surface.balanced_padding(TerminalPadding::uniform(4.0), PaddingBalance::Equal);

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
        let surface = TerminalSurface::for_size(
            Vec2::new(1090.0, 1070.0),
            CellMetrics::new(20.0, 40.0),
            TerminalPadding::default(),
        );

        let padding =
            surface.balanced_padding(TerminalPadding::default(), PaddingBalance::CappedTop);

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
        let surface = TerminalSurface::for_size(
            Vec2::new(100.0, 100.0),
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
            let surface = TerminalSurface::for_size(
                Vec2::new(width as f32, height as f32),
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
        let focal = Pos2::new(200.0, 150.0);
        let before = ViewTransform::IDENTITY;
        let under_cursor = before.inverse_point(focal);
        let after = before.pinched(2.0, focal, surface);
        let redisplayed = Pos2::new(
            under_cursor.x * after.zoom + after.pan_x,
            under_cursor.y * after.zoom + after.pan_y,
        );
        assert!((redisplayed.x - focal.x).abs() < 1e-3);
        assert!((redisplayed.y - focal.y).abs() < 1e-3);
    }

    #[test]
    fn pinch_clamps_zoom_to_the_maximum() {
        let surface = SurfaceRect::from_min_size(0.0, 0.0, 800.0, 600.0);
        let view = ViewTransform::IDENTITY.pinched(100.0, Pos2::new(400.0, 300.0), surface);
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
        let forward = zoomed.panned(Vec2::new(10_000.0, 10_000.0), surface);
        assert_eq!((forward.pan_x, forward.pan_y), (0.0, 0.0));
        let backward = zoomed.panned(Vec2::new(-10_000.0, -10_000.0), surface);
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
        let zoomed = ViewTransform::IDENTITY.pinched(3.0, Pos2::new(600.0, 400.0), surface);
        assert!(zoomed.is_zoomed());
        let reset = zoomed.pinched(0.01, Pos2::new(600.0, 400.0), surface);
        assert_eq!(reset.zoom, 1.0);
        assert_eq!((reset.pan_x, reset.pan_y), (0.0, 0.0));
    }
}
