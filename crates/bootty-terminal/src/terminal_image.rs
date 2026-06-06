use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use libghostty_vt::{
    Terminal,
    kitty::graphics::{ImageFormat, Layer, PlacementIterator, PlacementRenderInfo, SourceRect},
    style::StyleColor,
};

use crate::geometry::{SurfaceRect, TerminalSurface};

mod virtual_placement;

#[derive(Default)]
pub struct KittyImageDataCache {
    images: HashMap<u32, CachedKittyImageData>,
}

struct CachedKittyImageData {
    fingerprint: KittyImageFingerprint,
    data: Arc<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KittyImageFingerprint {
    number: u32,
    width: u32,
    height: u32,
    format: ImageFormat,
    len: usize,
    first: Option<u8>,
    last: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KittyImageFrame {
    pub placements: Vec<KittyImagePlacement>,
    pub virtual_placements: Vec<KittyVirtualPlacement>,
    pub virtual_placeholder_rows: Vec<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KittyImagePlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub layer: KittyImageLayer,
    pub image_width: u32,
    pub image_height: u32,
    pub image_format: ImageFormat,
    pub source: SourceRect,
    pub destination: SurfaceRect,
    pub data: Arc<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KittyVirtualPlacement {
    pub image_id: u32,
    pub placement_id: u32,
    pub columns: u32,
    pub rows: u32,
    pub z: i32,
}

#[derive(Clone, Debug)]
pub struct KittyVirtualCell {
    pub x: u16,
    pub y: u16,
    pub grapheme: Vec<char>,
    pub foreground: StyleColor,
    pub underline_color: StyleColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KittyImageLayer {
    BelowBackground,
    BelowText,
    AboveText,
}

impl KittyImageLayer {
    pub fn ordered() -> [Self; 3] {
        [Self::BelowBackground, Self::BelowText, Self::AboveText]
    }

    fn to_ghostty(self) -> Layer {
        match self {
            Self::BelowBackground => Layer::BelowBg,
            Self::BelowText => Layer::BelowText,
            Self::AboveText => Layer::AboveText,
        }
    }

    fn from_z(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Self::BelowBackground
        } else if z < 0 {
            Self::BelowText
        } else {
            Self::AboveText
        }
    }
}

pub fn collect_kitty_image_frame(
    terminal: &Terminal<'_, '_>,
    surface: TerminalSurface,
    placement_iterator: &mut PlacementIterator<'_>,
    image_cache: &mut KittyImageDataCache,
) -> Result<KittyImageFrame> {
    let graphics = terminal.kitty_graphics()?;
    let mut frame = KittyImageFrame::default();
    let mut visible_images = HashMap::<u32, ()>::new();

    for layer in KittyImageLayer::ordered() {
        let mut placements = placement_iterator.update(&graphics)?;
        placements.set_layer(layer.to_ghostty())?;

        while let Some(placement) = placements.next() {
            let image_id = placement.image_id()?;
            if placement.is_virtual()? {
                frame.virtual_placements.push(KittyVirtualPlacement {
                    image_id,
                    placement_id: placement.placement_id()?,
                    columns: placement.columns()?,
                    rows: placement.rows()?,
                    z: placement.z()?,
                });
                continue;
            }
            let Some(image) = graphics.image(image_id) else {
                continue;
            };
            let width = image.width()?;
            let height = image.height()?;
            let format = image.format()?;
            let image_bytes = image.data()?;
            let fingerprint = KittyImageFingerprint {
                number: image.number()?,
                width,
                height,
                format,
                len: image_bytes.len(),
                first: image_bytes.first().copied(),
                last: image_bytes.last().copied(),
            };
            let render_info = placement.placement_render_info(&image, terminal)?;
            if !render_info.viewport_visible {
                continue;
            }
            visible_images.insert(image_id, ());
            let data = image_cache.data_for(image_id, fingerprint, image_bytes);
            frame.placements.push(KittyImagePlacement {
                image_id,
                placement_id: placement.placement_id()?,
                layer,
                image_width: width,
                image_height: height,
                image_format: format,
                source: SourceRect {
                    x: render_info.source_x,
                    y: render_info.source_y,
                    width: render_info.source_width,
                    height: render_info.source_height,
                },
                destination: placement_destination(
                    surface,
                    render_info,
                    placement.x_offset()?,
                    placement.y_offset()?,
                ),
                data,
            });
        }
    }

    image_cache.retain_visible(&visible_images);
    Ok(frame)
}

impl KittyImageDataCache {
    fn data_for(
        &mut self,
        image_id: u32,
        fingerprint: KittyImageFingerprint,
        bytes: &[u8],
    ) -> Arc<Vec<u8>> {
        if let Some(cached) = self.images.get(&image_id)
            && cached.fingerprint == fingerprint
        {
            return cached.data.clone();
        }

        let data = Arc::new(bytes.to_vec());
        self.images.insert(
            image_id,
            CachedKittyImageData {
                fingerprint,
                data: data.clone(),
            },
        );
        data
    }

    fn retain_visible(&mut self, visible_images: &HashMap<u32, ()>) {
        self.images
            .retain(|image_id, _| visible_images.contains_key(image_id));
    }
}

pub fn placement_destination(
    surface: TerminalSurface,
    info: PlacementRenderInfo,
    x_offset: u32,
    y_offset: u32,
) -> SurfaceRect {
    let origin = surface.content_origin();
    SurfaceRect::from_min_size(
        origin.x + info.viewport_col as f32 * surface.cell.width + x_offset as f32,
        origin.y + info.viewport_row as f32 * surface.cell.height + y_offset as f32,
        info.pixel_width as f32,
        info.pixel_height as f32,
    )
}

pub fn append_virtual_image_placements(
    terminal: &Terminal<'_, '_>,
    surface: TerminalSurface,
    frame: &mut KittyImageFrame,
    cells: &[KittyVirtualCell],
) -> Result<()> {
    virtual_placement::append_virtual_image_placements(terminal, surface, frame, cells)
}
