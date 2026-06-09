use crate::{
    geometry::{CellMetrics, SurfaceRect},
    paint_plan::{DecorationStyle, PlanColor},
    terminal_image::KittyImagePlacement,
    terminal_render::{
        CursorCommand, SpriteCommandBatch, TerminalRenderCommand, TerminalRenderFrame, TextCommand,
    },
    terminal_sprite::{WgpuSpriteBackend, WgpuSpriteVertex},
    terminal_text_atlas::{GlyphAtlasFormat, TextAtlasBuilder, TexturedGlyphQuad},
};
use eframe::{egui, egui_wgpu, wgpu};
use wgpu::util::DeviceExt;

mod font_lookup;
mod glyph_draw;
mod image_upload;
mod pipelines;
mod vertices;

#[cfg(test)]
use font_lookup::{GHOSTTY_FONT_FAMILY_PRIORITY, terminal_font_family_priority};
use font_lookup::{ghostty_cell_metrics_from_font, terminal_font, terminal_font_for_char};
use glyph_draw::text_glyph_draws;
use image_upload::{image_fits_device_limits, rgba_image_pixels};
use pipelines::{
    background_pipeline, image_bind_group_layout, image_pipeline, text_bind_group_layout,
    text_pipeline, text_texture_format,
};
use std::collections::HashMap;
use std::sync::Arc;
#[cfg(test)]
use vertices::color_to_float;
#[cfg(test)]
use vertices::text_vertices;
use vertices::{
    BackgroundVertex, TerminalQuadDraw, TextVertex, background_quad_vertices, image_vertices,
    text_vertices_into, vertex_bytes,
};

#[cfg(test)]
pub(crate) use crate::terminal_text::{FontStyle, ResolvedFontFace};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalBackgroundDraw {
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalTextDraw {
    pub ch: char,
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalSpriteDraw {
    pub ch: char,
    pub vertices: Vec<WgpuSpriteVertex>,
    pub indices: Vec<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalCursorDraw {
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

pub fn terminal_background_draws(frame: &TerminalRenderFrame) -> Vec<TerminalBackgroundDraw> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::FillRect(fill) => Some(TerminalBackgroundDraw {
                rect: fill.rect,
                color: fill.color,
            }),
            TerminalRenderCommand::Text(_)
            | TerminalRenderCommand::Sprite(_)
            | TerminalRenderCommand::Image(_)
            | TerminalRenderCommand::KittyVirtualPlacement(_)
            | TerminalRenderCommand::Decoration(_)
            | TerminalRenderCommand::Cursor(_) => None,
        })
        .collect()
}

pub fn terminal_text_cell_metrics(
    config: &crate::terminal_text::TerminalTextConfig,
) -> CellMetrics {
    let face = crate::terminal_text::FontResolver::new(config.clone()).resolve_face(
        &crate::paint_plan::TextAttrs {
            fg: PlanColor {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        },
    );
    let Some(font) = terminal_font(&face) else {
        return CellMetrics::new(config.cell_width, config.cell_height);
    };

    ghostty_cell_metrics_from_font(&font, config.font_size)
}

pub fn terminal_text_draws(frame: &TerminalRenderFrame) -> Vec<TerminalTextDraw> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Text(text) => Some(text),
            TerminalRenderCommand::FillRect(_)
            | TerminalRenderCommand::Sprite(_)
            | TerminalRenderCommand::Image(_)
            | TerminalRenderCommand::KittyVirtualPlacement(_)
            | TerminalRenderCommand::Decoration(_)
            | TerminalRenderCommand::Cursor(_) => None,
        })
        .flat_map(|text| text_draws(text, 1.0))
        .collect()
}

pub fn terminal_sprite_draws(frame: &TerminalRenderFrame) -> Vec<TerminalSpriteDraw> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Sprite(sprite) => Some(sprite),
            TerminalRenderCommand::FillRect(_)
            | TerminalRenderCommand::Text(_)
            | TerminalRenderCommand::Image(_)
            | TerminalRenderCommand::KittyVirtualPlacement(_)
            | TerminalRenderCommand::Decoration(_)
            | TerminalRenderCommand::Cursor(_) => None,
        })
        .map(sprite_draw)
        .collect()
}

pub fn terminal_cursor_draws(frame: &TerminalRenderFrame) -> Vec<TerminalCursorDraw> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Cursor(cursor) => Some(cursor),
            TerminalRenderCommand::FillRect(_)
            | TerminalRenderCommand::Text(_)
            | TerminalRenderCommand::Sprite(_)
            | TerminalRenderCommand::Image(_)
            | TerminalRenderCommand::KittyVirtualPlacement(_)
            | TerminalRenderCommand::Decoration(_) => None,
        })
        .flat_map(cursor_draws)
        .collect()
}

