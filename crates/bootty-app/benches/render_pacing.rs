use std::{hint::black_box, time::Duration};

use bootty_app::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 160,
    rows: 48,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum UpdatePattern {
    CursorOnly,
    SingleCell,
    StatusLine,
    OneRow,
    OneColumn,
    Random5Pct,
    Random25Pct,
    FullRepaint,
    ScrollOneLine,
    ScrollHalfScreen,
    ScrollFullScreen,
    AlternateScreenRedraw,
    ScrollbackAppend,
}

impl UpdatePattern {
    const fn name(self) -> &'static str {
        match self {
            Self::CursorOnly => "cursor_only",
            Self::SingleCell => "single_cell",
            Self::StatusLine => "statusline",
            Self::OneRow => "one_row",
            Self::OneColumn => "one_column",
            Self::Random5Pct => "random_5pct",
            Self::Random25Pct => "random_25pct",
            Self::FullRepaint => "full_repaint",
            Self::ScrollOneLine => "scroll_one_line",
            Self::ScrollHalfScreen => "scroll_half_screen",
            Self::ScrollFullScreen => "scroll_full_screen",
            Self::AlternateScreenRedraw => "alternate_screen_redraw",
            Self::ScrollbackAppend => "scrollback_append",
        }
    }
}

#[derive(Default)]
struct PacingStats {
    frames: usize,
    missed_budget: usize,
    max_frame_ns: u128,
    commands: usize,
    dirty_rows: usize,
    cells: usize,
    chars: usize,
    hash: u64,
}

impl PacingStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.frames as u64
            ^ (self.missed_budget as u64).rotate_left(7)
            ^ (self.max_frame_ns as u64).rotate_left(13)
            ^ (self.commands as u64).rotate_left(19)
            ^ (self.dirty_rows as u64).rotate_left(29)
            ^ (self.cells as u64).rotate_left(37)
            ^ (self.chars as u64).rotate_left(43)
    }
}

fn surface_for(geometry: TerminalGeometry) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::from(geometry.cols) * geometry.cell_width as f32 + 20.0,
        f32::from(geometry.rows) * geometry.cell_height as f32 + 20.0,
        CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
        TerminalPadding::uniform(10.0),
    )
}

fn seeded_engine() -> TerminalEngine {
    let mut engine = TerminalEngine::new_with_scrollback(GEOMETRY, Default::default(), 32_000_000)
        .expect("terminal engine");
    for row in 1..=GEOMETRY.rows {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;5;{}mseed row {row:03}\x1b[0m {}",
                16 + row % 200,
                "baseline cells ".repeat(12)
            )
            .as_bytes(),
        );
    }
    engine
}

fn apply_pattern(engine: &mut TerminalEngine, pattern: UpdatePattern, tick: u32) {
    match pattern {
        UpdatePattern::CursorOnly => {
            let row = 1 + tick % u32::from(GEOMETRY.rows);
            let col = 1 + (tick * 7) % u32::from(GEOMETRY.cols);
            engine.write_vt(format!("\x1b[{row};{col}H").as_bytes());
        }
        UpdatePattern::SingleCell => {
            let row = 1 + tick % u32::from(GEOMETRY.rows);
            let col = 1 + (tick * 13) % u32::from(GEOMETRY.cols);
            let ch = (b'a' + (tick % 26) as u8) as char;
            engine.write_vt(format!("\x1b[{row};{col}H{ch}").as_bytes());
        }
        UpdatePattern::StatusLine => {
            engine.write_vt(
                format!(
                    "\x1b[1;1H\x1b[48;5;236;38;5;81m frame {tick:06} {}\x1b[0m",
                    "status ".repeat(24)
                )
                .as_bytes(),
            );
        }
        UpdatePattern::OneRow => {
            let row = 1 + tick % u32::from(GEOMETRY.rows);
            engine.write_vt(
                format!(
                    "\x1b[{row};1H\x1b[38;2;{};{};220mrow update {tick:06} {}\x1b[0m",
                    tick % 255,
                    (tick * 3) % 255,
                    "row cells ".repeat(20)
                )
                .as_bytes(),
            );
        }
        UpdatePattern::OneColumn => {
            let col = 1 + tick % u32::from(GEOMETRY.cols);
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(format!("\x1b[{row};{col}H┃").as_bytes());
            }
        }
        UpdatePattern::Random5Pct => random_cells(engine, tick, 384),
        UpdatePattern::Random25Pct => random_cells(engine, tick, 1_920),
        UpdatePattern::FullRepaint => full_repaint(engine, tick),
        UpdatePattern::ScrollOneLine => {
            engine
                .write_vt(format!("\r\nscroll one {tick:06} {}", "payload ".repeat(18)).as_bytes());
        }
        UpdatePattern::ScrollHalfScreen => {
            for row in 0..(GEOMETRY.rows / 2) {
                engine.write_vt(
                    format!(
                        "\r\nscroll half {tick:06}-{row:02} {}",
                        "payload ".repeat(18)
                    )
                    .as_bytes(),
                );
            }
        }
        UpdatePattern::ScrollFullScreen => {
            for row in 0..GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\r\nscroll full {tick:06}-{row:02} {}",
                        "payload ".repeat(18)
                    )
                    .as_bytes(),
                );
            }
        }
        UpdatePattern::AlternateScreenRedraw => {
            engine.write_vt(b"\x1b[?1049h\x1b[2J\x1b[H");
            full_repaint(engine, tick);
            engine.write_vt(b"\x1b[?1049l");
        }
        UpdatePattern::ScrollbackAppend => {
            for row in 0..8 {
                engine.write_vt(
                    format!(
                        "\r\nscrollback append {tick:06}-{row:02} {}",
                        "history ".repeat(22)
                    )
                    .as_bytes(),
                );
            }
        }
    }
}

