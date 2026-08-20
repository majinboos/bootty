use std::{hint::black_box, io::Cursor};

use bootty_app::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface, ViewTransform},
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::TerminalWgpuRenderer,
};
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use eframe::wgpu;

struct WgpuBenchContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,
}

fn terminal_engine(cols: u16, rows: u16) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("terminal engine")
}

fn surface_for(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::from(cols) * 9.0 + 20.0,
        f32::from(rows) * 22.0 + 20.0,
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
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
        label: Some("bootty kitty image bench device"),
        ..Default::default()
    }))
    .expect("wgpu device");
    WgpuBenchContext {
        device,
        queue,
        format: wgpu::TextureFormat::Rgba8Unorm,
    }
}

fn base64_encode_bytes(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

fn rgba_bytes(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let shade = ((x * 13 + y * 31 + seed * 17) % 255) as u8;
            bytes.extend_from_slice(&[shade, 255 - shade, shade.wrapping_add(seed as u8), 255]);
        }
    }
    bytes
}

fn rgb_bytes(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            bytes.push(((x * 7 + seed * 19) % 255) as u8);
            bytes.push(((y * 11 + seed * 23) % 255) as u8);
            bytes.push((((x + y) * 5 + seed * 29) % 255) as u8);
        }
    }
    bytes
}

fn raw_rgba_command(image_id: u32, placement_id: u32, width: u32, height: u32) -> String {
    format!(
        "\x1b_Ga=T,t=d,i={image_id},p={placement_id},s={width},v={height};{}\x1b\\",
        base64_encode_bytes(&rgba_bytes(width, height, image_id + placement_id))
    )
}

fn raw_rgb_transmit_command(image_id: u32, width: u32, height: u32) -> String {
    format!(
        "\x1b_Ga=t,t=d,f=24,i={image_id},s={width},v={height};{}\x1b\\",
        base64_encode_bytes(&rgb_bytes(width, height, image_id))
    )
}

fn place_command(image_id: u32, placement_id: u32, x: u32, y: u32) -> String {
    format!("\x1b_Ga=p,i={image_id},p={placement_id},x={x},y={y},c=8,r=4,q=1\x1b\\")
}

fn png_rgba_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut out), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer
            .write_image_data(&rgba_bytes(width, height, 91))
            .expect("png data");
    }
    out
}

fn png_command(image_id: u32, width: u32, height: u32) -> String {
    format!(
        "\x1b_Ga=T,f=100,q=1,i={image_id},p=1;{}\x1b\\",
        base64_encode_bytes(&png_rgba_bytes(width, height))
    )
}

fn text_and_images_engine(image_count: u32) -> TerminalEngine {
    let mut engine = terminal_engine(120, 40);
    engine.write_vt(b"\x1b[?25l");
    for row in 0..40 {
        engine.write_vt(
            format!(
                "\x1b[{};1H\x1b[38;2;125;207;255mimage row {row:03}\x1b[0m mixed text \
                 \x1b[38;2;224;175;104mtool stream -> render\x1b[0m {}",
                row + 1,
                "trace ".repeat(10),
            )
            .as_bytes(),
        );
    }
    for image_id in 1..=image_count {
        engine.write_vt(raw_rgb_transmit_command(image_id, 28, 16).as_bytes());
        engine.write_vt(
            place_command(
                image_id,
                image_id,
                70 + (image_id % 4) * 10,
                4 + image_id * 2,
            )
            .as_bytes(),
        );
    }
    engine
}

fn render_frame_from_engine(
    engine: &mut TerminalEngine,
    cols: u16,
    rows: u16,
) -> TerminalRenderFrame {
    let frame = engine.extract_frame().expect("extract frame");
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface_for(cols, rows), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images)
}

