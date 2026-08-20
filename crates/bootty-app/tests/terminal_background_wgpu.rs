use bootty_app::{
    geometry::{CellMetrics, SurfaceRect, TerminalPadding, ViewTransform},
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, CursorTextPlan, DecorationLine, DecorationStyle,
        PlanColor, TerminalPaintPlan, TextAttrs, TextRun,
    },
    terminal::TerminalEngine,
    terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
    terminal_render::{TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::{
        TerminalRendererId, TerminalWgpuRenderer, terminal_background_draws, terminal_cursor_draws,
        terminal_decoration_draws, terminal_render_callback_for_renderer, terminal_sprite_draws,
        terminal_text_draws,
    },
};
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

fn render_frame_from_plan(
    plan: &TerminalPaintPlan,
    symbol_policy: NativeSymbolPolicy,
) -> TerminalRenderFrame {
    let contract = TerminalTextContract::new(TerminalTextConfig::default(), symbol_policy);
    TerminalRenderFrame::from_plan(plan, &contract)
}

fn primitive_render_frame(plan: &TerminalPaintPlan) -> TerminalRenderFrame {
    render_frame_from_plan(plan, NativeSymbolPolicy::terminal_glyph_primitives())
}

fn single_text_frame(
    surface: SurfaceRect,
    rect: SurfaceRect,
    cells: u16,
    text: &str,
    symbol_policy: NativeSymbolPolicy,
) -> TerminalRenderFrame {
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs: vec![text_run(rect, cells, text)],
        decorations: Vec::new(),
        cursor: None,
    };
    render_frame_from_plan(&plan, symbol_policy)
}

fn background_frame() -> TerminalRenderFrame {
    let plan = TerminalPaintPlan {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        default_background: color(1, 2, 3),
        backgrounds: vec![BackgroundRect {
            rect: SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0),
            color: color(4, 5, 6),
        }],
        text_runs: Vec::new(),
        decorations: Vec::new(),
        cursor: None,
    };
    primitive_render_frame(&plan)
}

fn ascii_text_frame() -> TerminalRenderFrame {
    single_text_frame(
        SurfaceRect::from_min_size(0.0, 0.0, 63.0, 21.0),
        SurfaceRect::from_min_size(0.0, 0.0, 63.0, 21.0),
        9,
        "hello 123",
        NativeSymbolPolicy::terminal_glyph_primitives(),
    )
}

fn fallback_text_frame() -> TerminalRenderFrame {
    single_text_frame(
        SurfaceRect::from_min_size(0.0, 0.0, 21.0, 21.0),
        SurfaceRect::from_min_size(0.0, 0.0, 21.0, 21.0),
        3,
        "\u{f0e7}",
        NativeSymbolPolicy::font_only(),
    )
}

fn prompt_sprite_frame() -> TerminalRenderFrame {
    single_text_frame(
        SurfaceRect::from_min_size(0.0, 0.0, 28.0, 21.0),
        SurfaceRect::from_min_size(0.0, 0.0, 28.0, 21.0),
        4,
        "a┃b\u{E0B8}",
        NativeSymbolPolicy::terminal_glyph_primitives(),
    )
}

fn image_only_frame() -> TerminalRenderFrame {
    TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0),
        commands: vec![TerminalRenderCommand::Image(KittyImagePlacement {
            image_id: 1,
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
        })],
    }
}