pub fn terminal_decoration_draws(frame: &TerminalRenderFrame) -> Vec<TerminalBackgroundDraw> {
    frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Decoration(line) => Some(line),
            TerminalRenderCommand::FillRect(_)
            | TerminalRenderCommand::Text(_)
            | TerminalRenderCommand::Sprite(_)
            | TerminalRenderCommand::Image(_)
            | TerminalRenderCommand::KittyVirtualPlacement(_)
            | TerminalRenderCommand::Cursor(_) => None,
        })
        .flat_map(decoration_draws)
        .map(|draw| TerminalBackgroundDraw {
            rect: draw.rect,
            color: draw.color,
        })
        .collect()
}

pub fn terminal_render_callback(
    frame: &TerminalRenderFrame,
    target_format: wgpu::TextureFormat,
) -> Option<egui::Shape> {
    if !has_wgpu_draw_commands(&frame.commands) {
        return None;
    }

    Some(
        egui_wgpu::Callback::new_paint_callback(
            egui_rect(frame.surface),
            TerminalRenderCallback {
                target_format,
                key: terminal_callback_key(frame.surface, target_format),
                frame: frame.clone(),
            },
        )
        .into(),
    )
}

fn has_wgpu_draw_commands(commands: &[TerminalRenderCommand]) -> bool {
    commands.iter().any(|command| {
        matches!(
            command,
            TerminalRenderCommand::FillRect(_)
                | TerminalRenderCommand::Text(_)
                | TerminalRenderCommand::Sprite(_)
                | TerminalRenderCommand::Image(_)
                | TerminalRenderCommand::Decoration(_)
                | TerminalRenderCommand::Cursor(_)
        )
    })
}

struct TerminalRenderCallback {
    target_format: wgpu::TextureFormat,
    key: TerminalCallbackKey,
    frame: TerminalRenderFrame,
}

impl egui_wgpu::CallbackTrait for TerminalRenderCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if callback_resources
            .get::<TerminalWgpuRendererCache>()
            .is_none()
        {
            callback_resources.insert(TerminalWgpuRendererCache::default());
        }
        let cache = callback_resources
            .get_mut::<TerminalWgpuRendererCache>()
            .expect("terminal wgpu renderer cache");
        cache
            .renderers
            .entry(self.key)
            .or_insert_with(|| TerminalWgpuRenderer::new(device, self.target_format))
            .prepare_terminal_frame(
                device,
                queue,
                &self.frame,
                screen_descriptor.pixels_per_point,
            );
        Vec::new()
    }

    fn paint(
        &self,
        _info: egui::epaint::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &egui_wgpu::CallbackResources,
    ) {
        let Some(cache) = callback_resources.get::<TerminalWgpuRendererCache>() else {
            return;
        };
        if let Some(renderer) = cache.renderers.get(&self.key) {
            renderer.paint(render_pass);
        };
    }
}

#[derive(Default)]
struct TerminalWgpuRendererCache {
    renderers: HashMap<TerminalCallbackKey, TerminalWgpuRenderer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TerminalCallbackKey {
    target_format: wgpu::TextureFormat,
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

fn terminal_callback_key(
    surface: SurfaceRect,
    target_format: wgpu::TextureFormat,
) -> TerminalCallbackKey {
    TerminalCallbackKey {
        target_format,
        min_x: surface.min_x.to_bits(),
        min_y: surface.min_y.to_bits(),
        max_x: surface.max_x.to_bits(),
        max_y: surface.max_y.to_bits(),
    }
}

struct TerminalBackgroundFrameResources {
    vertex_buffer: wgpu::Buffer,
    vertices: Vec<BackgroundVertex>,
    vertex_count: u32,
    byte_capacity: usize,
}

impl TerminalBackgroundFrameResources {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &mut Vec<BackgroundVertex>,
    ) -> Self {
        let mut resources = Self {
            vertex_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bootty_terminal_renderer_vertices"),
                size: 1,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            vertices: Vec::new(),
            vertex_count: 0,
            byte_capacity: 0,
        };
        resources.update(device, queue, vertices);
        resources
    }

    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        vertices: &mut Vec<BackgroundVertex>,
    ) {
        if vertices.is_empty() {
            self.vertices.clear();
            self.vertex_count = 0;
            return;
        }

        let vertex_count = vertices.len() as u32;
        let bytes = vertex_bytes(vertices);
        if bytes.len() > self.byte_capacity {
            self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bootty_terminal_renderer_vertices"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.byte_capacity = bytes.len();
            std::mem::swap(&mut self.vertices, vertices);
        } else if self.vertices.as_slice() != vertices {
            queue.write_buffer(&self.vertex_buffer, 0, bytes);
            std::mem::swap(&mut self.vertices, vertices);
        }
        self.vertex_count = vertex_count;
    }
}

