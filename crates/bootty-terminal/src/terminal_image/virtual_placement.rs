use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use libghostty_vt::{Terminal, kitty::graphics::SourceRect, style::StyleColor};

use super::{
    KittyImageDataCache, KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualCell,
    KittyVirtualPlacement,
};
use crate::geometry::{SurfaceRect, TerminalSurface};

const PLACEHOLDER: char = '\u{10EEEE}';
const DIACRITIC_RANGES: &[(char, char, u32)] = &[
    ('\u{305}', '\u{305}', 0),
    ('\u{30D}', '\u{30E}', 1),
    ('\u{310}', '\u{310}', 3),
    ('\u{312}', '\u{312}', 4),
    ('\u{33D}', '\u{33F}', 5),
    ('\u{346}', '\u{346}', 8),
    ('\u{34A}', '\u{34C}', 9),
    ('\u{350}', '\u{352}', 12),
    ('\u{357}', '\u{357}', 15),
    ('\u{35B}', '\u{35B}', 16),
    ('\u{363}', '\u{36F}', 17),
    ('\u{483}', '\u{487}', 30),
    ('\u{592}', '\u{595}', 35),
    ('\u{597}', '\u{599}', 39),
    ('\u{59C}', '\u{5A1}', 42),
    ('\u{5A8}', '\u{5A9}', 48),
    ('\u{5AB}', '\u{5AC}', 50),
    ('\u{5AF}', '\u{5AF}', 52),
    ('\u{5C4}', '\u{5C4}', 53),
    ('\u{610}', '\u{617}', 54),
    ('\u{657}', '\u{65B}', 62),
    ('\u{65D}', '\u{65E}', 67),
    ('\u{6D6}', '\u{6DC}', 69),
    ('\u{6DF}', '\u{6E2}', 76),
    ('\u{6E4}', '\u{6E4}', 80),
    ('\u{6E7}', '\u{6E8}', 81),
    ('\u{6EB}', '\u{6EC}', 83),
    ('\u{730}', '\u{730}', 85),
    ('\u{732}', '\u{733}', 86),
    ('\u{735}', '\u{736}', 88),
    ('\u{73A}', '\u{73A}', 90),
    ('\u{73D}', '\u{73D}', 91),
    ('\u{73F}', '\u{741}', 92),
    ('\u{743}', '\u{743}', 95),
    ('\u{745}', '\u{745}', 96),
    ('\u{747}', '\u{747}', 97),
    ('\u{749}', '\u{74A}', 98),
    ('\u{7EB}', '\u{7F1}', 100),
    ('\u{7F3}', '\u{7F3}', 107),
    ('\u{816}', '\u{819}', 108),
    ('\u{81B}', '\u{823}', 112),
    ('\u{825}', '\u{827}', 121),
    ('\u{829}', '\u{82D}', 124),
    ('\u{951}', '\u{951}', 129),
    ('\u{953}', '\u{954}', 130),
    ('\u{F82}', '\u{F83}', 132),
    ('\u{F86}', '\u{F87}', 134),
    ('\u{135D}', '\u{135F}', 136),
    ('\u{17DD}', '\u{17DD}', 139),
    ('\u{193A}', '\u{193A}', 140),
    ('\u{1A17}', '\u{1A17}', 141),
    ('\u{1A75}', '\u{1A7C}', 142),
    ('\u{1B6B}', '\u{1B6B}', 150),
    ('\u{1B6D}', '\u{1B73}', 151),
    ('\u{1CD0}', '\u{1CD2}', 158),
    ('\u{1CDA}', '\u{1CDB}', 161),
    ('\u{1CE0}', '\u{1CE0}', 163),
    ('\u{1DC0}', '\u{1DC1}', 164),
    ('\u{1DC3}', '\u{1DC9}', 166),
    ('\u{1DCB}', '\u{1DCC}', 173),
    ('\u{1DD1}', '\u{1DE6}', 175),
    ('\u{1DFE}', '\u{1DFE}', 197),
    ('\u{20D0}', '\u{20D1}', 198),
    ('\u{20D4}', '\u{20D7}', 200),
    ('\u{20DB}', '\u{20DC}', 204),
    ('\u{20E1}', '\u{20E1}', 206),
    ('\u{20E7}', '\u{20E7}', 207),
    ('\u{20E9}', '\u{20E9}', 208),
    ('\u{20F0}', '\u{20F0}', 209),
    ('\u{2CEF}', '\u{2CF1}', 210),
    ('\u{2DE0}', '\u{2DFF}', 213),
    ('\u{A66F}', '\u{A66F}', 245),
    ('\u{A67C}', '\u{A67D}', 246),
    ('\u{A6F0}', '\u{A6F1}', 248),
    ('\u{A8E0}', '\u{A8F1}', 250),
    ('\u{AAB0}', '\u{AAB0}', 268),
    ('\u{AAB2}', '\u{AAB3}', 269),
    ('\u{AAB7}', '\u{AAB8}', 271),
    ('\u{AABE}', '\u{AABF}', 273),
    ('\u{AAC1}', '\u{AAC1}', 275),
    ('\u{FE20}', '\u{FE26}', 276),
    ('\u{10A0F}', '\u{10A0F}', 283),
    ('\u{10A38}', '\u{10A38}', 284),
    ('\u{1D185}', '\u{1D189}', 285),
    ('\u{1D1AA}', '\u{1D1AD}', 290),
    ('\u{1D242}', '\u{1D244}', 294),
];