fn background_then_image_frame() -> TerminalRenderFrame {
    let mut frame = image_only_frame();
    frame.commands.insert(
        0,
        TerminalRenderCommand::FillRect(bootty_app::terminal_render::FillCommand {
            rect: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0),
            color: color(226, 230, 249),
            role: bootty_app::terminal_render::FillRole::SurfaceBackground,
        }),
    );
    frame
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
    let contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );
    TerminalRenderFrame::from_plan_and_images(&plan, &contract, &images)
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
    let context = offscreen_wgpu_context()
        .lock()
        .expect("offscreen WGPU context lock");
    let device = &context.device;
    let queue = &context.queue;
    let format = context.format;
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
    let bytes_per_pixel = 4_u32;
    let unpadded_bytes_per_row = width * bytes_per_pixel;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let output_size = u64::from(padded_bytes_per_row) * u64::from(height);
    let output = device.create_buffer(&eframe::wgpu::BufferDescriptor {
        label: Some("bootty terminal offscreen readback"),
        size: output_size,
        usage: eframe::wgpu::BufferUsages::COPY_DST | eframe::wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut renderer = TerminalWgpuRenderer::new(device, format);
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
    let mapped = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * bytes_per_pixel) as usize);
    for row in mapped
        .chunks(padded_bytes_per_row as usize)
        .take(height as usize)
    {
        pixels.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    output.unmap();
    pixels
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
        let mapped = slice.get_mapped_range();
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

fn cursor_frame(shape: CursorShape) -> TerminalRenderFrame {
    let plan = TerminalPaintPlan {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 21.0, 21.0),
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs: Vec::new(),
        decorations: Vec::new(),
        cursor: Some(CursorPlan {
            rect: SurfaceRect::from_min_size(7.0, 0.0, 7.0, 21.0),
            color: color(200, 201, 202),
            shape,
            text_under_cursor: Some(CursorTextPlan {
                rect: SurfaceRect::from_min_size(7.0, 0.0, 7.0, 21.0),
                text: "X".to_owned(),
                color: color(10, 11, 12),
            }),
        }),
    };
    primitive_render_frame(&plan)
}

fn decoration_frame(styles: &[DecorationStyle]) -> TerminalRenderFrame {
    let decorations = styles
        .iter()
        .enumerate()
        .map(|(index, style)| DecorationLine {
            start_x: 0.0,
            start_y: 3.0 + index as f32 * 4.0,
            end_x: 28.0,
            end_y: 3.0 + index as f32 * 4.0,
            color: color(200, 201, 202),
            style: *style,
        })
        .collect();
    let plan = TerminalPaintPlan {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 28.0, 28.0),
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs: Vec::new(),
        decorations,
        cursor: None,
    };
    primitive_render_frame(&plan)
}

#[test]
fn background_callback_input_uses_terminal_render_fill_commands() {
    let draws = terminal_background_draws(&background_frame());

    assert_eq!(draws.len(), 2);
    assert_eq!(
        draws[0].rect,
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0)
    );
    assert_eq!(draws[0].color, color(1, 2, 3));
    assert_eq!(
        draws[1].rect,
        SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0)
    );
    assert_eq!(draws[1].color, color(4, 5, 6));
}

#[test]
fn terminal_frame_command_variants_produce_wgpu_callback_shape() {
    for (name, frame) in [
        ("mixed", mixed_background_image_text_cursor_frame()),
        ("sprite-only", prompt_sprite_frame()),
        ("image-only", image_only_frame()),
        ("cursor-only", cursor_frame(CursorShape::Block)),
    ] {
        let shape = terminal_render_callback_for_renderer(
            TerminalRendererId::unique(),
            &frame,
            eframe::wgpu::TextureFormat::Rgba8Unorm,
            ViewTransform::IDENTITY,
        )
        .unwrap_or_else(|| panic!("{name} frame should produce a callback shape"));

        assert!(
            matches!(shape, eframe::egui::Shape::Callback(_)),
            "{name} frame should produce a callback shape"
        );
    }
}

#[test]
fn ascii_text_draws_are_batched_from_terminal_render_text_commands() {
    let draws = terminal_text_draws(&ascii_text_frame());

    assert!(
        !draws.is_empty(),
        "ASCII text should produce WGPU text quads"
    );
    assert!(draws.iter().any(|draw| draw.ch == 'h'));
    assert!(draws.iter().any(|draw| draw.ch == '1'));
    assert!(draws.iter().any(|draw| draw.ch == '3'));
    assert!(!draws.iter().any(|draw| draw.ch == ' '));
    assert!(draws.iter().all(|draw| {
        draw.color.r == 220 && draw.color.g == 221 && draw.color.b == 222 && draw.color.a > 0
    }));
    assert!(
        draws
            .iter()
            .all(|draw| draw.rect.min_x >= 0.0 && draw.rect.max_x <= 63.0)
    );
}

