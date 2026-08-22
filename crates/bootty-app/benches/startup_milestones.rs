use std::{
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_app::{AppState, platform::native_options_for_config};
use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig, load_config_from_path};
use bootty_render::geometry::TerminalGeometry;
use bootty_runtime::terminal_session::{
    SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

mod support;

const FIRST_FRAME_MARKER: &str = "BOOTTY_STARTUP_READY";
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

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
}

fn empty_config_fixture() -> ConfigFixture {
    let dir = BenchDir::new("startup-empty-config");
    let config_path = dir.path("config.toml");
    fs::write(&config_path, "version = 1\n").expect("write empty config");
    ConfigFixture {
        _dir: dir,
        config_path,
    }
}

fn broken_config_fixture() -> ConfigFixture {
    let dir = BenchDir::new("startup-broken-config");
    let config_path = dir.path("config.toml");
    fs::write(&config_path, "version = \"not-a-number\"\n[font\n").expect("write broken config");
    ConfigFixture {
        _dir: dir,
        config_path,
    }
}

fn large_config_fixture() -> ConfigFixture {
    let dir = BenchDir::new("startup-large-config");
    let themes_dir = dir.path("themes");
    fs::create_dir_all(&themes_dir).expect("create themes dir");
    fs::write(
        themes_dir.join("large-bench.toml"),
        r##"
[metadata]
name = "Large Bench"
source = "benchmark"
license = "MIT"

[colors]
background = "#101014"
foreground = "#eeeeee"
cursor = "#ffffff"
palette = ["#000000", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#bbbbbb"]
"##,
    )
    .expect("write theme");

    let mut includes = Vec::new();
    for index in 0..24 {
        let name = format!("include-{index:02}.toml");
        includes.push(format!("\"{name}\""));
        fs::write(
            dir.path(&name),
            format!(
                r#"
[session]
env = [{{ name = "BOOTTY_STARTUP_INCLUDE_{index:02}", value = "{index}" }}]
"#
            ),
        )
        .expect("write include");
    }

    let mut keybinds = vec![
        "cmd+p=session_picker".to_owned(),
        "cmd+n=new_mux_session".to_owned(),
        "performable:cmd+v=paste_from_clipboard".to_owned(),
    ];
    for index in 0..48 {
        keybinds.push(format!(
            "ctrl+space>{}=esc:{}",
            (b'a' + (index % 26) as u8) as char,
            index
        ));
    }
    let keybinds = keybinds
        .into_iter()
        .map(|entry| format!("\"{entry}\""))
        .collect::<Vec<_>>()
        .join(", ");

    let mut env = Vec::new();
    for index in 0..128 {
        env.push(format!(
            "{{ name = \"BOOTTY_STARTUP_ENV_{index:03}\", value = \"{}\" }}",
            "x".repeat(32)
        ));
    }

    let config_path = dir.path("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"
version = 1
theme = "large-bench"
include = [{}]

[font]
family = ["JetBrains Mono", "Apple Color Emoji", "Noto Color Emoji"]
size = 15.0
cell-width = 9.0
cell-height = 22.0

[chrome]
sidebar = true
status-bar = true
sidebar-width = 320.0
status-height = 30.0
gap = 1.0
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.08

[input]
modifier-remap = ["right_alt=alt", "right_shift=shift"]
macos-option-as-alt = "both"
keybind = [{keybinds}]
sidebar-keybind = ["j=next_session", "k=previous_session", "Enter=activate_session"]

[input.backend-keybind]
native = ["ctrl+space>c=new_tab", "ctrl+space>v=split_right", "alt+j=select_pane:down"]
tmux = ["alt+j=esc:J", "cmd+c=csi:72~"]

[session]
shell = "/bin/sh"
working-directory = "/tmp"
term = "xterm-bootty"
colorterm = "truecolor"
env = [{}]

[window]
title = "Bootty Startup Benchmark"
width = 1200.0
height = 900.0
fullscreen = "disabled"
window-decoration = "client"
macos-titlebar-style = "transparent"
"#,
            includes.join(", "),
            env.join(", ")
        ),
    )
    .expect("write large config");

    ConfigFixture {
        _dir: dir,
        config_path,
    }
}

fn load_to_native_options(path: &Path) -> eframe::NativeOptions {
    let config = load_config_from_path(path).expect("load startup config");
    native_options_for_config(&config)
}

fn app_state_ready(config: BoottyConfig) -> usize {
    let repaint: bootty_mux::RepaintHandle = Arc::new(|| {});
    let state =
        AppState::new(config, support::backends(), repaint, None, None).expect("create app state");
    black_box(state.config().window.title.len())
}

