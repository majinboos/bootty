use std::{hint::black_box, path::PathBuf, sync::Arc, time::Instant};

use anyhow::Result;
use bootty_app::{
    app::{AppState, FrameInputs, ViewportSnapshot},
    config::{BoottyConfig, MultiplexerBackendConfig},
    geometry::{TerminalGeometry, ViewTransform},
    mux::{
        RepaintHandle,
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow},
    },
    renderer::{RendererMetrics, TerminalRenderSource, TerminalWidget},
    terminal::{RenderFrame, TerminalEngine},
    ui::{
        chrome::{self, SidebarModel, StatusBarModel},
        icons,
        sidebar::build_sidebar_items,
    },
};
use criterion::{Criterion, criterion_group, criterion_main};
use eframe::{egui, wgpu};

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

impl TerminalRenderSource for BenchTerminal {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.engine.resize(geometry)
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(self.engine.extract_frame()?.clone()))
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.engine.scroll_viewport_delta(delta);
        Ok(())
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
    AppState::new(config, repaint, None, None).expect("app state")
}

fn frame_inputs_at(
    now: Instant,
    events: Vec<egui::Event>,
    renderer_metrics: RendererMetrics,
) -> FrameInputs {
    FrameInputs {
        now,
        stable_dt_ms: 16.0,
        events,
        dropped_file_paths: Vec::<PathBuf>::new(),
        modifiers: egui::Modifiers::default(),
        hover_pos: Some(egui::Pos2::new(420.0, 240.0)),
        pressed_mouse_button: None,
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

fn sidebar_ui_frame(ui: &mut egui::Ui, sessions: &[MuxSession], selected: Option<&str>) {
    let palette = bootty_ui::ThemePalette::default();
    let sidebar_rect = egui::Rect::from_min_size(FRAME_RECT.min, egui::vec2(280.0, 900.0));
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(sidebar_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            let items = build_sidebar_items(sessions, selected);
            black_box(chrome::show_sidebar(
                ui,
                palette,
                sidebar_rect.height(),
                SidebarModel {
                    items: &items,
                    footer_items: &[],
                    session_count: sessions.len(),
                    has_sessions: !sessions.is_empty(),
                    title_visible: true,
                    reserve_titlebar_buttons: true,
                    title_icon: None,
                    top_inset: 0.0,
                    border_visible: true,
                    separator_visible: true,
                    focused: false,
                    hovered_session: None,
                    unfocused_dim: 0.0,
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
            let segments = [chrome::ResolvedSegment {
                align: bootty_app::config::SegmentAlign::Left,
                source_slot: 0,
                items: vec![chrome::ResolvedItem {
                    text: selected.unwrap_or("session").to_owned(),
                    ..Default::default()
                }],
            }];
            chrome::show_status_bar(
                ui,
                bootty_ui::ThemePalette::default(),
                StatusBarModel {
                    segments: &segments,
                    tab_context: None,
                    background: bootty_ui::ThemePalette::default().base,
                    left_padding: chrome::STATUS_EDGE_PAD,
                    row_height: 30.0,
                    notch_x: None,
                    tab_rows: 1,
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

fn bench_app_state_update(c: &mut Criterion) {
    let metrics = renderer_metrics(0, 0);
    c.bench_function("app_state_update_idle_frame", |b| {
        let mut state = app_state(false);
        let now = Instant::now();
        black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        })
    });

    c.bench_function("app_state_update_active_terminal_frame", |b| {
        let mut state = app_state(false);
        let now = Instant::now();
        let metrics = renderer_metrics(48, 3);
        let events = vec![egui::Event::PointerMoved(egui::Pos2::new(600.0, 400.0))];
        black_box(state.update_frame(frame_inputs_at(now, events.clone(), metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, events.clone(), metrics)));
        })
    });

    c.bench_function("app_state_update_sidebar_status_frame", |b| {
        let mut state = app_state(true);
        let now = Instant::now();
        let metrics = renderer_metrics(48, 3);
        black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        b.iter(|| {
            black_box(state.update_frame(frame_inputs_at(now, Vec::new(), metrics)));
        })
    });
}

fn bench_egui_app_frames(c: &mut Criterion) {
    let sessions = sidebar_sessions(SIDEBAR_FRAME_SESSIONS);
    let selected = sessions
        .get(SIDEBAR_FRAME_SESSIONS / 2)
        .map(|session| session.id.as_str());
    let context = egui::Context::default();
    icons::install_icon_fonts(&context);

    c.bench_function("egui_frame_terminal_active_109x39", |b| {
        let mut terminal = BenchTerminal::new(109, 39);
        let mut widget = TerminalWidget::new(Some(wgpu::TextureFormat::Rgba8Unorm))
            .with_text_config(bootty_app::terminal_text::TerminalTextConfig::default());
        let mut tick = 0_u32;
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
                        sidebar_ui_frame(ui, black_box(&sessions), selected);
                        status_ui_frame(ui, selected);
                    });
                },
            );
            black_box(output.shapes.len())
        })
    });

    c.bench_function(
        "egui_frame_terminal_sidebar_status_109x39_384_sessions",
        |b| {
            let mut terminal = BenchTerminal::new(109, 39);
            let mut widget = TerminalWidget::new(Some(wgpu::TextureFormat::Rgba8Unorm))
                .with_text_config(bootty_app::terminal_text::TerminalTextConfig::default());
            let mut tick = 0_u32;
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
                            sidebar_ui_frame(ui, black_box(&sessions), selected);
                            status_ui_frame(ui, selected);
                            terminal_widget_frame(ui, &mut terminal, &mut widget);
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
