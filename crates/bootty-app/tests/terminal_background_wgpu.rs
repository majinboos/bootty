use pretty_assertions::assert_eq;

use bootty_render::{
    geometry::{CellMetrics, SurfaceRect, TerminalPadding, ViewTransform},
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, DecorationLine, DecorationStyle, PlanColor,
        TerminalPaintPlan, TextAttrs, TextRun,
    },
    terminal_render::TerminalRenderFrame,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::{
        TerminalRendererId, TerminalWgpuRenderer, terminal_render_callback_for_renderer,
    },
};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_terminal::terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement};
use bootty_winit::bare_host::{BareTerminalViewport, terminal_render_frame_for_bare_host};
use std::sync::{Arc, Mutex, OnceLock};

fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}

fn text_attrs() -> TextAttrs {
    TextAttrs {
        fg: color(220, 221, 222),
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn text_run(rect: SurfaceRect, cells: u16, text: &str) -> TextRun {
    TextRun {
        cell_rect: rect,
        rect,
        cells,
        text: text.to_owned(),
        attrs: text_attrs(),
    }
}

fn mixed_background_image_text_cursor_frame() -> TerminalRenderFrame {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0);
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(1, 2, 3),
        backgrounds: vec![BackgroundRect {
            rect: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
            color: color(4, 5, 6),
        }],
        text_runs: vec![text_run(
            SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0),
            2,
            "Hi",
        )],
        decorations: vec![DecorationLine {
            start_x: 0.0,
            start_y: 18.0,
            end_x: 40.0,
            end_y: 18.0,
            color: color(250, 250, 250),
            style: DecorationStyle::Single,
        }],
        cursor: Some(CursorPlan {
            rect: SurfaceRect::from_min_size(30.0, 0.0, 10.0, 20.0),
            color: color(255, 255, 255),
            shape: CursorShape::Block,
            text_under_cursor: None,
        }),
    };
    let mut images = KittyImageFrame::default();
    images.placements.push(KittyImagePlacement {
        image_id: 7,
        placement_id: 1,
        layer: KittyImageLayer::BelowText,
        image_width: 1,
        image_height: 1,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0),
        data: Arc::new(vec![255, 0, 0, 255]),
    });
    let text = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );
    TerminalRenderFrame::from_plan_and_images(&plan, &text, &images)
}

struct OffscreenWgpuContext {
    device: eframe::wgpu::Device,
    queue: eframe::wgpu::Queue,
    format: eframe::wgpu::TextureFormat,
}

fn offscreen_wgpu_context() -> &'static Mutex<OffscreenWgpuContext> {
    static CONTEXT: OnceLock<Mutex<OffscreenWgpuContext>> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let instance = eframe::wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(
            &eframe::wgpu::RequestAdapterOptions {
                power_preference: eframe::wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            },
        ))
        .expect("request WGPU adapter");
        let (device, queue) =
            pollster::block_on(adapter.request_device(&eframe::wgpu::DeviceDescriptor {
                label: Some("bootty terminal offscreen test device"),
                ..Default::default()
            }))
            .expect("request WGPU device");
        Mutex::new(OffscreenWgpuContext {
            device,
            queue,
            format: eframe::wgpu::TextureFormat::Rgba8UnormSrgb,
        })
    })
}

fn render_frame_to_pixels(frame: &TerminalRenderFrame, width: u32, height: u32) -> Vec<u8> {
    render_frames_with_reused_renderer(std::slice::from_ref(frame), width, height)
        .pop()
        .expect("one rendered frame")
}

