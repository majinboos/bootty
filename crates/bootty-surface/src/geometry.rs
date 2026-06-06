use eframe::egui::{Pos2, Rect, Vec2};

pub const COMPARISON_GHOSTTY_FONT_POINTS_MACOS: f32 = 11.75;
pub const DEFAULT_FONT_DPI: f32 = 96.0;
pub const DEFAULT_FONT_SIZE: f32 = COMPARISON_GHOSTTY_FONT_POINTS_MACOS * DEFAULT_FONT_DPI / 72.0;
pub const GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT: f32 = 1.45;
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
pub enum TerminalCoordinate {
    Surface(SurfacePoint),
    Terminal(SurfacePoint),
    Grid(GridPoint),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinateSpace {
    Surface,
    Terminal,
    Grid,
}

impl TerminalCoordinate {
    fn space(self) -> CoordinateSpace {
        match self {
            Self::Surface(_) => CoordinateSpace::Surface,
            Self::Terminal(_) => CoordinateSpace::Terminal,
            Self::Grid(_) => CoordinateSpace::Grid,
        }
    }

    fn to_surface(self, surface: TerminalSurface) -> SurfacePoint {
        match self {
            Self::Surface(point) => point,
            Self::Terminal(point) => {
                let origin = surface.content_origin();
                SurfacePoint {
                    x: point.x + origin.x,
                    y: point.y + origin.y,
                }
            }
            Self::Grid(point) => {
                let origin = surface.content_origin();
                SurfacePoint {
                    x: f32::from(point.x) * surface.cell.width + origin.x,
                    y: f32::from(point.y) * surface.cell.height + origin.y,
                }
            }
        }
    }
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
        let padding = self.padding.rounded();
        let width = self.rect.width().max(0.0).round() as u32;
        let height = self.rect.height().max(0.0).round() as u32;
        let horizontal_padding = padding.left.saturating_add(padding.right);
        let vertical_padding = padding.top.saturating_add(padding.bottom);
        GridDimensions::for_pixels(
            width.saturating_sub(horizontal_padding),
            height.saturating_sub(vertical_padding),
            self.rounded_cell(),
        )
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

    pub fn convert_coordinate(
        self,
        coordinate: TerminalCoordinate,
        to: CoordinateSpace,
    ) -> TerminalCoordinate {
        if coordinate.space() == to {
            return coordinate;
        }

        let surface = coordinate.to_surface(self);
        match to {
            CoordinateSpace::Surface => TerminalCoordinate::Surface(surface),
            CoordinateSpace::Terminal => {
                let origin = self.content_origin();
                TerminalCoordinate::Terminal(SurfacePoint {
                    x: surface.x - origin.x,
                    y: surface.y - origin.y,
                })
            }
            CoordinateSpace::Grid => {
                let origin = self.content_origin();
                let grid = self.raw_grid_size();
                let x = ((surface.x - origin.x).max(0.0) / self.cell.width).floor();
                let y = ((surface.y - origin.y).max(0.0) / self.cell.height).floor();
                TerminalCoordinate::Grid(GridPoint {
                    x: (x as u16).min(grid.cols.saturating_sub(1)),
                    y: (y as u16).min(grid.rows.saturating_sub(1)),
                })
            }
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

    pub fn mouse_metrics(self) -> MouseSurfaceMetrics {
        let (cell_width, cell_height) = self.cell_size();
        MouseSurfaceMetrics {
            screen_width: self.rect.width().max(0.0).round() as u32,
            screen_height: self.rect.height().max(0.0).round() as u32,
            cell_width,
            cell_height,
            padding: self.padding.rounded(),
        }
    }
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
    fn renderer_grid_dimensions_update_exact() {
        assert_eq!(
            GridDimensions::for_pixels(
                100,
                40,
                RoundedCellMetrics {
                    width: 5,
                    height: 10,
                },
            ),
            GridDimensions { cols: 20, rows: 4 }
        );
    }

    #[test]
    fn renderer_grid_dimensions_update_rounding() {
        assert_eq!(
            GridDimensions::for_pixels(
                20,
                40,
                RoundedCellMetrics {
                    width: 6,
                    height: 15,
                },
            ),
            GridDimensions { cols: 3, rows: 2 }
        );
    }

    #[test]
    fn renderer_coordinate_conversion_clamps_surface_to_grid() {
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
            assert_eq!(
                surface.convert_coordinate(
                    TerminalCoordinate::Surface(actual),
                    CoordinateSpace::Grid,
                ),
                TerminalCoordinate::Grid(expected)
            );
        }
    }

    #[test]
    fn renderer_coordinate_conversion_round_trips_terminal_and_surface_padding() {
        let surface = TerminalSurface::for_size(
            Vec2::new(100.0, 100.0),
            CellMetrics::new(5.0, 10.0),
            TerminalPadding {
                top: 3.0,
                right: 0.0,
                bottom: 0.0,
                left: 7.0,
            },
        );

        assert_eq!(
            surface.convert_coordinate(
                TerminalCoordinate::Terminal(SurfacePoint { x: 8.0, y: 12.0 }),
                CoordinateSpace::Surface,
            ),
            TerminalCoordinate::Surface(SurfacePoint { x: 15.0, y: 15.0 })
        );
        assert_eq!(
            surface.convert_coordinate(
                TerminalCoordinate::Grid(GridPoint { x: 2, y: 3 }),
                CoordinateSpace::Terminal,
            ),
            TerminalCoordinate::Terminal(SurfacePoint { x: 10.0, y: 30.0 })
        );
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
}
