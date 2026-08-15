use std::{collections::HashMap, hint::black_box};

mod paint_plan_fixtures;

use bootty_app::{
    config::{BoottyConfig, MultiplexerBackendConfig},
    geometry::ViewTransform,
    input_binding::BindingAction,
    input_binding_set::BindingSet,
    modifier_remap::ModifierRemapSet,
    mux::{
        controller::{BindingId, MuxScope, SpaceId},
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow},
    },
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
    ui::{
        chrome::{self, SidebarModel},
        session_navigation::BindingSessionGroup,
        session_picker::SessionPickerDialog,
        sidebar::build_binding_sidebar_items,
    },
};
use bootty_extension::{ModuleItem, PublishedSurfaceItem};
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_frame::FrameSelection,
    terminal_input_model::{
        KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseEncoderSize,
        MouseInput, TerminalKey,
    },
};
use bootty_ui::icons;
use bootty_winit::input::{
    InputSnapshot, WheelScrollState, terminal_input_commands_with_wheel_state,
};
use criterion::{Criterion, criterion_group, criterion_main};
use eframe::egui::{self, Pos2, Rect};
use paint_plan_fixtures::{
    mutate_single_row, prepared_scenarios, scenario_builders, surface_for, terminal_engine,
    write_agent_dashboard_frame,
};

const SIDEBAR_BENCH_SESSION_COUNTS: [usize; 3] = [24, 96, 384];

fn sidebar_sessions(count: usize) -> Vec<MuxSession> {
    (0..count)
        .map(|index| {
            let group = match index % 6 {
                0 => "agents",
                1 => "infra",
                2 => "app",
                3 => "research",
                4 => "review",
                _ => "ops",
            };
            let id = format!("${}", index + 1);
            let anchor = MuxPaneAnchor {
                session_id: id.clone(),
                pane_id: Some(format!("%{}", index + 10)),
                pane_pid: None,
                cwd: Some("/Users/luan/src/bootty".to_owned()),
                process: Some(
                    match index % 4 {
                        0 => "codex",
                        1 => "cargo",
                        2 => "nvim",
                        _ => "zsh",
                    }
                    .to_owned(),
                ),
            };
            MuxSession {
                id: id.clone(),
                name: format!("{group}/session-{index:03}"),
                active: index == 0,
                anchor: anchor.clone(),
                active_window_id: None,
                windows: (0..3)
                    .map(|window| MuxWindow {
                        id: format!("@{}:{window}", index + 1),
                        index: window,
                        name: format!("window-{window}"),
                        active: window == 0,
                        anchor: anchor.clone(),
                        panes: Vec::new(),
                        layout: None,
                        progress: None,
                    })
                    .collect(),
            }
        })
        .collect()
}

fn sidebar_group(sessions: Vec<MuxSession>, selected_session: String) -> BindingSessionGroup {
    BindingSessionGroup {
        scope: MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1)),
        label: "Native".to_owned(),
        sessions,
        selected_session: Some(selected_session),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    }
}

fn usage_footer_items(name: &str) -> Vec<ModuleItem> {
    vec![
        ModuleItem {
            text: format!("{name} 5h 90%"),
            icon: Some("openai".to_owned()),
            ..ModuleItem::default()
        },
        ModuleItem {
            text: format!("{name} 7d 73%"),
            icon: Some("rotate-ccw".to_owned()),
            ..ModuleItem::default()
        },
    ]
}

fn default_native_keybinds() -> Vec<String> {
    BoottyConfig::default()
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native)
}

fn key_input(key: TerminalKey, mods: KeyMods) -> KeyInput {
    KeyInput {
        key,
        mods,
        repeat: false,
        utf8: None,
        unshifted: None,
    }
}