#[test]
fn ascii_text_rasterization_uses_configured_font_size_without_cell_stretching() {
    let draws = terminal_text_draws(&ascii_text_frame());
    let h_draws = draws
        .iter()
        .filter(|draw| draw.ch == 'h')
        .collect::<Vec<_>>();
    let min_y = h_draws
        .iter()
        .map(|draw| draw.rect.min_y)
        .fold(f32::INFINITY, f32::min);
    let max_y = h_draws
        .iter()
        .map(|draw| draw.rect.max_y)
        .fold(f32::NEG_INFINITY, f32::max);

    assert!(
        max_y - min_y >= TerminalTextConfig::default().font_size * 0.45,
        "ASCII glyphs should be visible at the configured font size; got {}",
        max_y - min_y
    );
    assert!(
        max_y - min_y <= 21.0 * 0.80,
        "ASCII glyphs should not be stretched to fill the terminal cell height; got {}",
        max_y - min_y
    );
}

#[test]
fn non_ascii_text_commands_are_not_dropped_by_wgpu_text_batching() {
    let draws = terminal_text_draws(&fallback_text_frame());

    assert!(
        draws.iter().any(|draw| draw.ch == '\u{f0e7}'),
        "fallback/Nerd Font text commands should reach the WGPU text batcher"
    );
}

#[test]
fn prompt_sprite_draws_are_batched_from_terminal_render_sprite_commands() {
    let draws = terminal_sprite_draws(&prompt_sprite_frame());

    assert!(
        draws.iter().any(|draw| draw.ch == '┃'),
        "prompt bar sprite should produce WGPU sprite triangles"
    );
    assert!(
        draws.iter().any(|draw| draw.ch == '\u{E0B8}'),
        "Powerline separator sprite should produce WGPU sprite triangles"
    );
    assert!(draws.iter().all(|draw| !draw.vertices.is_empty()));
    assert!(draws.iter().all(|draw| !draw.indices.is_empty()));
    assert!(draws.iter().all(|draw| {
        draw.vertices.iter().all(|vertex| {
            vertex.position[0] >= 0.0
                && vertex.position[0] <= 28.0
                && vertex.position[1] >= 0.0
                && vertex.position[1] <= 21.0
        })
    }));
}

#[test]
#[ignore = "requires WGPU offscreen readback; run `mise run test:wgpu`"]
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
    assert_eq!(frame.images.placements.len(), 1);
    assert_eq!(&frame.images.placements[0].data[..4], &[255, 0, 0, 255]);
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());
    assert_eq!(
        render_frame.surface,
        SurfaceRect::from_min_size(0.0, 0.0, 200.0, 80.0)
    );
    assert!(matches!(
        render_frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Image(_),
            ..
        ]
    ));
    assert!(
        !render_frame.commands[2..]
            .iter()
            .any(|command| matches!(command, TerminalRenderCommand::FillRect(_)))
    );
    let image = render_frame
        .commands
        .iter()
        .find_map(|command| match command {
            TerminalRenderCommand::Image(image) => Some(image),
            _ => None,
        })
        .expect("render frame image command");
    assert_eq!(
        image.destination,
        SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0)
    );
    assert_eq!(
        image.image_format,
        libghostty_vt::kitty::graphics::ImageFormat::Rgba
    );
    assert_eq!(image.image_width, 1);
    assert_eq!(image.image_height, 1);
    assert_eq!(&image.data[..4], &[255, 0, 0, 255]);
    assert_eq!(image.source.width, 1);
    assert_eq!(image.source.height, 1);
    assert_eq!(image.source.x, 0);
    assert_eq!(image.source.y, 0);
    let pixels = render_frame_to_pixels(&render_frame, 200, 80);
    let max_pixel = pixels
        .chunks_exact(4)
        .max_by_key(|rgba| u16::from(rgba[0]) + u16::from(rgba[1]) + u16::from(rgba[2]))
        .unwrap_or(&[0, 0, 0, 0]);

    assert!(
        pixels
            .chunks_exact(4)
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200),
        "kitty APC image should render visible red pixels through bootty-bare WGPU; max pixel {max_pixel:?}"
    );
}

