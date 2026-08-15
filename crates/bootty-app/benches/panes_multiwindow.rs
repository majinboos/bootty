use std::hint::black_box;

mod paint_plan_fixtures;

use bootty_app::{
    app_actions::{AppKeyBindings, KeybindAction, MuxKeyAction},
    commands::{CommandExecutor, CommandRegistry, CoreCommandExecutor},
    config::{BoottyConfig, MultiplexerBackendConfig},
    geometry::TerminalSurface,
    paint_plan::PaintPlanner,
    terminal::{KeyInput, KeyMods, TerminalEngine, TerminalKey},
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use criterion::{Criterion, criterion_group, criterion_main};
use paint_plan_fixtures::{surface_for, terminal_engine};

const PANE_COUNTS: [usize; 4] = [1, 4, 16, 64];
const TAB_COUNTS: [usize; 3] = [1, 8, 32];
const MULTI_WINDOW_COUNTS: [usize; 3] = [1, 4, 16];
const PANE_COLS: u16 = 240;
const PANE_ROWS: u16 = 90;
const WINDOW_COLS: u16 = 120;
const WINDOW_ROWS: u16 = 40;

#[derive(Clone, Copy)]
struct PaneBounds {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

#[derive(Clone, Copy)]
enum MuxEquivalent {
    Native,
    Tmux,
    Zellij,
}

impl MuxEquivalent {
    fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Tmux => "tmux",
            Self::Zellij => "zellij",
        }
    }

    fn status_prefix(self) -> &'static str {
        match self {
            Self::Native => "bootty",
            Self::Tmux => "tmux",
            Self::Zellij => "zellij",
        }
    }
}

const MUX_EQUIVALENTS: [MuxEquivalent; 3] = [
    MuxEquivalent::Native,
    MuxEquivalent::Tmux,
    MuxEquivalent::Zellij,
];

fn pane_grid_for(count: usize) -> (usize, usize) {
    match count {
        1 => (1, 1),
        4 => (2, 2),
        16 => (4, 4),
        64 => (8, 8),
        _ => panic!("unsupported pane count {count}"),
    }
}

fn pane_bounds(index: usize, count: usize, cols: u16, rows: u16) -> PaneBounds {
    let (grid_cols, grid_rows) = pane_grid_for(count);
    let cell_width = usize::from(cols) / grid_cols;
    let usable_rows = usize::from(rows.saturating_sub(1));
    let cell_height = usable_rows / grid_rows;
    let grid_col = index % grid_cols;
    let grid_row = index / grid_cols;
    let left = grid_col * cell_width + 1;
    let top = grid_row * cell_height + 1;
    let right = if grid_col + 1 == grid_cols {
        usize::from(cols)
    } else {
        (grid_col + 1) * cell_width
    };
    let bottom = if grid_row + 1 == grid_rows {
        usable_rows
    } else {
        (grid_row + 1) * cell_height
    };
    PaneBounds {
        left: left as u16,
        top: top as u16,
        width: right.saturating_sub(left).saturating_add(1) as u16,
        height: bottom.saturating_sub(top).saturating_add(1) as u16,
    }
}

fn write_at(engine: &mut TerminalEngine, row: u16, col: u16, text: &str) {
    engine.write_vt(format!("\x1b[{row};{col}H{text}").as_bytes());
}

fn clipped_ascii(value: &str, width: u16) -> String {
    value.chars().take(width as usize).collect()
}

fn write_pane_frame(engine: &mut TerminalEngine, bounds: PaneBounds, pane: usize, active: bool) {
    if bounds.width < 4 || bounds.height < 3 {
        return;
    }
    let inner_width = bounds.width.saturating_sub(2);
    let right = bounds.left + bounds.width - 1;
    let bottom = bounds.top + bounds.height - 1;
    let color = if active { "38;5;81" } else { "38;5;240" };
    let header = format!("pane-{pane:02} {}", if active { "ACTIVE" } else { "idle" });

    write_at(
        engine,
        bounds.top,
        bounds.left,
        &format!("\x1b[{color}m+{}+\x1b[0m", "-".repeat(inner_width as usize)),
    );
    for row in bounds.top + 1..bottom {
        write_at(engine, row, bounds.left, &format!("\x1b[{color}m|\x1b[0m"));
        write_at(engine, row, right, &format!("\x1b[{color}m|\x1b[0m"));
    }
    write_at(
        engine,
        bottom,
        bounds.left,
        &format!("\x1b[{color}m+{}+\x1b[0m", "-".repeat(inner_width as usize)),
    );
    write_at(
        engine,
        bounds.top,
        bounds.left + 2,
        &format!(
            "\x1b[1;{color}m{}\x1b[0m",
            clipped_ascii(&header, inner_width)
        ),
    );

    let content_rows = bounds.height.saturating_sub(2).min(6);
    for line in 0..content_rows {
        let text = format!(
            "job={pane:02}.{line:02} cpu={:02}% rss={:04}M stream {}",
            (pane * 7 + line as usize * 13) % 100,
            128 + pane * 11 + line as usize,
            if active { "tailing" } else { "parked" }
        );
        write_at(
            engine,
            bounds.top + 1 + line,
            bounds.left + 2,
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                70 + pane % 120,
                clipped_ascii(&text, inner_width)
            ),
        );
    }
}