fn binding_lookup_inputs() -> Vec<KeyInput> {
    vec![
        key_input(
            TerminalKey::Space,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::V,
            KeyMods {
                command: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::J,
            KeyMods {
                alt: true,
                ..Default::default()
            },
        ),
        key_input(
            TerminalKey::Tab,
            KeyMods {
                ctrl: true,
                ..Default::default()
            },
        ),
        KeyInput {
            key: TerminalKey::A,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("a"),
            unshifted: Some('a'),
        },
    ]
}

fn input_burst_events() -> Vec<egui::Event> {
    let keys = [
        egui::Key::C,
        egui::Key::D,
        egui::Key::J,
        egui::Key::K,
        egui::Key::ArrowUp,
        egui::Key::ArrowDown,
    ];
    let mut events = Vec::with_capacity(320);
    for index in 0..64 {
        let pos = Pos2::new(24.0 + (index % 80) as f32, 40.0 + (index % 36) as f32);
        events.push(egui::Event::Text(format!("input-{index:03}")));
        events.push(egui::Event::Key {
            key: keys[index % keys.len()],
            physical_key: None,
            pressed: true,
            repeat: index % 5 == 0,
            modifiers: egui::Modifiers {
                ctrl: index % 2 == 0,
                alt: index % 3 == 0,
                shift: index % 7 == 0,
                ..Default::default()
            },
        });
        events.push(egui::Event::PointerMoved(pos));
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: index % 2 == 0,
            modifiers: egui::Modifiers::default(),
        });
        events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 11.0 + (index % 3) as f32),
            modifiers: egui::Modifiers::default(),
            phase: egui::TouchPhase::Move,
        });
    }
    events
}

fn input_snapshot(events: Vec<egui::Event>) -> InputSnapshot {
    InputSnapshot {
        events,
        modifiers: egui::Modifiers::default(),
        modifier_sides: Default::default(),
        hover_pos: Some(Pos2::new(64.0, 64.0)),
        pressed_mouse_button: Some(MouseButton::Left),
        surface: Some(surface_for(120, 40)),
        mouse_exclusion: Some(Rect::from_min_max(
            Pos2::new(2_000.0, 2_000.0),
            Pos2::new(2_100.0, 2_100.0),
        )),
        view: ViewTransform::IDENTITY,
    }
}

fn encode_mouse_input() -> MouseInput {
    MouseInput {
        action: MouseAction::Press,
        button: Some(MouseButton::Left),
        mods: KeyMods {
            ctrl: true,
            ..Default::default()
        },
        x: 42.0,
        y: 84.0,
        size: MouseEncoderSize {
            screen_width: 1080,
            screen_height: 880,
            cell_width: 9,
            cell_height: 22,
            padding_left: 10,
            padding_top: 10,
            padding_right: 10,
            padding_bottom: 10,
        },
    }
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

/// Contrasts planning a fully dirty frame against a frame where only a single
/// row changed.
///
fn bench_paint_plan_overlay_scaling(c: &mut Criterion) {
    let scenario = prepared_scenarios()
        .into_iter()
        .find(|scenario| scenario.name == "complex_shell_180x80")
        .expect("complex shell scenario");

    for matches in [0, 64, 1_024] {
        let mut frame = scenario.frame.clone();
        frame.search_matches = (0..matches)
            .map(|index| FrameSelection {
                row: (index % usize::from(frame.rows)) as u16,
                start_col: (index * 7 % usize::from(frame.cols)) as u16,
                end_col: ((index * 7 % usize::from(frame.cols)) + 4)
                    .min(usize::from(frame.cols) - 1) as u16,
            })
            .collect();
        let mut planner = PaintPlanner::default();
        c.bench_function(&format!("paint_plan_overlay_matches_{matches}"), |b| {
            b.iter(|| {
                black_box(
                    planner
                        .plan(scenario.surface, black_box(&frame), 16.0)
                        .text_runs
                        .len(),
                )
            })
        });
    }
}
/// NOTE: bootty currently reports `Dirty::Full` with every row dirty for any
/// edit (see the `dirty_tracking` characterization test in bootty-terminal), so
/// the `one_row` arm exercises a full-dirty frame and tracks `full` today. It
/// becomes a real incremental-vs-full contrast only once localized edits report
/// `Dirty::Partial`. Kept as standing measurement scaffolding for that work.
fn bench_paint_plan_dirty_scope(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let (cols, rows) = {
            let mut engine = builder();
            engine.extract_frame().expect("frame");
            engine.grid_size()
        };
        let surface = surface_for(cols, rows);

        let full_frame = {
            let mut engine = builder();
            engine.extract_frame().expect("frame").clone()
        };

        let one_row_frame = {
            let mut engine = builder();
            engine.extract_frame().expect("frame");
            mutate_single_row(&mut engine, 1);
            engine.extract_frame().expect("frame").clone()
        };

        let mut group = c.benchmark_group(format!("paint_plan_dirty_{name}"));
        group.bench_function("full", |b| {
            let mut planner = PaintPlanner::default();
            b.iter(|| black_box(planner.plan(surface, &full_frame, 16.0).text_runs.len()))
        });
        group.bench_function("one_row", |b| {
            let mut planner = PaintPlanner::default();
            planner.plan(surface, &full_frame, 16.0);
            b.iter(|| black_box(planner.plan(surface, &one_row_frame, 16.0).text_runs.len()))
        });
        group.finish();
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

// Rewrite the entire screen, forcing a Dirty::Full extraction (which clears the row
// cache). Used to set up the cold-cache state the next localized edit must extract from.
fn full_repaint(engine: &mut TerminalEngine, tick: u32) {
    let rows = engine.grid_size().1;
    engine.write_vt(b"\x1b[2J");
    for row in 1..=rows {
        engine.write_vt(
            format!("\x1b[{row};1Hfull repaint {tick:08x} row {row:03} content abcdef 0123")
                .as_bytes(),
        );
    }
}

// The full-extraction cost itself (every row dirty). Unifying the full path through the
// row cache adds a row-cache assemble pass here in exchange for keeping the cache warm so
// the *next* edit is incremental — this bench quantifies that full-frame side of the trade.
fn bench_extract_full_redraw(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        let mut tick = 0_u32;
        c.bench_function(&format!("extract_full_redraw_{name}"), |b| {
            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    tick = tick.wrapping_add(1);
                    full_repaint(&mut engine, tick);
                    let start = std::time::Instant::now();
                    black_box(engine.extract_frame().expect("frame").stats.cells);
                    total += start.elapsed();
                }
                total
            })
        });
    }
}

