use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

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
    payload_hash: u64,
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
    display_scale: f32,
    placement_iterator: &mut PlacementIterator<'_>,
    image_cache: &mut KittyImageDataCache,
) -> Result<KittyImageFrame> {
    let graphics = terminal.kitty_graphics()?;
    let mut frame = KittyImageFrame::default();

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
            let Some(image_bytes) = image.data()? else {
                continue;
            };
            let fingerprint = KittyImageFingerprint {
                number: image.number()?,
                width,
                height,
                format,
                payload_hash: kitty_image_payload_hash(image_bytes),
            };
            let render_info = placement.placement_render_info(&image, terminal)?;
            if !render_info.viewport_visible {
                continue;
            }
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
                    display_scale,
                    placement.x_offset()?,
                    placement.y_offset()?,
                    placement.columns()?,
                    placement.rows()?,
                ),
                data,
            });
        }
    }

    Ok(frame)
}

fn kitty_image_payload_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
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

    pub(super) fn data_for_image(
        &mut self,
        image_id: u32,
        number: u32,
        width: u32,
        height: u32,
        format: ImageFormat,
        bytes: &[u8],
    ) -> Arc<Vec<u8>> {
        self.data_for(
            image_id,
            KittyImageFingerprint {
                number,
                width,
                height,
                format,
                payload_hash: kitty_image_payload_hash(bytes),
            },
            bytes,
        )
    }

    pub(crate) fn retain_frame(&mut self, frame: &KittyImageFrame) {
        let visible_images = frame
            .placements
            .iter()
            .map(|placement| placement.image_id)
            .collect::<HashSet<_>>();
        self.retain_visible(&visible_images);
    }

    fn retain_visible(&mut self, visible_images: &HashSet<u32>) {
        self.images
            .retain(|image_id, _| visible_images.contains(image_id));
    }
}

pub fn placement_destination(
    surface: TerminalSurface,
    info: PlacementRenderInfo,
    display_scale: f32,
    x_offset: u32,
    y_offset: u32,
    columns: u32,
    rows: u32,
) -> SurfaceRect {
    let origin = surface.content_origin();
    let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
        display_scale
    } else {
        1.0
    };
    let width = if columns > 0 {
        columns as f32 * surface.cell.width
    } else {
        info.pixel_width as f32 / display_scale
    };
    let height = if rows > 0 {
        rows as f32 * surface.cell.height
    } else {
        info.pixel_height as f32 / display_scale
    };
    SurfaceRect::from_min_size(
        origin.x + info.viewport_col as f32 * surface.cell.width + x_offset as f32 / display_scale,
        origin.y + info.viewport_row as f32 * surface.cell.height + y_offset as f32 / display_scale,
        width,
        height,
    )
}

pub fn append_virtual_image_placements(
    terminal: &Terminal<'_, '_>,
    surface: TerminalSurface,
    display_scale: f32,
    frame: &mut KittyImageFrame,
    cells: &[KittyVirtualCell],
    image_cache: &mut KittyImageDataCache,
) -> Result<Vec<u16>> {
    virtual_placement::append_virtual_image_placements(
        terminal,
        surface,
        display_scale,
        frame,
        cells,
        image_cache,
    )
}
