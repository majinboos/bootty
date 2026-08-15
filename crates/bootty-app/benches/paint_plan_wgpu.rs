use std::hint::black_box;

mod paint_plan_fixtures;

use bootty_app::{
    geometry::{SurfaceRect, ViewTransform},
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, DecorationLine, DecorationStyle, PaintPlanner,
        PlanColor, TerminalPaintPlan, TextAttrs, TextRun,
    },
    terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
    terminal_render::{TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
    terminal_text_atlas::TextAtlasBuilder,
    terminal_wgpu::TerminalWgpuRenderer,
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use eframe::wgpu;
use paint_plan_fixtures::{
    agent_render_frame, prepared_render_scenarios, surface_for, terminal_engine,
};
use std::sync::Arc;

struct WgpuBenchContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
}

fn create_wgpu_bench_context() -> WgpuBenchContext {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("wgpu adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("bootty bench device"),
        ..Default::default()
    }))
    .expect("wgpu device");
    WgpuBenchContext {
        device,
        queue,
        format: wgpu::TextureFormat::Rgba8Unorm,
    }
}

struct OffscreenRenderTarget {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

fn create_offscreen_target(
    context: &WgpuBenchContext,
    width: u32,
    height: u32,
) -> OffscreenRenderTarget {
    let texture = context.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bootty bench offscreen target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: context.format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    OffscreenRenderTarget {
        _texture: texture,
        view,
    }
}

fn target_size(frame: &TerminalRenderFrame) -> (u32, u32) {
    (
        frame.surface.max_x.ceil().max(1.0) as u32,
        frame.surface.max_y.ceil().max(1.0) as u32,
    )
}

fn submit_prepared_render_pass(
    context: &WgpuBenchContext,
    renderer: &TerminalWgpuRenderer,
    target: &OffscreenRenderTarget,
) {
    let mut encoder = context
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bootty bench render pass encoder"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bootty bench render pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        renderer.paint(&mut pass);
    }
    context.queue.submit([encoder.finish()]);
    context
        .device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("poll WGPU device");
}

fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}

fn text_attrs(fg: PlanColor) -> TextAttrs {
    TextAttrs {
        fg,
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn text_run(rect: SurfaceRect, cells: u16, text: &str, fg: PlanColor) -> TextRun {
    TextRun {
        cell_rect: rect,
        rect,
        cells,
        text: text.to_owned(),
        attrs: text_attrs(fg),
    }
}

fn rgba_checkerboard(width: u32, height: u32, seed: u8) -> Arc<Vec<u8>> {
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let shade = ((x * 17 + y * 29 + u32::from(seed)) % 255) as u8;
            pixels.extend_from_slice(&[shade, seed.wrapping_add(shade / 2), 255 - shade, 255]);
        }
    }
    Arc::new(pixels)
}

fn image_placement(
    image_id: u32,
    layer: KittyImageLayer,
    destination: SurfaceRect,
    seed: u8,
) -> KittyImagePlacement {
    KittyImagePlacement {
        image_id,
        placement_id: image_id,
        layer,
        image_width: 24,
        image_height: 16,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 24,
            height: 16,
        },
        destination,
        data: rgba_checkerboard(24, 16, seed),
    }
}

fn mixed_text_sprite_image_frame() -> TerminalRenderFrame {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 320.0, 160.0);
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(10, 12, 18),
        backgrounds: vec![
            BackgroundRect {
                rect: SurfaceRect::from_min_size(0.0, 0.0, 320.0, 24.0),
                color: color(31, 41, 59),
            },
            BackgroundRect {
                rect: SurfaceRect::from_min_size(24.0, 54.0, 230.0, 52.0),
                color: color(20, 24, 36),
            },
        ],
        text_runs: vec![
            text_run(
                SurfaceRect::from_min_size(12.0, 4.0, 252.0, 22.0),
                28,
                "bootty WGPU mix ┃ █▓▒░  ready",
                color(220, 230, 255),
            ),
            text_run(
                SurfaceRect::from_min_size(24.0, 56.0, 270.0, 22.0),
                30,
                "agent stream: tool_call → render pass 🥟",
                color(158, 206, 106),
            ),
            text_run(
                SurfaceRect::from_min_size(24.0, 84.0, 240.0, 22.0),
                26,
                "images below and above text remain ordered",
                color(224, 175, 104),
            ),
        ],
        decorations: vec![DecorationLine {
            start_x: 24.0,
            start_y: 112.0,
            end_x: 280.0,
            end_y: 112.0,
            color: color(125, 207, 255),
            style: DecorationStyle::Curly,
        }],
        cursor: Some(CursorPlan {
            rect: SurfaceRect::from_min_size(292.0, 56.0, 10.0, 22.0),
            color: color(255, 255, 255),
            shape: CursorShape::Block,
            text_under_cursor: None,
        }),
    };
    let mut images = KittyImageFrame::default();
    images.placements.push(image_placement(
        1,
        KittyImageLayer::BelowBackground,
        SurfaceRect::from_min_size(260.0, 0.0, 48.0, 32.0),
        19,
    ));
    images.placements.push(image_placement(
        2,
        KittyImageLayer::BelowText,
        SurfaceRect::from_min_size(258.0, 58.0, 48.0, 32.0),
        83,
    ));
    images.placements.push(image_placement(
        3,
        KittyImageLayer::AboveText,
        SurfaceRect::from_min_size(212.0, 96.0, 48.0, 32.0),
        151,
    ));
    let text_contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );
    TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &images)
}

