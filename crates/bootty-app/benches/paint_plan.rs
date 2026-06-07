use std::hint::black_box;

use bootty_app::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal::{RenderFrame, TerminalEngine},
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::TerminalWgpuRenderer,
};
use criterion::{Criterion, criterion_group, criterion_main};
use eframe::{egui::Vec2, wgpu};

type ScenarioBuilder = (&'static str, fn() -> TerminalEngine);

struct PreparedScenario {
    name: &'static str,
    frame: RenderFrame,
    surface: TerminalSurface,
}

struct PreparedRenderScenario {
    name: &'static str,
    frame: TerminalRenderFrame,
}

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
    TerminalSurface::for_size(
        Vec2::new(f32::from(cols) * 9.0 + 20.0, f32::from(rows) * 22.0 + 20.0),
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn sample_engine() -> TerminalEngine {
    let mut engine = terminal_engine(120, 40);

    for row in 0..40 {
        let line = format!(
            "\x1b[{};1Hrow {:03}  abcdefghijklmnopqrstuvwxyz  0123456789",
            row + 1,
            row
        );
        engine.write_vt(line.as_bytes());
    }

    engine
}

fn complex_shell_engine() -> TerminalEngine {
    let mut engine = terminal_engine(180, 80);

    for row in 0..80 {
        let hue = row * 5 % 255;
        let alt = (row * 11 + 40) % 255;
        let line = format!(
            "\x1b[{};1H\
             \x1b[1;38;2;125;207;255mrow {row:03}\x1b[0m \
             \x1b[48;5;238;38;5;{}mindexed-bg\x1b[0m \
             \x1b[3;4;38;2;{};{};210municode 🥟 界 café e\u{301} \x1b[0m\
             \x1b[38;5;214m█░▒▓ ┃╋╬╣╠╦╩\x1b[0m \
             \x1b[38;2;{};160;{}m λ ∑ → ←\x1b[0m \
             \x1b[38;5;{}m{}\x1b[0m",
            row + 1,
            16 + row % 216,
            hue,
            255 - hue,
            alt,
            255 - alt,
            196 + row % 36,
            "0123456789abcdef".repeat(5)
        );
        engine.write_vt(line.as_bytes());
    }

    engine
}

fn ai_agent_dashboard_engine() -> TerminalEngine {
    let mut engine = terminal_engine(220, 70);
    engine.write_vt(b"\x1b[?25l");
    write_agent_dashboard_frame(&mut engine, 37, 220, 70);
    engine
}

fn tmux_image_truecolor_engine() -> TerminalEngine {
    let mut engine = terminal_engine(240, 90);
    engine.write_vt(b"\x1b[?25l");
    write_tmux_layout(&mut engine, 240, 90);
    write_kitty_image_grid(&mut engine);
    engine
}

fn scenario_builders() -> [ScenarioBuilder; 4] {
    [
        ("simple_shell_120x40", sample_engine),
        ("complex_shell_180x80", complex_shell_engine),
        ("ai_agent_dashboard_220x70", ai_agent_dashboard_engine),
        ("tmux_images_truecolor_240x90", tmux_image_truecolor_engine),
    ]
}

fn prepared_scenarios() -> Vec<PreparedScenario> {
    scenario_builders()
        .into_iter()
        .map(|(name, builder)| {
            let mut engine = builder();
            let (cols, rows) = engine.grid_size();
            PreparedScenario {
                name,
                frame: engine.extract_frame().expect("frame").clone(),
                surface: surface_for(cols, rows),
            }
        })
        .collect()
}

fn prepared_render_scenarios() -> Vec<PreparedRenderScenario> {
    prepared_scenarios()
        .into_iter()
        .map(|scenario| {
            let mut planner = PaintPlanner::default();
            let plan = planner
                .plan(scenario.surface, &scenario.frame, 16.0)
                .clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            PreparedRenderScenario {
                name: scenario.name,
                frame: TerminalRenderFrame::from_plan(&plan, &text_contract),
            }
        })
        .collect()
}

fn create_wgpu_bench_context() -> WgpuBenchContext {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::LowPower,
        compatible_surface: None,
        force_fallback_adapter: false,
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

fn warm_wgpu_renderer(
    context: &WgpuBenchContext,
    renderer: &mut TerminalWgpuRenderer,
    frame: &TerminalRenderFrame,
) {
    black_box(renderer.prepare_terminal_frame(&context.device, &context.queue, frame, 1.0));
}

fn agent_render_frame(
    engine: &mut TerminalEngine,
    planner: &mut PaintPlanner,
    surface: TerminalSurface,
    tick: u32,
) -> TerminalRenderFrame {
    write_agent_dashboard_frame(engine, tick, 160, 60);
    let frame = engine.extract_frame().expect("frame");
    let plan = planner.plan(surface, frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    TerminalRenderFrame::from_plan(&plan, &text_contract)
}

fn mutate_single_row(engine: &mut TerminalEngine, tick: u32) {
    let row = tick % u32::from(engine.grid_size().1) + 1;
    engine.write_vt(format!("\x1b[{row};1Htick-{tick:08x}").as_bytes());
}

fn write_agent_dashboard_frame(engine: &mut TerminalEngine, tick: u32, cols: u16, rows: u16) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"][(tick as usize) % 8];
    let progress = tick % 101;
    let phase = ["planning", "running tools", "patching", "verifying"][(tick as usize / 7) % 4];
    let width = cols.saturating_sub(2);

    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[48;2;17;24;39;38;2;192;202;245m {spinner} Bootty agent · {phase:<14} · {progress:>3}% · tokens 184,392 · jobs 7/9 {}\x1b[0m",
            " ".repeat(width.saturating_sub(88) as usize),
        )
        .as_bytes(),
    );

    for row in 1..rows {
        let row_u32 = u32::from(row);
        let tool = ["shell", "patch", "cargo", "sym", "render", "pty"][row as usize % 6];
        let status = ["queued", "running", "streaming", "cached", "done"][row as usize % 5];
        let color = 80 + row_u32 * 3 % 150;
        let heat = 30 + row_u32 * 5 % 190;
        let gutter = if row % 9 == 0 {
            "\x1b[38;2;255;158;100m│ warn\x1b[0m"
        } else if row % 13 == 0 {
            "\x1b[38;2;255;90;90m│ err \x1b[0m"
        } else {
            "\x1b[38;2;125;207;255m│ info\x1b[0m"
        };
        let diff = if row % 4 == 0 {
            "\x1b[38;2;158;206;106m+ async drain budget respected\x1b[0m"
        } else if row % 4 == 1 {
            "\x1b[38;2;247;118;142m- redundant redraw removed\x1b[0m"
        } else {
            "\x1b[38;2;224;175;104m~ streaming stdout chunk → renderer batch\x1b[0m"
        };
        let glyphs = ["🥟", "", "█▓▒░", "╭─╮", "λ∑→←"][row as usize % 5];
        engine.write_vt(
            format!(
                "\x1b[{};1H{gutter} \
                 \x1b[38;2;{};{};230m{tool:<6}\x1b[0m \
                 \x1b[48;5;236;38;5;{}m{status:<9}\x1b[0m \
                 \x1b[38;2;{};180;{}magent-{row:03}\x1b[0m \
                 {diff} \
                 \x1b[38;5;{}m{glyphs}\x1b[0m \
                 {}",
                row + 1,
                color,
                255 - color,
                16 + row % 216,
                heat,
                255 - heat,
                160 + row % 60,
                "trace=".repeat(8),
            )
            .as_bytes(),
        );
    }
}

fn write_tmux_layout(engine: &mut TerminalEngine, cols: u16, rows: u16) {
    let bottom = rows - 2;
    let right = cols - 1;
    let split_x = cols * 3 / 5;
    let split_y = rows * 2 / 5;

    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[38;2;125;207;255m╭{}╮\x1b[0m",
            "─".repeat(right.saturating_sub(1) as usize)
        )
        .as_bytes(),
    );
    for row in 2..bottom {
        engine.write_vt(format!("\x1b[{row};1H\x1b[38;2;125;207;255m│\x1b[0m").as_bytes());
        engine.write_vt(format!("\x1b[{row};{right}H\x1b[38;2;125;207;255m│\x1b[0m").as_bytes());
        engine.write_vt(format!("\x1b[{row};{split_x}H\x1b[38;2;86;95;137m┃\x1b[0m").as_bytes());
    }
    engine.write_vt(
        format!(
            "\x1b[{bottom};1H\x1b[38;2;125;207;255m╰{}╯\x1b[0m",
            "─".repeat(right.saturating_sub(1) as usize)
        )
        .as_bytes(),
    );
    engine.write_vt(
        format!(
            "\x1b[{split_y};2H\x1b[38;2;86;95;137m{}\x1b[0m",
            "━".repeat(right.saturating_sub(3) as usize)
        )
        .as_bytes(),
    );
    engine.write_vt(
        format!(
            "\x1b[{};1H\x1b[48;2;31;41;59;38;2;192;202;245m[bootty] 0:zsh* 1:agents 2:logs 3:images  cpu 73% mem 12.4G{}\x1b[0m",
            rows,
            " ".repeat(cols.saturating_sub(67) as usize)
        )
        .as_bytes(),
    );

    for row in 3..split_y {
        engine.write_vt(
            format!(
                "\x1b[{row};3H\x1b[38;2;158;206;106magent\x1b[0m \
                 \x1b[38;2;224;175;104mtool_call\x1b[0m \
                 id=call_{row:03} stdout={} 🥟",
                "stream ".repeat(9)
            )
            .as_bytes(),
        );
    }
    for row in split_y + 1..bottom {
        let pct = row * 100 / bottom;
        engine.write_vt(
            format!(
                "\x1b[{row};3H\x1b[38;2;247;118;142mhtop\x1b[0m pid={:05} \
                 \x1b[48;2;40;42;54;38;2;125;207;255m{:>3}% {}\x1b[0m \
                 \x1b[38;5;214m╭─╮ ┃╋ █▓▒░ \x1b[0m",
                10_000 + row,
                pct,
                "▰".repeat((pct / 5) as usize),
            )
            .as_bytes(),
        );
    }
}

fn write_kitty_image_grid(engine: &mut TerminalEngine) {
    for image_index in 0..8 {
        let id = 400 + image_index;
        let x = 150 + (image_index % 2) * 35;
        let y = 5 + (image_index / 2) * 18;
        engine.write_vt(rgb_image_command(id, 28, 12, 30, 12, x, y, image_index).as_bytes());
    }
}

#[allow(clippy::too_many_arguments)]
fn rgb_image_command(
    image_id: u32,
    pixel_width: u32,
    pixel_height: u32,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    seed: u32,
) -> String {
    let mut bytes = Vec::with_capacity((pixel_width * pixel_height * 3) as usize);
    for py in 0..pixel_height {
        for px in 0..pixel_width {
            bytes.push(((px * 7 + seed * 23) % 255) as u8);
            bytes.push(((py * 11 + seed * 31) % 255) as u8);
            bytes.push((((px + py) * 5 + seed * 17) % 255) as u8);
        }
    }
    format!(
        "\x1b_Ga=T,t=d,f=24,i={image_id},p=1,s={pixel_width},v={pixel_height},c={cols},r={rows},x={x},y={y},q=1;{}\x1b\\",
        base64_encode_bytes(&bytes)
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

fn bench_paint_plan(c: &mut Criterion) {
    for scenario in prepared_scenarios() {
        let mut planner = PaintPlanner::default();
        c.bench_function(&format!("paint_plan_{}", scenario.name), |b| {
            b.iter(|| {
                let plan = planner.plan(scenario.surface, &scenario.frame, 16.0);
                black_box(
                    plan.text_runs
                        .iter()
                        .map(|run| run.text.len())
                        .sum::<usize>(),
                )
            })
        });
    }
}

fn bench_extract_frame(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        c.bench_function(&format!("extract_frame_clean_steady_state_{name}"), |b| {
            b.iter(|| black_box(engine.extract_frame().expect("frame").stats.cells))
        });
    }
}

fn bench_extract_frame_one_row_mutate(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        let mut tick = 0_u32;
        c.bench_function(&format!("extract_frame_one_row_mutate_{name}"), |b| {
            b.iter(|| {
                tick = tick.wrapping_add(1);
                mutate_single_row(&mut engine, tick);
                black_box(engine.extract_frame().expect("frame").stats.dirty_rows)
            })
        });
    }
}

fn bench_render_commands(c: &mut Criterion) {
    for scenario in prepared_scenarios() {
        let mut planner = PaintPlanner::default();
        let plan = planner
            .plan(scenario.surface, &scenario.frame, 16.0)
            .clone();
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());

        c.bench_function(&format!("render_commands_{}", scenario.name), |b| {
            b.iter(|| {
                black_box(
                    TerminalRenderFrame::from_plan(&plan, &text_contract)
                        .commands
                        .len(),
                )
            })
        });
    }
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
                ))
            })
        });
    }
}

fn bench_animated_agent_pipeline(c: &mut Criterion) {
    let mut engine = terminal_engine(160, 60);
    let surface = surface_for(160, 60);
    let mut planner = PaintPlanner::default();
    let mut tick = 0_u32;

    c.bench_function("animated_agent_update_extract_plan_render_160x60", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            write_agent_dashboard_frame(&mut engine, tick, 160, 60);
            let frame = engine.extract_frame().expect("frame");
            let plan = planner.plan(surface, frame, 16.0).clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            black_box(
                TerminalRenderFrame::from_plan(&plan, &text_contract)
                    .commands
                    .len(),
            )
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
                ))
            })
        },
    );
}

criterion_group!(
    name = benches;
    // These benches include WGPU preparation and full-frame terminal workloads that
    // vary on developer desktops under browser/GPU scheduler load.
    config = Criterion::default().noise_threshold(0.15);
    targets =
        bench_paint_plan,
        bench_extract_frame,
        bench_extract_frame_one_row_mutate,
        bench_render_commands,
        bench_wgpu_prepare,
        bench_animated_agent_pipeline,
        bench_animated_agent_pipeline_wgpu_prepare
);
criterion_main!(benches);
