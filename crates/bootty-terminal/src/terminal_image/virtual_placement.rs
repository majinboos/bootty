use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use libghostty_vt::{Terminal, kitty::graphics::SourceRect, style::StyleColor};

use super::{
    KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualCell, KittyVirtualPlacement,
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
    frame: &mut KittyImageFrame,
    cells: &[KittyVirtualCell],
) -> Result<()> {
    let graphics = terminal.kitty_graphics()?;
    let storage = virtual_storage(&frame.virtual_placements);
    let mut image_data = HashMap::<u32, Arc<Vec<u8>>>::new();
    let mut run: Option<IncompletePlacement> = None;

    for cell in cells {
        let current = IncompletePlacement::from_cell(cell);
        let Some(current) = current else {
            if let Some(done) = run.take() {
                append_run(surface, frame, &graphics, &storage, &mut image_data, done)?;
            }
            continue;
        };

        if let Some(previous) = &mut run {
            if previous.append(&current) {
                continue;
            }
            let done = run.take().expect("run exists");
            append_run(surface, frame, &graphics, &storage, &mut image_data, done)?;
        }
        run = Some(current.with_default_origin());
    }

    if let Some(done) = run {
        append_run(surface, frame, &graphics, &storage, &mut image_data, done)?;
    }

    Ok(())
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
    image_data: &mut HashMap<u32, Arc<Vec<u8>>>,
    run: IncompletePlacement,
) -> Result<()> {
    let placement = run.complete();
    let Some(storage_placement) = find_storage_placement(storage, &placement) else {
        return Ok(());
    };
    let Some(image) = graphics.image(placement.image_id) else {
        return Ok(());
    };
    let grid = placement.grid(storage_placement, image.width()?, image.height()?, surface)?;
    let Some(rendered) = placement.render(grid, image.width()?, image.height()?, surface) else {
        return Ok(());
    };
    let data = if let Some(data) = image_data.get(&placement.image_id) {
        data.clone()
    } else {
        let data = Arc::new(image.data()?.to_vec());
        image_data.insert(placement.image_id, data.clone());
        data
    };

    frame.placements.push(KittyImagePlacement {
        image_id: placement.image_id,
        placement_id: placement.placement_id,
        layer: KittyImageLayer::from_z(storage_placement.z),
        image_width: image.width()?,
        image_height: image.height()?,
        image_format: image.format()?,
        source: rendered.source,
        destination: rendered.destination,
        data,
    });

    Ok(())
}

fn find_storage_placement(
    storage: &HashMap<(u32, u32), KittyVirtualPlacement>,
    placement: &Placement,
) -> Option<KittyVirtualPlacement> {
    if placement.placement_id > 0 {
        return storage
            .get(&(placement.image_id, placement.placement_id))
            .copied();
    }
    storage
        .values()
        .find(|stored| stored.image_id == placement.image_id)
        .copied()
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
            height: 1,
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
    height: u32,
}