fn layer_ordering_image_frame() -> TerminalRenderFrame {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 320.0, 160.0);
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(7, 8, 10),
        backgrounds: vec![BackgroundRect {
            rect: SurfaceRect::from_min_size(18.0, 18.0, 284.0, 124.0),
            color: color(22, 27, 39),
        }],
        text_runs: vec![text_run(
            SurfaceRect::from_min_size(36.0, 68.0, 236.0, 22.0),
            26,
            "layer ordering: below bg/text/above text",
            color(235, 245, 255),
        )],
        decorations: Vec::new(),
        cursor: None,
    };
    let layers = [
        KittyImageLayer::BelowBackground,
        KittyImageLayer::BelowText,
        KittyImageLayer::AboveText,
    ];
    let mut images = KittyImageFrame::default();
    for (index, layer) in layers.into_iter().cycle().take(18).enumerate() {
        let x = 18.0 + (index % 6) as f32 * 48.0;
        let y = 20.0 + (index / 6) as f32 * 38.0;
        images.placements.push(image_placement(
            100 + index as u32,
            layer,
            SurfaceRect::from_min_size(x, y, 40.0, 28.0),
            index as u8 * 13,
        ));
    }
    let text_contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );
    TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &images)
}
fn ascii_dirty_text_frame(tick: u32) -> TerminalRenderFrame {
    const COLS: u16 = 240;
    const ROWS: u16 = 90;
    const CELL_WIDTH: f32 = 9.0;
    const CELL_HEIGHT: f32 = 22.0;

    let surface = SurfaceRect::from_min_size(
        0.0,
        0.0,
        f32::from(COLS) * CELL_WIDTH,
        f32::from(ROWS) * CELL_HEIGHT,
    );
    let text_runs = (0..ROWS)
        .map(|row| {
            let mut text = format!(
                "dirty ascii frame {tick:06} row {row:03} render preparation keeps every glyph moving "
            );
            while text.len() < COLS as usize {
                text.push_str("ascii cells ");
            }
            text.truncate(COLS as usize);
            text_run(
                SurfaceRect::from_min_size(
                    0.0,
                    f32::from(row) * CELL_HEIGHT,
                    f32::from(COLS) * CELL_WIDTH,
                    CELL_HEIGHT,
                ),
                COLS,
                &text,
                color(180, 210, 255),
            )
        })
        .collect();
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(8, 10, 16),
        backgrounds: Vec::new(),
        text_runs,
        decorations: Vec::new(),
        cursor: None,
    };
    let text_contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    TerminalRenderFrame::from_plan(&plan, &text_contract)
}

fn single_glyph_frame() -> TerminalRenderFrame {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 9.0, 22.0);
    let plan = TerminalPaintPlan {
        surface: rect,
        default_background: color(8, 10, 16),
        backgrounds: Vec::new(),
        text_runs: vec![text_run(rect, 1, "A", color(180, 210, 255))],
        decorations: Vec::new(),
        cursor: None,
    };
    TerminalRenderFrame::from_plan(
        &plan,
        &TerminalTextContract::new(
            TerminalTextConfig::default(),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        ),
    )
}

fn warm_wgpu_renderer(
    context: &WgpuBenchContext,
    renderer: &mut TerminalWgpuRenderer,
    frame: &TerminalRenderFrame,
) {
    black_box(renderer.prepare_terminal_frame(
        &context.device,
        &context.queue,
        frame,
        1.0,
        ViewTransform::IDENTITY,
    ));
}