fn bench_kitty_protocol_parse(c: &mut Criterion) {
    let command = raw_rgba_command(11, 1, 32, 16);
    c.bench_function("kitty_protocol_parse_raw_rgba_32x16", |b| {
        b.iter_batched(
            || terminal_engine(80, 24),
            |mut engine| {
                engine.write_vt(black_box(command.as_bytes()));
                black_box(engine);
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_kitty_png_decode(c: &mut Criterion) {
    let command = png_command(31, 32, 32);
    c.bench_function("kitty_png_decode_extract_32x32", |b| {
        b.iter_batched(
            || terminal_engine(80, 24),
            |mut engine| {
                engine.write_vt(command.as_bytes());
                black_box(
                    engine
                        .extract_frame()
                        .expect("extract frame")
                        .images
                        .placements
                        .len(),
                );
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_kitty_placement_updates(c: &mut Criterion) {
    let transmit = raw_rgb_transmit_command(77, 24, 16);
    let placements = (1..=128)
        .map(|placement_id| place_command(77, placement_id, placement_id % 40, placement_id / 4))
        .collect::<Vec<_>>();
    c.bench_function("kitty_placement_updates_extract_128", |b| {
        b.iter_batched(
            || {
                let mut engine = terminal_engine(100, 40);
                engine.write_vt(transmit.as_bytes());
                engine
            },
            |mut engine| {
                for placement in &placements {
                    engine.write_vt(placement.as_bytes());
                }
                black_box(
                    engine
                        .extract_frame()
                        .expect("extract frame")
                        .images
                        .placements
                        .len(),
                );
            },
            BatchSize::LargeInput,
        )
    });
}

fn bench_kitty_deletion_storage(c: &mut Criterion) {
    let image_commands = (1..=64)
        .map(|image_id| raw_rgba_command(image_id, 1, 4, 4))
        .collect::<Vec<_>>();
    let deletes = [
        b"\x1b_Ga=d,d=i,i=7\x1b\\".as_slice(),
        b"\x1b_Ga=d,d=I,i=11\x1b\\".as_slice(),
        b"\x1b_Ga=d,d=r,x=1,y=2\x1b\\".as_slice(),
        b"\x1b_Ga=d,d=A\x1b\\".as_slice(),
    ];
    c.bench_function("kitty_deletion_storage_changes_64", |b| {
        b.iter_batched(
            || {
                let mut engine = terminal_engine(80, 24);
                for command in &image_commands {
                    engine.write_vt(command.as_bytes());
                }
                black_box(
                    engine
                        .extract_frame()
                        .expect("extract frame")
                        .images
                        .placements
                        .len(),
                );
                engine
            },
            |mut engine| {
                for delete in deletes {
                    engine.write_vt(delete);
                    black_box(
                        engine
                            .extract_frame()
                            .expect("extract frame")
                            .images
                            .placements
                            .len(),
                    );
                }
            },
            BatchSize::LargeInput,
        )
    });
}

fn bench_kitty_mixed_text_image_render_commands(c: &mut Criterion) {
    c.bench_function("kitty_mixed_text_image_extract_plan_render_120x40", |b| {
        b.iter_batched(
            || text_and_images_engine(8),
            |mut engine| {
                engine.write_vt(b"\x1b[12;1Htick update beside image payloads");
                black_box(render_frame_from_engine(&mut engine, 120, 40));
            },
            BatchSize::LargeInput,
        )
    });
}

fn bench_kitty_wgpu_upload(c: &mut Criterion) {
    let context = create_wgpu_bench_context();
    let mut engine = text_and_images_engine(8);
    let frame = render_frame_from_engine(&mut engine, 120, 40);
    c.bench_function("kitty_wgpu_image_upload_mixed_8", |b| {
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
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
name = benches;
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_kitty_protocol_parse,
    bench_kitty_png_decode,
    bench_kitty_placement_updates,
    bench_kitty_deletion_storage,
    bench_kitty_mixed_text_image_render_commands,
    bench_kitty_wgpu_upload
);
criterion_main!(benches);