pub(super) fn append_virtual_image_placements(
    terminal: &Terminal<'_, '_>,
    surface: TerminalSurface,
    display_scale: f32,
    frame: &mut KittyImageFrame,
    cells: &[KittyVirtualCell],
    image_cache: &mut KittyImageDataCache,
) -> Result<Vec<u16>> {
    let graphics = terminal.kitty_graphics()?;
    let storage = virtual_storage(&frame.virtual_placements);
    let placement_start = frame.placements.len();
    let mut rendered_rows = Vec::new();
    let mut run: Option<IncompletePlacement> = None;
    let mut finish_run = |run| -> Result<()> {
        if let Some((row, next)) = render_run(
            surface,
            &graphics,
            &storage,
            display_scale,
            image_cache,
            run,
        )? {
            // Native placements before this call are outside the virtual-row merge chain.
            if frame.placements.len() > placement_start
                && let Some(previous) = frame.placements.last_mut()
                && can_merge_virtual_image_rows(previous, &next)
            {
                previous.source.height += next.source.height;
                previous.destination.max_y = next.destination.max_y;
            } else {
                frame.placements.push(next);
            }
            rendered_rows.push(row);
        }
        Ok(())
    };

    for cell in cells {
        let current = IncompletePlacement::from_cell(cell);
        let Some(current) = current else {
            if let Some(done) = run.take() {
                finish_run(done)?;
            }
            continue;
        };

        if let Some(previous) = &mut run {
            if previous.append(&current) {
                continue;
            }
            let done = run.take().expect("run exists");
            finish_run(done)?;
        }
        run = Some(current.with_default_origin());
    }

    if let Some(done) = run {
        finish_run(done)?;
    }

    rendered_rows.sort_unstable();
    rendered_rows.dedup();
    Ok(rendered_rows)
}

fn virtual_storage(
    placements: &[KittyVirtualPlacement],
) -> HashMap<(u32, u32), KittyVirtualPlacement> {
    placements
        .iter()
        .copied()
        .map(|placement| ((placement.image_id, placement.placement_id), placement))
        .collect()
}

fn render_run(
    surface: TerminalSurface,
    graphics: &libghostty_vt::kitty::graphics::Graphics<'_>,
    storage: &HashMap<(u32, u32), KittyVirtualPlacement>,
    display_scale: f32,
    image_cache: &mut KittyImageDataCache,
    run: IncompletePlacement,
) -> Result<Option<(u16, KittyImagePlacement)>> {
    let placement = run.complete();
    let Some(storage_placement) = find_storage_placement(storage, &placement) else {
        return Ok(None);
    };
    let image_id = storage_placement.image_id;
    let Some(image) = graphics.image(image_id) else {
        return Ok(None);
    };
    let image_width = image.width()?;
    let image_height = image.height()?;
    let image_format = image.format()?;
    let image_generation = image.generation()?;
    let grid = placement.grid(
        storage_placement,
        image_width,
        image_height,
        surface,
        display_scale,
    );
    let Some(rendered) = placement.render(grid, image_width, image_height, surface) else {
        return Ok(None);
    };
    let Some(image_bytes) = image.data()? else {
        return Ok(None);
    };
    let data = image_cache.data_for_image(image_id, image_generation, image_bytes);

    let next = KittyImagePlacement {
        image_id,
        placement_id: placement.placement_id,
        layer: KittyImageLayer::from_z(storage_placement.z),
        image_width,
        image_height,
        image_format,
        source: rendered.source,
        destination: rendered.destination,
        data,
    };
    Ok(Some((placement.y, next)))
}