enum TerminalPreparedLayer {
    Background(usize),
    Text(usize),
    Image(usize),
}

struct PreparedTerminalFrameCache {
    frame: TerminalRenderFrame,
    pixels_per_point_bits: u32,
    vertex_count: u32,
}

const PREPARED_FRAME_CACHE_MISS_COOLDOWN: u8 = 8;

pub struct TerminalWgpuRenderer {
    pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    text_bind_group_layout: wgpu::BindGroupLayout,
    image_bind_group_layout: wgpu::BindGroupLayout,
    text_sampler: wgpu::Sampler,
    image_sampler: wgpu::Sampler,
    text_builder: TextAtlasBuilder,
    text_texture: Option<TerminalTextAtlasTexture>,
    layers: Vec<TerminalPreparedLayer>,
    background_resources: Vec<TerminalBackgroundFrameResources>,
    text_resources: Option<TerminalTextFrameResources>,
    text_vertex_scratch: Vec<TextVertex>,
    background_batch_scratch: Vec<Vec<BackgroundVertex>>,
    text_batch_scratch: Vec<Vec<TexturedGlyphQuad>>,
    text_batch_dirty_scratch: Vec<bool>,
    image_resources: Vec<Option<TerminalImageFrameResources>>,
    prepared_frame_cache: Option<PreparedTerminalFrameCache>,
    prepared_frame_cache_cooldown: u8,
}