// The §5.4 cold-cache cliff: the first localized edit after a full redraw. The full
// redraw takes the Dirty::Full path, which clears the row cache, so the following
// single-row edit can't extract incrementally and re-reads the whole grid. `iter_custom`
// times ONLY that edit's extraction, excluding the full-redraw setup.
fn bench_extract_edit_after_full_redraw(c: &mut Criterion) {
    for (name, builder) in scenario_builders() {
        let mut engine = builder();
        for tick in 0..40 {
            mutate_single_row(&mut engine, tick);
            black_box(engine.extract_frame().expect("frame").stats.dirty_rows);
        }
        let mut tick = 1_000_u32;
        c.bench_function(&format!("extract_edit_after_full_redraw_{name}"), |b| {
            b.iter_custom(|iters| {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    tick = tick.wrapping_add(1);
                    full_repaint(&mut engine, tick);
                    black_box(engine.extract_frame().expect("frame").stats.dirty_rows);
                    mutate_single_row(&mut engine, tick);
                    let start = std::time::Instant::now();
                    black_box(engine.extract_frame().expect("frame").stats.dirty_rows);
                    total += start.elapsed();
                }
                total
            })
        });
    }
}

fn bench_terminal_write_vt(c: &mut Criterion) {
    let mut plain = terminal_engine(120, 40);
    c.bench_function("terminal_write_vt_plain_carriage_return", |b| {
        b.iter(|| {
            plain.write_vt(black_box(b"plain terminal output\r"));
            black_box(plain.grid_size())
        })
    });

    let mut ansi = terminal_engine(120, 40);
    c.bench_function("terminal_write_vt_ansi_csi_color", |b| {
        b.iter(|| {
            ansi.write_vt(black_box(
                b"\x1b[1;1H\x1b[38;2;1;2;3mcolored terminal output\x1b[0m",
            ));
            black_box(ansi.grid_size())
        })
    });
}

