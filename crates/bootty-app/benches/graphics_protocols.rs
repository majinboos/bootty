use std::{hint::black_box, io::Cursor};

use bootty_render::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Default)]
struct GraphicsStats {
    protocol_bytes: usize,
    placements: usize,
    virtual_placements: usize,
    render_commands: usize,
    text_chars: usize,
    unsupported: usize,
    hash: u64,
}

enum ExpectedGraphicsOutput {
    UnsupportedNative,
    TextFallback,
}

impl GraphicsStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.protocol_bytes as u64
            ^ (self.placements as u64).rotate_left(11)
            ^ (self.virtual_placements as u64).rotate_left(19)
            ^ (self.render_commands as u64).rotate_left(29)
            ^ (self.text_chars as u64).rotate_left(37)
            ^ (self.unsupported as u64).rotate_left(47)
    }
}

fn terminal_engine() -> TerminalEngine {
    TerminalEngine::new(GEOMETRY).expect("terminal engine")
}

fn surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::from(GEOMETRY.cols) * GEOMETRY.cell_width as f32 + 20.0,
        f32::from(GEOMETRY.rows) * GEOMETRY.cell_height as f32 + 20.0,
        CellMetrics::new(GEOMETRY.cell_width as f32, GEOMETRY.cell_height as f32),
        TerminalPadding::uniform(10.0),
    )
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
        out.push(if chunk.len() > 1 {
            TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b2 & 0b0011_1111) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn rgba_bytes(width: u32, height: u32, seed: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let shade = ((x * 17 + y * 31 + seed * 13) % 255) as u8;
            bytes.extend_from_slice(&[shade, shade.wrapping_add(80), 255 - shade, 255]);
        }
    }
    bytes
}

fn png_rgba_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut out), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer
            .write_image_data(&rgba_bytes(width, height, 17))
            .expect("png data");
    }
    out
}

fn iterm2_image_osc(width: u32, height: u32) -> Vec<u8> {
    format!(
        "\x1b]1337;File=inline=1;width={width}px;height={height}px:{}\x07",
        base64_encode_bytes(&png_rgba_bytes(width, height))
    )
    .into_bytes()
}

fn sixel_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1bPq\"1;1;64;64#1;2;100;40;20".to_vec();
    for index in 0..repeats {
        payload
            .extend_from_slice(format!("#{}{}-$", 1 + index % 6, "?~~@@vv".repeat(4)).as_bytes());
    }
    payload.extend_from_slice(b"\x1b\\");
    payload
}

fn block_fallback_frame(rows: usize) -> Vec<u8> {
    let mut payload = Vec::new();
    for row in 0..rows {
        payload.extend_from_slice(
            format!(
                "\x1b[38;2;{};{};{}m{}\x1b[0m\r\n",
                row % 255,
                row * 3 % 255,
                row * 7 % 255,
                "▀▄█▌▐░▒▓".repeat(18)
            )
            .as_bytes(),
        );
    }
    payload
}

fn payload_hash(payload: &[u8]) -> u64 {
    payload
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn protocol_stats(
    mut engine: TerminalEngine,
    payloads: &[Vec<u8>],
    expected: ExpectedGraphicsOutput,
) -> u64 {
    let mut stats = GraphicsStats::default();
    for payload in payloads {
        stats.protocol_bytes += payload.len();
        stats.hash ^= payload_hash(payload);
        engine.write_vt(payload);
    }

    let frame = engine.extract_frame().expect("graphics protocol frame");
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface(), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    let render_frame =
        TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images);

    stats.placements = frame.images.placements.len();
    stats.virtual_placements = frame.images.virtual_placements.len();
    stats.render_commands = render_frame.commands.len();
    stats.text_chars = frame.text.len();
    match expected {
        ExpectedGraphicsOutput::UnsupportedNative
            if stats.placements == 0 && stats.virtual_placements == 0 =>
        {
            stats.unsupported += 1;
        }
        ExpectedGraphicsOutput::TextFallback => {
            stats.hash ^= (stats.text_chars as u64).rotate_left(7);
        }
        ExpectedGraphicsOutput::UnsupportedNative => {}
    }
    stats.checksum()
}

fn bench_iterm2_protocol(c: &mut Criterion) {
    let small = iterm2_image_osc(32, 32);
    c.bench_function("graphics_iterm2_image_osc_unsupported_32x32", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(protocol_stats(
                    engine,
                    std::slice::from_ref(&small),
                    ExpectedGraphicsOutput::UnsupportedNative,
                ))
            },
            BatchSize::SmallInput,
        )
    });

    let thumbnails = (0..100).map(|_| iterm2_image_osc(8, 8)).collect::<Vec<_>>();
    c.bench_function(
        "graphics_iterm2_image_osc_unsupported_100_thumbnails",
        |b| {
            b.iter_batched(
                terminal_engine,
                |engine| {
                    black_box(protocol_stats(
                        engine,
                        &thumbnails,
                        ExpectedGraphicsOutput::UnsupportedNative,
                    ))
                },
                BatchSize::LargeInput,
            )
        },
    );
}

fn bench_sixel_protocol(c: &mut Criterion) {
    let small = sixel_payload(8);
    c.bench_function("graphics_sixel_unsupported_small", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(protocol_stats(
                    engine,
                    std::slice::from_ref(&small),
                    ExpectedGraphicsOutput::UnsupportedNative,
                ))
            },
            BatchSize::SmallInput,
        )
    });

    let stress = sixel_payload(256);
    c.bench_function("graphics_sixel_unsupported_stress", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(protocol_stats(
                    engine,
                    std::slice::from_ref(&stress),
                    ExpectedGraphicsOutput::UnsupportedNative,
                ))
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_block_fallback(c: &mut Criterion) {
    let small = block_fallback_frame(8);
    c.bench_function("graphics_block_fallback_8_rows", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(protocol_stats(
                    engine,
                    std::slice::from_ref(&small),
                    ExpectedGraphicsOutput::TextFallback,
                ))
            },
            BatchSize::SmallInput,
        )
    });

    let rows = block_fallback_frame(100);
    c.bench_function("graphics_block_fallback_100_rows", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(protocol_stats(
                    engine,
                    std::slice::from_ref(&rows),
                    ExpectedGraphicsOutput::TextFallback,
                ))
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_iterm2_protocol, bench_sixel_protocol, bench_block_fallback,
}
criterion_main!(benches);