fn write_pane_grid(engine: &mut TerminalEngine, pane_count: usize, active_pane: usize, tick: u32) {
    engine.write_vt(b"\x1b[?25l\x1b[H\x1b[0m\x1b[2J");
    for pane in 0..pane_count {
        write_pane_frame(
            engine,
            pane_bounds(pane, pane_count, PANE_COLS, PANE_ROWS),
            pane,
            pane == active_pane,
        );
    }
    write_at(
        engine,
        PANE_ROWS,
        1,
        &format!(
            "\x1b[48;5;236;38;5;252m bootty mux-equivalent panes={} active={} tick={} {}\x1b[0m",
            pane_count,
            active_pane,
            tick,
            " ".repeat(PANE_COLS.saturating_sub(58) as usize)
        ),
    );
}

fn write_active_pane_update(
    engine: &mut TerminalEngine,
    pane_count: usize,
    active_pane: usize,
    tick: u32,
) {
    let bounds = pane_bounds(active_pane, pane_count, PANE_COLS, PANE_ROWS);
    let row = bounds.top + 1 + (tick as u16 % bounds.height.saturating_sub(2).max(1));
    let text =
        format!("input tick={tick:08x} pane={active_pane:02} latency-probe keypress echo burst");
    write_at(
        engine,
        row,
        bounds.left + 2,
        &format!(
            "\x1b[48;5;24;38;5;231m{}\x1b[0m",
            clipped_ascii(&text, bounds.width.saturating_sub(4))
        ),
    );
}

