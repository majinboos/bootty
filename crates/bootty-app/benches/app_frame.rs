use std::{collections::HashMap, hint::black_box, path::PathBuf, sync::Arc, time::Instant};

use anyhow::Result;
use bootty_app::{
    AppState, FrameInputs, ModalDialog, ViewportSnapshot,
    renderer::{RendererMetrics, TerminalFrameSource, TerminalWidget},
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        session_navigation::BindingSessionGroup,
        sidebar::build_binding_sidebar_items,
        space::{SpaceDraft, SpaceEditorIntent},
    },
};
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_extension::ModuleItem;
use bootty_mux::{
    RepaintHandle,
    controller::{BindingId, MuxScope, SpaceId},
    snapshot::{MuxPaneAnchor, MuxSession, MuxWindow},
};
use bootty_render::geometry::{TerminalGeometry, ViewTransform};
use bootty_render::terminal_text::TerminalTextConfig;
use bootty_terminal::{terminal_engine::TerminalEngine, terminal_frame::RenderFrame};
use bootty_ui::icons;
use bootty_workspace::SpaceMuxOverride;
use criterion::{Criterion, criterion_group, criterion_main};
use eframe::{egui, wgpu};

mod support;

const SIDEBAR_FRAME_SESSIONS: usize = 384;
const FRAME_RECT: egui::Rect = egui::Rect {
    min: egui::Pos2 { x: 0.0, y: 0.0 },
    max: egui::Pos2 {
        x: 1280.0,
        y: 900.0,
    },
};

struct BenchTerminal {
    engine: TerminalEngine,
}

impl BenchTerminal {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            engine: terminal_engine(cols, rows),
        }
    }

    fn write_agent_frame(&mut self, tick: u32, cols: u16, rows: u16) {
        write_agent_dashboard_frame(&mut self.engine, tick, cols, rows);
    }
}

impl TerminalFrameSource for BenchTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.engine.set_display_scale(display_scale);
        Ok(())
    }

    fn set_render_cell_metrics(
        &mut self,
        cell: bootty_render::geometry::CellMetrics,
    ) -> Result<()> {
        self.engine.set_render_cell_metrics(cell);
        Ok(())
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.engine.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(self.engine.extract_frame()?.clone()))
    }
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

fn app_state(sidebar: bool) -> AppState {
    let repaint: RepaintHandle = Arc::new(|| {});
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let mut config = BoottyConfig {
        config_path: std::env::temp_dir().join(format!("bootty-app-frame-bench-{unique}.toml")),
        ..BoottyConfig::default()
    };
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    config.chrome.sidebar = sidebar;
    AppState::new(config, support::backends(), repaint, None, None).expect("app state")
}

fn app_state_with_spaces(count: usize) -> AppState {
    let mut state = app_state(false);
    for index in 1..count {
        assert!(state.open_create_space_dialog_from_ui());
        assert!(matches!(
            state.modal_dialog(),
            Some(ModalDialog::SpaceEditor(_))
        ));
        let mux = SpaceMuxOverride::default();
        state.apply_space_editor_intent(SpaceEditorIntent::Save(SpaceDraft {
            space_id: None,
            name: format!("Benchmark Space {index}"),
            icon: "folder".to_owned(),
            color: [0x7a, 0xa2, 0xf7],
            tint_sidebar: false,
            backend: mux.backend,
            remote_source: mux.remote,
        }));
    }
    state
}

fn frame_inputs_at(
    now: Instant,
    events: Vec<egui::Event>,
    renderer_metrics: RendererMetrics,
) -> FrameInputs {
    FrameInputs {
        now,
        events,
        dropped_file_paths: Vec::<PathBuf>::new(),
        modifiers: egui::Modifiers::default(),
        hover_pos: Some(egui::Pos2::new(420.0, 240.0)),
        pressed_mouse_button: None,
        window_focused: true,
        viewport: ViewportSnapshot {
            fullscreen: false,
            maximized: false,
            content_height: FRAME_RECT.height(),
        },
        renderer_metrics,
        terminal_cell_width: 9.0,
        terminal_cell_height: 22.0,
        terminal_scale_factor: 1.0,
        terminal_view_transform: ViewTransform::IDENTITY,
    }
}

fn renderer_metrics(text_runs: usize, dirty_rows: usize) -> RendererMetrics {
    RendererMetrics {
        extract_total_us: 117,
        render_state_update_us: 22,
        frame_extraction_us: 95,
        paint_us: 80,
        cells: 160 * 60,
        chars: 160 * 60,
        dirty_rows,
        image_placements: 0,
        virtual_placements: 0,
        text_runs,
        cursor_blinking: false,
    }
}

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

