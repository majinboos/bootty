use std::hint::black_box;

use bootty_app::{
    direct_input::{
        ModifierSideState, direct_key_input_from_winit_code, suppress_egui_events_for_direct_input,
    },
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_input_model::{KeyInput, KeyMods, TerminalKey},
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use winit::keyboard::{KeyCode, ModifiersState};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum EchoCase {
    ShellPrompt,
    RawEcho,
    ReadlineEdit,
    ZshPrompt,
    NeovimInsert,
    TmuxEcho,
    SshRemoteEcho,
    UnderRedraw,
    UnderFlood,
    FirstKeyAfterIdle,
    KeyRepeatBurst,
}

impl EchoCase {
    const fn name(self) -> &'static str {
        match self {
            Self::ShellPrompt => "shell_prompt",
            Self::RawEcho => "raw_echo",
            Self::ReadlineEdit => "readline_edit",
            Self::ZshPrompt => "zsh_prompt",
            Self::NeovimInsert => "neovim_insert",
            Self::TmuxEcho => "tmux_echo",
            Self::SshRemoteEcho => "ssh_remote_echo",
            Self::UnderRedraw => "under_redraw",
            Self::UnderFlood => "under_flood",
            Self::FirstKeyAfterIdle => "first_key_after_idle",
            Self::KeyRepeatBurst => "key_repeat_burst",
        }
    }
}

#[derive(Default)]
struct LatencyStats {
    events: usize,
    encoded_bytes: usize,
    render_commands: usize,
    cells: usize,
    chars: usize,
    backlog_bytes: usize,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
    dropped_events: usize,
    hash: u64,
}

impl LatencyStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.events as u64
            ^ (self.encoded_bytes as u64).rotate_left(7)
            ^ (self.render_commands as u64).rotate_left(13)
            ^ (self.cells as u64).rotate_left(19)
            ^ (self.chars as u64).rotate_left(29)
            ^ (self.backlog_bytes as u64).rotate_left(37)
            ^ self.p50_ns.rotate_left(43)
            ^ self.p95_ns.rotate_left(47)
            ^ self.p99_ns.rotate_left(53)
            ^ self.max_ns.rotate_left(59)
            ^ (self.dropped_events as u64).rotate_left(61)
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

fn key_input(ch: char, repeat: bool) -> KeyInput {
    let (key, utf8) = match ch.to_ascii_lowercase() {
        'a' => (TerminalKey::A, "a"),
        'b' => (TerminalKey::B, "b"),
        'c' => (TerminalKey::C, "c"),
        'd' => (TerminalKey::D, "d"),
        'e' => (TerminalKey::E, "e"),
        'f' => (TerminalKey::F, "f"),
        'g' => (TerminalKey::G, "g"),
        'h' => (TerminalKey::H, "h"),
        'i' => (TerminalKey::I, "i"),
        'j' => (TerminalKey::J, "j"),
        'k' => (TerminalKey::K, "k"),
        'l' => (TerminalKey::L, "l"),
        'm' => (TerminalKey::M, "m"),
        'n' => (TerminalKey::N, "n"),
        'o' => (TerminalKey::O, "o"),
        'p' => (TerminalKey::P, "p"),
        'q' => (TerminalKey::Q, "q"),
        'r' => (TerminalKey::R, "r"),
        's' => (TerminalKey::S, "s"),
        't' => (TerminalKey::T, "t"),
        'u' => (TerminalKey::U, "u"),
        'v' => (TerminalKey::V, "v"),
        'w' => (TerminalKey::W, "w"),
        'x' => (TerminalKey::X, "x"),
        'y' => (TerminalKey::Y, "y"),
        'z' => (TerminalKey::Z, "z"),
        ' ' => (TerminalKey::Space, " "),
        _ => (TerminalKey::A, "a"),
    };
    KeyInput {
        key,
        mods: KeyMods::default(),
        repeat,
        utf8: Some(utf8),
        unshifted: Some(ch.to_ascii_lowercase()),
    }
}

fn direct_input_burst(repeats: usize) -> Vec<bootty_app::direct_input::DirectKeyInput> {
    (0..repeats)
        .map(|index| {
            direct_key_input_from_winit_code(
                if index % 2 == 0 {
                    KeyCode::Numpad1
                } else {
                    KeyCode::Numpad2
                },
                ModifiersState::empty(),
                ModifierSideState::default(),
                index > 0,
            )
            .expect("numpad key maps to direct input")
        })
        .collect()
}