fn app_state_config(path: &Path) -> BoottyConfig {
    let mut config = load_config_from_path(path).expect("load startup config");
    config.multiplexer.backend = MultiplexerBackendConfig::Native;
    config
}

fn app_state_ready_from_path(path: &Path) -> usize {
    let config = app_state_config(path);
    let _options = native_options_for_config(&config);
    app_state_ready(config)
}

fn sequential_window_models(path: &Path, count: usize) -> usize {
    (0..count)
        .map(|_| app_state_ready_from_path(path))
        .sum::<usize>()
}

fn concurrent_window_models(path: &Path, count: usize) -> usize {
    let handles = (0..count)
        .map(|_| {
            let path = path.to_path_buf();
            thread::spawn(move || app_state_ready_from_path(&path))
        })
        .collect::<Vec<_>>();
    handles
        .into_iter()
        .map(|handle| handle.join().expect("startup worker thread"))
        .sum()
}

fn startup_geometry() -> TerminalGeometry {
    TerminalGeometry {
        cols: 120,
        rows: 40,
        cell_width: 10,
        cell_height: 22,
    }
}

fn command_launch_config(command: &str) -> SessionLaunchConfig {
    #[cfg(windows)]
    let (shell, args) = (
        "cmd.exe".to_owned(),
        vec!["/C".to_owned(), command.to_owned()],
    );
    #[cfg(not(windows))]
    let (shell, args) = (
        "/bin/sh".to_owned(),
        vec!["-c".to_owned(), command.to_owned()],
    );

    SessionLaunchConfig {
        shell: Some(shell),
        args,
        ..SessionLaunchConfig::default()
    }
}

fn command_to_first_frame(command: &str) -> usize {
    let config = TerminalSessionConfig {
        launch: command_launch_config(command),
        ..TerminalSessionConfig::default()
    };
    let mut terminal =
        TerminalSession::new_with_config(startup_geometry(), config, Arc::new(|| {}))
            .expect("spawn startup command");
    let started = Instant::now();
    loop {
        let frame = terminal.extract_frame().expect("extract startup frame");
        let text = frame.text.iter().collect::<String>();
        if text.contains(FIRST_FRAME_MARKER) {
            return black_box(text.len() + terminal.pending_pty_len());
        }
        if terminal.child_exited().unwrap_or(false) && started.elapsed() > Duration::from_millis(50)
        {
            let frame = terminal
                .extract_frame()
                .expect("extract final startup frame");
            let text = frame.text.iter().collect::<String>();
            if text.contains(FIRST_FRAME_MARKER) {
                return black_box(text.len() + terminal.pending_pty_len());
            }
        }
        assert!(
            started.elapsed() < FIRST_FRAME_TIMEOUT,
            "startup command did not publish first frame before timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn bench_startup_configs(c: &mut Criterion) {
    let empty = empty_config_fixture();
    c.bench_function("startup_empty_config_to_native_options", |b| {
        b.iter(|| black_box(load_to_native_options(black_box(&empty.config_path))))
    });

    let large = large_config_fixture();
    c.bench_function("startup_large_config_to_native_options", |b| {
        b.iter(|| black_box(load_to_native_options(black_box(&large.config_path))))
    });

    let broken = broken_config_fixture();
    c.bench_function("startup_broken_config_error", |b| {
        b.iter(|| black_box(load_config_from_path(black_box(&broken.config_path)).is_err()))
    });
}

fn bench_window_models(c: &mut Criterion) {
    let fixture = large_config_fixture();
    c.bench_function("startup_window_only_state_ready", |b| {
        b.iter(|| black_box(app_state_ready_from_path(black_box(&fixture.config_path))))
    });

    c.bench_function("startup_open_10_windows_sequential_model", |b| {
        b.iter(|| {
            black_box(sequential_window_models(
                black_box(&fixture.config_path),
                10,
            ))
        })
    });

    c.bench_function("startup_open_10_windows_concurrent_model", |b| {
        b.iter(|| {
            black_box(concurrent_window_models(
                black_box(&fixture.config_path),
                10,
            ))
        })
    });
}

fn bench_first_frame(c: &mut Criterion) {
    c.bench_function("startup_window_shell_first_frame", |b| {
        b.iter_batched(
            || format!("printf '%s\\n' {FIRST_FRAME_MARKER}"),
            |command| black_box(command_to_first_frame(&command)),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("startup_window_command_first_frame", |b| {
        b.iter_batched(
            || format!("printf 'command:%s\\n' {FIRST_FRAME_MARKER}"),
            |command| black_box(command_to_first_frame(&command)),
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.20);
    targets = bench_startup_configs, bench_window_models, bench_first_frame,
);
criterion_main!(benches);