fn sidebar_ui_frame(ui: &mut egui::Ui, group: &BindingSessionGroup) {
    let palette = bootty_ui::ThemePalette::default();
    let sidebar_rect = egui::Rect::from_min_size(FRAME_RECT.min, egui::vec2(280.0, 900.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(sidebar_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            let items = build_binding_sidebar_items(std::slice::from_ref(group));
            black_box(chrome::show_sidebar(
                ui,
                palette,
                sidebar_rect.height(),
                SidebarModel {
                    items: &items,
                    footer_items: &[],
                    session_count: group.sessions.len(),
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
        },
    );
}

fn status_ui_frame(ui: &mut egui::Ui, selected: Option<&str>) {
    let status_rect = egui::Rect::from_min_size(
        egui::Pos2::new(296.0, 0.0),
        egui::vec2(FRAME_RECT.width() - 296.0, 34.0),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(status_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            let item = ModuleItem {
                text: selected.unwrap_or("session").to_owned(),
                ..Default::default()
            };
            let segments = [chrome::ResolvedSegment {
                align: bootty_ui::status_layout::Align::Left,
                wrappable: false,
                source_slot: 0,
                items: vec![chrome::ResolvedItem {
                    item: &item,
                    icon: None,
                    fg: None,
                    bg: None,
                    stroke: None,
                }],
                ..Default::default()
            }];
            let layout = chrome::status_bar_layout(
                ui,
                status_rect,
                &segments,
                chrome::STATUS_EDGE_PAD,
                None,
            );
            chrome::show_status_bar(
                ui,
                bootty_ui::ThemePalette::default(),
                StatusBarModel {
                    layout: &layout,
                    tab_context: None,
                    background: bootty_ui::ThemePalette::default().base,
                    row_height: 30.0,
                    interaction_id: "status-bar-bench",
                },
            );
        },
    );
}

fn terminal_widget_frame(
    ui: &mut egui::Ui,
    terminal: &mut BenchTerminal,
    widget: &mut TerminalWidget,
) {
    let terminal_rect = egui::Rect::from_min_max(egui::Pos2::new(296.0, 34.0), FRAME_RECT.max);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(terminal_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            black_box(widget.show(ui, terminal).expect("terminal widget"));
        },
    );
}

fn write_agent_dashboard_frame(engine: &mut TerminalEngine, tick: u32, cols: u16, rows: u16) {
    let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"][(tick as usize) % 8];
    let progress = tick % 101;
    let width = cols.saturating_sub(2);

    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[48;2;17;24;39;38;2;192;202;245m {spinner} Bootty agent · app frame · {progress:>3}% · tokens 184,392 · jobs 7/9 {}\x1b[0m",
            " ".repeat(width.saturating_sub(78) as usize),
        )
        .as_bytes(),
    );

    for row in 1..rows {
        let row_u32 = u32::from(row);
        let color = 80 + row_u32 * 3 % 150;
        let glyphs = ["🥟", "", "█▓▒░", "╭─╮", "λ∑→←"][row as usize % 5];
        engine.write_vt(
            format!(
                "\x1b[{};1H\x1b[38;2;125;207;255m│ info\x1b[0m \
                 \x1b[38;2;{};{};230mframe\x1b[0m \
                 \x1b[48;5;236;38;5;{}mactive\x1b[0m \
                 \x1b[38;2;158;206;106m+ terminal update feeds egui frame\x1b[0m \
                 \x1b[38;5;{}m{glyphs}\x1b[0m {}",
                row + 1,
                color,
                255 - color,
                16 + row % 216,
                160 + row % 60,
                "trace=".repeat(8),
            )
            .as_bytes(),
        );
    }
}

/// Build a benchmark's state the first time criterion runs it, then keep it.
///
/// Criterion calls the routine it times once per sample, so a state built inside that routine is
/// rebuilt a hundred times over — and an `AppState` costs whole seconds, almost all of it opening
/// and closing the workspace database. Building it up front instead would charge every benchmark in
/// this group to whichever one you asked for. Neither is what the numbers are about: the frame being
/// measured already runs against a state it has updated before, so the first sample can build it and
/// the rest can measure against it. Rebuilding it per sample measures the same frame cost, so the
/// state that survives is the cheap way to measure it, not a different measurement.
fn warmed_state(slot: &mut Option<AppState>, build: impl FnOnce() -> AppState) -> &mut AppState {
    let state = slot.get_or_insert_with(build);
    // Hand the state back through a value the optimizer cannot see into. A state built as a local
    // right next to the loop that updates it lets a release build with fat LTO hoist the frame work
    // out of the loop entirely, which is how these benchmarks came to report a few hundred
    // nanoseconds for a frame that costs closer to a hundred microseconds.
    black_box(state)
}

fn bench_app_state_update(c: &mut Criterion) {
    let metrics = renderer_metrics(0, 0);
    let active_metrics = renderer_metrics(48, 3);

    let mut idle = None;
    c.bench_function("app_state_update_idle_frame", |b| {
        // Read the clock after fixture construction but outside Criterion's timed iterator. Each
        // sample measures one stable frame workload without charging clock acquisition or letting
        // periodic background work become due partway through the sample.
        let state = warmed_state(&mut idle, || app_state(false));
        let now = Instant::now();
        black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        })
    });

    let events = vec![egui::Event::PointerMoved(egui::Pos2::new(600.0, 400.0))];
    let mut active = None;
    c.bench_function("app_state_update_active_terminal_frame", |b| {
        let state = warmed_state(&mut active, || app_state(false));
        let now = Instant::now();
        black_box(state.update_frame(frame_inputs_at(now, events.clone(), active_metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, events.clone(), active_metrics)));
        })
    });

    let mut sidebar = None;
    c.bench_function("app_state_update_sidebar_status_frame", |b| {
        let state = warmed_state(&mut sidebar, || app_state(true));
        let now = Instant::now();
        black_box(state.update_frame(frame_inputs_at(now, Vec::new(), active_metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, Vec::new(), active_metrics)));
        })
    });

    for spaces in [8, 32] {
        let mut spaced = None;
        c.bench_function(
            &format!("app_state_update_idle_frame_{spaces}_spaces"),
            |b| {
                let state = warmed_state(&mut spaced, || app_state_with_spaces(spaces));
                let now = Instant::now();
                black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
                b.iter(|| {
                    black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
                })
            },
        );
    }
}