fn egui_events_for_direct_burst(repeats: usize) -> Vec<eframe::egui::Event> {
    let mut events = Vec::with_capacity(repeats * 3 + 8);
    for index in 0..repeats {
        let (key, text) = if index % 2 == 0 {
            (eframe::egui::Key::Num1, "1")
        } else {
            (eframe::egui::Key::Num2, "2")
        };
        events.push(eframe::egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: index > 0,
            modifiers: eframe::egui::Modifiers::default(),
        });
        events.push(eframe::egui::Event::Text(text.to_owned()));
        events.push(eframe::egui::Event::PointerMoved(eframe::egui::pos2(
            index as f32,
            (index % 17) as f32,
        )));
    }
    events
}

fn seeded_engine(case: EchoCase) -> TerminalEngine {
    let mut engine = TerminalEngine::new_with_scrollback(GEOMETRY, Default::default(), 8_000_000)
        .expect("terminal engine");
    match case {
        EchoCase::ShellPrompt | EchoCase::FirstKeyAfterIdle | EchoCase::KeyRepeatBurst => {
            engine.write_vt(b"$ ");
        }
        EchoCase::RawEcho => engine.write_vt(b"raw-mode> "),
        EchoCase::ReadlineEdit => engine.write_vt(b"$ cargo test --workspace"),
        EchoCase::ZshPrompt => engine.write_vt(b"\x1b[38;5;81m~/src/bootty\x1b[0m main * % "),
        EchoCase::NeovimInsert => {
            engine.write_vt(b"\x1b[?1049h\x1b[2J\x1b[H-- INSERT --\x1b[2;1Hfn main() {\r\n}");
        }
        EchoCase::TmuxEcho => engine.write_vt(b"\x1b[48;5;236mtmux:0:zsh\x1b[0m\r\n$ "),
        EchoCase::SshRemoteEcho => engine.write_vt(b"remote@example:~$ "),
        EchoCase::UnderRedraw => write_dashboard(&mut engine, 0),
        EchoCase::UnderFlood => {
            for row in 0..2_000 {
                engine
                    .write_vt(format!("flood backlog {row:05} {}\r\n", "x".repeat(96)).as_bytes());
            }
            engine.write_vt(b"$ ");
        }
    }
    engine
}

fn write_dashboard(engine: &mut TerminalEngine, tick: u32) {
    engine.write_vt(b"\x1b[H");
    for row in 1..=GEOMETRY.rows {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;2;{};{};220mredraw {tick:06} row {row:02}\x1b[0m {}",
                (tick + u32::from(row)) % 255,
                (tick * 3 + u32::from(row)) % 255,
                "dashboard ".repeat(14)
            )
            .as_bytes(),
        );
    }
}

fn echo_payload(case: EchoCase, ch: char, tick: u32) -> Vec<u8> {
    match case {
        EchoCase::ShellPrompt
        | EchoCase::RawEcho
        | EchoCase::TmuxEcho
        | EchoCase::SshRemoteEcho => ch.to_string().into_bytes(),
        EchoCase::ReadlineEdit => format!("\x1b[2K\r$ cargo test --workspace {ch}").into_bytes(),
        EchoCase::ZshPrompt => {
            format!("\x1b[2K\r\x1b[38;5;81m~/src/bootty\x1b[0m main * % {ch}").into_bytes()
        }
        EchoCase::NeovimInsert => format!("\x1b[2;{}H{ch}", 13 + tick % 40).into_bytes(),
        EchoCase::UnderRedraw => {
            let mut payload = Vec::new();
            write_dashboard_bytes(&mut payload, tick);
            payload.extend_from_slice(ch.to_string().as_bytes());
            payload
        }
        EchoCase::UnderFlood => {
            let mut payload = Vec::new();
            for row in 0..64 {
                payload.extend_from_slice(
                    format!("\r\nflood-latency {tick:04}-{row:02} {}", "x".repeat(64)).as_bytes(),
                );
            }
            payload.extend_from_slice(ch.to_string().as_bytes());
            payload
        }
        EchoCase::FirstKeyAfterIdle => ch.to_string().into_bytes(),
        EchoCase::KeyRepeatBurst => ch.to_string().into_bytes(),
    }
}

