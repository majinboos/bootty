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

#[cfg(test)]
use font_lookup::{GHOSTTY_FONT_FAMILY_PRIORITY, terminal_font_family_priority};
use font_lookup::{ghostty_cell_metrics_from_font, terminal_font, terminal_font_for_char};
use glyph_draw::text_glyph_draws;
use image_upload::{image_fits_device_limits, rgba_image_pixels, source_uv_rect};
use pipelines::{
    background_pipeline, image_bind_group_layout, image_pipeline, text_bind_group_layout,
    text_pipeline, text_texture_format,
};
use std::collections::HashMap;

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
    vertex_count: u32,
    byte_capacity: usize,
}

impl TerminalBackgroundFrameResources {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[BackgroundVertex]) -> Self {
        let mut resources = Self {
            vertex_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bootty_terminal_renderer_vertices"),
                size: 1,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
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
        vertices: &[BackgroundVertex],
    ) {
        let bytes = vertex_bytes(vertices);
        if bytes.len() > self.byte_capacity {
            self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bootty_terminal_renderer_vertices"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.byte_capacity = bytes.len();
        } else if !bytes.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytes);
        }
        self.vertex_count = vertices.len() as u32;
    }
}

enum TerminalPreparedLayer {
    Background(usize),
    Text(usize),
    Image(usize),
}

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
    image_resources: Vec<Option<TerminalImageFrameResources>>,
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
            image_resources: Vec::new(),
        }
    }

    pub fn prepare_terminal_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &TerminalRenderFrame,
        pixels_per_point: f32,
    ) -> u32 {
        self.layers.clear();
        self.image_resources.clear();

        let mut background_batches = Vec::new();
        let mut text_batches = Vec::new();
        let mut image_vertex_count = 0;

        for command in &frame.commands {
            match command {
                TerminalRenderCommand::Text(text) => {
                    let quads = self
                        .text_builder
                        .prepare_text_command(text, pixels_per_point);
                    push_text_batch(&mut self.layers, &mut text_batches, quads);
                }
                TerminalRenderCommand::Sprite(sprite) => {
                    let quads = self
                        .text_builder
                        .prepare_sprite_command(sprite, pixels_per_point);
                    push_text_batch(&mut self.layers, &mut text_batches, quads);
                }
                TerminalRenderCommand::FillRect(_)
                | TerminalRenderCommand::Decoration(_)
                | TerminalRenderCommand::Cursor(_) => {
                    push_background_batch(
                        &mut self.layers,
                        &mut background_batches,
                        background_command_vertices(frame.surface, command),
                    );
                }
                TerminalRenderCommand::Image(image) => {
                    let resources = prepare_image_resource(
                        device,
                        queue,
                        frame.surface,
                        image,
                        &self.image_bind_group_layout,
                        &self.image_sampler,
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

        image_vertex_count
            + self.prepare_background_resources(device, queue, &background_batches)
            + self.prepare_text_resources(
                device,
                queue,
                frame.surface,
                text_batches.iter().map(Vec::as_slice),
            )
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
        batches: &[Vec<BackgroundVertex>],
    ) -> u32 {
        let mut vertex_count = 0;
        for (layer_index, vertices) in batches.iter().enumerate() {
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
        let mut layer_index = 0;
        for quads in batches {
            let vertices = text_vertices(surface, quads);
            if let Some(layer) = resources.layers.get_mut(layer_index) {
                layer.update(device, queue, &vertices);
            } else {
                resources
                    .layers
                    .push(TerminalTextLayerResources::new(device, queue, &vertices));
            }
            vertex_count += resources.layers[layer_index].vertex_count;
            layer_index += 1;
        }
        resources.layers.truncate(layer_index);
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
    vertex_count: u32,
    byte_capacity: usize,
}

impl TerminalTextLayerResources {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[TextVertex]) -> Self {
        let mut resources = Self {
            vertex_buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bootty_terminal_text_vertices"),
                size: 1,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            vertex_count: 0,
            byte_capacity: 0,
        };
        resources.update(device, queue, vertices);
        resources
    }

    fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, vertices: &[TextVertex]) {
        let bytes = vertex_bytes(vertices);
        if bytes.len() > self.byte_capacity {
            self.vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bootty_terminal_text_vertices"),
                contents: bytes,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            });
            self.byte_capacity = bytes.len();
        } else if !bytes.is_empty() {
            queue.write_buffer(&self.vertex_buffer, 0, bytes);
        }
        self.vertex_count = vertices.len() as u32;
    }
}

struct TerminalImageFrameResources {
    _texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    vertex_count: u32,
}

fn prepare_image_resource(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    surface: SurfaceRect,
    image: &KittyImagePlacement,
    bind_group_layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
) -> Option<TerminalImageFrameResources> {
    if !image_fits_device_limits(device, image) {
        return None;
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
    let vertices = image_vertices(surface, image);
    if vertices.is_empty() {
        return None;
    }
    Some(TerminalImageFrameResources {
        _texture: texture,
        _view: view,
        bind_group,
        vertex_buffer: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bootty_terminal_image_vertices"),
            contents: vertex_bytes(&vertices),
            usage: wgpu::BufferUsages::VERTEX,
        }),
        vertex_count: vertices.len() as u32,
    })
}