fn bench_terminal_input_pipeline(c: &mut Criterion) {
    let events = input_burst_events();
    let remaps = ModifierRemapSet::default();
    let mut wheel_state = WheelScrollState::default();

    c.bench_function("terminal_input_events_to_commands_mixed_burst", |b| {
        b.iter(|| {
            let snapshot = input_snapshot(black_box(events.clone()));
            black_box(
                terminal_input_commands_with_wheel_state(
                    snapshot,
                    black_box(&remaps),
                    black_box(MacosOptionAsAlt::Both),
                    black_box(&mut wheel_state),
                )
                .len(),
            )
        })
    });

    let keybinds = default_native_keybinds();
    c.bench_function("keybinding_parse_default_native_config", |b| {
        b.iter(|| {
            let mut set = BindingSet::default();
            for entry in &keybinds {
                set.parse_and_put(black_box(entry.as_str()))
                    .expect("default keybind parses");
            }
            black_box(set.get_trigger(black_box(&BindingAction::NewTab)).is_some())
        })
    });

    let mut binding_set = BindingSet::default();
    for entry in default_native_keybinds() {
        binding_set
            .parse_and_put(&entry)
            .expect("default keybind parses");
    }
    let inputs = binding_lookup_inputs();
    c.bench_function("keybinding_lookup_default_native_config", |b| {
        b.iter(|| {
            let mut hits = 0_u8;
            for input in &inputs {
                if binding_set.get_event(black_box(*input)).is_some() {
                    hits = hits.saturating_add(1);
                }
            }
            black_box(hits)
        })
    });

    let mut engine = terminal_engine(120, 40);
    let keys = binding_lookup_inputs();
    let mouse = encode_mouse_input();
    let mut out = Vec::with_capacity(4096);
    c.bench_function("terminal_encode_keys_mouse_paste_burst", |b| {
        b.iter(|| {
            for key in &keys {
                out.clear();
                engine
                    .encode_key_to_vec(black_box(*key), black_box(&mut out))
                    .expect("encode key");
                black_box(out.len());
            }
            out.clear();
            engine
                .encode_mouse_to_vec(black_box(mouse), black_box(&mut out))
                .expect("encode mouse");
            black_box(out.len());
            out.clear();
            engine
                .encode_paste_to_vec(
                    black_box("paste burst with unicode 🥟"),
                    black_box(&mut out),
                )
                .expect("encode paste");
            black_box(out.len())
        })
    });
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

fn bench_sidebar_items(c: &mut Criterion) {
    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        let selected = sessions
            .get(count / 2)
            .map(|session| session.id.clone())
            .unwrap_or_else(|| "$1".to_owned());
        let groups = [sidebar_group(sessions, selected)];
        c.bench_function(&format!("sidebar_items_{count}_rich_sessions"), |b| {
            b.iter(|| black_box(build_binding_sidebar_items(black_box(&groups))).len())
        });
    }
}

fn bench_sidebar_ui(c: &mut Criterion) {
    for count in SIDEBAR_BENCH_SESSION_COUNTS {
        let sessions = sidebar_sessions(count);
        let selected = sessions
            .get(count / 2)
            .map(|session| session.id.clone())
            .unwrap_or_else(|| "$1".to_owned());
        let group = sidebar_group(sessions, selected);
        let items = build_binding_sidebar_items(std::slice::from_ref(&group));
        let context = egui::Context::default();
        icons::install_icon_fonts(&context);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(280.0, 900.0));

        c.bench_function(&format!("sidebar_ui_{count}_rich_sessions"), |b| {
            b.iter(|| {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen_rect),
                        events: vec![egui::Event::PointerMoved(Pos2::new(32.0, 180.0))],
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            black_box(chrome::show_sidebar(
                                ui,
                                bootty_ui::ThemePalette::default(),
                                900.0,
                                SidebarModel {
                                    items: black_box(&items),
                                    footer_items: &[],
                                    session_count: group.sessions.len(),
                                    has_sessions: !group.sessions.is_empty(),
                                    title_visible: true,
                                    reserve_titlebar_buttons: true,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: true,
                                    border_bottom: true,
                                    separator_visible: true,
                                    focused: false,
                                    hovered_session: None,
                                    fullscreen: false,
                                    hover_override: None,
                                    current_override: None,
                                    border_override: None,
                                },
                            ));
                        });
                    },
                );
                black_box(output.shapes.len())
            })
        });
    }
}