fn write_dashboard_bytes(out: &mut Vec<u8>, tick: u32) {
    out.extend_from_slice(b"\x1b[H");
    for row in 1..=GEOMETRY.rows {
        out.extend_from_slice(
            format!(
                "\x1b[{row};1H\x1b[38;2;{};{};220mredraw {tick:06} row {row:02}\x1b[0m {}",
                (tick + u32::from(row)) % 255,
                (tick * 3 + u32::from(row)) % 255,
                "dashboard ".repeat(14)
            )
            .as_bytes(),
        );
    }
}

fn run_input_case(case: EchoCase, events: u32) -> u64 {
    let mut engine = seeded_engine(case);
    let mut planner = PaintPlanner::default();
    let surface = surface_for(GEOMETRY);
    let mut encoded = Vec::new();
    let mut stats = LatencyStats::default();
    let mut latencies = Vec::with_capacity(events as usize);
    let text_contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    if matches!(case, EchoCase::FirstKeyAfterIdle) {
        let _ = engine.extract_frame().expect("idle frame");
        let _ = engine.extract_frame().expect("clean idle frame");
    }

    for tick in 0..events {
        let repeat = matches!(case, EchoCase::KeyRepeatBurst) && tick > 0;
        let ch = (b'a' + (tick % 26) as u8) as char;
        let input = key_input(ch, repeat);
        let start = std::time::Instant::now();
        engine
            .encode_key_to_vec(input, &mut encoded)
            .expect("encode key");
        let payload = echo_payload(case, ch, tick);
        stats.encoded_bytes += encoded.len();
        stats.backlog_bytes += payload.len().saturating_sub(1);
        engine.write_vt(&payload);
        let frame = engine.extract_frame().expect("input latency frame");
        let plan = planner.plan(surface, frame, 16.0).clone();
        let render_frame = TerminalRenderFrame::from_plan(&plan, &text_contract);
        let elapsed_ns = start.elapsed().as_nanos() as u64;
        latencies.push(elapsed_ns);

        stats.events += 1;
        stats.render_commands += render_frame.commands.len();
        stats.cells += frame.stats.cells;
        stats.chars += frame.stats.chars;
        stats.hash ^= frame
            .text
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
                (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
            });
    }

    latencies.sort_unstable();
    let percentile = |numerator: usize, denominator: usize| -> u64 {
        let index = ((latencies.len().saturating_sub(1)) * numerator) / denominator;
        latencies[index]
    };
    stats.p50_ns = percentile(50, 100);
    stats.p95_ns = percentile(95, 100);
    stats.p99_ns = percentile(99, 100);
    stats.max_ns = percentile(100, 100);
    assert_eq!(stats.events, events as usize);
    stats.checksum()
}

fn bench_internal_input_cases(c: &mut Criterion) {
    let cases = [
        EchoCase::ShellPrompt,
        EchoCase::RawEcho,
        EchoCase::ReadlineEdit,
        EchoCase::ZshPrompt,
        EchoCase::NeovimInsert,
        EchoCase::TmuxEcho,
        EchoCase::SshRemoteEcho,
        EchoCase::UnderRedraw,
        EchoCase::UnderFlood,
        EchoCase::FirstKeyAfterIdle,
        EchoCase::KeyRepeatBurst,
    ];
    for case in cases {
        c.bench_function(&format!("input_latency_internal_{}", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| black_box(run_input_case(case, 64)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_direct_input_suppression(c: &mut Criterion) {
    for repeats in [16_usize, 256] {
        c.bench_function(
            &format!("input_latency_direct_suppression_{repeats}_keys"),
            |b| {
                b.iter_batched(
                    || {
                        (
                            egui_events_for_direct_burst(repeats),
                            direct_input_burst(repeats),
                        )
                    },
                    |(mut events, direct_inputs)| {
                        suppress_egui_events_for_direct_input(&mut events, &direct_inputs);
                        black_box(events.len())
                    },
                    BatchSize::SmallInput,
                )
            },
        );
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_internal_input_cases, bench_direct_input_suppression,
}
criterion_main!(benches);