impl TerminalWgpuRenderer {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let text_bind_group_layout = text_bind_group_layout(device);
        let image_bind_group_layout = image_bind_group_layout(device);
        Self {
            pipeline: background_pipeline(device, target_format),
            text_pipeline: text_pipeline(device, target_format, &text_bind_group_layout),
            image_pipeline: image_pipeline(device, target_format, &image_bind_group_layout),
            text_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bootty_terminal_text_sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            }),
            image_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bootty_terminal_image_sampler"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            text_bind_group_layout,
            image_bind_group_layout,
            text_builder: TextAtlasBuilder::new_rgba(1024, 1024),
            text_texture: None,
            layers: Vec::new(),
            background_resources: Vec::new(),
            text_resources: None,
            text_vertex_scratch: Vec::new(),
            background_batch_scratch: Vec::new(),
            text_batch_scratch: Vec::new(),
            text_batch_dirty_scratch: Vec::new(),
            image_resources: Vec::new(),
            prepared_frame_cache: None,
            prepared_frame_cache_cooldown: 0,
        }
    }

    pub fn prepare_terminal_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &TerminalRenderFrame,
        pixels_per_point: f32,
    ) -> u32 {
        let pixels_per_point_bits = pixels_per_point.to_bits();
        let mut update_frame_cache = true;
        if self.prepared_frame_cache_cooldown > 0 {
            self.prepared_frame_cache_cooldown -= 1;
            update_frame_cache = self.prepared_frame_cache_cooldown == 0;
        } else if let Some(cache) = &self.prepared_frame_cache {
            if cache.pixels_per_point_bits == pixels_per_point_bits && cache.frame == *frame {
                return cache.vertex_count;
            }
            self.prepared_frame_cache_cooldown = PREPARED_FRAME_CACHE_MISS_COOLDOWN;
            update_frame_cache = false;
        }

        self.layers.clear();
        let mut previous_image_resources = std::mem::take(&mut self.image_resources).into_iter();

        let mut background_batches = std::mem::take(&mut self.background_batch_scratch);
        let mut text_batches = std::mem::take(&mut self.text_batch_scratch);
        let mut text_batch_dirty = std::mem::take(&mut self.text_batch_dirty_scratch);
        background_batches.iter_mut().for_each(Vec::clear);
        text_batches.iter_mut().for_each(Vec::clear);
        text_batch_dirty.clear();
        let mut background_batch_count = 0;
        let mut text_batch_count = 0;
        let mut image_vertex_count = 0;

        self.text_builder.begin_text_frame();
        for command in &frame.commands {
            match command {
                TerminalRenderCommand::Text(text) => {
                    push_text_command(
                        &mut self.layers,
                        &mut text_batches,
                        &mut text_batch_dirty,
                        &mut text_batch_count,
                        &mut self.text_builder,
                        text,
                        pixels_per_point,
                    );
                }
                TerminalRenderCommand::Sprite(sprite) => {
                    let quad = self
                        .text_builder
                        .prepare_sprite_command(sprite, pixels_per_point);
                    push_text_quad(
                        &mut self.layers,
                        &mut text_batches,
                        &mut text_batch_dirty,
                        &mut text_batch_count,
                        quad,
                    );
                }
                TerminalRenderCommand::FillRect(fill) => {
                    push_background_quad(
                        &mut self.layers,
                        &mut background_batches,
                        &mut background_batch_count,
                        frame.surface,
                        TerminalQuadDraw {
                            rect: fill.rect,
                            color: fill.color,
                        },
                    );
                }
                TerminalRenderCommand::Decoration(line) => {
                    push_decoration_command(
                        &mut self.layers,
                        &mut background_batches,
                        &mut background_batch_count,
                        frame.surface,
                        line,
                    );
                }
                TerminalRenderCommand::Cursor(cursor) => push_cursor_background_quads(
                    &mut self.layers,
                    &mut background_batches,
                    &mut background_batch_count,
                    frame.surface,
                    cursor,
                ),
                TerminalRenderCommand::Image(image) => {
                    let previous = previous_image_resources.next().flatten();
                    let resources = prepare_image_resource(
                        device,
                        queue,
                        frame.surface,
                        image,
                        &self.image_bind_group_layout,
                        &self.image_sampler,
                        previous,
                    );
                    image_vertex_count += resources
                        .as_ref()
                        .map_or(0, |resources| resources.vertex_count);
                    let image_layer_index = self.image_resources.len();
                    self.image_resources.push(resources);
                    self.layers
                        .push(TerminalPreparedLayer::Image(image_layer_index));
                }
                TerminalRenderCommand::KittyVirtualPlacement(_) => {}
            }
        }
        self.text_builder.finish_text_frame();

        let background_vertex_count = self.prepare_background_resources(
            device,
            queue,
            &mut background_batches[..background_batch_count],
        );
        let text_vertex_count = self.prepare_text_resources(
            device,
            queue,
            frame.surface,
            text_batches[..text_batch_count].iter().map(Vec::as_slice),
            &text_batch_dirty[..text_batch_count],
        );
        self.background_batch_scratch = background_batches;
        self.text_batch_scratch = text_batches;
        self.text_batch_dirty_scratch = text_batch_dirty;

        let vertex_count = image_vertex_count + background_vertex_count + text_vertex_count;
        if update_frame_cache {
            self.prepared_frame_cache = Some(PreparedTerminalFrameCache {
                frame: frame.clone(),
                pixels_per_point_bits,
                vertex_count,
            });
        }
        vertex_count
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        for layer in &self.layers {
            match layer {
                TerminalPreparedLayer::Background(index) => {
                    let Some(resources) = self.background_resources.get(*index) else {
                        continue;
                    };
                    if resources.vertex_count == 0 {
                        continue;
                    }
                    render_pass.set_pipeline(&self.pipeline);
                    render_pass.set_vertex_buffer(0, resources.vertex_buffer.slice(..));
                    render_pass.draw(0..resources.vertex_count, 0..1);
                }
                TerminalPreparedLayer::Text(index) => {
                    let Some(resources) = &self.text_resources else {
                        continue;
                    };
                    let Some(layer) = resources.layers.get(*index) else {
                        continue;
                    };
                    if layer.vertex_count == 0 {
                        continue;
                    }
                    let Some(texture) = &self.text_texture else {
                        continue;
                    };
                    render_pass.set_pipeline(&self.text_pipeline);
                    render_pass.set_bind_group(0, &texture.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, layer.vertex_buffer.slice(..));
                    render_pass.draw(0..layer.vertex_count, 0..1);
                }
                TerminalPreparedLayer::Image(index) => {
                    let Some(Some(resources)) = self.image_resources.get(*index) else {
                        continue;
                    };
                    if resources.vertex_count == 0 {
                        continue;
                    }
                    render_pass.set_pipeline(&self.image_pipeline);
                    render_pass.set_bind_group(0, &resources.bind_group, &[]);
                    render_pass.set_vertex_buffer(0, resources.vertex_buffer.slice(..));
                    render_pass.draw(0..resources.vertex_count, 0..1);
                }
            }
        }
    }

    fn prepare_background_resources(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        batches: &mut [Vec<BackgroundVertex>],
    ) -> u32 {
        let mut vertex_count = 0;
        for (layer_index, vertices) in batches.iter_mut().enumerate() {
            if let Some(resources) = self.background_resources.get_mut(layer_index) {
                resources.update(device, queue, vertices);
            } else {
                self.background_resources
                    .push(TerminalBackgroundFrameResources::new(
                        device, queue, vertices,
                    ));
            }
            vertex_count += self.background_resources[layer_index].vertex_count;
        }
        self.background_resources.truncate(batches.len());
        vertex_count
    }

    fn prepare_text_resources<'a>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: SurfaceRect,
        batches: impl Iterator<Item = &'a [TexturedGlyphQuad]>,
        dirty_batches: &[bool],
    ) -> u32 {
        let mut batches = batches.peekable();
        if batches.peek().is_none() {
            self.text_resources = None;
            return 0;
        }

        self.prepare_text_texture(device, queue);
        if self.text_texture.is_none() {
            self.text_resources = None;
            return 0;
        }

        let mut resources = self.text_resources.take().unwrap_or_default();
        let mut vertex_count = 0;
        let mut vertices = std::mem::take(&mut self.text_vertex_scratch);
        for (layer_index, quads) in batches.enumerate() {
            let changed = dirty_batches.get(layer_index).copied().unwrap_or(true);
            if let Some(layer) = resources.layers.get_mut(layer_index) {
                if changed || layer.surface != Some(surface) {
                    layer.update(device, queue, surface, quads, changed, &mut vertices);
                }
            } else {
                resources.layers.push(TerminalTextLayerResources::new(
                    device,
                    queue,
                    surface,
                    quads,
                    &mut vertices,
                ));
            }
            vertex_count += resources.layers[layer_index].vertex_count;
        }
        self.text_vertex_scratch = vertices;
        resources.layers.truncate(dirty_batches.len());
        self.text_resources = Some(resources);
        vertex_count
    }

    fn prepare_text_texture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let (width, height) = self.text_builder.atlas_size();
        let format = self.text_builder.atlas_format();
        let modified_count = self.text_builder.atlas_modified_count();
        let resized_count = self.text_builder.atlas_resized_count();
        let needs_texture = self.text_texture.as_ref().is_none_or(|texture| {
            texture.width != width
                || texture.height != height
                || texture.format != format
                || texture.resized_count != resized_count
        });

        if needs_texture {
            self.text_texture = Some(TerminalTextAtlasTexture::new(
                device,
                &self.text_bind_group_layout,
                &self.text_sampler,
                width,
                height,
                format,
                resized_count,
            ));
        }

        let texture = self
            .text_texture
            .as_mut()
            .expect("text atlas texture exists");
        if texture.modified_count == modified_count {
            return;
        }

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            self.text_builder.atlas_pixels(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * format.depth()),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        texture.modified_count = modified_count;
    }
}