fn random_cells(engine: &mut TerminalEngine, tick: u32, cells: usize) {
    let mut seed = tick.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    for index in 0..cells {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let row = 1 + seed % u32::from(GEOMETRY.rows);
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let col = 1 + seed % u32::from(GEOMETRY.cols);
        let ch = (b'a' + ((tick as usize + index) % 26) as u8) as char;
        engine.write_vt(format!("\x1b[{row};{col}H{ch}").as_bytes());
    }
}

fn full_repaint(engine: &mut TerminalEngine, tick: u32) {
    engine.write_vt(b"\x1b[H");
    for row in 1..=GEOMETRY.rows {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;2;{};{};230mfull {tick:06} row {row:03}\x1b[0m {}",
                (tick + u32::from(row)) % 255,
                (tick * 3 + u32::from(row)) % 255,
                "frame cells ".repeat(16)
            )
            .as_bytes(),
        );
    }
}

fn hash_text(text: &[char]) -> u64 {
    text.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn run_pacing(pattern: UpdatePattern, target_hz: u32, frames: u32) -> u64 {
    let mut engine = seeded_engine();
    let mut planner = PaintPlanner::default();
    let surface = surface_for(GEOMETRY);
    let budget = Duration::from_secs_f64(1.0 / f64::from(target_hz));
    let mut stats = PacingStats::default();

    for tick in 0..frames {
        let start = std::time::Instant::now();
        apply_pattern(&mut engine, pattern, tick);
        let frame = engine.extract_frame().expect("render pacing frame");
        let plan = planner.plan(surface, frame, 16.0).clone();
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
        let render_frame = TerminalRenderFrame::from_plan(&plan, &text_contract);
        let elapsed = start.elapsed();

        stats.frames += 1;
        stats.missed_budget += usize::from(elapsed > budget);
        stats.max_frame_ns = stats.max_frame_ns.max(elapsed.as_nanos());
        stats.commands += render_frame.commands.len();
        stats.dirty_rows += frame.stats.dirty_rows;
        stats.cells += frame.stats.cells;
        stats.chars += frame.stats.chars;
        stats.hash ^= hash_text(&frame.text);
    }

    assert_eq!(stats.frames, frames as usize);
    stats.checksum()
}

fn core_patterns() -> [UpdatePattern; 13] {
    [
        UpdatePattern::CursorOnly,
        UpdatePattern::SingleCell,
        UpdatePattern::StatusLine,
        UpdatePattern::OneRow,
        UpdatePattern::OneColumn,
        UpdatePattern::Random5Pct,
        UpdatePattern::Random25Pct,
        UpdatePattern::FullRepaint,
        UpdatePattern::ScrollOneLine,
        UpdatePattern::ScrollHalfScreen,
        UpdatePattern::ScrollFullScreen,
        UpdatePattern::AlternateScreenRedraw,
        UpdatePattern::ScrollbackAppend,
    ]
}

fn bench_core_patterns(c: &mut Criterion) {
    for pattern in core_patterns() {
        c.bench_function(&format!("render_pacing_{}_120hz", pattern.name()), |b| {
            b.iter_batched(
                || pattern,
                |pattern| black_box(run_pacing(pattern, 120, 32)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_refresh_targets(c: &mut Criterion) {
    for target_hz in [30, 60, 120, 144, 240] {
        for pattern in [UpdatePattern::Random25Pct, UpdatePattern::FullRepaint] {
            c.bench_function(
                &format!("render_refresh_target_{}_{}hz", pattern.name(), target_hz),
                |b| {
                    b.iter_batched(
                        || pattern,
                        |pattern| black_box(run_pacing(pattern, target_hz, 32)),
                        BatchSize::SmallInput,
                    )
                },
            );
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_core_patterns, bench_refresh_targets,
}
criterion_main!(benches);