fn bench_wgpu_prepare(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    for scenario in prepared_render_scenarios() {
        let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
        warm_wgpu_renderer(&context, &mut renderer, &scenario.frame);
        c.bench_function(&format!("wgpu_prepare_{}", scenario.name), |b| {
            b.iter(|| {
                black_box(renderer.prepare_terminal_frame(
                    &context.device,
                    &context.queue,
                    &scenario.frame,
                    1.0,
                    ViewTransform::IDENTITY,
                ))
            })
        });
    }
}
fn bench_wgpu_zoom_prepare(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let frame = ascii_dirty_text_frame(0);
    let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
    let steady_view = ViewTransform {
        zoom: 1.5,
        pan_x: 0.0,
        pan_y: 0.0,
    };
    black_box(renderer.prepare_terminal_frame(
        &context.device,
        &context.queue,
        &frame,
        1.0,
        steady_view,
    ));

    c.bench_function("wgpu_prepare_zoom_steady_ascii_text_240x90", |b| {
        b.iter(|| {
            black_box(renderer.prepare_terminal_frame(
                &context.device,
                &context.queue,
                &frame,
                1.0,
                steady_view,
            ))
        })
    });

    let mut tick = 0_u32;
    c.bench_function("wgpu_prepare_zoom_churn_ascii_text_240x90", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let zoom = 1.1 + (tick % 64) as f32 * (0.8 / 63.0);
            black_box(renderer.prepare_terminal_frame(
                &context.device,
                &context.queue,
                &frame,
                1.0,
                ViewTransform {
                    zoom,
                    pan_x: 0.0,
                    pan_y: 0.0,
                },
            ))
        })
    });
}
fn bench_wgpu_dirty_text_prepare(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let frames = (0..16).map(ascii_dirty_text_frame).collect::<Vec<_>>();
    let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
    warm_wgpu_renderer(&context, &mut renderer, &frames[0]);
    let mut tick = 0_usize;

    c.bench_function("wgpu_prepare_dirty_ascii_text_240x90", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let frame = &frames[tick % frames.len()];
            black_box(renderer.prepare_terminal_frame(
                &context.device,
                &context.queue,
                frame,
                1.0,
                ViewTransform::IDENTITY,
            ))
        })
    });
}
// Forward-scroll workload: a fixed ring of distinct line strings, windowed onto the
// screen and shifted up by one row each tick. Every row's rect is stable but its text
// changes, so the positional prepared-text cache misses on all 90 rows; yet 89 of those
// strings were shaped on the previous frame (one row higher), so a position-independent
// shaping cache turns them into hits. This is the common streaming-output case.
fn ascii_scroll_text_frame(tick: u32) -> TerminalRenderFrame {
    const COLS: u16 = 240;
    const ROWS: u16 = 90;
    const RING: u32 = 128;
    const CELL_WIDTH: f32 = 9.0;
    const CELL_HEIGHT: f32 = 22.0;

    let surface = SurfaceRect::from_min_size(
        0.0,
        0.0,
        f32::from(COLS) * CELL_WIDTH,
        f32::from(ROWS) * CELL_HEIGHT,
    );
    let text_runs = (0..ROWS)
        .map(|row| {
            let doc_line = (tick + u32::from(row)) % RING;
            let mut text = format!(
                "scrollback line {doc_line:05} streaming output alpha beta gamma delta epsilon "
            );
            while text.len() < COLS as usize {
                text.push_str("more ascii content ");
            }
            text.truncate(COLS as usize);
            text_run(
                SurfaceRect::from_min_size(
                    0.0,
                    f32::from(row) * CELL_HEIGHT,
                    f32::from(COLS) * CELL_WIDTH,
                    CELL_HEIGHT,
                ),
                COLS,
                &text,
                color(180, 210, 255),
            )
        })
        .collect();
    let plan = TerminalPaintPlan {
        surface,
        default_background: color(8, 10, 16),
        backgrounds: Vec::new(),
        text_runs,
        decorations: Vec::new(),
        cursor: None,
    };
    let text_contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    TerminalRenderFrame::from_plan(&plan, &text_contract)
}

fn bench_wgpu_scroll_text_prepare(c: &mut Criterion) {
    const RING: u32 = 128;
    let context = create_wgpu_bench_context();
    let frames = (0..RING).map(ascii_scroll_text_frame).collect::<Vec<_>>();
    let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
    // Warm every distinct line so the steady state measures scroll reuse, not the
    // one-time first-shape cost of each line.
    for frame in &frames {
        warm_wgpu_renderer(&context, &mut renderer, frame);
    }
    let mut tick = 0_usize;

    c.bench_function("wgpu_prepare_scroll_ascii_text_240x90", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let frame = &frames[tick % frames.len()];
            black_box(renderer.prepare_terminal_frame(
                &context.device,
                &context.queue,
                frame,
                1.0,
                ViewTransform::IDENTITY,
            ))
        })
    });
}
fn bench_text_atlas_prepare_dirty_ascii(c: &mut Criterion) {
    let frames = (0..16).map(ascii_dirty_text_frame).collect::<Vec<_>>();
    let mut builder = TextAtlasBuilder::new(2048, 2048);
    let mut tick = 0_usize;
    let mut quads = Vec::new();

    c.bench_function("text_atlas_prepare_dirty_ascii_240x90", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let frame = &frames[tick % frames.len()];
            quads.clear();
            for command in &frame.commands {
                if let TerminalRenderCommand::Text(text) = command {
                    builder.prepare_text_command_into(text, 1.0, &mut quads);
                }
            }
            black_box(quads.len())
        })
    });
}