#[derive(Default)]
struct TerminalTextFrameResources {
    layers: Vec<TerminalTextLayerResources>,
}

struct TerminalTextAtlasTexture {
    texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
    format: GlyphAtlasFormat,
    modified_count: u64,
    resized_count: u64,
}

impl TerminalTextAtlasTexture {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
        resized_count: u64,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bootty_terminal_text_atlas"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: text_texture_format(format),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bootty_terminal_text_bind_group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });

        Self {
            texture,
            _view: view,
            bind_group,
            width,
            height,
            format,
            modified_count: u64::MAX,
            resized_count,
        }
    }
}

struct TerminalTextLayerResources {
    vertex_buffer: wgpu::Buffer,
    surface: Option<SurfaceRect>,
    quads: Vec<TexturedGlyphQuad>,
    vertex_count: u32,
    byte_capacity: usize,
}

impl TerminalTextLayerResources {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: SurfaceRect,
        quads: &[TexturedGlyphQuad],
        vertices: &mut Vec<TextVertex>,
    ) -> Self {
        let mut resources = Self {
            vertex_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bootty_terminal_text_vertices"),
                size: 1,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            surface: None,
            quads: Vec::new(),
            vertex_count: 0,
            byte_capacity: 0,
        };
        resources.update(device, queue, surface, quads, true, vertices);
        resources
    }

    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        surface: SurfaceRect,
        quads: &[TexturedGlyphQuad],
        changed: bool,
        vertices: &mut Vec<TextVertex>,
    ) {
        if quads.is_empty() {
            self.surface = None;
            self.quads.clear();
            self.vertex_count = 0;
            return;
        }
        if !changed && self.surface == Some(surface) {
            return;
        }
        if self.surface == Some(surface) && self.quads.as_slice() == quads {
            return;
        }

        vertices.clear();
        text_vertices_into(surface, quads, vertices);
        let vertex_count = vertices.len() as u32;
        let bytes = vertex_bytes(vertices);
        if bytes.len() > self.byte_capacity {
            self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bootty_terminal_text_vertices"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.byte_capacity = bytes.len();
        } else {
            queue.write_buffer(&self.vertex_buffer, 0, bytes);
        }
        self.surface = Some(surface);
        self.quads.clear();
        self.quads.extend_from_slice(quads);
        self.vertex_count = vertex_count;
    }
}