impl Placement {
    fn grid(
        self,
        storage: KittyVirtualPlacement,
        image_width: u32,
        image_height: u32,
        surface: TerminalSurface,
    ) -> Result<GridSize> {
        let mut rows = storage.rows;
        let mut columns = storage.columns;
        if rows == 0 {
            rows = image_height.div_ceil(surface.cell.height as u32);
        }
        if columns == 0 {
            columns = image_width.div_ceil(surface.cell.width as u32);
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
        let image_width = f64::from(image_width);
        let image_height = f64::from(image_height);
        let grid_width = f64::from(grid.columns) * f64::from(surface.cell.width);
        let grid_height = f64::from(grid.rows) * f64::from(surface.cell.height);

        let scale = if image_width * grid_height > image_height * grid_width {
            let scale = grid_width / image_width.max(1.0);
            Scale {
                x: scale,
                y: scale,
                x_offset: 0.0,
                y_offset: (grid_height - image_height * scale) / 2.0,
            }
        } else {
            let scale = grid_height / image_height.max(1.0);
            Scale {
                x: scale,
                y: scale,
                x_offset: (grid_width - image_width * scale) / 2.0,
                y_offset: 0.0,
            }
        };

        let image_scaled = ScaledImage {
            x_offset: scale.x_offset / scale.x,
            y_offset: scale.y_offset / scale.y,
            width: image_width + (scale.x_offset / scale.x * 2.0),
            height: image_height + (scale.y_offset / scale.y * 2.0),
        };
        let mut source = FloatRect {
            x: image_scaled.width * (f64::from(self.col) / f64::from(grid.columns)),
            y: image_scaled.height * (f64::from(self.row) / f64::from(grid.rows)),
            width: image_scaled.width * (f64::from(self.width) / f64::from(grid.columns)),
            height: image_scaled.height * (f64::from(self.height) / f64::from(grid.rows)),
        };
        let mut destination = FloatRect {
            x: 0.0,
            y: 0.0,
            width: f64::from(self.width) * f64::from(surface.cell.width),
            height: f64::from(self.height) * f64::from(surface.cell.height),
        };

        clip_axis(
            AxisSlice {
                source_offset: &mut source.y,
                source_size: &mut source.height,
                dest_offset: &mut destination.y,
                dest_size: &mut destination.height,
            },
            AxisBounds {
                image_offset: image_scaled.y_offset,
                scaled_size: image_scaled.height,
                image_size: image_height,
                scale: scale.y,
            },
        );
        clip_axis(
            AxisSlice {
                source_offset: &mut source.x,
                source_size: &mut source.width,
                dest_offset: &mut destination.x,
                dest_size: &mut destination.width,
            },
            AxisBounds {
                image_offset: image_scaled.x_offset,
                scaled_size: image_scaled.width,
                image_size: image_width,
                scale: scale.x,
            },
        );
        if source.width <= 0.0 || source.height <= 0.0 {
            return None;
        }

        let origin = surface.content_origin();
        Some(RenderedPlacement {
            source: SourceRect {
                x: source.x.round() as u32,
                y: source.y.round() as u32,
                width: source.width.round() as u32,
                height: source.height.round() as u32,
            },
            destination: SurfaceRect::from_min_size(
                origin.x + f32::from(self.x) * surface.cell.width + destination.x as f32,
                origin.y + f32::from(self.y) * surface.cell.height + destination.y as f32,
                destination.width.round() as f32,
                destination.height.round() as f32,
            ),
        })
    }
}

fn clip_axis(axis: AxisSlice<'_>, bounds: AxisBounds) {
    if *axis.source_offset < bounds.image_offset {
        let offset = bounds.image_offset - *axis.source_offset;
        *axis.source_size -= offset;
        *axis.dest_offset = offset;
        *axis.dest_size -= offset * bounds.scale;
        *axis.source_offset = 0.0;
        if *axis.source_size > bounds.image_size {
            *axis.source_size = bounds.image_size;
            *axis.dest_size = bounds.image_size * bounds.scale;
        }
    } else if *axis.source_offset + *axis.source_size > bounds.scaled_size - bounds.image_offset {
        *axis.source_offset -= bounds.image_offset;
        *axis.source_size = bounds.scaled_size - bounds.image_offset - *axis.source_offset;
        *axis.source_size -= bounds.image_offset;
        *axis.dest_size = *axis.source_size * bounds.scale;
    } else {
        *axis.source_offset -= bounds.image_offset;
    }
}

struct AxisSlice<'a> {
    source_offset: &'a mut f64,
    source_size: &'a mut f64,
    dest_offset: &'a mut f64,
    dest_size: &'a mut f64,
}

struct AxisBounds {
    image_offset: f64,
    scaled_size: f64,
    image_size: f64,
    scale: f64,
}

#[derive(Clone, Copy)]
struct GridSize {
    rows: u32,
    columns: u32,
}

#[derive(Clone, Copy)]
struct Scale {
    x: f64,
    y: f64,
    x_offset: f64,
    y_offset: f64,
}

#[derive(Clone, Copy)]
struct ScaledImage {
    x_offset: f64,
    y_offset: f64,
    width: f64,
    height: f64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_unicode_diacritics_are_sorted_for_binary_search() {
        assert!(DIACRITICS.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn kitty_unicode_diacritic_indices_match_upstream_spots() {
        assert_eq!(diacritic_index('\u{0483}'), Some(30));
        assert_eq!(diacritic_index('\u{1D242}'), Some(294));
    }
}
