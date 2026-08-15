use std::{fs, hint::black_box, path::PathBuf};

use bootty_app::{
    config::{
        BoottyConfig, MultiplexerBackendConfig, builtin_theme_names, load_config_from_path,
        resolve_theme, write_font_size_preference,
    },
    input::resolve_modifier_remaps,
    input_binding_set::BindingSet,
};
use bootty_workspace::{SessionOrderStore, WorkspaceRepository};
use criterion::{Criterion, criterion_group, criterion_main};

struct BenchDir {
    root: PathBuf,
}

impl BenchDir {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "bootty-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("create benchmark temp dir");
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for BenchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct ConfigFixture {
    _dir: BenchDir,
    config_path: PathBuf,
    config_dir: PathBuf,
}

fn config_fixture() -> ConfigFixture {
    let dir = BenchDir::new("startup-config");
    let themes_dir = dir.path("themes");
    fs::create_dir_all(&themes_dir).expect("create themes dir");
    fs::write(
        dir.path("base.toml"),
        r##"
[colors]
background = "#1a1b26"
foreground = "#c0caf5"
cursor = "#c0caf5"
selection-background = "#33467c"
palette = ["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]
palette-generate = true
palette-harmonious = true

[multiplexer]
backend = "native"
"##,
    )
    .expect("write base config");
    fs::write(
        themes_dir.join("bench-theme.toml"),
        r##"
[metadata]
name = "Bench Theme"
source = "benchmark fixture"
license = "MIT"

[colors]
background = "#101014"
foreground = "#eeeeee"
cursor = "#ffffff"
cursor-text = "#000000"
palette = ["#000000", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#bbbbbb"]
"##,
    )
    .expect("write theme");
    let config_path = dir.path("config.toml");
    fs::write(
        &config_path,
        r#"
version = 1
theme = "bench-theme"
include = ["base.toml", "?missing.toml"]

[font]
family = ["JetBrains Mono", "Apple Color Emoji"]
size = 15.0
cell-width = 9.0
cell-height = 22.0
baseline-adjustment = 0.0
underline-position = 1.0
underline-thickness = 1.0

[chrome]
sidebar = true
status-bar = true
sidebar-width = 300.0
status-height = 30.0
gap = 1.0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.08

[input]
modifier-remap = ["right_alt=alt", "right_shift=shift"]
macos-option-as-alt = "both"
keybind = ["cmd+p=session_picker", "cmd+n=new_mux_session", "performable:cmd+v=paste_from_clipboard"]
sidebar-keybind = ["j=next_session", "k=previous_session", "Enter=activate_session"]

[input.backend-keybind]
native = ["ctrl+space>c=new_tab", "ctrl+space>v=split_right", "alt+j=select_pane:down"]
tmux = ["alt+j=esc:J", "cmd+c=csi:72~"]
zellij = ["alt+n=next_tab"]

[session]
shell = "/bin/zsh"
working-directory = "/tmp"
term = "xterm-bootty"
colorterm = "truecolor"
env = [{ name = "BOOTTY_BENCH", value = "1" }]

[diagnostics]
stability-trace = "/tmp/bootty-stability.csv"

[window]
title = "Bootty Benchmark"
width = 1200.0
height = 900.0
fullscreen = "disabled"
window-decoration = "client"
macos-titlebar-style = "transparent"
"#,
    )
    .expect("write config");
    ConfigFixture {
        config_dir: dir.root.clone(),
        _dir: dir,
        config_path,
    }
}

fn session_names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("group-{}/session-{index:03}", index % 12))
        .collect()
}

fn session_order(config_path: &std::path::Path) -> SessionOrderStore {
    let (_, snapshot) = WorkspaceRepository::open(config_path).expect("workspace repository");
    snapshot.spaces()[0].bindings()[0].session_order().clone()
}

fn parse_keybinds(config: &BoottyConfig) -> usize {
    let mut set = BindingSet::default();
    for entry in config
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native)
    {
        set.parse_and_put(&entry).expect("keybind parses");
    }
    set.format_entries().len()
}

fn bench_config_load(c: &mut Criterion) {
    let fixture = config_fixture();
    c.bench_function("startup_config_load_includes_theme_keybinds", |b| {
        b.iter(|| {
            black_box(
                load_config_from_path(black_box(&fixture.config_path))
                    .expect("load benchmark config"),
            )
        })
    });

    let names = builtin_theme_names().collect::<Vec<_>>();
    c.bench_function("startup_config_resolve_builtin_themes", |b| {
        b.iter(|| {
            black_box(
                names
                    .iter()
                    .map(|name| {
                        resolve_theme(black_box(name), black_box(&fixture.config_dir))
                            .expect("resolve built-in theme")
                            .colors
                            .palette
                            .len()
                    })
                    .sum::<usize>(),
            )
        })
    });

    let config = load_config_from_path(&fixture.config_path).expect("load benchmark config");
    c.bench_function("startup_config_modifier_and_keybind_build", |b| {
        b.iter(|| {
            let remaps =
                resolve_modifier_remaps(&config.input.modifier_remap).expect("modifier remaps");
            black_box((remaps, parse_keybinds(black_box(&config))))
        })
    });

    let font_fixture = config_fixture();
    let mut tick = 0_u32;
    c.bench_function("startup_config_write_font_size_preference", |b| {
        b.iter(|| {
            tick = tick.wrapping_add(1);
            let size = 12.0 + (tick % 8) as f32;
            write_font_size_preference(black_box(&font_fixture.config_path), size)
                .expect("write font size preference");
            black_box(size)
        })
    });
}

fn bench_session_order(c: &mut Criterion) {
    let sessions = session_names(384);
    let session_refs = sessions.iter().map(String::as_str).collect::<Vec<_>>();

    let steady_dir = BenchDir::new("session-order-steady");
    let steady_config_path = steady_dir.path("config.toml");
    let mut steady_store = session_order(&steady_config_path);
    steady_store.sync_sessions(session_refs.iter().copied());
    c.bench_function("session_order_steady_sync_384", |b| {
        b.iter(|| {
            black_box(
                steady_store
                    .sync_sessions(black_box(session_refs.iter().copied()))
                    .len(),
            )
        })
    });

    let move_dir = BenchDir::new("session-order-move");
    let move_config_path = move_dir.path("config.toml");
    let mut move_store = session_order(&move_config_path);
    move_store.sync_sessions(session_refs.iter().copied());
    c.bench_function("session_order_move_session_persist_384", |b| {
        b.iter(|| {
            let moved_up = move_store.move_session_before(
                black_box("group-5/session-005"),
                black_box(Some("group-0/session-000")),
                black_box(session_refs.iter().copied()),
            );
            let moved_down = move_store.move_session_before(
                black_box("group-5/session-005"),
                black_box(None),
                black_box(session_refs.iter().copied()),
            );
            black_box((moved_up, moved_down))
        })
    });

    let mut cold_index = 0_u32;
    c.bench_function("session_order_cold_sync_sqlite_384", |b| {
        b.iter(|| {
            cold_index = cold_index.wrapping_add(1);
            let dir = BenchDir::new(&format!("session-order-cold-{cold_index}"));
            let config_path = dir.path("config.toml");
            let mut store = session_order(&config_path);
            black_box(store.sync_sessions(session_refs.iter().copied()).len())
        })
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.15);
    targets = bench_config_load, bench_session_order,
);
criterion_main!(benches);
