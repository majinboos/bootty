use crate::{
    geometry::{
        CellMetrics, DEFAULT_CELL_WIDTH, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT, SurfaceRect,
        ViewTransform,
    },
    paint_plan::{DecorationStyle, PlanColor},
    terminal_image::KittyImagePlacement,
    terminal_render::{CursorCommand, TerminalRenderCommand, TerminalRenderFrame, TextCommand},
    terminal_text_atlas::{GlyphAtlasEntry, GlyphAtlasFormat, TextAtlasBuilder, TexturedGlyphQuad},
};
use eframe::{egui, egui_wgpu, wgpu};
use wgpu::util::DeviceExt;

mod font_lookup;
mod image_upload;
mod pipelines;
mod vertices;

use font_lookup::{ghostty_cell_metrics_from_font, terminal_font};
use image_upload::{image_fits_device_limits, rgba_image_texture_pixels};
use pipelines::{
    background_pipeline, image_bind_group_layout, image_pipeline, text_bind_group_layout,
    text_pipeline, text_texture_format,
};
use std::collections::HashMap;
use std::sync::{
    Arc, Weak,
    atomic::{AtomicU64, Ordering},
};
use vertices::{
    BackgroundVertex, TerminalQuadDraw, TextVertex, background_quad_vertices, image_vertices,
    text_vertices_into, vertex_bytes,
};

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
    let ratio = config.font_size.max(1.0) / DEFAULT_FONT_SIZE;
    let mut cell = terminal_font(&face)
        .map(|font| ghostty_cell_metrics_from_font(&font, config.font_size))
        .unwrap_or_else(|| {
            CellMetrics::new(DEFAULT_CELL_WIDTH * ratio, DEFAULT_LINE_HEIGHT * ratio)
        });

    if let Some(width) = config.cell_width {
        cell.width = width.max(1.0);
    }
    if let Some(height) = config.cell_height {
        cell.height = height.max(1.0);
    }
    cell
}