struct TerminalImageFrameResources {
    texture_key: TerminalImageTextureKey,
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    vertices: [TextVertex; 6],
    vertex_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalImageTextureKey {
    data_ptr: usize,
    data_len: usize,
    image_id: u32,
    image_width: u32,
    image_height: u32,
    image_format: libghostty_vt::kitty::graphics::ImageFormat,
}

impl TerminalImageTextureKey {
    fn from_image(image: &KittyImagePlacement) -> Self {
        Self {
            data_ptr: Arc::as_ptr(&image.data) as usize,
            data_len: image.data.len(),
            image_id: image.image_id,
            image_width: image.image_width,
            image_height: image.image_height,
            image_format: image.image_format,
        }
    }
}

impl TerminalImageFrameResources {
    fn update_vertices(&mut self, queue: &wgpu::Queue, vertices: [TextVertex; 6]) {
        if self.vertices != vertices {
            queue.write_buffer(&self.vertex_buffer, 0, vertex_bytes(&vertices));
            self.vertices = vertices;
        }
    }
}

fn prepare_image_resource(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface: SurfaceRect,
    image: &KittyImagePlacement,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    previous: Option<TerminalImageFrameResources>,
) -> Option<TerminalImageFrameResources> {
    if !image_fits_device_limits(device, image) {
        return None;
    }
    let vertices = image_vertices(surface, image)?;
    let texture_key = TerminalImageTextureKey::from_image(image);
    if let Some(mut previous) = previous
        && previous.texture_key == texture_key
    {
        previous.update_vertices(queue, vertices);
        return Some(previous);
    }
    let pixels = rgba_image_pixels(image)?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bootty_terminal_kitty_image"),
        size: wgpu::Extent3d {
            width: image.image_width,
            height: image.image_height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        pixels.as_ref(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(image.image_width * 4),
            rows_per_image: Some(image.image_height),
        },
        wgpu::Extent3d {
            width: image.image_width,
            height: image.image_height,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bootty_terminal_image_bind_group"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    Some(TerminalImageFrameResources {
        texture_key,
        _texture: texture,
        _view: view,
        bind_group,
        vertex_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bootty_terminal_image_vertices"),
            contents: vertex_bytes(&vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        }),
        vertices,
        vertex_count: vertices.len() as u32,
    })
}

fn background_batch_mut<'a>(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &'a mut Vec<Vec<BackgroundVertex>>,
    batch_count: &mut usize,
) -> &'a mut Vec<BackgroundVertex> {
    if let Some(TerminalPreparedLayer::Background(index)) = layers.last() {
        &mut batches[*index]
    } else {
        let index = *batch_count;
        *batch_count += 1;
        if index == batches.len() {
            batches.push(Vec::new());
        }
        layers.push(TerminalPreparedLayer::Background(index));
        &mut batches[index]
    }
}

fn push_background_quad(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<BackgroundVertex>>,
    batch_count: &mut usize,
    surface: SurfaceRect,
    draw: TerminalQuadDraw,
) {
    let vertices = background_quad_vertices(surface, draw);
    background_batch_mut(layers, batches, batch_count).extend(vertices);
}

fn push_decoration_command(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<BackgroundVertex>>,
    batch_count: &mut usize,
    surface: SurfaceRect,
    line: &crate::terminal_render::LineCommand,
) {
    let batch = background_batch_mut(layers, batches, batch_count);
    decoration_command_vertices_into(surface, line, batch);
}

