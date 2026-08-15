use std::hint::black_box;

use bootty_render::{
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
enum PowerCase {
    IdlePrompt,
    TypingEightCps,
    ReadlineEditing,
    LessScrolling,
    NeovimEditing,
    StdoutFlood,
    DoomFireAnimation,
    DashboardAnimation,
    ImageAnimation,
    ManyTabsIdle,
    ManyPanesActive,
}

impl PowerCase {
    fn name(self) -> &'static str {
        match self {
            Self::IdlePrompt => "idle_prompt",
            Self::TypingEightCps => "typing_8cps",
            Self::ReadlineEditing => "readline_editing",
            Self::LessScrolling => "less_scrolling",
            Self::NeovimEditing => "neovim_editing",
            Self::StdoutFlood => "stdout_flood",
            Self::DoomFireAnimation => "doom_fire_animation",
            Self::DashboardAnimation => "dashboard_animation",
            Self::ImageAnimation => "image_animation",
            Self::ManyTabsIdle => "many_tabs_idle",
            Self::ManyPanesActive => "many_panes_active",
        }
    }

    fn modeled_instances(self) -> usize {
        match self {
            Self::ManyTabsIdle => 32,
            Self::ManyPanesActive => 16,
            _ => 1,
        }
    }

    fn target_hz(self) -> u32 {
        match self {
            Self::IdlePrompt | Self::ManyTabsIdle => 1,
            Self::TypingEightCps => 8,
            Self::ReadlineEditing | Self::LessScrolling | Self::NeovimEditing => 30,
            Self::StdoutFlood
            | Self::DoomFireAnimation
            | Self::DashboardAnimation
            | Self::ImageAnimation
            | Self::ManyPanesActive => 60,
        }
    }
}

const CASES: [PowerCase; 11] = [
    PowerCase::IdlePrompt,
    PowerCase::TypingEightCps,
    PowerCase::ReadlineEditing,
    PowerCase::LessScrolling,
    PowerCase::NeovimEditing,
    PowerCase::StdoutFlood,
    PowerCase::DoomFireAnimation,
    PowerCase::DashboardAnimation,
    PowerCase::ImageAnimation,
    PowerCase::ManyTabsIdle,
    PowerCase::ManyPanesActive,
];

#[derive(Default)]
struct PowerStats {
    frames: usize,
    commands: usize,
    cells: usize,
    chars: usize,
    instances: usize,
    hash: u64,
}

impl PowerStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.frames as u64
            ^ (self.commands as u64).rotate_left(7)
            ^ (self.cells as u64).rotate_left(13)
            ^ (self.chars as u64).rotate_left(19)
            ^ (self.instances as u64).rotate_left(29)
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

fn write_case_frame(engine: &mut TerminalEngine, case: PowerCase, tick: u32) {
    match case {
        PowerCase::IdlePrompt | PowerCase::ManyTabsIdle => {
            if tick == 0 {
                engine.write_vt(b"$ ");
            }
        }
        PowerCase::TypingEightCps => {
            engine.write_vt(format!("\x1b[H$ typed-{tick:04}").as_bytes());
        }
        PowerCase::ReadlineEditing => {
            engine.write_vt(
                format!("\x1b[H$ cargo test --workspace --lib --tests # edit {tick:04}").as_bytes(),
            );
        }
        PowerCase::LessScrolling => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1Hless line {:06} {}",
                        tick + u32::from(row),
                        "doc ".repeat(20)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::NeovimEditing => {
            engine.write_vt(b"\x1b[H\x1b[48;5;236;38;5;252m NORMAL src/main.rs ");
            for row in 2..GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[38;5;{}m{:04} let value_{tick}_{row} = compute()?; // λ 🥟\x1b[0m",
                        70 + row % 120,
                        row
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::StdoutFlood => {
            for line in 0..128 {
                engine.write_vt(
                    format!(
                        "flood tick={tick:04} line={line:03} {}\r\n",
                        "payload ".repeat(8)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::DoomFireAnimation => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[48;5;{}m{}\x1b[0m",
                        16 + (u32::from(row) + tick) % 200,
                        " ".repeat(GEOMETRY.cols as usize)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::DashboardAnimation | PowerCase::ManyPanesActive => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[38;2;{};{};{}m▌ cpu={:02}% mem={:04}M net={} tick={tick:04}\x1b[0m",
                        (tick + u32::from(row) * 3) % 255,
                        (tick * 2 + u32::from(row) * 5) % 255,
                        (tick * 3 + u32::from(row) * 7) % 255,
                        (tick + u32::from(row)) % 100,
                        256 + row * 7,
                        "▁▂▃▄▅▆▇█".repeat(8)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::ImageAnimation => {
            engine.write_vt(
                format!(
                    "\x1b[Himage animation frame {tick:04} {}\r\n",
                    "▀▄█▌▐░▒▓".repeat(24)
                )
                .as_bytes(),
            );
        }
    }
}

fn render_once(engine: &mut TerminalEngine, planner: &mut PaintPlanner, stats: &mut PowerStats) {
    let frame = engine.extract_frame().expect("power frame");
    let plan = planner.plan(surface(), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    let render_frame =
        TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images);
    stats.frames += 1;
    stats.commands += render_frame.commands.len();
    stats.cells += frame.stats.cells;
    stats.chars += frame.stats.chars;
    stats.hash ^= frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
        });
}

fn run_power_case(case: PowerCase, seconds: u32) -> u64 {
    let mut engines = (0..case.modeled_instances())
        .map(|_| terminal_engine())
        .collect::<Vec<_>>();
    let mut planners = (0..engines.len())
        .map(|_| PaintPlanner::default())
        .collect::<Vec<_>>();
    let ticks = seconds.saturating_mul(case.target_hz()).max(1);
    let mut stats = PowerStats {
        instances: engines.len(),
        ..PowerStats::default()
    };

    for tick in 0..ticks {
        for (engine, planner) in engines.iter_mut().zip(planners.iter_mut()) {
            write_case_frame(engine, case, tick);
            render_once(engine, planner, &mut stats);
        }
    }

    stats.checksum()
}

fn bench_power_workloads(c: &mut Criterion) {
    for case in CASES {
        c.bench_function(
            &format!("power_thermal_{}_1s_render_model", case.name()),
            |b| {
                b.iter_batched(
                    || case,
                    |case| black_box(run_power_case(case, 1)),
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_power_workloads,
}
criterion_main!(benches);