fn can_merge_virtual_image_rows(
    previous: &KittyImagePlacement,
    next: &KittyImagePlacement,
) -> bool {
    previous.image_id == next.image_id
        && previous.placement_id == next.placement_id
        && previous.layer == next.layer
        && previous.image_width == next.image_width
        && previous.image_height == next.image_height
        && previous.image_format == next.image_format
        && Arc::ptr_eq(&previous.data, &next.data)
        && previous.source.x == next.source.x
        && previous.source.width == next.source.width
        && previous.source.y + previous.source.height == next.source.y
        && rect_edges_equal(previous.destination.min_x, next.destination.min_x)
        && rect_edges_touch_or_overlap(previous.destination.max_y, next.destination.min_y)
}

fn rect_edges_equal(left: f32, right: f32) -> bool {
    (left - right).abs() <= f32::EPSILON
}

fn rect_edges_touch_or_overlap(previous_max: f32, next_min: f32) -> bool {
    next_min <= previous_max + f32::EPSILON
}

fn find_storage_placement(
    storage: &HashMap<(u32, u32), KittyVirtualPlacement>,
    placement: &Placement,
) -> Option<KittyVirtualPlacement> {
    if placement.placement_id > 0 {
        return storage
            .get(&(placement.image_id, placement.placement_id))
            .copied()
            .or_else(|| {
                unique_storage_placement(storage, |stored| {
                    stored.placement_id == placement.placement_id
                })
            });
    }
    storage
        .values()
        .find(|stored| stored.image_id == placement.image_id)
        .copied()
        .or_else(|| unique_storage_placement(storage, |_| true))
}

fn unique_storage_placement(
    storage: &HashMap<(u32, u32), KittyVirtualPlacement>,
    mut matches: impl FnMut(&KittyVirtualPlacement) -> bool,
) -> Option<KittyVirtualPlacement> {
    let mut found = storage.values().copied().filter(|stored| matches(stored));
    let placement = found.next()?;
    found.next().is_none().then_some(placement)
}

#[derive(Clone, Debug)]
struct IncompletePlacement {
    x: u16,
    y: u16,
    image_id_low: u32,
    image_id_high: Option<u8>,
    placement_id: Option<u32>,
    row: Option<u32>,
    col: Option<u32>,
    width: u32,
}

impl IncompletePlacement {
    fn from_cell(cell: &KittyVirtualCell) -> Option<Self> {
        if cell.grapheme.first().copied()? != PLACEHOLDER {
            return None;
        }
        let row = cell.grapheme.get(1).and_then(|ch| diacritic_index(*ch));
        let col = cell.grapheme.get(2).and_then(|ch| diacritic_index(*ch));
        let image_id_high = cell
            .grapheme
            .get(3)
            .and_then(|ch| diacritic_index(*ch))
            .and_then(|value| u8::try_from(value).ok());
        let placement_id = color_to_id(cell.underline_color).filter(|id| *id != 0);

        Some(Self {
            x: cell.x,
            y: cell.y,
            image_id_low: color_to_id(cell.foreground).unwrap_or(0),
            image_id_high,
            placement_id,
            row,
            col,
            width: 1,
        })
    }

    fn with_default_origin(mut self) -> Self {
        self.row.get_or_insert(0);
        self.col.get_or_insert(0);
        self
    }

    fn append(&mut self, other: &Self) -> bool {
        if self.y != other.y
            || self.image_id_low != other.image_id_low
            || self.placement_id != other.placement_id
            || other.row.is_some_and(|row| Some(row) != self.row)
            || other
                .col
                .is_some_and(|col| Some(col) != self.col.map(|start| start + self.width))
            || other
                .image_id_high
                .is_some_and(|high| Some(high) != self.image_id_high)
        {
            return false;
        }
        self.width += 1;
        true
    }