fn push_text_command(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<TexturedGlyphQuad>>,
    dirty_batches: &mut Vec<bool>,
    batch_count: &mut usize,
    text_builder: &mut TextAtlasBuilder,
    command: &TextCommand,
    pixels_per_point: f32,
) {
    if let Some(TerminalPreparedLayer::Text(index)) = layers.last() {
        let changed = text_builder.prepare_text_command_into_frame(
            command,
            pixels_per_point,
            &mut batches[*index],
        );
        dirty_batches[*index] |= changed;
    } else {
        let index = *batch_count;
        *batch_count += 1;
        if index == batches.len() {
            batches.push(Vec::new());
        }
        if index == dirty_batches.len() {
            dirty_batches.push(false);
        } else {
            dirty_batches[index] = false;
        }
        dirty_batches[index] = text_builder.prepare_text_command_into_frame(
            command,
            pixels_per_point,
            &mut batches[index],
        );
        layers.push(TerminalPreparedLayer::Text(index));
    }
}

fn push_text_quad(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<TexturedGlyphQuad>>,
    dirty_batches: &mut Vec<bool>,
    batch_count: &mut usize,
    quad: TexturedGlyphQuad,
) {
    if let Some(TerminalPreparedLayer::Text(index)) = layers.last() {
        batches[*index].push(quad);
        dirty_batches[*index] = true;
    } else {
        let index = *batch_count;
        *batch_count += 1;
        if index == batches.len() {
            batches.push(Vec::new());
        }
        if index == dirty_batches.len() {
            dirty_batches.push(true);
        } else {
            dirty_batches[index] = true;
        }
        batches[index].push(quad);
        layers.push(TerminalPreparedLayer::Text(index));
    }
}

fn decoration_command_vertices_into(
    surface: SurfaceRect,
    line: &crate::terminal_render::LineCommand,
    vertices: &mut Vec<BackgroundVertex>,
) {
    emit_decoration_draws(line, |draw| {
        vertices.extend(background_quad_vertices(surface, draw));
    });
}

fn decoration_draws(line: &crate::terminal_render::LineCommand) -> Vec<TerminalQuadDraw> {
    let mut draws = Vec::new();
    emit_decoration_draws(line, |draw| draws.push(draw));
    draws
}

fn emit_decoration_draws(
    line: &crate::terminal_render::LineCommand,
    mut emit: impl FnMut(TerminalQuadDraw),
) {
    let min_x = line.start_x.min(line.end_x);
    let max_x = line.start_x.max(line.end_x);
    let min_y = line.start_y.min(line.end_y);
    let max_y = line.start_y.max(line.end_y);
    let rect = if (line.end_y - line.start_y).abs() <= (line.end_x - line.start_x).abs() {
        SurfaceRect::from_min_size(min_x, line.start_y - 0.5, (max_x - min_x).max(1.0), 1.0)
    } else {
        SurfaceRect::from_min_size(line.start_x - 0.5, min_y, 1.0, (max_y - min_y).max(1.0))
    };

    match line.style {
        DecorationStyle::Double => {
            emit(TerminalQuadDraw {
                rect: SurfaceRect::from_min_size(rect.min_x, rect.min_y - 1.0, rect.width(), 1.0),
                color: line.color,
            });
            emit(TerminalQuadDraw {
                rect: SurfaceRect::from_min_size(rect.min_x, rect.min_y + 1.0, rect.width(), 1.0),
                color: line.color,
            });
        }
        DecorationStyle::Dotted => {
            emit_segmented_decoration_draws(rect, line.color, 1.0, 2.0, emit)
        }
        DecorationStyle::Dashed => {
            emit_segmented_decoration_draws(rect, line.color, 4.0, 3.0, emit)
        }
        DecorationStyle::Curly => emit_curly_decoration_draws(rect, line.color, emit),
        DecorationStyle::Single | DecorationStyle::Strikethrough | DecorationStyle::Overline => {
            emit(TerminalQuadDraw {
                rect,
                color: line.color,
            });
        }
    }
}

fn emit_segmented_decoration_draws(
    rect: SurfaceRect,
    color: PlanColor,
    segment_width: f32,
    gap_width: f32,
    mut emit: impl FnMut(TerminalQuadDraw),
) {
    let mut x = rect.min_x;
    while x < rect.max_x {
        let width = segment_width.min(rect.max_x - x).max(1.0);
        emit(TerminalQuadDraw {
            rect: SurfaceRect::from_min_size(x, rect.min_y, width, rect.height()),
            color,
        });
        x += segment_width + gap_width;
    }
}

fn emit_curly_decoration_draws(
    rect: SurfaceRect,
    color: PlanColor,
    mut emit: impl FnMut(TerminalQuadDraw),
) {
    let mut x = rect.min_x;
    let mut high = true;
    while x < rect.max_x {
        let y = if high {
            rect.min_y - 1.0
        } else {
            rect.min_y + 1.0
        };
        emit(TerminalQuadDraw {
            rect: SurfaceRect::from_min_size(x, y, 2.0_f32.min(rect.max_x - x).max(1.0), 1.0),
            color,
        });
        high = !high;
        x += 2.0;
    }
}