fn write_mux_equivalent_chrome(
    engine: &mut TerminalEngine,
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
    active_tab: usize,
    tick: u32,
) {
    let prefix = mode.status_prefix();
    match mode {
        MuxEquivalent::Native => {
            write_at(
                engine,
                1,
                1,
                &format!(
                    "\x1b[48;5;24;38;5;231m {prefix} tabs={tab_count} active={active_tab:02} panes={pane_count} tick={tick:08x} {}\x1b[0m",
                    " ".repeat(PANE_COLS.saturating_sub(64) as usize)
                ),
            );
        }
        MuxEquivalent::Tmux => {
            write_at(
                engine,
                1,
                1,
                &format!(
                    "\x1b[48;5;22;38;5;231m[{}] {}\x1b[0m",
                    (0..tab_count)
                        .map(|tab| if tab == active_tab {
                            format!("#{tab}:active*")
                        } else {
                            format!("#{tab}:idle")
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    " ".repeat(PANE_COLS.saturating_sub(tab_count as u16 * 10).max(1) as usize)
                ),
            );
            write_at(
                engine,
                PANE_ROWS,
                1,
                &format!(
                    "\x1b[48;5;236;38;5;252m {prefix}: panes={pane_count} ctrl-b passthrough tick={tick:08x} {}\x1b[0m",
                    " ".repeat(PANE_COLS.saturating_sub(64) as usize)
                ),
            );
        }
        MuxEquivalent::Zellij => {
            write_at(
                engine,
                1,
                1,
                &format!(
                    "\x1b[48;5;55;38;5;231m zellij mode tabs={tab_count} active={active_tab:02} resize=locked {}\x1b[0m",
                    " ".repeat(PANE_COLS.saturating_sub(58) as usize)
                ),
            );
            write_at(
                engine,
                PANE_ROWS,
                1,
                &format!(
                    "\x1b[48;5;238;38;5;252m Alt-n new-pane Alt-hjkl focus tick={tick:08x} panes={pane_count} {}\x1b[0m",
                    " ".repeat(PANE_COLS.saturating_sub(72) as usize)
                ),
            );
        }
    }
}

fn mux_equivalent_engine(
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
) -> TerminalEngine {
    let mut engine = pane_engine(pane_count);
    write_mux_equivalent_chrome(&mut engine, mode, pane_count, tab_count, 0, 0);
    engine
}

fn write_all_panes_tailing(
    engine: &mut TerminalEngine,
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
    tick: u32,
) {
    let active_tab = tick as usize % tab_count;
    for pane in 0..pane_count {
        let bounds = pane_bounds(pane, pane_count, PANE_COLS, PANE_ROWS);
        let content_rows = bounds.height.saturating_sub(2).max(1);
        let row = bounds.top + 1 + ((tick as usize + pane) as u16 % content_rows);
        let text = format!(
            "{} pane={pane:02} tab={active_tab:02} line={} cpu={:02}% tail {}",
            mode.label(),
            tick.wrapping_add(pane as u32),
            (tick as usize * 3 + pane * 11) % 100,
            "log ".repeat(6)
        );
        write_at(
            engine,
            row,
            bounds.left + 2,
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                100 + pane % 80,
                clipped_ascii(&text, bounds.width.saturating_sub(4))
            ),
        );
    }
    write_mux_equivalent_chrome(engine, mode, pane_count, tab_count, active_tab, tick);
}

fn pane_engine(pane_count: usize) -> TerminalEngine {
    let mut engine = terminal_engine(PANE_COLS, PANE_ROWS);
    write_pane_grid(&mut engine, pane_count, 0, 0);
    engine
}

fn extract_plan_render(
    engine: &mut TerminalEngine,
    planner: &mut PaintPlanner,
    surface: TerminalSurface,
) -> usize {
    let frame = engine.extract_frame().expect("frame");
    let plan = planner.plan(surface, frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    TerminalRenderFrame::from_plan(&plan, &text_contract)
        .commands
        .len()
}

fn write_window_frame(engine: &mut TerminalEngine, window: usize, tick: u32) {
    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[48;5;24;38;5;231m window {window:02} active frame {tick:08x} {}\x1b[0m",
            " ".repeat(76)
        )
        .as_bytes(),
    );
    for row in 2..WINDOW_ROWS {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;5;{}mwin={window:02} row={row:02} log={}\x1b[0m",
                80 + (row as usize + window) % 120,
                "event ".repeat(10)
            )
            .as_bytes(),
        );
    }
}

fn window_engines(count: usize) -> Vec<TerminalEngine> {
    (0..count)
        .map(|window| {
            let mut engine = terminal_engine(WINDOW_COLS, WINDOW_ROWS);
            write_window_frame(&mut engine, window, 0);
            engine
        })
        .collect()
}

fn tab_key_inputs() -> Vec<KeyInput> {
    vec![
        KeyInput {
            key: TerminalKey::Tab,
            mods: KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyInput {
            key: TerminalKey::Digit1,
            mods: KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
    ]
}

fn bench_pane_grid_pipeline(c: &mut Criterion) {
    for pane_count in PANE_COUNTS {
        c.bench_function(
            &format!("pane_grid_extract_plan_render_{pane_count}_panes_240x90"),
            |b| {
                let mut engine = pane_engine(pane_count);
                let mut planner = PaintPlanner::default();
                let surface = surface_for(PANE_COLS, PANE_ROWS);
                let mut tick = 0_u32;
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let active_pane = tick as usize % pane_count;
                    write_active_pane_update(&mut engine, pane_count, active_pane, tick);
                    black_box(extract_plan_render(&mut engine, &mut planner, surface))
                })
            },
        );
    }
}

fn bench_pane_active_and_inactive_paths(c: &mut Criterion) {
    for pane_count in PANE_COUNTS {
        c.bench_function(
            &format!("pane_active_update_write_{pane_count}_panes_240x90"),
            |b| {
                let mut engine = pane_engine(pane_count);
                let mut tick = 0_u32;
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let active_pane = tick as usize % pane_count;
                    write_active_pane_update(&mut engine, pane_count, active_pane, tick);
                    black_box(engine.grid_size())
                })
            },
        );

        c.bench_function(
            &format!("pane_inactive_clean_extract_{pane_count}_panes_240x90"),
            |b| {
                let mut engine = pane_engine(pane_count);
                black_box(engine.extract_frame().expect("warm frame").stats.cells);
                b.iter(|| black_box(engine.extract_frame().expect("clean frame").stats.cells))
            },
        );
    }
}