fn bench_wgpu_incremental_text_atlas_upload(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
    let mut frame = single_glyph_frame();
    warm_wgpu_renderer(&context, &mut renderer, &frame);
    let mut tick = 0_u32;

    c.bench_function("wgpu_incremental_text_atlas_upload", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let ch = char::from_u32(0x4e00 + tick % 20_000).expect("CJK codepoint");
            let text = frame
                .commands
                .iter_mut()
                .find_map(|command| match command {
                    TerminalRenderCommand::Text(text) => Some(text),
                    _ => None,
                })
                .expect("single text command");
            text.text.clear();
            text.text.push(ch);
            let vertices = renderer.prepare_terminal_frame(
                &context.device,
                &context.queue,
                &frame,
                1.0,
                ViewTransform::IDENTITY,
            );
            black_box((vertices, renderer.last_text_upload_bytes()))
        })
    });
}

fn bench_animated_agent_pipeline_wgpu_prepare(c: &mut Criterion) {
    let mut engine = terminal_engine(160, 60);
    let surface = surface_for(160, 60);
    let mut planner = PaintPlanner::default();
    let context = create_wgpu_bench_context();
    let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
    let mut tick = 0_u32;
    let render_frame = agent_render_frame(&mut engine, &mut planner, surface, tick);
    warm_wgpu_renderer(&context, &mut renderer, &render_frame);

    c.bench_function(
        "animated_agent_update_extract_plan_render_wgpu_prepare_160x60",
        |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                let render_frame = agent_render_frame(&mut engine, &mut planner, surface, tick);
                black_box(renderer.prepare_terminal_frame(
                    &context.device,
                    &context.queue,
                    &render_frame,
                    1.0,
                    ViewTransform::IDENTITY,
                ))
            })
        },
    );
}

fn bench_wgpu_render_pass(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let mut scenarios = prepared_render_scenarios();
    scenarios.push(paint_plan_fixtures::PreparedRenderScenario {
        name: "mixed_text_sprite_image_320x160",
        frame: mixed_text_sprite_image_frame(),
    });
    scenarios.push(paint_plan_fixtures::PreparedRenderScenario {
        name: "layer_ordering_images_320x160",
        frame: layer_ordering_image_frame(),
    });

    for scenario in scenarios {
        let mut renderer = TerminalWgpuRenderer::new(&context.device, context.format);
        warm_wgpu_renderer(&context, &mut renderer, &scenario.frame);
        let (width, height) = target_size(&scenario.frame);
        let target = create_offscreen_target(&context, width, height);
        c.bench_function(
            &format!("wgpu_render_pass_steady_reuse_{}", scenario.name),
            |b| {
                b.iter(|| {
                    submit_prepared_render_pass(&context, black_box(&renderer), black_box(&target));
                })
            },
        );
    }
}

fn bench_wgpu_first_frame_upload(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let frame = mixed_text_sprite_image_frame();
    let (width, height) = target_size(&frame);
    let target = create_offscreen_target(&context, width, height);

    c.bench_function(
        "wgpu_first_frame_upload_mixed_text_sprite_image_320x160",
        |b| {
            b.iter_batched(
                || TerminalWgpuRenderer::new(&context.device, context.format),
                |mut renderer| {
                    black_box(renderer.prepare_terminal_frame(
                        &context.device,
                        &context.queue,
                        &frame,
                        1.0,
                        ViewTransform::IDENTITY,
                    ));
                    submit_prepared_render_pass(&context, black_box(&renderer), black_box(&target));
                },
                BatchSize::SmallInput,
            )
        },
    );
}

criterion_group!(
name = benches;
// These benches include WGPU preparation and full-frame terminal workloads that
// vary on developer desktops under browser/GPU scheduler load.
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_wgpu_prepare,
    bench_wgpu_zoom_prepare,
    bench_wgpu_dirty_text_prepare,
    bench_wgpu_scroll_text_prepare,
    bench_text_atlas_prepare_dirty_ascii,
    bench_wgpu_incremental_text_atlas_upload,
    bench_wgpu_render_pass,
    bench_wgpu_first_frame_upload,
    bench_animated_agent_pipeline_wgpu_prepare
);
criterion_main!(benches);