fn push_background_batch(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<BackgroundVertex>>,
    vertices: Vec<BackgroundVertex>,
) {
    if vertices.is_empty() {
        return;
    }
    if let Some(TerminalPreparedLayer::Background(index)) = layers.last() {
        batches[*index].extend(vertices);
    } else {
        let index = batches.len();
        batches.push(vertices);
        layers.push(TerminalPreparedLayer::Background(index));
    }
}

fn push_text_batch(
    layers: &mut Vec<TerminalPreparedLayer>,
    batches: &mut Vec<Vec<TexturedGlyphQuad>>,
    quads: Vec<TexturedGlyphQuad>,
) {
    if quads.is_empty() {
        return;
    }
    if let Some(TerminalPreparedLayer::Text(index)) = layers.last() {
        batches[*index].extend(quads);
    } else {
        let index = batches.len();
        batches.push(quads);
        layers.push(TerminalPreparedLayer::Text(index));
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TerminalQuadDraw {
    rect: SurfaceRect,
    color: PlanColor,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct BackgroundVertex {
    position: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct TextVertex {
    position: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
}

fn background_vertices(surface: SurfaceRect, draws: &[TerminalQuadDraw]) -> Vec<BackgroundVertex> {
    let mut vertices = Vec::with_capacity(draws.len() * 6);
    for draw in draws {
        let min = surface_to_ndc(surface, draw.rect.min_x, draw.rect.min_y);
        let max = surface_to_ndc(surface, draw.rect.max_x, draw.rect.max_y);
        let top_left = [min[0], min[1]];
        let top_right = [max[0], min[1]];
        let bottom_right = [max[0], max[1]];
        let bottom_left = [min[0], max[1]];
        let color = color_to_float(draw.color);

        vertices.extend([
            BackgroundVertex {
                position: top_left,
                color,
            },
            BackgroundVertex {
                position: bottom_left,
                color,
            },
            BackgroundVertex {
                position: bottom_right,
                color,
            },
            BackgroundVertex {
                position: top_left,
                color,
            },
            BackgroundVertex {
                position: bottom_right,
                color,
            },
            BackgroundVertex {
                position: top_right,
                color,
            },
        ]);
    }
    vertices
}

fn text_vertices(surface: SurfaceRect, quads: &[TexturedGlyphQuad]) -> Vec<TextVertex> {
    let mut vertices = Vec::with_capacity(quads.len() * 6);
    for quad in quads {
        let min = surface_to_ndc(surface, quad.rect.min_x, quad.rect.min_y);
        let max = surface_to_ndc(surface, quad.rect.max_x, quad.rect.max_y);
        let color = color_to_float(quad.color);
        let top_left = TextVertex {
            position: [min[0], min[1]],
            uv: [quad.uv.min_x, quad.uv.min_y],
            color,
        };
        let top_right = TextVertex {
            position: [max[0], min[1]],
            uv: [quad.uv.max_x, quad.uv.min_y],
            color,
        };
        let bottom_right = TextVertex {
            position: [max[0], max[1]],
            uv: [quad.uv.max_x, quad.uv.max_y],
            color,
        };
        let bottom_left = TextVertex {
            position: [min[0], max[1]],
            uv: [quad.uv.min_x, quad.uv.max_y],
            color,
        };
        vertices.extend([
            top_left,
            bottom_left,
            bottom_right,
            top_left,
            bottom_right,
            top_right,
        ]);
    }
    vertices
}

fn image_vertices(surface: SurfaceRect, image: &KittyImagePlacement) -> Vec<TextVertex> {
    let Some(uv) = source_uv_rect(image) else {
        return Vec::new();
    };
    let min = surface_to_ndc(surface, image.destination.min_x, image.destination.min_y);
    let max = surface_to_ndc(surface, image.destination.max_x, image.destination.max_y);
    let color = [1.0, 1.0, 1.0, 1.0];
    let top_left = TextVertex {
        position: [min[0], min[1]],
        uv: [uv.min_x, uv.min_y],
        color,
    };
    let top_right = TextVertex {
        position: [max[0], min[1]],
        uv: [uv.max_x, uv.min_y],
        color,
    };
    let bottom_right = TextVertex {
        position: [max[0], max[1]],
        uv: [uv.max_x, uv.max_y],
        color,
    };
    let bottom_left = TextVertex {
        position: [min[0], max[1]],
        uv: [uv.min_x, uv.max_y],
        color,
    };
    vec![
        top_left,
        bottom_left,
        bottom_right,
        top_left,
        bottom_right,
        top_right,
    ]
}

fn background_command_vertices(
    surface: SurfaceRect,
    command: &TerminalRenderCommand,
) -> Vec<BackgroundVertex> {
    match command {
        TerminalRenderCommand::FillRect(fill) => background_vertices(
            surface,
            &[TerminalQuadDraw {
                rect: fill.rect,
                color: fill.color,
            }],
        ),
        TerminalRenderCommand::Cursor(cursor) => cursor_command_vertices(surface, cursor),
        TerminalRenderCommand::Decoration(line) => decoration_command_vertices(surface, line),
        TerminalRenderCommand::Text(_)
        | TerminalRenderCommand::Sprite(_)
        | TerminalRenderCommand::Image(_)
        | TerminalRenderCommand::KittyVirtualPlacement(_) => Vec::new(),
    }
}

fn decoration_command_vertices(
    surface: SurfaceRect,
    line: &crate::terminal_render::LineCommand,
) -> Vec<BackgroundVertex> {
    let draws = decoration_draws(line);
    background_vertices(surface, &draws)
}

fn decoration_draws(line: &crate::terminal_render::LineCommand) -> Vec<TerminalQuadDraw> {
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
        DecorationStyle::Double => vec![
            TerminalQuadDraw {
                rect: SurfaceRect::from_min_size(rect.min_x, rect.min_y - 1.0, rect.width(), 1.0),
                color: line.color,
            },
            TerminalQuadDraw {
                rect: SurfaceRect::from_min_size(rect.min_x, rect.min_y + 1.0, rect.width(), 1.0),
                color: line.color,
            },
        ],
        DecorationStyle::Dotted => segmented_decoration_draws(rect, line.color, 1.0, 2.0),
        DecorationStyle::Dashed => segmented_decoration_draws(rect, line.color, 4.0, 3.0),
        DecorationStyle::Curly => curly_decoration_draws(rect, line.color),
        DecorationStyle::Single | DecorationStyle::Strikethrough | DecorationStyle::Overline => {
            vec![TerminalQuadDraw {
                rect,
                color: line.color,
            }]
        }
    }
}

fn segmented_decoration_draws(
    rect: SurfaceRect,
    color: PlanColor,
    segment_width: f32,
    gap_width: f32,
) -> Vec<TerminalQuadDraw> {
    let mut draws = Vec::new();
    let mut x = rect.min_x;
    while x < rect.max_x {
        let width = segment_width.min(rect.max_x - x).max(1.0);
        draws.push(TerminalQuadDraw {
            rect: SurfaceRect::from_min_size(x, rect.min_y, width, rect.height()),
            color,
        });
        x += segment_width + gap_width;
    }
    draws
}

fn curly_decoration_draws(rect: SurfaceRect, color: PlanColor) -> Vec<TerminalQuadDraw> {
    let mut draws = Vec::new();
    let mut x = rect.min_x;
    let mut high = true;
    while x < rect.max_x {
        let y = if high {
            rect.min_y - 1.0
        } else {
            rect.min_y + 1.0
        };
        draws.push(TerminalQuadDraw {
            rect: SurfaceRect::from_min_size(x, y, 2.0_f32.min(rect.max_x - x).max(1.0), 1.0),
            color,
        });
        high = !high;
        x += 2.0;
    }
    draws
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
    .into_iter()
    .map(|rect| TerminalCursorDraw {
        rect,
        color: cursor.color,
    })
    .collect()
}

fn cursor_command_vertices(surface: SurfaceRect, cursor: &CursorCommand) -> Vec<BackgroundVertex> {
    let draws = cursor_draws(cursor)
        .into_iter()
        .map(|draw| TerminalQuadDraw {
            rect: draw.rect,
            color: draw.color,
        })
        .collect::<Vec<_>>();

    background_vertices(surface, &draws)
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

fn surface_to_ndc(surface: SurfaceRect, x: f32, y: f32) -> [f32; 2] {
    let width = surface.width().max(1.0);
    let height = surface.height().max(1.0);
    [
        ((x - surface.min_x) / width) * 2.0 - 1.0,
        1.0 - ((y - surface.min_y) / height) * 2.0,
    ]
}

fn color_to_float(color: PlanColor) -> [f32; 4] {
    [
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        f32::from(color.a) / 255.0,
    ]
}

fn vertex_bytes<T>(vertices: &[T]) -> &[u8] {
    // SAFETY: `BackgroundVertex` and `TextVertex` are `#[repr(C)]` aggregates composed only of
    // `f32` arrays. Reinterpreting their contiguous slice storage as bytes is valid for upload to
    // WGPU, and `u8` has alignment 1 so there is no stricter alignment requirement.
    unsafe {
        std::slice::from_raw_parts(
            vertices.as_ptr().cast::<u8>(),
            std::mem::size_of_val(vertices),
        )
    }
}

fn egui_rect(rect: SurfaceRect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::Pos2::new(rect.min_x, rect.min_y),
        egui::Pos2::new(rect.max_x, rect.max_y),
    )
}

#[cfg(test)]
mod tests;
