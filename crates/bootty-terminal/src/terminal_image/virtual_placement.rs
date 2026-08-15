use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use libghostty_vt::{Terminal, kitty::graphics::SourceRect, style::StyleColor};

use super::{
    KittyImageDataCache, KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualCell,
    KittyVirtualPlacement,
};
use crate::geometry::{SurfaceRect, TerminalSurface};

const PLACEHOLDER: char = '\u{10EEEE}';
const DIACRITICS: &[char] = &[
    '\u{0305}',
    '\u{030D}',
    '\u{030E}',
    '\u{0310}',
    '\u{0312}',
    '\u{033D}',
    '\u{033E}',
    '\u{033F}',
    '\u{0346}',
    '\u{034A}',
    '\u{034B}',
    '\u{034C}',
    '\u{0350}',
    '\u{0351}',
    '\u{0352}',
    '\u{0357}',
    '\u{035B}',
    '\u{0363}',
    '\u{0364}',
    '\u{0365}',
    '\u{0366}',
    '\u{0367}',
    '\u{0368}',
    '\u{0369}',
    '\u{036A}',
    '\u{036B}',
    '\u{036C}',
    '\u{036D}',
    '\u{036E}',
    '\u{036F}',
    '\u{0483}',
    '\u{0484}',
    '\u{0485}',
    '\u{0486}',
    '\u{0487}',
    '\u{0592}',
    '\u{0593}',
    '\u{0594}',
    '\u{0595}',
    '\u{0597}',
    '\u{0598}',
    '\u{0599}',
    '\u{059C}',
    '\u{059D}',
    '\u{059E}',
    '\u{059F}',
    '\u{05A0}',
    '\u{05A1}',
    '\u{05A8}',
    '\u{05A9}',
    '\u{05AB}',
    '\u{05AC}',
    '\u{05AF}',
    '\u{05C4}',
    '\u{0610}',
    '\u{0611}',
    '\u{0612}',
    '\u{0613}',
    '\u{0614}',
    '\u{0615}',
    '\u{0616}',
    '\u{0617}',
    '\u{0657}',
    '\u{0658}',
    '\u{0659}',
    '\u{065A}',
    '\u{065B}',
    '\u{065D}',
    '\u{065E}',
    '\u{06D6}',
    '\u{06D7}',
    '\u{06D8}',
    '\u{06D9}',
    '\u{06DA}',
    '\u{06DB}',
    '\u{06DC}',
    '\u{06DF}',
    '\u{06E0}',
    '\u{06E1}',
    '\u{06E2}',
    '\u{06E4}',
    '\u{06E7}',
    '\u{06E8}',
    '\u{06EB}',
    '\u{06EC}',
    '\u{0730}',
    '\u{0732}',
    '\u{0733}',
    '\u{0735}',
    '\u{0736}',
    '\u{073A}',
    '\u{073D}',
    '\u{073F}',
    '\u{0740}',
    '\u{0741}',
    '\u{0743}',
    '\u{0745}',
    '\u{0747}',
    '\u{0749}',
    '\u{074A}',
    '\u{07EB}',
    '\u{07EC}',
    '\u{07ED}',
    '\u{07EE}',
    '\u{07EF}',
    '\u{07F0}',
    '\u{07F1}',
    '\u{07F3}',
    '\u{0816}',
    '\u{0817}',
    '\u{0818}',
    '\u{0819}',
    '\u{081B}',
    '\u{081C}',
    '\u{081D}',
    '\u{081E}',
    '\u{081F}',
    '\u{0820}',
    '\u{0821}',
    '\u{0822}',
    '\u{0823}',
    '\u{0825}',
    '\u{0826}',
    '\u{0827}',
    '\u{0829}',
    '\u{082A}',
    '\u{082B}',
    '\u{082C}',
    '\u{082D}',
    '\u{0951}',
    '\u{0953}',
    '\u{0954}',
    '\u{0F82}',
    '\u{0F83}',
    '\u{0F86}',
    '\u{0F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
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

    for cell in cells {
        let current = IncompletePlacement::from_cell(cell);
        let Some(current) = current else {
            if let Some(done) = run.take()
                && let Some(row) = append_run(
                    surface,
                    frame,
                    &graphics,
                    &storage,
                    display_scale,
                    image_cache,
                    done,
                )?
            {
                rendered_rows.push(row);
            }
            continue;
        };

        if let Some(previous) = &mut run {
            if previous.append(&current) {
                continue;
            }
            let done = run.take().expect("run exists");
            if let Some(row) = append_run(
                surface,
                frame,
                &graphics,
                &storage,
                display_scale,
                image_cache,
                done,
            )? {
                rendered_rows.push(row);
            }
        }
        run = Some(current.with_default_origin());
    }

    if let Some(done) = run
        && let Some(row) = append_run(
            surface,
            frame,
            &graphics,
            &storage,
            display_scale,
            image_cache,
            done,
        )?
    {
        rendered_rows.push(row);
    }

    merge_adjacent_virtual_image_rows(&mut frame.placements, placement_start);

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

fn append_run(
    surface: TerminalSurface,
    frame: &mut KittyImageFrame,
    graphics: &libghostty_vt::kitty::graphics::Graphics<'_>,
    storage: &HashMap<(u32, u32), KittyVirtualPlacement>,
    display_scale: f32,
    image_cache: &mut KittyImageDataCache,
    run: IncompletePlacement,
) -> Result<Option<u16>> {
    let placement = run.complete();
    let Some(storage_placement) = find_storage_placement(storage, &placement) else {
        return Ok(None);
    };
    let image_id = storage_placement.image_id;
    let Some(image) = graphics.image(image_id) else {
        return Ok(None);
    };
    let grid = placement.grid(
        storage_placement,
        image.width()?,
        image.height()?,
        surface,
        display_scale,
    )?;
    let Some(rendered) = placement.render(grid, image.width()?, image.height()?, surface) else {
        return Ok(None);
    };
    let Some(image_bytes) = image.data()? else {
        return Ok(None);
    };
    let data = image_cache.data_for_image(
        image_id,
        image.number()?,
        image.width()?,
        image.height()?,
        image.format()?,
        image_bytes,
    );

    frame.placements.push(KittyImagePlacement {
        image_id,
        placement_id: placement.placement_id,
        layer: KittyImageLayer::from_z(storage_placement.z),
        image_width: image.width()?,
        image_height: image.height()?,
        image_format: image.format()?,
        source: rendered.source,
        destination: rendered.destination,
        data,
    });
    Ok(Some(placement.y))
}

fn merge_adjacent_virtual_image_rows(placements: &mut Vec<KittyImagePlacement>, start: usize) {
    if placements.len().saturating_sub(start) < 2 {
        return;
    }

    let mut merged = Vec::with_capacity(placements.len() - start);
    for placement in placements.drain(start..) {
        if let Some(previous) = merged.last_mut()
            && can_merge_virtual_image_rows(previous, &placement)
        {
            previous.source.height += placement.source.height;
            previous.destination.max_y = placement.destination.max_y;
            continue;
        }
        merged.push(placement);
    }
    placements.extend(merged);
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
        if let Some(stored) = storage
            .get(&(placement.image_id, placement.placement_id))
            .copied()
        {
            return Some(stored);
        }
        return unique_storage_placement(storage, |stored| {
            stored.placement_id == placement.placement_id
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
    let mut found = None;
    for stored in storage.values().filter(|stored| matches(stored)) {
        if found.is_some() {
            return None;
        }
        found = Some(*stored);
    }
    found
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
        if self.row.is_none() {
            self.row = Some(0);
        }
        if self.col.is_none() {
            self.col = Some(0);
        }
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
    ) -> Result<GridSize> {
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
        Ok(GridSize { rows, columns })
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
    DIACRITICS
        .binary_search(&ch)
        .ok()
        .and_then(|index| u32::try_from(index).ok())
}