fn render_frames_with_reused_renderer(
    frames: &[TerminalRenderFrame],
    width: u32,
    height: u32,
) -> Vec<Vec<u8>> {
    let context = offscreen_wgpu_context()
        .lock()
        .expect("offscreen WGPU context lock");
    let device = &context.device;
    let queue = &context.queue;
    let format = context.format;
    let bytes_per_pixel = 4_u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let output_size = u64::from(padded_bytes_per_row) * u64::from(height);
    let mut renderer = TerminalWgpuRenderer::new(device, format);
    let mut outputs = Vec::with_capacity(frames.len());

    for frame in frames {
        let texture = device.create_texture(&eframe::wgpu::TextureDescriptor {
            label: Some("bootty terminal offscreen target"),
            size: eframe::wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: eframe::wgpu::TextureDimension::D2,
            format,
            usage: eframe::wgpu::TextureUsages::RENDER_ATTACHMENT
                | eframe::wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&eframe::wgpu::TextureViewDescriptor::default());
        let output = device.create_buffer(&eframe::wgpu::BufferDescriptor {
            label: Some("bootty terminal offscreen readback"),
            size: output_size,
            usage: eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        renderer.prepare_terminal_frame(device, queue, frame, 1.0, ViewTransform::IDENTITY);
        let mut encoder = device.create_command_encoder(&eframe::wgpu::CommandEncoderDescriptor {
            label: Some("bootty terminal offscreen encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&eframe::wgpu::RenderPassDescriptor {
                label: Some("bootty terminal offscreen pass"),
                color_attachments: &[Some(eframe::wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: eframe::wgpu::Operations {
                        load: eframe::wgpu::LoadOp::Clear(eframe::wgpu::Color::TRANSPARENT),
                        store: eframe::wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.paint(&mut pass);
        }
        encoder.copy_texture_to_buffer(
            eframe::wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: eframe::wgpu::Origin3d::ZERO,
                aspect: eframe::wgpu::TextureAspect::All,
            },
            eframe::wgpu::TexelCopyBufferInfo {
                buffer: &output,
                layout: eframe::wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            eframe::wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);

        let slice = output.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(eframe::wgpu::MapMode::Read, move |result| {
            sender.send(result).expect("send map result");
        });
        device
            .poll(eframe::wgpu::PollType::wait_indefinitely())
            .expect("poll WGPU device");
        receiver
            .recv()
            .expect("receive map result")
            .expect("map readback buffer");
        let mapped = slice.get_mapped_range().expect("read mapped WGPU buffer");
        let mut pixels = Vec::with_capacity((width * height * bytes_per_pixel) as usize);
        for row in mapped
            .chunks(padded_bytes_per_row as usize)
            .take(height as usize)
        {
            pixels.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        output.unmap();
        outputs.push(pixels);
    }

    outputs
}

#[test]
fn a_renderable_frame_produces_a_wgpu_callback() {
    let shape = terminal_render_callback_for_renderer(
        TerminalRendererId::unique(),
        &mixed_background_image_text_cursor_frame(),
        eframe::wgpu::TextureFormat::Rgba8Unorm,
        ViewTransform::IDENTITY,
    )
    .expect("renderable frame callback");

    assert!(matches!(shape, eframe::egui::Shape::Callback(_)));
}

#[test]
fn kitty_apc_image_renders_visible_pixels_through_bare_host_wgpu_path() {
    let viewport = BareTerminalViewport::new(
        10,
        10,
        CellMetrics::new(10.0, 10.0),
        TerminalPadding::default(),
    );
    let mut terminal = TerminalEngine::new(viewport.geometry()).expect("terminal engine");
    terminal.write_vt(b"\x1b_Ga=T,t=d,i=90,s=1,v=1,c=1,r=1,q=1;/wAA/w==\x1b\\");
    let frame = terminal.extract_frame().expect("extract image frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());
    let pixels = render_frame_to_pixels(&render_frame, 200, 80);
    let max_pixel = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .max_by_key(|rgba| u16::from(rgba[0]) + u16::from(rgba[1]) + u16::from(rgba[2]))
        .unwrap_or(&[0, 0, 0, 0]);

    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200),
        "kitty APC image should render visible red pixels through bootty-bare WGPU; max pixel {max_pixel:?}"
    );
}

#[test]
fn reused_renderer_prepares_mixed_layers_without_pixel_drift() {
    let frame = mixed_background_image_text_cursor_frame();
    let pixels = render_frames_with_reused_renderer(&[frame.clone(), frame], 40, 20);
    assert_eq!(pixels.len(), 2);
    assert_eq!(pixels[0], pixels[1]);
    assert!(
        pixels[0]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200)
    );
}