fn cursor_draws(cursor: &CursorCommand) -> Vec<TerminalCursorDraw> {
    // Cursor blink timing and opacity are resolved before this backend; WGPU
    // only draws the current cursor command color/alpha.
    match cursor.shape {
        crate::paint_plan::CursorShape::Bar
        | crate::paint_plan::CursorShape::Underline
        | crate::paint_plan::CursorShape::Block => vec![TerminalCursorDraw {
            rect: cursor.fill_rect,
            color: cursor.color,
        }],
        crate::paint_plan::CursorShape::HollowBlock => hollow_cursor_draws(cursor),
    }
}

fn hollow_cursor_draws(cursor: &CursorCommand) -> Vec<TerminalCursorDraw> {
    hollow_cursor_rects(cursor)
        .into_iter()
        .map(|rect| TerminalCursorDraw {
            rect,
            color: cursor.color,
        })
        .collect()
}

fn hollow_cursor_rects(cursor: &CursorCommand) -> [SurfaceRect; 4] {
    let rect = cursor.rect;
    let stroke = 1.0_f32.min(rect.width()).min(rect.height());
    let right_x = (rect.max_x - stroke).max(rect.min_x);
    let bottom_y = (rect.max_y - stroke).max(rect.min_y);

    [
        SurfaceRect::from_min_size(rect.min_x, rect.min_y, rect.width(), stroke),
        SurfaceRect::from_min_size(rect.min_x, bottom_y, rect.width(), stroke),
        SurfaceRect::from_min_size(rect.min_x, rect.min_y, stroke, rect.height()),
        SurfaceRect::from_min_size(right_x, rect.min_y, stroke, rect.height()),
    ]
}

fn push_cursor_background_quads(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<BackgroundVertex>>,
    batch_count: &mut usize,
    surface: SurfaceRect,
    cursor: &CursorCommand,
) {
    match cursor.shape {
        crate::paint_plan::CursorShape::Bar
        | crate::paint_plan::CursorShape::Underline
        | crate::paint_plan::CursorShape::Block => push_background_quad(
            layers,
            batches,
            batch_count,
            surface,
            TerminalQuadDraw {
                rect: cursor.fill_rect,
                color: cursor.color,
            },
        ),
        crate::paint_plan::CursorShape::HollowBlock => {
            for rect in hollow_cursor_rects(cursor) {
                push_background_quad(
                    layers,
                    batches,
                    batch_count,
                    surface,
                    TerminalQuadDraw {
                        rect,
                        color: cursor.color,
                    },
                );
            }
        }
    }
}

fn sprite_draw(command: &SpriteCommandBatch) -> TerminalSpriteDraw {
    let primitives = WgpuSpriteBackend::build_primitives(&command.commands, command.color);
    TerminalSpriteDraw {
        ch: command.ch,
        vertices: primitives.vertices,
        indices: primitives.indices,
    }
}

fn text_draws(command: &TextCommand, pixels_per_point: f32) -> Vec<TerminalTextDraw> {
    let mut draws = Vec::new();
    let pixels_per_point = pixels_per_point.max(1.0);
    if command.text.is_empty() {
        return draws;
    }
    let total_cells = command
        .text
        .chars()
        .map(crate::terminal_text::terminal_char_width)
        .sum::<u16>()
        .max(1);

    let cell_width = command.rect.width() / f32::from(total_cells);
    crate::terminal_text::for_terminal_text_cells(&command.text, |cell, text| {
        let cells = text
            .chars()
            .map(crate::terminal_text::terminal_char_width)
            .sum::<u16>()
            .max(1);
        let cell_rect = SurfaceRect::from_min_size(
            command.rect.min_x + f32::from(cell) * cell_width,
            command.rect.min_y,
            cell_width * f32::from(cells),
            command.rect.height(),
        );
        for ch in text.chars() {
            if ch == ' ' {
                continue;
            }
            let font = terminal_font_for_char(&command.face, ch);
            draws.extend(text_glyph_draws(
                ch,
                cell_rect,
                command.attrs.fg,
                command.font_size,
                pixels_per_point,
                font.as_ref(),
            ));
        }
    });
    draws
}

fn egui_rect(rect: SurfaceRect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::Pos2::new(rect.min_x, rect.min_y),
        egui::Pos2::new(rect.max_x, rect.max_y),
    )
}

#[cfg(test)]
mod tests;