#[test]
#[ignore = "requires WGPU offscreen readback; run `mise run test:wgpu`"]
fn image_only_frame_renders_visible_pixels_offscreen() {
    let pixels = render_frame_to_pixels(&image_only_frame(), 10, 10);

    assert!(
        pixels
            .chunks_exact(4)
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200),
        "kitty image command should render visible red pixels"
    );
}

#[test]
#[ignore = "requires WGPU offscreen readback; run `mise run test:wgpu`"]
fn image_after_surface_background_renders_visible_pixels_offscreen() {
    let pixels = render_frame_to_pixels(&background_then_image_frame(), 10, 10);

    assert!(
        pixels
            .chunks_exact(4)
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200),
        "image command after surface background should render visible red pixels"
    );
}

#[test]
#[ignore = "requires WGPU offscreen readback; run `mise run test:wgpu`"]
fn reused_renderer_prepares_same_frame_without_pixel_drift() {
    let frame = background_then_image_frame();
    let pixels = render_frames_with_reused_renderer(&[frame.clone(), frame], 10, 10);
    assert_eq!(pixels.len(), 2);
    assert_eq!(pixels[0], pixels[1]);
}

#[test]
#[ignore = "requires WGPU offscreen readback; run `mise run test:wgpu`"]
fn reused_renderer_preserves_mixed_layer_order_without_pixel_drift() {
    let frame = mixed_background_image_text_cursor_frame();
    let pixels = render_frames_with_reused_renderer(&[frame.clone(), frame], 40, 20);
    assert_eq!(pixels.len(), 2);
    assert_eq!(pixels[0], pixels[1]);
    assert!(
        pixels[0]
            .chunks_exact(4)
            .any(|rgba| rgba[0] > 200 && rgba[1] < 20 && rgba[2] < 20 && rgba[3] > 200),
        "mixed layer frame should preserve visible image pixels"
    );
}

#[test]
fn cursor_draws_are_batched_from_terminal_render_cursor_commands() {
    let block = terminal_cursor_draws(&cursor_frame(CursorShape::Block));
    let bar = terminal_cursor_draws(&cursor_frame(CursorShape::Bar));
    let underline = terminal_cursor_draws(&cursor_frame(CursorShape::Underline));
    let hollow = terminal_cursor_draws(&cursor_frame(CursorShape::HollowBlock));

    assert_eq!(block.len(), 1);
    assert_eq!(
        block[0].rect,
        SurfaceRect::from_min_size(7.0, 0.0, 7.0, 21.0)
    );
    assert_eq!(bar.len(), 1);
    assert_eq!(bar[0].rect, SurfaceRect::from_min_size(6.0, 0.0, 2.0, 21.0));
    assert_eq!(underline.len(), 1);
    assert_eq!(
        underline[0].rect,
        SurfaceRect::from_min_size(7.0, 19.0, 7.0, 2.0)
    );
    assert_eq!(hollow.len(), 4);
    assert!(hollow.iter().all(|draw| draw.color == color(200, 201, 202)));
}

#[test]
fn decoration_draws_are_batched_from_terminal_render_decoration_commands() {
    let draws = terminal_decoration_draws(&decoration_frame(&[
        DecorationStyle::Single,
        DecorationStyle::Double,
        DecorationStyle::Dotted,
        DecorationStyle::Dashed,
        DecorationStyle::Curly,
        DecorationStyle::Strikethrough,
        DecorationStyle::Overline,
    ]));

    assert!(draws.len() >= 10);
    assert!(draws.iter().all(|draw| draw.color == color(200, 201, 202)));
    assert!(
        draws
            .iter()
            .any(|draw| draw.rect == SurfaceRect::from_min_size(0.0, 2.5, 28.0, 1.0))
    );
    assert!(
        draws
            .iter()
            .any(|draw| draw.rect == SurfaceRect::from_min_size(0.0, 5.5, 28.0, 1.0))
    );
    assert!(draws.iter().any(|draw| draw.rect.width() == 4.0));
}