fn bench_tab_keybind_lookup(c: &mut Criterion) {
    let keybinds = BoottyConfig::default()
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    let inputs = tab_key_inputs();

    c.bench_function("tab_switch_keybinding_lookup_native", |b| {
        let mut bindings = AppKeyBindings::from_keybinds(&keybinds).expect("keybindings");
        b.iter(|| {
            let mut hits = 0_u8;
            for input in &inputs {
                let executor =
                    bindings
                        .invocation_for_input(black_box(*input))
                        .and_then(|invocation| {
                            CommandRegistry::core()
                                .resolve(invocation)
                                .ok()
                                .map(|resolved| resolved.executor)
                        });
                if matches!(
                    executor,
                    Some(CommandExecutor::Core(CoreCommandExecutor::Keybind(
                        KeybindAction::Mux(MuxKeyAction::NextTab | MuxKeyAction::SelectTab(_))
                    )))
                ) {
                    hits = hits.saturating_add(1);
                }
            }
            black_box(hits)
        })
    });
}

fn bench_multi_window_frame_paths(c: &mut Criterion) {
    for window_count in MULTI_WINDOW_COUNTS {
        c.bench_function(
            &format!("multi_window_extract_plan_render_{window_count}_windows_120x40"),
            |b| {
                let mut engines = window_engines(window_count);
                let mut planners = (0..window_count)
                    .map(|_| PaintPlanner::default())
                    .collect::<Vec<_>>();
                let surface = surface_for(WINDOW_COLS, WINDOW_ROWS);
                let mut tick = 0_u32;
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let mut commands = 0_usize;
                    for (index, (engine, planner)) in
                        engines.iter_mut().zip(planners.iter_mut()).enumerate()
                    {
                        write_window_frame(engine, index, tick);
                        commands += extract_plan_render(engine, planner, surface);
                    }
                    black_box(commands)
                })
            },
        );
    }
}

fn bench_mux_equivalent_pane_modes(c: &mut Criterion) {
    for mode in MUX_EQUIVALENTS {
        for pane_count in PANE_COUNTS {
            c.bench_function(
                &format!(
                    "mux_equivalent_{}_mixed_active_{pane_count}_panes",
                    mode.label()
                ),
                |b| {
                    let mut engine = mux_equivalent_engine(mode, pane_count, 8);
                    let mut planner = PaintPlanner::default();
                    let surface = surface_for(PANE_COLS, PANE_ROWS);
                    let mut tick = 0_u32;
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        let active_pane = tick as usize % pane_count;
                        write_active_pane_update(&mut engine, pane_count, active_pane, tick);
                        write_mux_equivalent_chrome(
                            &mut engine,
                            mode,
                            pane_count,
                            8,
                            tick as usize % 8,
                            tick,
                        );
                        black_box(extract_plan_render(&mut engine, &mut planner, surface))
                    })
                },
            );

            c.bench_function(
                &format!(
                    "mux_equivalent_{}_all_tailing_{pane_count}_panes",
                    mode.label()
                ),
                |b| {
                    let mut engine = mux_equivalent_engine(mode, pane_count, 8);
                    let mut planner = PaintPlanner::default();
                    let surface = surface_for(PANE_COLS, PANE_ROWS);
                    let mut tick = 0_u32;
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        write_all_panes_tailing(&mut engine, mode, pane_count, 8, tick);
                        black_box(extract_plan_render(&mut engine, &mut planner, surface))
                    })
                },
            );
        }
    }
}

fn bench_mux_equivalent_tab_modes(c: &mut Criterion) {
    for mode in MUX_EQUIVALENTS {
        for tab_count in TAB_COUNTS {
            c.bench_function(
                &format!(
                    "mux_equivalent_{}_tab_switch_{tab_count}_tabs",
                    mode.label()
                ),
                |b| {
                    let mut engine = mux_equivalent_engine(mode, 4, tab_count);
                    let mut planner = PaintPlanner::default();
                    let surface = surface_for(PANE_COLS, PANE_ROWS);
                    let mut tick = 0_u32;
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        write_mux_equivalent_chrome(
                            &mut engine,
                            mode,
                            4,
                            tab_count,
                            tick as usize % tab_count,
                            tick,
                        );
                        black_box(extract_plan_render(&mut engine, &mut planner, surface))
                    })
                },
            );
        }
    }
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.15);
    targets =
        bench_pane_grid_pipeline,
        bench_pane_active_and_inactive_paths,
        bench_tab_keybind_lookup,
        bench_multi_window_frame_paths,
        bench_mux_equivalent_pane_modes,
        bench_mux_equivalent_tab_modes
);
criterion_main!(benches);