fn bench_sidebar_ui_usage_footer(c: &mut Criterion) {
    let count = 96;
    let sessions = sidebar_sessions(count);
    let selected = sessions
        .get(count / 2)
        .map(|session| session.id.clone())
        .unwrap_or_else(|| "$1".to_owned());
    let group = sidebar_group(sessions, selected);
    let items = build_binding_sidebar_items(std::slice::from_ref(&group));
    let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(280.0, 900.0));

    for name in ["plain_usage_footer", "compact_usage_footer"] {
        let footer_items = usage_footer_items(name)
            .into_iter()
            .map(|item| PublishedSurfaceItem {
                module: "benchmark.luau".to_owned(),
                generation: 1,
                surface: name.to_owned(),
                item,
            })
            .collect::<Vec<_>>();
        let context = egui::Context::default();
        icons::install_icon_fonts(&context);
        c.bench_function(&format!("sidebar_ui_96_rich_sessions_{name}"), |b| {
            b.iter(|| {
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(screen_rect),
                        events: vec![egui::Event::PointerMoved(Pos2::new(32.0, 180.0))],
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            black_box(chrome::show_sidebar(
                                ui,
                                bootty_ui::ThemePalette::default(),
                                900.0,
                                SidebarModel {
                                    items: black_box(&items),
                                    footer_items: black_box(&footer_items),
                                    session_count: group.sessions.len(),
                                    has_sessions: !group.sessions.is_empty(),
                                    title_visible: true,
                                    reserve_titlebar_buttons: true,
                                    title_icon: None,
                                    top_inset: 0.0,
                                    border_visible: true,
                                    border_bottom: true,
                                    separator_visible: true,
                                    focused: false,
                                    hovered_session: None,
                                    fullscreen: false,
                                    hover_override: None,
                                    current_override: None,
                                    border_override: None,
                                },
                            ));
                        });
                    },
                );
                black_box(output.shapes.len())
            })
        });
    }
}

fn bench_session_picker_ui(c: &mut Criterion) {
    let sessions = sidebar_sessions(384);
    let selected = sessions
        .get(sessions.len() / 2)
        .map(|session| session.id.clone())
        .unwrap_or_else(|| "$1".to_owned());
    let groups = [BindingSessionGroup {
        scope: MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1)),
        label: "Default Binding".to_owned(),
        sessions,
        selected_session: Some(selected),
        active: true,
        can_return_to_last_session: false,
        display_names: std::collections::HashMap::new(),
    }];
    let context = egui::Context::default();
    icons::install_icon_fonts(&context);
    let theme = bootty_ui::Theme::new(bootty_ui::ThemePalette::default());
    let screen_rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 900.0));

    c.bench_function("session_picker_ui_384_sessions_unfiltered", |b| {
        b.iter(|| {
            let mut dialog = SessionPickerDialog::open();
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    events: vec![egui::Event::PointerMoved(Pos2::new(600.0, 450.0))],
                    ..Default::default()
                },
                |ui| {
                    black_box(dialog.show(ui.ctx(), black_box(theme), black_box(&groups)));
                },
            );
            black_box(output.shapes.len())
        })
    });
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

criterion_group!(
name = benches;
// These benches cover CPU planning, terminal extraction, input routing,
// render command building, and egui chrome. WGPU preparation lives in paint_plan_wgpu.
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_paint_plan,
    bench_paint_plan_overlay_scaling,
    bench_paint_plan_dirty_scope,
    bench_extract_frame,
    bench_extract_frame_one_row_mutate,
    bench_extract_edit_after_full_redraw,
    bench_extract_full_redraw,
    bench_terminal_write_vt,
    bench_terminal_input_pipeline,
    bench_render_commands,
    bench_sidebar_items,
    bench_sidebar_ui,
    bench_sidebar_ui_usage_footer,
    bench_session_picker_ui,
    bench_animated_agent_pipeline,
);
criterion_main!(benches);