pub fn terminal_render_callback_for_renderer(
    renderer_id: TerminalRendererId,
    frame: &TerminalRenderFrame,
    target_format: wgpu::TextureFormat,
    view: ViewTransform,
) -> Option<egui::Shape> {
    if !has_wgpu_draw_commands(&frame.commands) {
        return None;
    }

    Some(
        egui_wgpu::Callback::new_paint_callback(
            egui_rect(frame.surface),
            TerminalRenderCallback {
                key: TerminalCallbackKey {
                    renderer: renderer_id.value,
                    target_format,
                },
                lifetime: Arc::clone(&renderer_id.lifetime),
                frame: frame.clone(),
                view,
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
    key: TerminalCallbackKey,
    lifetime: Arc<()>,
    frame: TerminalRenderFrame,
    view: ViewTransform,
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
        let TerminalWgpuRendererCache {
            renderers,
            text_builder,
        } = cache;
        renderers.retain(|_, cached| cached.is_alive());
        let cached = renderers
            .entry(self.key)
            .or_insert_with(|| CachedTerminalRenderer {
                renderer: TerminalWgpuRenderer::new(device, self.key.target_format),
                lifetime: Arc::downgrade(&self.lifetime),
            });
        cached.renderer.prepare_terminal_frame_with_text_builder(
            device,
            queue,
            text_builder,
            &self.frame,
            screen_descriptor.pixels_per_point,
            self.view,
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
        if let Some(cached) = cache.renderers.get(&self.key) {
            cached.renderer.paint(render_pass);
        };
    }
}

struct TerminalWgpuRendererCache {
    renderers: HashMap<TerminalCallbackKey, CachedTerminalRenderer>,
    text_builder: TextAtlasBuilder,
}

struct CachedTerminalRenderer {
    renderer: TerminalWgpuRenderer,
    lifetime: Weak<()>,
}

impl CachedTerminalRenderer {
    fn is_alive(&self) -> bool {
        self.lifetime.strong_count() > 0
    }
}

impl Default for TerminalWgpuRendererCache {
    fn default() -> Self {
        Self {
            renderers: HashMap::new(),
            text_builder: TextAtlasBuilder::new_rgba(1024, 1024),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TerminalRendererId {
    value: u64,
    lifetime: Arc<()>,
}

impl TerminalRendererId {
    pub fn unique() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            value: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            lifetime: Arc::new(()),
        }
    }
}

impl Default for TerminalRendererId {
    fn default() -> Self {
        Self::unique()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TerminalCallbackKey {
    renderer: u64,
    target_format: wgpu::TextureFormat,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActiveTerminalPipeline {
    Background,
    Text,
    Image,
}

struct PreparedTerminalFrameCache {
    frame: TerminalRenderFrame,
    pixels_per_point_bits: u32,
    view_bits: [u32; 3],
    // Atlas growth shifts every glyph's UVs, so a stale count must invalidate the cached vertices.
    atlas_resized_count: u64,
    vertex_count: u32,
}

impl PreparedTerminalFrameCache {
    fn matches(
        &self,
        frame: &TerminalRenderFrame,
        pixels_per_point_bits: u32,
        view_bits: [u32; 3],
        atlas_resized_count: u64,
    ) -> bool {
        self.pixels_per_point_bits == pixels_per_point_bits
            && self.view_bits == view_bits
            && self.atlas_resized_count == atlas_resized_count
            && self.frame == *frame
    }
}

const PREPARED_FRAME_CACHE_MISS_COOLDOWN: u8 = 8;

pub struct TerminalWgpuRenderer {
    pipeline: wgpu::RenderPipeline,
    text_pipeline: wgpu::RenderPipeline,
    image_pipeline: wgpu::RenderPipeline,
    text_bind_group_layout: wgpu::BindGroupLayout,
    image_bind_group_layout: wgpu::BindGroupLayout,
    text_sampler: wgpu::Sampler,
    text_sampler_linear: wgpu::Sampler,
    image_sampler: wgpu::Sampler,
    text_texture: Option<TerminalTextAtlasTexture>,
    local_text_builder: Option<TextAtlasBuilder>,
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
    text_linear_filter: bool,
    last_text_upload_bytes: usize,
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
            text_sampler_linear: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bootty_terminal_text_sampler_linear"),
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
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
            text_texture: None,
            local_text_builder: None,
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
            text_linear_filter: false,
            last_text_upload_bytes: 0,
        }
    }

    pub fn prepare_terminal_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &TerminalRenderFrame,
        pixels_per_point: f32,
        view: ViewTransform,
    ) -> u32 {
        let mut text_builder = self
            .local_text_builder
            .take()
            .unwrap_or_else(|| TextAtlasBuilder::new_rgba(1024, 1024));
        let vertex_count = self.prepare_terminal_frame_with_text_builder(
            device,
            queue,
            &mut text_builder,
            frame,
            pixels_per_point,
            view,
        );
        self.local_text_builder = Some(text_builder);
        vertex_count
    }

    fn prepare_terminal_frame_with_text_builder(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text_builder: &mut TextAtlasBuilder,
        frame: &TerminalRenderFrame,
        pixels_per_point: f32,
        view: ViewTransform,
    ) -> u32 {
        let raster_ppp = text_raster_pixels_per_point(pixels_per_point, view);
        // Set before the cache early-return so `paint` always picks the right sampler.
        self.text_linear_filter = text_uses_linear_filter(pixels_per_point, view, raster_ppp);
        let render_surface = view.applied_to(frame.surface);
        let pixels_per_point_bits = raster_ppp.to_bits();
        let view_bits = [
            view.zoom.to_bits(),
            view.pan_x.to_bits(),
            view.pan_y.to_bits(),
        ];
        let mut atlas_resized_count = text_builder.atlas_resized_count();
        let mut update_frame_cache = true;
        if self.prepared_frame_cache_cooldown > 0 {
            self.prepared_frame_cache_cooldown -= 1;
            update_frame_cache = self.prepared_frame_cache_cooldown == 0;
        } else if let Some(cache) = &self.prepared_frame_cache {
            if cache.matches(frame, pixels_per_point_bits, view_bits, atlas_resized_count) {
                return cache.vertex_count;
            }
            self.prepared_frame_cache_cooldown = PREPARED_FRAME_CACHE_MISS_COOLDOWN;
            update_frame_cache = false;
        }

        // If the atlas grows mid-build (a new glyph forces a resize), quads cached before the grow
        // keep pre-resize UVs and would render existing glyphs against the larger atlas, some
        // sampling empty space and vanishing until the next repaint. The atlas now fits every
        // glyph, so rebuild once against the final size: cache entries from before the grow miss on
        // the changed resize count and re-emit correct UVs.
        let mut build_attempt = 0;
        loop {
            build_attempt += 1;
            self.layers.clear();
            let mut previous_image_resources =
                std::mem::take(&mut self.image_resources).into_iter();

            let mut background_batches = std::mem::take(&mut self.background_batch_scratch);
            let mut text_batches = std::mem::take(&mut self.text_batch_scratch);
            let mut text_batch_dirty = std::mem::take(&mut self.text_batch_dirty_scratch);
            background_batches.iter_mut().for_each(Vec::clear);
            text_batches.iter_mut().for_each(Vec::clear);
            text_batch_dirty.clear();
            let mut background_batch_count = 0;
            let mut text_batch_count = 0;
            let mut image_vertex_count = 0;

            text_builder.begin_text_frame();
            for command in &frame.commands {
                match command {
                    TerminalRenderCommand::Text(text) => {
                        push_text_command(
                            &mut self.layers,
                            &mut text_batches,
                            &mut text_batch_dirty,
                            &mut text_batch_count,
                            text_builder,
                            text,
                            raster_ppp,
                        );
                    }
                    TerminalRenderCommand::Sprite(sprite) => {
                        let quad = text_builder.prepare_sprite_command(sprite, raster_ppp);
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
                            render_surface,
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
                            render_surface,
                            line,
                        );
                    }
                    TerminalRenderCommand::Cursor(cursor) => push_cursor_background_quads(
                        &mut self.layers,
                        &mut background_batches,
                        &mut background_batch_count,
                        render_surface,
                        cursor,
                    ),
                    TerminalRenderCommand::Image(image) => {
                        let previous = previous_image_resources.next().flatten();
                        let resources = prepare_image_resource(
                            device,
                            queue,
                            ImageRenderTarget {
                                surface: render_surface,
                                pixels_per_point,
                            },
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
            text_builder.finish_text_frame();

            let background_vertex_count = self.prepare_background_resources(
                device,
                queue,
                &mut background_batches[..background_batch_count],
            );
            let text_vertex_count = self.prepare_text_resources(
                device,
                queue,
                text_builder,
                TextRenderTarget {
                    surface: render_surface,
                    ppp: pixels_per_point,
                },
                text_batches[..text_batch_count].iter().map(Vec::as_slice),
                &text_batch_dirty[..text_batch_count],
            );
            self.background_batch_scratch = background_batches;
            self.text_batch_scratch = text_batches;
            self.text_batch_dirty_scratch = text_batch_dirty;

            let vertex_count = image_vertex_count + background_vertex_count + text_vertex_count;
            let grew_during_build = text_builder.atlas_resized_count() != atlas_resized_count;
            if grew_during_build && build_attempt < 2 {
                text_builder.reset_atlas_for_frame_rebuild();
                atlas_resized_count = text_builder.atlas_resized_count();
                continue;
            }
            if update_frame_cache && !grew_during_build {
                self.prepared_frame_cache = Some(PreparedTerminalFrameCache {
                    frame: frame.clone(),
                    pixels_per_point_bits,
                    view_bits,
                    atlas_resized_count,
                    vertex_count,
                });
            }
            break vertex_count;
        }
    }

    pub fn paint(&self, render_pass: &mut wgpu::RenderPass<'_>) {
        let mut active_pipeline = None;
        for layer in &self.layers {
            match layer {
                TerminalPreparedLayer::Background(index) => {
                    let Some(resources) = self.background_resources.get(*index) else {
                        continue;
                    };
                    if resources.vertex_count == 0 {
                        continue;
                    }
                    if active_pipeline != Some(ActiveTerminalPipeline::Background) {
                        render_pass.set_pipeline(&self.pipeline);
                        active_pipeline = Some(ActiveTerminalPipeline::Background);
                    }
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
                    if active_pipeline != Some(ActiveTerminalPipeline::Text) {
                        render_pass.set_pipeline(&self.text_pipeline);
                        active_pipeline = Some(ActiveTerminalPipeline::Text);
                    }
                    let bind_group = if self.text_linear_filter {
                        &texture.bind_group_linear
                    } else {
                        &texture.bind_group
                    };
                    render_pass.set_bind_group(0, bind_group, &[]);
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
                    if active_pipeline != Some(ActiveTerminalPipeline::Image) {
                        render_pass.set_pipeline(&self.image_pipeline);
                        active_pipeline = Some(ActiveTerminalPipeline::Image);
                    }
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
        text_builder: &TextAtlasBuilder,
        target: TextRenderTarget,
        batches: impl Iterator<Item = &'a [TexturedGlyphQuad]>,
        dirty_batches: &[bool],
    ) -> u32 {
        let mut batches = batches.peekable();
        if batches.peek().is_none() {
            self.text_resources = None;
            return 0;
        }

        self.prepare_text_texture(device, queue, text_builder);
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
                if changed
                    || layer.surface != Some(target.surface)
                    || layer.pixels_per_point != target.ppp
                {
                    layer.update(device, queue, target, quads, changed, &mut vertices);
                }
            } else {
                resources.layers.push(TerminalTextLayerResources::new(
                    device,
                    queue,
                    target,
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

    pub fn last_text_upload_bytes(&self) -> usize {
        self.last_text_upload_bytes
    }

    fn prepare_text_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text_builder: &TextAtlasBuilder,
    ) {
        self.last_text_upload_bytes = 0;
        let (width, height) = text_builder.atlas_size();
        let format = text_builder.atlas_format();
        let modified_count = text_builder.atlas_modified_count();
        let resized_count = text_builder.atlas_resized_count();
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
                (&self.text_sampler, &self.text_sampler_linear),
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
        let region = if needs_texture {
            GlyphAtlasEntry {
                x: 0,
                y: 0,
                width,
                height,
            }
        } else {
            let Some(region) = text_builder.atlas_dirty_rect_since(texture.modified_count) else {
                texture.modified_count = modified_count;
                return;
            };
            region
        };
        let depth = format.depth();
        let bytes_per_row = width * depth;
        let offset = ((region.y * width + region.x) * depth) as usize;
        let byte_len = ((region.height - 1) * bytes_per_row + region.width * depth) as usize;
        let pixels = &text_builder.atlas_pixels()[offset..offset + byte_len];

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: region.x,
                    y: region.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(region.height),
            },
            wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
        );
        self.last_text_upload_bytes = (region.width * region.height * depth) as usize;
        texture.modified_count = modified_count;
    }
}

fn text_raster_pixels_per_point(pixels_per_point: f32, view: ViewTransform) -> f32 {
    // Zoomed text already supersamples by integer zoom steps. Windows gets an extra base
    // supersample because the generic outline rasterizer has no DirectWrite/ClearType hinting.
    pixels_per_point * view.raster_supersample() * platform_base_text_supersample()
}

fn text_uses_linear_filter(
    pixels_per_point: f32,
    view: ViewTransform,
    raster_pixels_per_point: f32,
) -> bool {
    view.is_zoomed()
        || raster_pixels_per_point > pixels_per_point + f32::EPSILON
        || has_fractional_scale(pixels_per_point)
}

fn has_fractional_scale(value: f32) -> bool {
    (value - value.round()).abs() > 0.001
}

#[cfg(windows)]
const fn platform_base_text_supersample() -> f32 {
    2.0
}

#[cfg(not(windows))]
const fn platform_base_text_supersample() -> f32 {
    1.0
}

#[derive(Default)]
struct TerminalTextFrameResources {
    layers: Vec<TerminalTextLayerResources>,
}

struct TerminalTextAtlasTexture {
    texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    bind_group_linear: wgpu::BindGroup,
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
        samplers: (&wgpu::Sampler, &wgpu::Sampler),
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
        resized_count: u64,
    ) -> Self {
        let (sampler, sampler_linear) = samplers;
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
        let text_bind_group = |label, sampler| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
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
            })
        };
        let bind_group = text_bind_group("bootty_terminal_text_bind_group", sampler);
        let bind_group_linear =
            text_bind_group("bootty_terminal_text_bind_group_linear", sampler_linear);

        Self {
            texture,
            _view: view,
            bind_group,
            bind_group_linear,
            width,
            height,
            format,
            modified_count: u64::MAX,
            resized_count,
        }
    }
}

/// The geometry a text layer's glyph quads are projected onto: the surface rect they map into and
/// the physical pixel density they snap to. Bundled so the render path threads one value instead of
/// a pair.
#[derive(Clone, Copy)]
struct TextRenderTarget {
    surface: SurfaceRect,
    ppp: f32,
}

struct TerminalTextLayerResources {
    vertex_buffer: wgpu::Buffer,
    surface: Option<SurfaceRect>,
    pixels_per_point: f32,
    quads: Vec<TexturedGlyphQuad>,
    vertex_count: u32,
    byte_capacity: usize,
}

impl TerminalTextLayerResources {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: TextRenderTarget,
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
            pixels_per_point: target.ppp,
            quads: Vec::new(),
            vertex_count: 0,
            byte_capacity: 0,
        };
        resources.update(device, queue, target, quads, true, vertices);
        resources
    }

    fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: TextRenderTarget,
        quads: &[TexturedGlyphQuad],
        changed: bool,
        vertices: &mut Vec<TextVertex>,
    ) {
        let TextRenderTarget { surface, ppp } = target;
        self.pixels_per_point = ppp;
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
        text_vertices_into(surface, ppp, quads, vertices);
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

#[derive(Clone, Copy)]
struct ImageRenderTarget {
    surface: SurfaceRect,
    pixels_per_point: f32,
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
    target: ImageRenderTarget,
    image: &KittyImagePlacement,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    previous: Option<TerminalImageFrameResources>,
) -> Option<TerminalImageFrameResources> {
    if !image_fits_device_limits(device, image) {
        return None;
    }
    let vertices = image_vertices(target.surface, target.pixels_per_point, image)?;
    let texture_key = TerminalImageTextureKey::from_image(image);
    if let Some(mut previous) = previous
        && previous.texture_key == texture_key
    {
        previous.update_vertices(queue, vertices);
        return Some(previous);
    }
    let pixels = rgba_image_texture_pixels(image)?;
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
        format: terminal_image_texture_format(),
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

fn terminal_image_texture_format() -> wgpu::TextureFormat {
    wgpu::TextureFormat::Rgba8Unorm
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

fn egui_rect(rect: SurfaceRect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::Pos2::new(rect.min_x, rect.min_y),
        egui::Pos2::new(rect.max_x, rect.max_y),
    )
}