    fn complete(self) -> Placement {
        Placement {
            x: self.x,
            y: self.y,
            image_id: self.image_id_low | (u32::from(self.image_id_high.unwrap_or(0)) << 24),
            placement_id: self.placement_id.unwrap_or(0),
            col: self.col.unwrap_or(0),
            row: self.row.unwrap_or(0),
            width: self.width,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Placement {
    x: u16,
    y: u16,
    image_id: u32,
    placement_id: u32,
    col: u32,
    row: u32,
    width: u32,
}

impl Placement {
    fn grid(
        self,
        storage: KittyVirtualPlacement,
        image_width: u32,
        image_height: u32,
        surface: TerminalSurface,
        display_scale: f32,
    ) -> GridSize {
        let mut rows = storage.rows;
        let mut columns = storage.columns;
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if rows == 0 {
            rows = logical_pixel_cells(image_height, display_scale, surface.cell.height);
        }
        if columns == 0 {
            columns = logical_pixel_cells(image_width, display_scale, surface.cell.width);
        }
        GridSize { rows, columns }
    }

    fn render(
        self,
        grid: GridSize,
        image_width: u32,
        image_height: u32,
        surface: TerminalSurface,
    ) -> Option<RenderedPlacement> {
        if grid.columns == 0
            || grid.rows == 0
            || image_width == 0
            || image_height == 0
            || self.width == 0
        {
            return None;
        }

        let image_width = f64::from(image_width);
        let image_height = f64::from(image_height);
        let uses_full_image_width = self.width == grid.columns
            || (self.col > 0 && self.width.saturating_mul(2) >= grid.columns);
        let source = FloatRect {
            x: if uses_full_image_width {
                0.0
            } else {
                image_width * (f64::from(self.col) / f64::from(grid.columns))
            },
            y: image_height * (f64::from(self.row) / f64::from(grid.rows)),
            width: if uses_full_image_width {
                image_width
            } else {
                image_width * (f64::from(self.width) / f64::from(grid.columns))
            },
            height: image_height / f64::from(grid.rows),
        };
        if source.width <= 0.0 || source.height <= 0.0 {
            return None;
        }

        let origin = surface.content_origin();
        let x = if uses_full_image_width {
            i32::from(self.x) - self.col as i32
        } else {
            i32::from(self.x)
        };
        let width = if uses_full_image_width {
            grid.columns as f32
        } else {
            self.width as f32
        };
        let full_grid_width = grid.columns as f32 * surface.cell.width;
        let full_grid_height = grid.rows as f32 * surface.cell.height;
        let source_aspect = image_width as f32 / image_height.max(1.0) as f32;
        let grid_aspect = full_grid_width / full_grid_height.max(1.0);
        let preserve_full_grid_aspect = uses_full_image_width
            && grid.columns > 1
            && grid.rows > 1
            && ((source_aspect / grid_aspect) - 1.0).abs() <= 0.01;
        let preserve_single_row_square_icon = uses_full_image_width
            && grid.columns > 1
            && grid.rows == 1
            && (source_aspect - 1.0).abs() <= 0.01;
        let preserve_source_aspect = preserve_full_grid_aspect || preserve_single_row_square_icon;
        let row_height = if preserve_source_aspect {
            (width * surface.cell.width) * (source.height as f32 / image_width as f32)
        } else {
            surface.cell.height
        };
        let y = if preserve_single_row_square_icon {
            f32::from(self.y) * surface.cell.height + (surface.cell.height - row_height) * 0.5
        } else if preserve_full_grid_aspect {
            let top = f32::from(self.y) - self.row as f32;
            top * surface.cell.height + self.row as f32 * row_height
        } else {
            f32::from(self.y) * surface.cell.height
        };
        Some(RenderedPlacement {
            source: source_rect_from_float(source)?,
            destination: SurfaceRect::from_min_size(
                origin.x + x as f32 * surface.cell.width,
                origin.y + y,
                width * surface.cell.width,
                row_height,
            ),
        })
    }
}

fn logical_pixel_cells(pixels: u32, display_scale: f32, cell_size: f32) -> u32 {
    ((pixels as f32 / display_scale) / cell_size)
        .ceil()
        .max(1.0) as u32
}

fn source_rect_from_float(source: FloatRect) -> Option<SourceRect> {
    let x = source.x.round() as u32;
    let y = source.y.round() as u32;
    let max_x = (source.x + source.width).round() as u32;
    let max_y = (source.y + source.height).round() as u32;
    Some(SourceRect {
        x,
        y,
        width: max_x.checked_sub(x)?,
        height: max_y.checked_sub(y)?,
    })
}

#[derive(Clone, Copy)]
struct GridSize {
    rows: u32,
    columns: u32,
}

#[derive(Clone, Copy)]
struct FloatRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

struct RenderedPlacement {
    source: SourceRect,
    destination: SurfaceRect,
}

fn color_to_id(color: StyleColor) -> Option<u32> {
    match color {
        StyleColor::None => Some(0),
        StyleColor::Palette(index) => Some(u32::from(index.0)),
        StyleColor::Rgb(rgb) => {
            Some((u32::from(rgb.r) << 16) | (u32::from(rgb.g) << 8) | u32::from(rgb.b))
        }
    }
}

fn diacritic_index(ch: char) -> Option<u32> {
    let index = DIACRITIC_RANGES.partition_point(|(_, end, _)| *end < ch);
    let &(start, end, base) = DIACRITIC_RANGES.get(index)?;
    (start..=end)
        .contains(&ch)
        .then_some(base + u32::from(ch) - u32::from(start))
}