fn bench_egui_app_frames(c: &mut Criterion) {
    let sessions = sidebar_sessions(SIDEBAR_FRAME_SESSIONS);
    let selected = sessions
        .get(SIDEBAR_FRAME_SESSIONS / 2)
        .map(|session| session.id.clone());
    let group = BindingSessionGroup {
        scope: MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1)),
        label: "Native".to_owned(),
        sessions,
        selected_session: selected.clone(),
        active: true,
        can_return_to_last_session: false,
        display_names: HashMap::new(),
    };
    let context = egui::Context::default();
    icons::install_icon_fonts(&context);

    let mut terminal = BenchTerminal::new(109, 39);
    let mut widget = TerminalWidget::new(Some(wgpu::TextureFormat::Rgba8Unorm))
        .with_text_config(TerminalTextConfig::default());
    let mut tick = 0_u32;
    c.bench_function("egui_frame_terminal_active_109x39", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            terminal.write_agent_frame(tick, 109, 39);
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(FRAME_RECT),
                    events: vec![egui::Event::PointerMoved(egui::Pos2::new(600.0, 400.0))],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        terminal_widget_frame(ui, &mut terminal, &mut widget);
                    });
                },
            );
            black_box(output.shapes.len())
        })
    });

    c.bench_function("egui_frame_sidebar_status_384_sessions", |b| {
        b.iter(|| {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(FRAME_RECT),
                    events: vec![egui::Event::PointerMoved(egui::Pos2::new(42.0, 220.0))],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        sidebar_ui_frame(ui, black_box(&group));
                        status_ui_frame(ui, selected.as_deref());
                    });
                },
            );
            black_box(output.shapes.len())
        })
    });

    let mut combined_terminal = BenchTerminal::new(109, 39);
    let mut combined_widget = TerminalWidget::new(Some(wgpu::TextureFormat::Rgba8Unorm))
        .with_text_config(TerminalTextConfig::default());
    let mut combined_tick = 0_u32;
    c.bench_function(
        "egui_frame_terminal_sidebar_status_109x39_384_sessions",
        |b| {
            b.iter(|| {
                combined_tick = combined_tick.wrapping_add(1);
                combined_terminal.write_agent_frame(combined_tick, 109, 39);
                let output = context.run_ui(
                    egui::RawInput {
                        screen_rect: Some(FRAME_RECT),
                        events: vec![egui::Event::PointerMoved(egui::Pos2::new(600.0, 400.0))],
                        ..Default::default()
                    },
                    |ui| {
                        egui::CentralPanel::default().show(ui, |ui| {
                            sidebar_ui_frame(ui, black_box(&group));
                            status_ui_frame(ui, selected.as_deref());
                            terminal_widget_frame(ui, &mut combined_terminal, &mut combined_widget);
                        });
                    },
                );
                black_box(output.shapes.len())
            })
        },
    );
}

criterion_group!(
name = benches;
config = Criterion::default().noise_threshold(0.15);
targets =
    bench_app_state_update,
    bench_egui_app_frames
);
criterion_main!(benches);
