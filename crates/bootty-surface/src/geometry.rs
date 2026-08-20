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

    pub fn contains(self, point: SurfacePoint) -> bool {
        point.x >= self.min_x
            && point.x <= self.max_x
            && point.y >= self.min_y
            && point.y <= self.max_y
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

    pub fn pinched(self, factor: f32, focal: SurfacePoint, surface: SurfaceRect) -> Self {
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

    pub fn panned(self, dx: f32, dy: f32, surface: SurfaceRect) -> Self {
        Self {
            zoom: self.zoom,
            pan_x: self.pan_x + dx,
            pan_y: self.pan_y + dy,
        }
        .clamped(surface)
    }

    pub fn inverse_point(self, point: SurfacePoint) -> SurfacePoint {
        SurfacePoint {
            x: (point.x - self.pan_x) / self.zoom,
            y: (point.y - self.pan_y) / self.zoom,
        }
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
    pub rect: SurfaceRect,
    pub padding: TerminalPadding,
    pub cell: CellMetrics,
}

impl TerminalSurface {
    pub fn new(rect: SurfaceRect, cell: CellMetrics, padding: TerminalPadding) -> Self {
        Self {
            rect,
            cell,
            padding,
        }
    }

    pub fn for_rect(rect: SurfaceRect, cell: CellMetrics) -> Self {
        Self::new(rect, cell, TerminalPadding::default())
    }

    pub fn for_logical_size(
        width: f32,
        height: f32,
        cell: CellMetrics,
        padding: TerminalPadding,
    ) -> Self {
        Self::new(
            SurfaceRect::from_min_size(0.0, 0.0, width, height),
            cell,
            padding,
        )
    }

    pub fn geometry(self) -> TerminalGeometry {
        geometry_for_pixels(
            self.rect.width(),
            self.rect.height(),
            self.cell,
            self.padding,
        )
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
            x: self.rect.min_x + self.padding.left,
            y: self.rect.min_y + self.padding.top,
        }
    }

    pub fn surface_rect(self) -> SurfaceRect {
        self.rect
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

    pub fn relative_position(self, pos: SurfacePoint) -> Option<SurfacePoint> {
        if !self.rect.contains(pos) {
            return None;
        }

        Some(SurfacePoint {
            x: pos.x - self.rect.min_x,
            y: pos.y - self.rect.min_y,
        })
    }

    pub fn mouse_position(self, pos: SurfacePoint) -> Option<SurfacePoint> {
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
