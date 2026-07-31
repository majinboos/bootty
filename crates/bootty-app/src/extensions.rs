//! Loader for built-in and user-provided Lua/Luau UI extension modules.
//!
//! Status extensions live in `<config>/status/`; sidebar extensions live in `<config>/sidebar/`.
//! Built-in defaults and user `.lua` / `.luau` overrides use the same item schema. A module
//! returns a render function or a table `{ interval = <secs>, render = ... }`. The render
//! function returns a string, one item table, or a list of item tables.
//!
//! Mux/session state is exposed through `bootty.windows()`, `bootty.session()`,
//! `bootty.sessions()`, and `bootty.session_color()`. System stats use `bootty.metrics()`;
//! explicit shell-outs use `bootty.run(cmd)`. Modules run on a worker thread so shell-outs
//! never block the UI.

use std::collections::{BTreeMap, BTreeSet, HashMap};
#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32};
use mlua::{Function, Lua, Table, Value};
use starship_battery::{Manager as BatteryManager, State as BatteryState, units::time::second};
use sysinfo::{MemoryRefreshKind, System};

mod codexbar;
mod http;

#[cfg(test)]
use codexbar::command_invokes_usage as command_invokes_codexbar_usage;
use codexbar::{
    reject_reserved_shell_command, resolve_program as resolve_codexbar_program,
    validate_provider as validate_codexbar_provider,
};
use http::get_local as http_get_local;
#[cfg(test)]
use http::response_body as http_response_body;

/// Default refresh cadence for a module that doesn't declare its own `interval`.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);
/// Background poll granularity; a module fires on the first tick at or after its interval elapses.
const TICK: Duration = Duration::from_millis(8);
/// How often extension dirs are re-scanned for edited/added/removed module files (hot reload).
const RELOAD_SCAN_INTERVAL: Duration = Duration::from_secs(1);
const CODEXBAR_SERVER_PORT: u16 = 17_613;
const CODEXBAR_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const ERROR_COLOR: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);
const EXTENSION_UI_PRELUDE: &str = include_str!("extension_ui.luau");
const SIDEBAR_FACTS_PRELUDE: &str = include_str!("sidebar_session_facts.luau");

const BUILTIN_STATUS_EXTENSIONS: &[(&str, &str)] = &[
    ("windows", include_str!("status_defaults/windows.luau")),
    ("clock", include_str!("status_defaults/clock.luau")),
    ("session", include_str!("status_defaults/session.luau")),
    ("sysinfo", include_str!("status_defaults/sysinfo.luau")),
];

const BUILTIN_SIDEBAR_EXTENSIONS: &[(&str, &str)] = &[
    ("sessions", include_str!("sidebar_defaults/sessions.luau")),
    ("codexbar", include_str!("sidebar_defaults/codexbar.luau")),
];

/// One renderable element a Lua/Luau module produced.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleItem {
    pub text: String,
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub stroke: Option<Color32>,
    pub icon: Option<String>,
    /// 0.0-1.0 fill drawn as a battery meter (status bar) or generic gauge.
    pub gauge: Option<f32>,
    pub primitives: Vec<ModulePrimitive>,
    /// Extra layout padding reserved inside the item for custom primitives.
    pub pad_left: f32,
    pub pad_right: f32,
    /// Whether this item may visually connect its background to adjacent items. Defaults to true.
    pub join: Option<bool>,
    /// Whether to keep the normal inter-item gap before this item. Defaults to true.
    pub gap: Option<bool>,
    pub action: Option<String>,
    /// Generic stable identity for clickable/draggable rows. If absent, renderers derive one.
    pub key: Option<String>,
    /// Sidebar row kind. Bootty owns only `group` and `session`; other values are generic rows.
    pub kind: Option<String>,
    pub number: Option<usize>,
    pub indent: Option<u16>,
    pub tree: Option<String>,
    pub selectable: Option<bool>,
    pub session_id: Option<String>,
    pub reorder_anchor: Option<String>,
    pub current: Option<bool>,
    pub active: Option<bool>,
    pub dim_fg: Option<Color32>,
}

/// A local coordinate for status item primitives: `frac` is relative to the item rect,
/// and `px` is an additional logical-pixel offset.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ModuleCoord {
    pub frac: f32,
    pub px: f32,
}

pub type ModuleCornerRadius = egui::CornerRadius;

/// Generic egui-style primitives drawn in the item's local rect before text/icons.
#[derive(Clone, Debug, PartialEq)]
pub enum ModulePrimitive {
    Rect {
        fill: Option<Color32>,
        stroke: Option<Color32>,
        x: ModuleCoord,
        y: ModuleCoord,
        w: ModuleCoord,
        h: ModuleCoord,
        radius: ModuleCornerRadius,
    },
    Polygon {
        fill: Option<Color32>,
        stroke: Option<Color32>,
        points: Vec<(ModuleCoord, ModuleCoord)>,
    },
    Text {
        text: String,
        color: Option<Color32>,
        x: ModuleCoord,
        y: ModuleCoord,
        size: f32,
        align: String,
        min_width: Option<f32>,
    },
    Icon {
        icon: String,
        color: Option<Color32>,
        x: ModuleCoord,
        y: ModuleCoord,
        size: f32,
        min_width: Option<f32>,
    },
}

/// A single window as exposed to modules via `bootty.windows()`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowView {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    /// Terminal progress percentage for an inactive window, if any pane has reported it.
    pub progress: Option<u8>,
    pub progress_indeterminate: bool,
}

/// Progress reported by one terminal pane in a session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionProgressView {
    pub process: String,
    pub value: u8,
    pub indeterminate: bool,
}

/// A mux session as exposed to sidebar/status extensions via `bootty.sessions()`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionView {
    pub id: String,
    pub name: String,
    pub active: bool,
    pub selected: bool,
    pub cwd: Option<String>,
    pub color: Option<String>,
    pub dim_color: Option<String>,
    pub progress: Option<u8>,
    pub progress_indeterminate: bool,
    pub progresses: Vec<SessionProgressView>,
    pub ports: Vec<u16>,
}

/// Mux state shared with the worker thread so modules can render it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MuxView {
    pub windows: Vec<WindowView>,
    pub sessions: Vec<SessionView>,
    pub session: Option<String>,
    pub sidebar_visible: bool,
    /// The active session's sidebar accent color as `#rrggbb`, so modules can
    /// match the bar to the session like the sidebar does.
    pub session_color: Option<String>,
    /// Whether Bootty is currently holding a keep-awake/caffeinate guard.
    pub keep_awake: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinWindowsTheme {
    pub accent: Color32,
    pub surface: Color32,
    pub base: Color32,
    pub subtext: Color32,
    pub text: Color32,
    pub border: Color32,
}

pub fn builtin_windows_items(view: &MuxView, theme: BuiltinWindowsTheme) -> Vec<ModuleItem> {
    let accent = view
        .session_color
        .as_deref()
        .and_then(parse_hex_color)
        .unwrap_or(theme.accent);
    let mut items = Vec::with_capacity(view.windows.len() * 2);
    for (position, window) in view.windows.iter().enumerate() {
        let last = position + 1 == view.windows.len();
        let active = window.active;
        let index_fill = if active { accent } else { theme.base };
        let name_fill = if active { theme.surface } else { theme.base };
        let index_fg = if active { theme.base } else { theme.subtext };
        let name_fg = if active { theme.text } else { theme.subtext };

        let mut index_primitives = Vec::new();
        if let Some(previous) = position
            .checked_sub(1)
            .and_then(|index| view.windows.get(index))
        {
            let previous_fill = if previous.active {
                theme.surface
            } else {
                theme.base
            };
            index_primitives.push(windows_underlay(previous_fill));
        }
        index_primitives.push(windows_rect(index_fill, windows_left_radius()));

        let mut name_primitives = vec![windows_rect(name_fill, windows_right_radius(last))];
        if active {
            name_primitives.push(windows_left_chevron(accent));
        }
        if let Some(progress) = window.progress {
            name_primitives.push(windows_progress_track(theme.border));
            name_primitives.push(windows_progress_fill(
                accent,
                progress,
                window.progress_indeterminate,
            ));
        }

        push_window_item(
            &mut items,
            window,
            ModuleItem {
                text: window.index.to_string(),
                fg: Some(index_fg),
                primitives: index_primitives,
                join: Some(false),
                gap: Some(false),
                ..ModuleItem::default()
            },
        );
        push_window_item(
            &mut items,
            window,
            ModuleItem {
                text: window.name.clone(),
                fg: Some(name_fg),
                primitives: name_primitives,
                pad_left: WINDOWS_WEDGE_PX,
                gap: Some(false),
                ..ModuleItem::default()
            },
        );
    }
    items
}

const WINDOWS_RADIUS_PX: u8 = 6;
const WINDOWS_WEDGE_PX: f32 = 8.0;
const WINDOWS_PROGRESS_HEIGHT: f32 = 2.0;
const WINDOWS_INDETERMINATE_PROGRESS_WIDTH: f32 = 0.25;
const WINDOWS_INDETERMINATE_PROGRESS_CYCLE: f64 = 1.5;

fn push_window_item(items: &mut Vec<ModuleItem>, window: &WindowView, mut item: ModuleItem) {
    item.action = Some(format!("activate-window:{}", window.id));
    item.reorder_anchor = Some(window.id.clone());
    items.push(item);
}

fn windows_left_radius() -> ModuleCornerRadius {
    egui::CornerRadius {
        nw: WINDOWS_RADIUS_PX,
        sw: WINDOWS_RADIUS_PX,
        ..egui::CornerRadius::default()
    }
}

fn windows_right_radius(enabled: bool) -> ModuleCornerRadius {
    let radius = if enabled { WINDOWS_RADIUS_PX } else { 0 };
    egui::CornerRadius {
        ne: radius,
        se: radius,
        ..egui::CornerRadius::default()
    }
}

fn windows_progress_track(fill: Color32) -> ModulePrimitive {
    ModulePrimitive::Rect {
        fill: Some(fill),
        stroke: None,
        x: ModuleCoord::default(),
        y: ModuleCoord {
            frac: 1.0,
            px: -WINDOWS_PROGRESS_HEIGHT,
        },
        w: ModuleCoord { frac: 1.0, px: 0.0 },
        h: ModuleCoord {
            frac: 0.0,
            px: WINDOWS_PROGRESS_HEIGHT,
        },
        radius: ModuleCornerRadius::ZERO,
    }
}

fn windows_progress_fill(fill: Color32, progress: u8, indeterminate: bool) -> ModulePrimitive {
    let (offset, width) = if indeterminate {
        (
            windows_indeterminate_progress_offset(window_progress_animation_time()),
            WINDOWS_INDETERMINATE_PROGRESS_WIDTH,
        )
    } else {
        (0.0, f32::from(progress) / 100.0)
    };
    ModulePrimitive::Rect {
        fill: Some(fill),
        stroke: None,
        x: ModuleCoord {
            frac: offset,
            px: 0.0,
        },
        y: ModuleCoord {
            frac: 1.0,
            px: -WINDOWS_PROGRESS_HEIGHT,
        },
        w: ModuleCoord {
            frac: width,
            px: 0.0,
        },
        h: ModuleCoord {
            frac: 0.0,
            px: WINDOWS_PROGRESS_HEIGHT,
        },
        radius: ModuleCornerRadius::ZERO,
    }
}

fn window_progress_animation_time() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

fn windows_indeterminate_progress_offset(time: f64) -> f32 {
    let phase = (time / WINDOWS_INDETERMINATE_PROGRESS_CYCLE).fract() as f32;
    let travel = 1.0 - (phase * 2.0 - 1.0).abs();
    (1.0 - WINDOWS_INDETERMINATE_PROGRESS_WIDTH) * travel
}

fn windows_rect(fill: Color32, radius: ModuleCornerRadius) -> ModulePrimitive {
    ModulePrimitive::Rect {
        fill: Some(fill),
        stroke: None,
        x: ModuleCoord::default(),
        y: ModuleCoord::default(),
        w: ModuleCoord { frac: 1.0, px: 0.0 },
        h: ModuleCoord { frac: 1.0, px: 0.0 },
        radius,
    }
}

fn windows_underlay(fill: Color32) -> ModulePrimitive {
    ModulePrimitive::Rect {
        fill: Some(fill),
        stroke: None,
        x: ModuleCoord::default(),
        y: ModuleCoord::default(),
        w: ModuleCoord {
            frac: 0.0,
            px: WINDOWS_RADIUS_PX as f32,
        },
        h: ModuleCoord { frac: 1.0, px: 0.0 },
        radius: egui::CornerRadius::default(),
    }
}

fn windows_left_chevron(fill: Color32) -> ModulePrimitive {
    ModulePrimitive::Polygon {
        fill: Some(fill),
        stroke: None,
        points: vec![
            (
                ModuleCoord {
                    frac: 0.0,
                    px: -1.0,
                },
                ModuleCoord { frac: 0.0, px: 0.0 },
            ),
            (
                ModuleCoord {
                    frac: 0.0,
                    px: WINDOWS_WEDGE_PX,
                },
                ModuleCoord { frac: 0.5, px: 0.0 },
            ),
            (
                ModuleCoord {
                    frac: 0.0,
                    px: -1.0,
                },
                ModuleCoord { frac: 1.0, px: 0.0 },
            ),
        ],
    }
}

/// Cross-platform system metrics gathered natively (no per-OS shell-outs), so
/// modules read them through `bootty.metrics()` on any platform.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    /// Global CPU usage, 0-100.
    pub cpu: f32,
    /// 1-minute load average; 0 where the OS has no concept of it (e.g. Windows).
    pub load1: f64,
    /// Memory in use as a percentage. On macOS this is real memory pressure (what
    /// Activity Monitor's pressure reflects), not the cache-inflated "used" figure.
    pub mem_used_pct: f64,
    pub mem_total_bytes: u64,
    /// Battery charge 0-100, or `None` on a machine with no battery (desktop).
    pub battery_percent: Option<f32>,
    /// Plugged in / charging / full / no battery (not draining).
    pub on_ac: bool,
    /// Seconds until empty while discharging, or `None` when unavailable/not discharging.
    pub battery_time_to_empty_secs: Option<f32>,
    /// Seconds until full while charging, or `None` when unavailable/not charging.
    pub battery_time_to_full_secs: Option<f32>,
}

/// A reorder gesture from the sidebar UI, routed to the named module's `on_reorder` handler
/// on the worker thread (where the Lua VM lives).
#[derive(Clone, Debug, PartialEq)]
struct ReorderRequest {
    module: String,
    source: String,
    before: Option<String>,
}

/// A session-order change a module requested via `bootty.reorder_session(source, before)`.
/// The app drains these each frame and applies them to the native session-order store.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionReorder {
    pub source: String,
    pub before: Option<String>,
}

/// One selectable row in a Luau-declared floating window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LuaWindowRow {
    pub key: String,
    pub text: String,
    pub icon: Option<String>,
    pub description: Option<String>,
}

/// The renderable description of a window a module opened via `bootty.window.open`.
/// Carries no Lua closures, so it can cross to the main thread; the `on_action`
/// handler stays worker-side keyed by `id`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LuaWindowSpec {
    pub id: u64,
    /// `"list"` (default) or `"prompt"`.
    pub kind: String,
    pub title: String,
    pub icon: Option<String>,
    pub hint: Option<String>,
    pub placeholder: Option<String>,
    pub rows: Vec<LuaWindowRow>,
}

/// A window open/close request a module made via `bootty.window`, drained by the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowRequest {
    Open(LuaWindowSpec),
    Close,
}

/// The fate of a Luau window, routed back to its worker so the `on_action` handler
/// is invoked on a choice and always dropped (freeing its slot) once the window goes
/// away — whether chosen, dismissed, or superseded.
#[derive(Clone, Debug, PartialEq, Eq)]
enum WindowOutcome {
    /// The user picked a row (`key`) or submitted a prompt (`value`).
    Chosen {
        id: u64,
        key: String,
        value: Option<String>,
    },
    /// The window closed without a choice; the handler is dropped, not called.
    Dismissed { id: u64 },
}

impl WindowOutcome {
    fn id(&self) -> u64 {
        match self {
            Self::Chosen { id, .. } | Self::Dismissed { id } => *id,
        }
    }
}

/// The worker's window request queue and id source; aliased to keep the
/// thread-local declaration legible.
type WindowQueue = (Arc<RwLock<Vec<WindowRequest>>>, Arc<AtomicU64>);

thread_local! {
    /// Per-worker `window id -> on_action` handlers. Lives thread-local because an
    /// mlua `Function` is `!Send`; the worker that owns the Lua VM also dispatches it.
    static WINDOW_HANDLERS: std::cell::RefCell<HashMap<u64, Function>> =
        std::cell::RefCell::new(HashMap::new());
    /// The worker's window request queue + id source, installed by `run_loop` so the
    /// `bootty.window` host fns reach them without widening `setup_lua`'s signature.
    static WINDOW_QUEUE: std::cell::RefCell<Option<WindowQueue>> =
        const { std::cell::RefCell::new(None) };
}

/// Parse a `bootty.window.open` spec table into the renderable [`LuaWindowSpec`].
fn parse_window_spec(id: u64, spec: &Table) -> LuaWindowSpec {
    let rows = spec
        .get::<Table>("rows")
        .ok()
        .map(|rows| {
            rows.sequence_values::<Table>()
                .filter_map(Result::ok)
                .map(|row| LuaWindowRow {
                    key: row.get::<String>("key").unwrap_or_default(),
                    text: row.get::<String>("text").unwrap_or_default(),
                    icon: string_field(&row, "icon"),
                    description: string_field(&row, "description"),
                })
                .collect()
        })
        .unwrap_or_default();
    LuaWindowSpec {
        id,
        kind: spec
            .get::<String>("kind")
            .ok()
            .filter(|kind| !kind.is_empty())
            .unwrap_or_else(|| "list".to_owned()),
        title: spec.get::<String>("title").unwrap_or_default(),
        icon: string_field(spec, "icon"),
        hint: string_field(spec, "hint"),
        placeholder: string_field(spec, "placeholder"),
        rows,
    }
}

/// How often native metrics are sampled (CPU needs a gap between samples).
const METRICS_INTERVAL: Duration = Duration::from_secs(2);

enum ModuleBody {
    Render(Function),
    /// The file failed to parse/evaluate; surfaced in the bar so edits aren't silently dropped.
    LoadError(String),
}

struct LoadedModule {
    name: String,
    interval: Duration,
    body: ModuleBody,
    /// Optional `on_reorder(source, before)` handler invoked when the UI drags one of this
    /// module's anchored rows. Lets a module own what reordering its items means.
    on_reorder: Option<Function>,
    last_run: Option<Instant>,
}

/// How `bootty.run` treats the shared shell-out cache during the current phase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Outside a render (e.g. an `on_reorder` mutation): always shell out, never cache. Keeps
    /// side-effecting commands like `tmux move-window` out of the cache and always executed.
    Live = 0,
    /// Interval render: return the last cached value immediately and refresh it in the background.
    Refresh = 1,
    /// Forced render (a reorder, structural mux change, or completed background refresh): serve
    /// cached output only so the render is instant and side-effect free.
    Cached = 2,
}

/// Caches `bootty.run` query output across renders and refreshes shell-outs off the extension
/// worker so one slow provider/command cannot block unrelated modules.
#[derive(Default)]
struct RunCache {
    entries: Mutex<HashMap<String, RunEntry>>,
    /// Current behavior, a `RunMode` discriminant; defaults to `Live`.
    mode: AtomicU8,
    waker: Option<Arc<Waker>>,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
    codexbar: CodexBarClient,
}

#[derive(Default)]
struct RunEntry {
    output: String,
    refreshing: bool,
}

#[derive(Default)]
struct CodexBarEntry {
    output: String,
    refreshing: bool,
    last_refresh: Option<Instant>,
}

impl RunCache {
    fn with_waker(
        waker: Arc<Waker>,
        run_jobs: Arc<PlatformRunJobs>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            waker: Some(waker),
            run_jobs,
            shutdown,
            ..Self::default()
        }
    }

    fn set_mode(&self, mode: RunMode) {
        self.mode.store(mode as u8, Ordering::Relaxed);
    }

    fn mode(&self) -> RunMode {
        match self.mode.load(Ordering::Relaxed) {
            x if x == RunMode::Cached as u8 => RunMode::Cached,
            x if x == RunMode::Refresh as u8 => RunMode::Refresh,
            _ => RunMode::Live,
        }
    }

    fn run(self: &Arc<Self>, cmd: &str) -> std::io::Result<String> {
        reject_reserved_shell_command(cmd)?;
        match self.mode() {
            RunMode::Live => shell_run_output(cmd, &self.run_jobs, &self.shutdown)
                .map(|output| output.trim().to_owned()),
            RunMode::Cached => Ok(self.cached(cmd).unwrap_or_default()),
            RunMode::Refresh => {
                let output = self.cached(cmd).unwrap_or_default();
                self.refresh(cmd.to_owned());
                Ok(output)
            }
        }
    }

    fn codexbar_usage(self: &Arc<Self>, provider: &str) -> std::io::Result<String> {
        validate_codexbar_provider(provider)?;
        #[cfg(test)]
        if let Some(output) = self.codexbar.mock_usage(provider) {
            return Ok(output.trim().to_owned());
        }

        let output = self.codexbar.cached(provider).unwrap_or_default();
        if self.mode() != RunMode::Cached {
            self.refresh_codexbar_usage(provider.to_owned());
        }
        Ok(output)
    }

    fn cached(&self, cmd: &str) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(cmd).map(|entry| entry.output.clone()))
    }

    fn refresh(self: &Arc<Self>, cmd: String) {
        {
            let Ok(mut entries) = self.entries.lock() else {
                return;
            };
            let entry = entries.entry(cmd.clone()).or_default();
            if entry.refreshing {
                return;
            }
            entry.refreshing = true;
        }

        let cache = Arc::clone(self);
        std::thread::spawn(move || {
            let output = shell_run_output(&cmd, &cache.run_jobs, &cache.shutdown)
                .map(|output| output.trim().to_owned())
                .unwrap_or_else(|error| format!("bootty.run: {error}"));
            if let Ok(mut entries) = cache.entries.lock() {
                let entry = entries.entry(cmd).or_default();
                entry.output = output;
                entry.refreshing = false;
            }
            if let Some(waker) = &cache.waker {
                waker.force();
            }
        });
    }

    fn refresh_codexbar_usage(self: &Arc<Self>, provider: String) {
        if !self
            .codexbar
            .mark_refreshing(&provider, CODEXBAR_REFRESH_INTERVAL)
        {
            return;
        }

        let cache = Arc::clone(self);
        std::thread::spawn(move || {
            let output = cache
                .codexbar
                .fetch_usage(&provider)
                .map(|output| output.trim().to_owned())
                .ok();
            let changed = cache.codexbar.finish_refresh(&provider, output);
            if changed && let Some(waker) = &cache.waker {
                waker.force();
            }
        });
    }
}

#[derive(Default)]
struct CodexBarClient {
    server: Mutex<CodexBarServerState>,
    entries: Mutex<HashMap<String, CodexBarEntry>>,
    #[cfg(test)]
    mock_usage: Mutex<HashMap<String, String>>,
}

#[derive(Default)]
struct CodexBarServerState {
    port: Option<u16>,
    child: Option<Child>,
}

impl Drop for CodexBarClient {
    fn drop(&mut self) {
        if let Ok(mut server) = self.server.lock()
            && let Some(mut child) = server.child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl CodexBarClient {
    #[cfg(test)]
    fn mock_usage(&self, provider: &str) -> Option<String> {
        self.mock_usage
            .lock()
            .ok()
            .and_then(|entries| entries.get(provider).cloned())
    }

    fn cached(&self, provider: &str) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(provider).map(|entry| entry.output.clone()))
    }

    fn mark_refreshing(&self, provider: &str, refresh_interval: Duration) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let entry = entries.entry(provider.to_owned()).or_default();
        if entry.refreshing {
            return false;
        }
        let now = Instant::now();
        if entry
            .last_refresh
            .is_some_and(|last| now.duration_since(last) < refresh_interval)
        {
            return false;
        }
        entry.refreshing = true;
        entry.last_refresh = Some(now);
        true
    }

    fn finish_refresh(&self, provider: &str, output: Option<String>) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let entry = entries.entry(provider.to_owned()).or_default();
        entry.refreshing = false;
        let Some(output) = output else {
            return false;
        };
        if entry.output == output {
            return false;
        }
        entry.output = output;
        true
    }

    fn fetch_usage(&self, provider: &str) -> std::io::Result<String> {
        let port = self.ensure_server()?;
        http_get_local(
            port,
            &format!("/usage?provider={provider}"),
            Duration::from_secs(35),
        )
    }

    #[cfg(test)]
    fn set_mock_usage(&self, provider: &str, output: &str) {
        self.mock_usage
            .lock()
            .expect("codexbar mock usage")
            .insert(provider.to_owned(), output.to_owned());
    }

    fn ensure_server(&self) -> std::io::Result<u16> {
        let mut server = self
            .server
            .lock()
            .map_err(|_| std::io::Error::other("codexbar server lock poisoned"))?;
        if let (Some(port), Some(child)) = (server.port, server.child.as_mut())
            && child.try_wait()?.is_none()
        {
            return Ok(port);
        }
        if let Some(port) = server.port
            && server.child.is_none()
            && http_get_local(port, "/health", Duration::from_millis(100)).is_ok()
        {
            return Ok(port);
        }

        server.child.take();
        server.port = None;
        if http_get_local(CODEXBAR_SERVER_PORT, "/health", Duration::from_millis(100)).is_ok() {
            server.port = Some(CODEXBAR_SERVER_PORT);
            return Ok(CODEXBAR_SERVER_PORT);
        }

        let port = CODEXBAR_SERVER_PORT;
        let port_arg = port.to_string();
        let child = Command::new(resolve_codexbar_program()?)
            .args([
                "serve",
                "--port",
                port_arg.as_str(),
                "--refresh-interval",
                "60",
                "--request-timeout",
                "30",
                "--log-level",
                "error",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        server.port = Some(port);
        server.child = Some(child);

        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(child) = server.child.as_mut()
                && let Some(status) = child.try_wait()?
            {
                server.child = None;
                server.port = None;
                return Err(std::io::Error::other(format!(
                    "codexbar serve exited during startup with {status}"
                )));
            }
            if http_get_local(port, "/health", Duration::from_millis(100)).is_ok() {
                return Ok(port);
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "codexbar serve did not become healthy",
        ))
    }
}

/// Wakes the worker out of its tick wait so reorders and structural mux changes apply promptly
/// instead of waiting out the poll interval. `force_render` makes the next tick re-render active
/// modules regardless of their interval.
#[derive(Default)]
struct Waker {
    force_render: AtomicBool,
    woken: Mutex<bool>,
    cond: Condvar,
}

impl Waker {
    fn wake(&self) {
        if let Ok(mut woken) = self.woken.lock() {
            *woken = true;
            self.cond.notify_one();
        }
    }

    fn force(&self) {
        self.force_render.store(true, Ordering::Relaxed);
        self.wake();
    }

    fn take_force(&self) -> bool {
        self.force_render.swap(false, Ordering::Relaxed)
    }

    fn wait(&self, timeout: Duration) {
        if let Ok(mut woken) = self.woken.lock() {
            if !*woken {
                woken = self
                    .cond
                    .wait_timeout(woken, timeout)
                    .map(|(guard, _)| guard)
                    .unwrap_or_else(|poisoned| poisoned.into_inner().0);
            }
            *woken = false;
        }
    }
}

/// Owns the Luau worker thread, the shared item map the UI reads, and the mux snapshot the UI feeds.
pub struct ExtensionHost {
    dir: PathBuf,
    items: Arc<RwLock<HashMap<String, Vec<ModuleItem>>>>,
    mux: Arc<RwLock<MuxView>>,
    metrics: Arc<RwLock<Metrics>>,
    active: Arc<RwLock<BTreeSet<String>>>,
    /// Reorder gestures from the UI, awaiting their module's `on_reorder` handler on the worker.
    pending_reorders: Arc<RwLock<Vec<ReorderRequest>>>,
    /// Session-order changes modules requested via `bootty.reorder_session`, drained by the app.
    session_reorders: Arc<RwLock<Vec<SessionReorder>>>,
    /// Floating-window open/close requests modules made via `bootty.window`, drained by the app.
    window_requests: Arc<RwLock<Vec<WindowRequest>>>,
    /// Fates of Luau windows, awaiting their `on_action` handler on the worker.
    pending_window_actions: Arc<RwLock<Vec<WindowOutcome>>>,
    waker: Arc<Waker>,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
}

impl ExtensionHost {
    /// Spawns the status-bar extension worker. `ctx` is woken when module output changes.
    pub fn spawn_status(dir: PathBuf, ctx: egui::Context, theme: Vec<(String, String)>) -> Self {
        Self::spawn_with_modules("bootty-status", dir, ctx, theme, BUILTIN_STATUS_EXTENSIONS)
    }

    /// Spawns the sidebar extension worker. User modules in `dir` override embedded defaults.
    pub fn spawn_sidebar(dir: PathBuf, ctx: egui::Context, theme: Vec<(String, String)>) -> Self {
        Self::spawn_with_modules(
            "bootty-sidebar",
            dir,
            ctx,
            theme,
            BUILTIN_SIDEBAR_EXTENSIONS,
        )
    }

    fn spawn_with_modules(
        thread_name: &str,
        dir: PathBuf,
        ctx: egui::Context,
        theme: Vec<(String, String)>,
        builtins: &'static [(&'static str, &'static str)],
    ) -> Self {
        let items: Arc<RwLock<HashMap<String, Vec<ModuleItem>>>> = Arc::default();
        let mux: Arc<RwLock<MuxView>> = Arc::default();
        let metrics: Arc<RwLock<Metrics>> = Arc::default();
        let active: Arc<RwLock<BTreeSet<String>>> = Arc::default();
        let pending_reorders: Arc<RwLock<Vec<ReorderRequest>>> = Arc::default();
        let session_reorders: Arc<RwLock<Vec<SessionReorder>>> = Arc::default();
        let window_requests: Arc<RwLock<Vec<WindowRequest>>> = Arc::default();
        let pending_window_actions: Arc<RwLock<Vec<WindowOutcome>>> = Arc::default();
        let next_window_id = Arc::new(AtomicU64::new(1));
        let waker: Arc<Waker> = Arc::default();
        let module_dir = dir.clone();
        let run_jobs = Arc::new(PlatformRunJobs::default());
        cleanup_stale_platform_shell_run_jobs();
        let shutdown = Arc::new(AtomicBool::new(false));
        let _handle = std::thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn({
                let items = Arc::clone(&items);
                let mux = Arc::clone(&mux);
                let metrics = Arc::clone(&metrics);
                let active = Arc::clone(&active);
                let pending_reorders = Arc::clone(&pending_reorders);
                let session_reorders = Arc::clone(&session_reorders);
                let window_requests = Arc::clone(&window_requests);
                let pending_window_actions = Arc::clone(&pending_window_actions);
                let next_window_id = Arc::clone(&next_window_id);
                let waker = Arc::clone(&waker);
                let shutdown = Arc::clone(&shutdown);
                let run_jobs = Arc::clone(&run_jobs);
                move || {
                    run_loop(
                        &dir,
                        &ctx,
                        &theme,
                        builtins,
                        &mux,
                        &metrics,
                        &active,
                        &items,
                        &pending_reorders,
                        &session_reorders,
                        &window_requests,
                        &pending_window_actions,
                        &next_window_id,
                        &waker,
                        &shutdown,
                        &run_jobs,
                    )
                }
            })
            .ok();
        Self {
            dir: module_dir,
            items,
            mux,
            metrics,
            active,
            pending_reorders,
            session_reorders,
            window_requests,
            pending_window_actions,
            waker,
            shutdown,
            run_jobs,
        }
    }

    /// Declares which modules are referenced by the UI. Only these run, so an
    /// unreferenced module never shells out on its interval.
    pub fn set_active(&self, names: impl IntoIterator<Item = String>) {
        let next: BTreeSet<String> = names.into_iter().collect();
        if let Ok(mut current) = self.active.write()
            && *current != next
        {
            *current = next;
        }
    }

    #[must_use]
    pub fn items(&self, name: &str) -> Vec<ModuleItem> {
        self.items
            .read()
            .ok()
            .and_then(|map| map.get(name).cloned())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn has_user_module(&self, name: &str) -> bool {
        user_module_exists(&self.dir, name)
    }

    #[must_use]
    pub fn metrics(&self) -> Metrics {
        self.metrics
            .read()
            .map(|metrics| *metrics)
            .unwrap_or_default()
    }

    /// Publishes the latest mux snapshot for modules to render. Cheap; the UI calls it per frame.
    /// Changes that affect module visibility or slow-changing module output wake the worker
    /// immediately; selection-only changes don't, since the UI reflects those natively.
    pub fn update_mux(&self, view: MuxView) {
        if let Ok(mut current) = self.mux.write()
            && *current != view
        {
            let should_force_render = current.sidebar_visible != view.sidebar_visible
                || current.keep_awake != view.keep_awake
                || current
                    .sessions
                    .iter()
                    .map(|session| session.name.as_str())
                    .ne(view.sessions.iter().map(|session| session.name.as_str()))
                || current
                    .windows
                    .iter()
                    .map(|window| window.id.as_str())
                    .ne(view.windows.iter().map(|window| window.id.as_str()));
            *current = view;
            drop(current);
            if should_force_render {
                self.waker.force();
            }
        }
    }

    /// Routes a sidebar reorder gesture to the named module's `on_reorder` handler. The handler
    /// runs on the worker thread, where the Lua VM lives.
    pub fn request_reorder(&self, module: &str, source: String, before: Option<String>) {
        if let Ok(mut queue) = self.pending_reorders.write() {
            queue.push(ReorderRequest {
                module: module.to_owned(),
                source,
                before,
            });
        }
        self.waker.wake();
    }

    /// Drains session-order changes modules asked for via `bootty.reorder_session`, for the app
    /// to apply to its native session-order store.
    #[must_use]
    pub fn take_session_reorders(&self) -> Vec<SessionReorder> {
        self.session_reorders
            .write()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }

    /// Drains floating-window open/close requests modules made via `bootty.window`,
    /// for the app to render with the native overlay framework.
    #[must_use]
    pub fn take_window_requests(&self) -> Vec<WindowRequest> {
        self.window_requests
            .write()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }

    /// Routes a user's window choice back to the owning window's `on_action`
    /// handler on this host's worker thread (where the Lua VM lives).
    pub fn push_window_action(&self, id: u64, key: String, value: Option<String>) {
        self.queue_window_outcome(WindowOutcome::Chosen { id, key, value });
    }

    /// Tells the worker a window closed without a choice so its `on_action` handler
    /// is dropped (not called), preventing a slow leak in `WINDOW_HANDLERS`.
    pub fn close_window(&self, id: u64) {
        self.queue_window_outcome(WindowOutcome::Dismissed { id });
    }

    fn queue_window_outcome(&self, outcome: WindowOutcome) {
        if let Ok(mut queue) = self.pending_window_actions.write() {
            queue.push(outcome);
        }
        self.waker.wake();
    }
}

impl Drop for ExtensionHost {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.waker.wake();
        self.run_jobs.cleanup();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    dir: &Path,
    ctx: &egui::Context,
    theme: &[(String, String)],
    builtins: &'static [(&'static str, &'static str)],
    mux: &Arc<RwLock<MuxView>>,
    metrics: &Arc<RwLock<Metrics>>,
    active: &RwLock<BTreeSet<String>>,
    items: &RwLock<HashMap<String, Vec<ModuleItem>>>,
    pending_reorders: &RwLock<Vec<ReorderRequest>>,
    session_reorders: &Arc<RwLock<Vec<SessionReorder>>>,
    window_requests: &Arc<RwLock<Vec<WindowRequest>>>,
    pending_window_actions: &RwLock<Vec<WindowOutcome>>,
    next_window_id: &Arc<AtomicU64>,
    waker: &Arc<Waker>,
    shutdown: &Arc<AtomicBool>,
    run_jobs: &Arc<PlatformRunJobs>,
) {
    let run_cache = Arc::new(RunCache::with_waker(
        Arc::clone(waker),
        Arc::clone(run_jobs),
        Arc::clone(shutdown),
    ));
    let Ok(lua) = setup_lua(
        theme,
        Arc::clone(mux),
        Arc::clone(metrics),
        Arc::clone(session_reorders),
        Arc::clone(&run_cache),
    ) else {
        return;
    };
    // Hand the worker its window channels so `bootty.window.open` (registered inside
    // `setup_lua`) can reach them without widening that function's signature.
    WINDOW_QUEUE.with(|queue| {
        *queue.borrow_mut() = Some((Arc::clone(window_requests), Arc::clone(next_window_id)));
    });
    let mut modules = load_modules(&lua, dir, builtins);
    let mut signature = dir_signature(dir);
    let mut last_scan = Instant::now();
    let mut system = System::new();
    let battery = BatteryManager::new().ok();
    let mut last_metrics: Option<Instant> = None;
    while !shutdown.load(Ordering::Relaxed) {
        let now = Instant::now();
        // A structural mux change (reorder, session/window added or removed) forces a re-render
        // this tick, so the new layout shows immediately instead of after the poll interval.
        let force = waker.take_force();
        // Hot reload: re-evaluate when extension files are added, edited, or removed.
        if now.duration_since(last_scan) >= RELOAD_SCAN_INTERVAL {
            last_scan = now;
            let current = dir_signature(dir);
            if current != signature {
                signature = current;
                modules = load_modules(&lua, dir, builtins);
                let module_names = modules
                    .iter()
                    .map(|module| module.name.clone())
                    .collect::<BTreeSet<_>>();
                prune_removed_items(items, &module_names);
                ctx.request_repaint();
            }
        }
        // Sample native metrics only while modules are active, and before running
        // them, so `bootty.metrics()` reads fresh values without per-OS shell-outs.
        let bar_active = active.read().is_ok_and(|names| !names.is_empty());
        if bar_active
            && last_metrics.is_none_or(|last| now.duration_since(last) >= METRICS_INTERVAL)
        {
            last_metrics = Some(now);
            refresh_metrics(&mut system, battery.as_ref(), metrics);
        }
        // Apply reorder gestures from the UI by invoking the owning module's handler, so a module
        // owns what reordering its rows means (persist order, remap, `bootty.reorder_session`, ...).
        let requests = pending_reorders
            .write()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        for request in requests {
            let Some(module) = modules
                .iter_mut()
                .find(|module| module.name == request.module)
            else {
                continue;
            };
            let Some(handler) = module.on_reorder.clone() else {
                continue;
            };
            if let Err(error) = handler.call::<()>((request.source, request.before))
                && let Ok(mut map) = items.write()
            {
                map.insert(module.name.clone(), vec![error_item(&error.to_string())]);
            }
            // Nudge the UI to apply the resulting state change (e.g. the session-order commit),
            // which republishes the mux and forces the re-render via `update_mux`.
            ctx.request_repaint();
        }
        // Once a window closes, drop its `on_action` handler; only a real choice
        // calls it. A stale id whose handler is already gone is simply ignored.
        let window_outcomes = pending_window_actions
            .write()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default();
        for outcome in window_outcomes {
            let handler =
                WINDOW_HANDLERS.with(|handlers| handlers.borrow_mut().remove(&outcome.id()));
            if let (Some(handler), WindowOutcome::Chosen { key, value, .. }) = (handler, outcome) {
                let _ = handler.call::<()>((key, value));
                ctx.request_repaint();
            }
        }
        // A forced render reuses cached shell-out results (the reorder/structural change that
        // forced it didn't alter any query's output); an interval render refreshes the cache.
        run_cache.set_mode(if force {
            RunMode::Cached
        } else {
            RunMode::Refresh
        });
        for module in &mut modules {
            // Only run modules a segment references, so an unused module never
            // shells out on its interval.
            if !active
                .read()
                .is_ok_and(|names| names.contains(&module.name))
            {
                continue;
            }
            if force
                || module
                    .last_run
                    .is_none_or(|last| now.duration_since(last) >= module.interval)
            {
                record_module_interval_run(force, &mut module.last_run, now);
                let produced = run_module(&module.body);
                if let Ok(mut map) = items.write()
                    && map.get(&module.name) != Some(&produced)
                {
                    map.insert(module.name.clone(), produced);
                    ctx.request_repaint();
                }
            }
        }
        // Back to Live so the next iteration's `on_reorder` mutations always execute and never cache.
        run_cache.set_mode(RunMode::Live);
        waker.wait(TICK);
    }
}

fn record_module_interval_run(force: bool, last_run: &mut Option<Instant>, now: Instant) {
    if !force {
        *last_run = Some(now);
    }
}

fn prune_removed_items(
    items: &RwLock<HashMap<String, Vec<ModuleItem>>>,
    module_names: &BTreeSet<String>,
) {
    let Ok(mut map) = items.write() else {
        return;
    };
    map.retain(|name, _| module_names.contains(name));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleKind {
    Sidebar,
    Status,
}

impl ModuleKind {
    fn builtins(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Sidebar => BUILTIN_SIDEBAR_EXTENSIONS,
            Self::Status => BUILTIN_STATUS_EXTENSIONS,
        }
    }
}

/// Module names available to reference from a segment: built-ins plus user `*.lua` / `*.luau`
/// files. Sorted and de-duplicated for settings.
pub fn available_module_names(dir: &Path) -> Vec<String> {
    module_names(dir, ModuleKind::Status)
}

pub fn module_names(dir: &Path, kind: ModuleKind) -> Vec<String> {
    available_module_names_with_builtins(dir, kind.builtins())
}

fn available_module_names_with_builtins(
    dir: &Path,
    builtins: &'static [(&'static str, &'static str)],
) -> Vec<String> {
    let mut names: BTreeSet<String> = builtins
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_extension_module_file(&path)
                && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
            {
                names.insert(stem.to_owned());
            }
        }
    }
    names.into_iter().collect()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleSource {
    pub source: String,
    pub path: PathBuf,
    pub customized: bool,
    pub has_builtin: bool,
}

pub fn module_source(dir: &Path, kind: ModuleKind, name: &str) -> Option<ModuleSource> {
    let builtin = kind
        .builtins()
        .iter()
        .find_map(|(candidate, source)| (*candidate == name).then_some(*source));
    let path = user_module_path(dir, name).unwrap_or_else(|| dir.join(format!("{name}.luau")));
    match std::fs::read_to_string(&path) {
        Ok(source) => Some(ModuleSource {
            source,
            path,
            customized: true,
            has_builtin: builtin.is_some(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            builtin.map(|source| ModuleSource {
                source: source.to_owned(),
                path,
                customized: false,
                has_builtin: true,
            })
        }
        Err(_) => None,
    }
}

pub fn valid_module_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

pub fn save_module(
    dir: &Path,
    _kind: ModuleKind,
    name: &str,
    source: &str,
) -> std::io::Result<PathBuf> {
    if !valid_module_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid module name",
        ));
    }
    std::fs::create_dir_all(dir)?;
    let path = user_module_path(dir, name).unwrap_or_else(|| dir.join(format!("{name}.luau")));
    std::fs::write(&path, source)?;
    Ok(path)
}

pub fn reset_module(dir: &Path, name: &str) -> std::io::Result<()> {
    for extension in ["luau", "lua"] {
        match std::fs::remove_file(dir.join(format!("{name}.{extension}"))) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn user_module_path(dir: &Path, name: &str) -> Option<PathBuf> {
    ["luau", "lua"]
        .into_iter()
        .map(|extension| dir.join(format!("{name}.{extension}")))
        .find(|path| path.is_file())
}

/// Sorted (path, mtime) of module files, so a reload can detect added/edited/removed files cheaply.
fn dir_signature(dir: &Path) -> Vec<(PathBuf, Option<SystemTime>)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut signature: Vec<(PathBuf, Option<SystemTime>)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_extension_module_file(path))
        .map(|path| {
            let mtime = std::fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .ok();
            (path, mtime)
        })
        .collect();
    signature.sort();
    signature
}

fn is_extension_module_file(path: &Path) -> bool {
    path.is_file()
        && matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("lua" | "luau")
        )
}

fn user_module_exists(dir: &Path, name: &str) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let path = entry.path();
            is_extension_module_file(&path)
                && path.file_stem().and_then(|stem| stem.to_str()) == Some(name)
        })
    })
}

fn refresh_metrics(
    system: &mut System,
    battery: Option<&BatteryManager>,
    metrics: &RwLock<Metrics>,
) {
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let load = System::load_average();
    let (battery_percent, on_ac, battery_time_to_empty_secs, battery_time_to_full_secs) =
        battery_status(battery);
    let next = Metrics {
        cpu: system.global_cpu_usage(),
        load1: load.one,
        mem_used_pct: memory_used_percent(system),
        mem_total_bytes: system.total_memory(),
        battery_percent,
        on_ac,
        battery_time_to_empty_secs,
        battery_time_to_full_secs,
    };
    if let Ok(mut current) = metrics.write() {
        *current = next;
    }
}

/// Memory in use as a percentage. macOS reports most RAM as "used" for reclaimable
/// caches, so its raw used/total is misleading; use the kernel's real pressure via
/// `memory_pressure` instead. Other platforms use sysinfo's available figure.
#[cfg(target_os = "macos")]
fn memory_used_percent(system: &System) -> f64 {
    macos_memory_pressure_used().unwrap_or_else(|| sysinfo_used_percent(system))
}

#[cfg(not(target_os = "macos"))]
fn memory_used_percent(system: &System) -> f64 {
    sysinfo_used_percent(system)
}

fn sysinfo_used_percent(system: &System) -> f64 {
    let total = system.total_memory();
    if total == 0 {
        return 0.0;
    }
    let available = system.available_memory().min(total);
    100.0 * (total - available) as f64 / total as f64
}

/// Parse `memory_pressure`'s "System-wide memory free percentage: NN%" and return
/// used = 100 - free, the figure Activity Monitor's memory-pressure graph reflects.
#[cfg(target_os = "macos")]
fn macos_memory_pressure_used() -> Option<f64> {
    let output = std::process::Command::new("/usr/bin/memory_pressure")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let free: f64 = text
        .lines()
        .find_map(|line| line.split("free percentage:").nth(1))
        .and_then(|rest| rest.trim().trim_end_matches('%').trim().parse().ok())?;
    Some((100.0 - free).clamp(0.0, 100.0))
}

/// Charge percentage, AC state, and remaining battery time. A machine with no battery
/// (desktop, or a probe error) reports `(None, true, None, None)` so the bar shows an AC icon.
fn battery_status(
    manager: Option<&BatteryManager>,
) -> (Option<f32>, bool, Option<f32>, Option<f32>) {
    let Some(manager) = manager else {
        return (None, true, None, None);
    };
    let Ok(mut batteries) = manager.batteries() else {
        return (None, true, None, None);
    };
    match batteries.next() {
        Some(Ok(battery)) => {
            let percent = battery.state_of_charge().value * 100.0;
            let on_ac = matches!(battery.state(), BatteryState::Charging | BatteryState::Full);
            let time_to_empty = battery.time_to_empty().map(|time| time.get::<second>());
            let time_to_full = battery.time_to_full().map(|time| time.get::<second>());
            (Some(percent), on_ac, time_to_empty, time_to_full)
        }
        _ => (None, true, None, None),
    }
}

fn json_value_to_lua(lua: &Lua, value: serde_json::Value) -> mlua::Result<Value> {
    match value {
        serde_json::Value::Null => Ok(Value::Nil),
        serde_json::Value::Bool(value) => Ok(Value::Boolean(value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Ok(Value::Integer(value))
            } else if let Some(value) = value.as_u64() {
                if let Ok(value) = i64::try_from(value) {
                    Ok(Value::Integer(value))
                } else {
                    Ok(Value::Number(value as f64))
                }
            } else {
                Ok(Value::Number(value.as_f64().unwrap_or_default()))
            }
        }
        serde_json::Value::String(value) => Ok(Value::String(lua.create_string(&value)?)),
        serde_json::Value::Array(values) => {
            let table = lua.create_table_with_capacity(values.len(), 0)?;
            for (index, value) in values.into_iter().enumerate() {
                table.set(index + 1, json_value_to_lua(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
        serde_json::Value::Object(entries) => {
            let table = lua.create_table_with_capacity(0, entries.len())?;
            for (key, value) in entries {
                table.set(key, json_value_to_lua(lua, value)?)?;
            }
            Ok(Value::Table(table))
        }
    }
}

#[cfg(target_os = "macos")]
static RUN_OUTPUT_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
struct PlatformRunJobs {
    #[cfg(target_os = "macos")]
    jobs: Mutex<BTreeMap<String, Vec<PathBuf>>>,
}

impl PlatformRunJobs {
    #[cfg(target_os = "macos")]
    fn register(&self, label: &str, paths: Vec<PathBuf>) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.insert(label.to_owned(), paths);
        }
    }

    #[cfg(target_os = "macos")]
    fn unregister(&self, label: &str) {
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(label);
        }
    }

    fn cleanup(&self) {
        cleanup_platform_shell_run_jobs(self);
    }
}

#[cfg(target_os = "macos")]
const MACOS_BACKGROUND_SHELL_SCRIPT: &str = r#"cwd=$1
shell=$2
command=$3
status_path=$4
cd "$cwd" 2>/dev/null || true
"$shell" -c "$command"
status=$?
printf '%s' "$status" > "$status_path"
exit "$status"
"#;

fn shell_run_output(
    cmd: &str,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    platform_shell_run_output(cmd, run_jobs, shutdown)
}

#[cfg(target_os = "macos")]
fn platform_shell_run_output(
    cmd: &str,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    let id = RUN_OUTPUT_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let output_path =
        std::env::temp_dir().join(format!("bootty-run-{}-{id}.out", std::process::id()));
    let status_path =
        std::env::temp_dir().join(format!("bootty-run-{}-{id}.status", std::process::id()));
    let output_path_arg = output_path.to_string_lossy().into_owned();
    let status_path_arg = status_path.to_string_lossy().into_owned();
    let label = format!("dev.bootty.run.{}.{}", std::process::id(), id);
    let launchctl = resolve_shell_program("launchctl")?;
    let shell = resolve_shell_program("sh")?;
    let cwd = std::env::current_dir()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned());
    run_jobs.register(&label, vec![output_path.clone(), status_path.clone()]);
    if shutdown.load(Ordering::Relaxed) {
        run_jobs.unregister(&label);
        let _ = std::fs::remove_file(&output_path);
        let _ = std::fs::remove_file(&status_path);
        return Err(std::io::Error::other("extension host stopped"));
    }
    let script = macos_shell_run_script();
    let result = (|| {
        let status = std::process::Command::new(&launchctl)
            .args([
                "submit",
                "-l",
                &label,
                "-o",
                &output_path_arg,
                "-e",
                &output_path_arg,
                "--",
                &shell,
                "-c",
            ])
            .arg(script)
            .args(["bootty-run", &cwd, &shell, cmd, &status_path_arg])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!(
                "launchctl submit failed with {status}"
            )));
        }

        wait_for_run_status(&status_path, shutdown, Duration::from_secs(60 * 60))?;
        std::fs::read_to_string(&output_path)
    })();
    run_jobs.unregister(&label);
    let _ = std::process::Command::new(&launchctl)
        .args(["remove", &label])
        .status();
    let _ = std::fs::remove_file(&output_path);
    let _ = std::fs::remove_file(&status_path);
    result
}

#[cfg(target_os = "macos")]
fn wait_for_run_status(
    status_path: &Path,
    shutdown: &AtomicBool,
    timeout: Duration,
) -> std::io::Result<i32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("extension host stopped"));
        }
        if let Ok(raw) = std::fs::read_to_string(status_path) {
            let status = raw.trim();
            if !status.is_empty() {
                return status.parse::<i32>().map_err(std::io::Error::other);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "bootty.run command did not finish before timeout",
    ))
}

#[cfg(target_os = "macos")]
fn cleanup_platform_shell_run_jobs(run_jobs: &PlatformRunJobs) {
    let jobs = run_jobs
        .jobs
        .lock()
        .map(|jobs| jobs.clone())
        .unwrap_or_default();
    if jobs.is_empty() {
        return;
    }
    let launchctl = resolve_shell_program("launchctl").ok();
    for (label, paths) in jobs {
        if let Some(launchctl) = &launchctl {
            let _ = std::process::Command::new(launchctl)
                .args(["remove", &label])
                .status();
        }
        for path in paths {
            let _ = std::fs::remove_file(path);
        }
        run_jobs.unregister(&label);
    }
}

#[cfg(not(target_os = "macos"))]
fn cleanup_platform_shell_run_jobs(_run_jobs: &PlatformRunJobs) {}

#[cfg(target_os = "macos")]
fn cleanup_stale_platform_shell_run_jobs() {
    let Ok(launchctl) = resolve_shell_program("launchctl") else {
        return;
    };
    let Ok(output) = std::process::Command::new(&launchctl).arg("list").output() else {
        return;
    };
    let listing = String::from_utf8_lossy(&output.stdout);
    for line in listing.lines() {
        let Some(label) = line
            .split_whitespace()
            .find(|field| field.starts_with("dev.bootty.run."))
        else {
            continue;
        };
        let Some(owner_pid) = stale_run_job_owner_pid(label) else {
            continue;
        };
        if owner_pid == std::process::id() || process_exists(owner_pid) {
            continue;
        }
        let _ = std::process::Command::new(&launchctl)
            .args(["remove", label])
            .status();
    }
}

#[cfg(not(target_os = "macos"))]
fn cleanup_stale_platform_shell_run_jobs() {}

#[cfg(target_os = "macos")]
fn stale_run_job_owner_pid(label: &str) -> Option<u32> {
    let rest = label.strip_prefix("dev.bootty.run.")?;
    rest.split('.').next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "macos")]
fn macos_shell_run_environment_from<I, K, V>(
    current: I,
    login_env: Option<Vec<(String, String)>>,
) -> BTreeMap<OsString, OsString>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut env = current
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect::<BTreeMap<OsString, OsString>>();
    if let Some(login_env) = login_env {
        for (key, value) in login_env {
            let key = OsString::from(key);
            if key == "PATH" {
                let login_path = OsString::from(value);
                let current_path = env.get(&key);
                env.insert(
                    key,
                    login_path_with_current_fallbacks(login_path, current_path),
                );
            } else {
                env.entry(key).or_insert_with(|| OsString::from(value));
            }
        }
    }
    env
}

#[cfg(target_os = "macos")]
fn login_path_with_current_fallbacks(
    login_path: OsString,
    current_path: Option<&OsString>,
) -> OsString {
    let mut entries = std::env::split_paths(&login_path).collect::<Vec<_>>();
    if let Some(current_path) = current_path {
        for path in std::env::split_paths(current_path) {
            if !entries.contains(&path) {
                entries.push(path);
            }
        }
    }
    std::env::join_paths(entries).unwrap_or(login_path)
}

#[cfg(target_os = "macos")]
fn macos_shell_run_script() -> String {
    let env = macos_shell_run_environment_from(
        std::env::vars_os(),
        crate::shell_env::login_shell_environment(),
    );
    let mut script = bootty_mux::process::macos_shell_environment_prelude_from(env);
    script.push_str(MACOS_BACKGROUND_SHELL_SCRIPT);
    script
}

#[cfg(target_os = "macos")]
fn resolve_shell_program(program: &str) -> std::io::Result<String> {
    bootty_mux::process::resolve_program(program).map_err(std::io::Error::other)
}

#[cfg(windows)]
fn platform_shell_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(not(windows))]
fn platform_shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn platform_shell_run_output(
    cmd: &str,
    _run_jobs: &PlatformRunJobs,
    _shutdown: &AtomicBool,
) -> std::io::Result<String> {
    use std::os::windows::process::CommandExt;

    let output = std::process::Command::new("cmd")
        .creation_flags(windows_no_window_flag())
        .raw_arg(format!("/S /C {cmd}"))
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(windows)]
const fn windows_no_window_flag() -> u32 {
    0x0800_0000
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn platform_shell_run_output(
    cmd: &str,
    _run_jobs: &PlatformRunJobs,
    _shutdown: &AtomicBool,
) -> std::io::Result<String> {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn setup_lua(
    theme: &[(String, String)],
    mux: Arc<RwLock<MuxView>>,
    metrics: Arc<RwLock<Metrics>>,
    session_reorders: Arc<RwLock<Vec<SessionReorder>>>,
    run_cache: Arc<RunCache>,
) -> mlua::Result<Lua> {
    let lua = Lua::new();
    let bootty = lua.create_table()?;

    // Shell out and return trimmed stdout, via the platform shell. Prefer
    // `bootty.metrics()` for system stats, which is native and cross-platform.
    // Render phases return cached output immediately and refresh in the background,
    // so a slow provider/command cannot block unrelated modules.
    let run_shell_cache = Arc::clone(&run_cache);
    bootty.set(
        "run",
        lua.create_function(move |_, cmd: String| {
            run_shell_cache.run(&cmd).map_err(mlua::Error::external)
        })?,
    )?;
    let shell_table = lua.create_table()?;
    let shell_run_cache = Arc::clone(&run_cache);
    shell_table.set(
        "run",
        lua.create_function(move |_, cmd: String| {
            shell_run_cache.run(&cmd).map_err(mlua::Error::external)
        })?,
    )?;
    shell_table.set(
        "quote",
        lua.create_function(|_, value: String| Ok(platform_shell_quote(&value)))?,
    )?;
    shell_table.set(
        "stderr_null",
        if cfg!(windows) {
            "2>nul"
        } else {
            "2>/dev/null"
        },
    )?;
    shell_table.set_readonly(true);
    bootty.set("shell", shell_table)?;

    let path_table = lua.create_table()?;
    path_table.set(
        "display",
        lua.create_function(|_, value: String| Ok(crate::strings::display_path(&value)))?,
    )?;
    path_table.set_readonly(true);
    bootty.set("path", path_table)?;

    let codexbar_cache = Arc::clone(&run_cache);
    bootty.set(
        "codexbar_usage",
        lua.create_function(move |_, provider: String| {
            codexbar_cache
                .codexbar_usage(&provider)
                .map_err(mlua::Error::external)
        })?,
    )?;

    bootty.set(
        "time",
        lua.create_function(|_, ()| {
            Ok(SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0.0, |duration| duration.as_secs_f64()))
        })?,
    )?;

    let json_table = lua.create_table()?;
    json_table.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let value = serde_json::from_str(&text).map_err(mlua::Error::external)?;
            json_value_to_lua(lua, value)
        })?,
    )?;
    json_table.set_readonly(true);
    bootty.set("json", json_table)?;

    // Mux state: the active session's windows, and the session name.
    let windows_mux = Arc::clone(&mux);
    bootty.set(
        "windows",
        lua.create_function(move |lua, ()| {
            let array = lua.create_table()?;
            if let Ok(view) = windows_mux.read() {
                for (index, window) in view.windows.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", window.id.as_str())?;
                    entry.set("index", window.index)?;
                    entry.set("name", window.name.as_str())?;
                    entry.set("active", window.active)?;
                    entry.set("progress", window.progress)?;
                    entry.set("progress_indeterminate", window.progress_indeterminate)?;
                    array.set(index + 1, entry)?;
                }
            }
            Ok(array)
        })?,
    )?;
    let sessions_mux = Arc::clone(&mux);
    bootty.set(
        "sessions",
        lua.create_function(move |lua, ()| {
            let array = lua.create_table()?;
            if let Ok(view) = sessions_mux.read() {
                for (index, session) in view.sessions.iter().enumerate() {
                    let entry = lua.create_table()?;
                    entry.set("id", session.id.as_str())?;
                    entry.set("name", session.name.as_str())?;
                    entry.set("active", session.active)?;
                    entry.set("selected", session.selected)?;
                    entry.set("progress", session.progress)?;
                    entry.set("progress_indeterminate", session.progress_indeterminate)?;
                    let progresses = lua.create_table()?;
                    for (progress_index, progress) in session.progresses.iter().enumerate() {
                        let progress_entry = lua.create_table()?;
                        progress_entry.set("process", progress.process.as_str())?;
                        progress_entry.set("value", progress.value)?;
                        progress_entry.set("indeterminate", progress.indeterminate)?;
                        progresses.set(progress_index + 1, progress_entry)?;
                    }
                    entry.set("progresses", progresses)?;
                    let ports = lua.create_table()?;
                    for (port_index, port) in session.ports.iter().enumerate() {
                        ports.set(port_index + 1, *port)?;
                    }
                    entry.set("ports", ports)?;
                    if let Some(value) = &session.cwd {
                        entry.set("cwd", value.as_str())?;
                    }
                    if let Some(value) = &session.color {
                        entry.set("color", value.as_str())?;
                    }
                    if let Some(value) = &session.dim_color {
                        entry.set("dim_color", value.as_str())?;
                    }
                    array.set(index + 1, entry)?;
                }
            }
            Ok(array)
        })?,
    )?;
    let session_mux = Arc::clone(&mux);
    bootty.set(
        "session",
        lua.create_function(move |_, ()| {
            Ok(session_mux
                .read()
                .ok()
                .and_then(|view| view.session.clone()))
        })?,
    )?;
    let color_mux = Arc::clone(&mux);
    bootty.set(
        "session_color",
        lua.create_function(move |_, ()| {
            Ok(color_mux
                .read()
                .ok()
                .and_then(|view| view.session_color.clone()))
        })?,
    )?;
    let awake_mux = Arc::clone(&mux);
    bootty.set(
        "awake",
        lua.create_function(move |_, ()| {
            Ok(awake_mux
                .read()
                .map(|view| view.keep_awake)
                .unwrap_or(false))
        })?,
    )?;

    // Ask Bootty to apply a session-order change to its native session-order store. Modules
    // call this from `on_reorder` to reorder bootty-owned sessions; the app drains and applies
    // it on the main thread. `before` nil means "move to the end".
    bootty.set(
        "reorder_session",
        lua.create_function(move |_, (source, before): (String, Option<String>)| {
            if let Ok(mut queue) = session_reorders.write() {
                queue.push(SessionReorder { source, before });
            }
            Ok(())
        })?,
    )?;

    // Floating windows: a module opens a native picker/prompt via `bootty.window.open{...}`
    // and receives the user's choice through the spec's `on_action(key, value)` handler.
    // The handler stays on this worker; only renderable data crosses to the UI thread.
    let window_table = lua.create_table()?;
    window_table.set(
        "open",
        lua.create_function(|_, spec: Table| {
            Ok(WINDOW_QUEUE.with(|queue| {
                let Some((requests, next_id)) = queue.borrow().clone() else {
                    return 0u64;
                };
                let id = next_id.fetch_add(1, Ordering::Relaxed);
                if let Ok(handler) = spec.get::<Function>("on_action") {
                    WINDOW_HANDLERS.with(|handlers| handlers.borrow_mut().insert(id, handler));
                }
                if let Ok(mut requests) = requests.write() {
                    requests.push(WindowRequest::Open(parse_window_spec(id, &spec)));
                }
                id
            }))
        })?,
    )?;
    window_table.set(
        "close",
        lua.create_function(|_, ()| {
            WINDOW_QUEUE.with(|queue| {
                if let Some((requests, _)) = queue.borrow().as_ref()
                    && let Ok(mut requests) = requests.write()
                {
                    requests.push(WindowRequest::Close);
                }
            });
            Ok(())
        })?,
    )?;
    window_table.set_readonly(true);
    bootty.set("window", window_table)?;

    // Native, cross-platform system metrics. `load1` is 0 where the OS has no load
    // average (e.g. Windows); fall back to `cpu` there. `mem_pct` is the used
    // percentage (real memory pressure on macOS); `mem_used`/`mem_total` are GiB
    // and stay consistent with `mem_pct`.
    bootty.set(
        "metrics",
        lua.create_function(move |lua, ()| {
            let m = metrics.read().map(|m| *m).unwrap_or_default();
            let table = lua.create_table()?;
            table.set("cpu", m.cpu)?;
            table.set("load1", m.load1)?;
            let total_gib = m.mem_total_bytes as f64 / 1_073_741_824.0;
            table.set("mem_total", total_gib)?;
            table.set("mem_pct", m.mem_used_pct)?;
            table.set("mem_used", total_gib * m.mem_used_pct / 100.0)?;
            if let Some(secs) = m.battery_time_to_empty_secs {
                table.set("battery_time_to_empty", secs)?;
            }
            if let Some(secs) = m.battery_time_to_full_secs {
                table.set("battery_time_to_full", secs)?;
            }
            // `battery` is nil on a machine with no battery; `on_ac` is true when
            // plugged in / charging / full (or no battery).
            if let Some(percent) = m.battery_percent {
                table.set("battery", percent)?;
            }
            table.set("on_ac", m.on_ac)?;
            Ok(table)
        })?,
    )?;

    let ui_table: Table = lua
        .load(EXTENSION_UI_PRELUDE)
        .set_name("bootty.ui")
        .eval()?;
    ui_table.set(
        "shell_quote",
        lua.create_function(|_, value: String| Ok(platform_shell_quote(&value)))?,
    )?;
    ui_table.set(
        "stderr_null",
        if cfg!(windows) {
            "2>nul"
        } else {
            "2>/dev/null"
        },
    )?;
    ui_table.set_readonly(true);
    bootty.set("ui", ui_table)?;
    let sidebar_table: Table = lua
        .load(SIDEBAR_FACTS_PRELUDE)
        .set_name("bootty.sidebar")
        .eval()?;
    let sidebar_mux = Arc::clone(&mux);
    sidebar_table.set(
        "visible",
        lua.create_function(move |_, ()| {
            Ok(sidebar_mux
                .read()
                .map(|view| view.sidebar_visible)
                .unwrap_or(false))
        })?,
    )?;
    sidebar_table.set_readonly(true);
    bootty.set("sidebar", sidebar_table)?;

    // Palette tokens so modules style with theme colors: `fg = bootty.theme.accent`.
    let theme_table = lua.create_table()?;
    for (name, hex) in theme {
        theme_table.set(name.as_str(), hex.as_str())?;
    }
    theme_table.set_readonly(true);
    bootty.set("theme", theme_table)?;
    bootty.set_readonly(true);

    lua.globals().set("bootty", bootty)?;
    lua.sandbox(true)?;
    Ok(lua)
}

fn module_environment(lua: &Lua) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let metatable = lua.create_table()?;
    metatable.set("__index", lua.globals())?;
    env.set_metatable(Some(metatable))?;
    env.set_safeenv(true);
    Ok(env)
}

fn load_modules(
    lua: &Lua,
    dir: &Path,
    builtins: &'static [(&'static str, &'static str)],
) -> Vec<LoadedModule> {
    let mut sources = builtins
        .iter()
        .map(|(name, source)| ((*name).to_owned(), Ok((*source).to_owned())))
        .collect::<BTreeMap<_, _>>();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_extension_module_file(&path) {
                continue;
            }
            if let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) {
                let source =
                    std::fs::read_to_string(&path).map_err(|error| first_line(&error.to_string()));
                sources.insert(name.to_owned(), source);
            }
        }
    }
    sources
        .into_iter()
        .map(|(name, source)| match source {
            Ok(code) => match module_environment(lua).and_then(|env| {
                lua.load(&code)
                    .set_name(&name)
                    .set_environment(env)
                    .eval::<Value>()
            }) {
                Ok(value) => loaded_module_from_value(name.clone(), value).unwrap_or_else(|| {
                    load_error(
                        name,
                        "must return a function or { render = ... }".to_owned(),
                    )
                }),
                Err(error) => load_error(name, first_line(&error.to_string())),
            },
            Err(error) => load_error(name, error),
        })
        .collect()
}

fn load_error(name: String, message: String) -> LoadedModule {
    LoadedModule {
        interval: DEFAULT_INTERVAL,
        body: ModuleBody::LoadError(format!("{name}: {message}")),
        on_reorder: None,
        name,
        last_run: None,
    }
}

fn loaded_module_from_value(name: String, value: Value) -> Option<LoadedModule> {
    match value {
        Value::Function(render) => Some(LoadedModule {
            name,
            interval: DEFAULT_INTERVAL,
            body: ModuleBody::Render(render),
            on_reorder: None,
            last_run: None,
        }),
        Value::Table(table) => {
            let render: Function = table.get("render").ok()?;
            let interval = table
                .get::<f64>("interval")
                .ok()
                .filter(|secs| *secs > 0.0)
                .map_or(DEFAULT_INTERVAL, Duration::from_secs_f64);
            let on_reorder = table.get::<Function>("on_reorder").ok();
            Some(LoadedModule {
                name,
                interval,
                body: ModuleBody::Render(render),
                on_reorder,
                last_run: None,
            })
        }
        _ => None,
    }
}

fn run_module(body: &ModuleBody) -> Vec<ModuleItem> {
    match body {
        ModuleBody::Render(render) => match render.call::<Value>(()) {
            Ok(value) => items_from_value(value),
            Err(error) => vec![error_item(&error.to_string())],
        },
        ModuleBody::LoadError(message) => vec![error_item(message)],
    }
}

fn error_item(message: &str) -> ModuleItem {
    ModuleItem {
        text: first_line(message),
        fg: Some(ERROR_COLOR),
        ..ModuleItem::default()
    }
}

fn items_from_value(value: Value) -> Vec<ModuleItem> {
    match value {
        Value::String(text) => vec![ModuleItem {
            text: text.to_string_lossy(),
            ..ModuleItem::default()
        }],
        Value::Table(table) => {
            // Item text is optional; icon/gauge/action-only tables are single items too.
            if table_looks_like_item(&table) {
                vec![item_from_table(&table)]
            } else {
                table
                    .sequence_values::<Table>()
                    .filter_map(Result::ok)
                    .map(|item| item_from_table(&item))
                    .collect()
            }
        }
        _ => Vec::new(),
    }
}

fn table_looks_like_item(table: &Table) -> bool {
    [
        "text",
        "fg",
        "bg",
        "stroke",
        "icon",
        "gauge",
        "primitives",
        "pad_left",
        "pad_right",
        "join",
        "gap",
        "action",
        "key",
        "kind",
        "number",
        "indent",
        "tree",
        "selectable",
        "session_id",
        "reorder_anchor",
        "current",
        "active",
        "dim_fg",
    ]
    .into_iter()
    .any(|key| table.contains_key(key).unwrap_or(false))
}

fn item_from_table(table: &Table) -> ModuleItem {
    ModuleItem {
        text: table.get::<String>("text").unwrap_or_default(),
        fg: color_field(table, "fg"),
        bg: color_field(table, "bg"),
        stroke: color_field(table, "stroke"),
        icon: string_field(table, "icon"),
        gauge: table
            .get::<f64>("gauge")
            .ok()
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0) as f32),
        primitives: table
            .get::<Table>("primitives")
            .ok()
            .map(|primitives| primitives_from_table(&primitives))
            .unwrap_or_default(),
        pad_left: table
            .get::<f64>("pad_left")
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .max(0.0) as f32,
        pad_right: table
            .get::<f64>("pad_right")
            .ok()
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .max(0.0) as f32,
        join: table.get::<bool>("join").ok(),
        gap: table.get::<bool>("gap").ok(),
        action: string_field(table, "action"),
        key: string_field(table, "key"),
        kind: string_field(table, "kind"),
        number: table.get::<u32>("number").ok().map(|value| value as usize),
        indent: table.get::<u16>("indent").ok(),
        tree: string_field(table, "tree"),
        selectable: table.get::<bool>("selectable").ok(),
        session_id: string_field(table, "session_id"),
        reorder_anchor: string_field(table, "reorder_anchor"),
        current: table.get::<bool>("current").ok(),
        active: table.get::<bool>("active").ok(),
        dim_fg: color_field(table, "dim_fg"),
    }
}

fn string_field(table: &Table, key: &str) -> Option<String> {
    table
        .get::<String>(key)
        .ok()
        .filter(|value| !value.is_empty())
}

fn color_field(table: &Table, key: &str) -> Option<Color32> {
    table
        .get::<String>(key)
        .ok()
        .and_then(|hex| parse_hex_color(&hex))
}

fn primitives_from_table(table: &Table) -> Vec<ModulePrimitive> {
    table
        .sequence_values::<Table>()
        .filter_map(Result::ok)
        .filter_map(|primitive| primitive_from_table(&primitive))
        .collect()
}

fn primitive_from_table(table: &Table) -> Option<ModulePrimitive> {
    let kind = table
        .get::<String>("type")
        .or_else(|_| table.get::<String>("kind"))
        .ok()?;
    let fill = table
        .get::<String>("fill")
        .ok()
        .and_then(|hex| parse_hex_color(&hex));
    let stroke = table
        .get::<String>("stroke")
        .ok()
        .and_then(|hex| parse_hex_color(&hex));
    match kind.as_str() {
        "rect" => Some(ModulePrimitive::Rect {
            fill,
            stroke,
            x: coord_from_table(table, "x", "x_px", 0.0),
            y: coord_from_table(table, "y", "y_px", 0.0),
            w: coord_from_table(table, "w", "w_px", 1.0),
            h: coord_from_table(table, "h", "h_px", 1.0),
            radius: radius_from_table(table),
        }),
        "polygon" => {
            let points = table
                .get::<Table>("points")
                .ok()?
                .sequence_values::<Table>()
                .filter_map(Result::ok)
                .map(|point| {
                    (
                        coord_from_table(&point, "x", "dx", 0.0),
                        coord_from_table(&point, "y", "dy", 0.0),
                    )
                })
                .collect::<Vec<_>>();
            (points.len() >= 3).then_some(ModulePrimitive::Polygon {
                fill,
                stroke,
                points,
            })
        }
        "text" => {
            let text = string_field(table, "text")?;
            Some(ModulePrimitive::Text {
                text,
                color: color_field(table, "color").or(fill),
                x: coord_from_table(table, "x", "x_px", 0.0),
                y: coord_from_table(table, "y", "y_px", 0.5),
                size: positive_f32_field(table, "size").unwrap_or(11.0),
                align: string_field(table, "align").unwrap_or_else(|| "left_center".to_owned()),
                min_width: positive_f32_field(table, "min_width"),
            })
        }
        "icon" => {
            let icon = string_field(table, "icon").or_else(|| string_field(table, "slug"))?;
            Some(ModulePrimitive::Icon {
                icon,
                color: color_field(table, "color").or(fill),
                x: coord_from_table(table, "x", "x_px", 0.0),
                y: coord_from_table(table, "y", "y_px", 0.5),
                size: positive_f32_field(table, "size").unwrap_or(12.0),
                min_width: positive_f32_field(table, "min_width"),
            })
        }
        _ => None,
    }
}

fn coord_from_table(table: &Table, frac_key: &str, px_key: &str, default_frac: f32) -> ModuleCoord {
    let frac = table
        .get::<f64>(frac_key)
        .ok()
        .filter(|value| value.is_finite())
        .map_or(default_frac, |value| value as f32);
    let px = table
        .get::<f64>(px_key)
        .ok()
        .filter(|value| value.is_finite())
        .map_or(0.0, |value| value as f32);
    ModuleCoord { frac, px }
}

fn positive_f32_field(table: &Table, key: &str) -> Option<f32> {
    table
        .get::<f64>(key)
        .ok()
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value as f32)
}

fn radius_from_table(table: &Table) -> ModuleCornerRadius {
    if let Ok(radius) = table.get::<f64>("radius") {
        let radius = radius.clamp(0.0, u8::MAX as f64) as u8;
        return egui::CornerRadius {
            nw: radius,
            ne: radius,
            sw: radius,
            se: radius,
        };
    }
    let Ok(radius) = table.get::<Table>("radius") else {
        return egui::CornerRadius::default();
    };
    let corner = |key: &str| {
        radius
            .get::<f64>(key)
            .ok()
            .filter(|value| value.is_finite())
            .map_or(0, |value| value.clamp(0.0, u8::MAX as f64) as u8)
    };
    egui::CornerRadius {
        nw: corner("nw"),
        ne: corner("ne"),
        sw: corner("sw"),
        se: corner("se"),
    }
}

fn parse_hex_color(value: &str) -> Option<Color32> {
    let hex = value.trim().strip_prefix('#')?;
    match hex.len() {
        6 => {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        3 => {
            let expand = |slice: &str| u8::from_str_radix(slice, 16).map(|v| v * 17);
            let r = expand(&hex[0..1]).ok()?;
            let g = expand(&hex[1..2]).ok()?;
            let b = expand(&hex[2..3]).ok()?;
            Some(Color32::from_rgb(r, g, b))
        }
        _ => None,
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn parse_window_spec_defaults_kind_and_reads_rows() {
        let lua = Lua::new();
        let spec = lua.create_table().expect("spec table");
        spec.set("title", "Pick a server").expect("title");
        let rows = lua.create_table().expect("rows table");
        let row = lua.create_table().expect("row table");
        row.set("key", "restart").expect("key");
        row.set("text", "Restart").expect("text");
        rows.set(1, row).expect("push row");
        spec.set("rows", rows).expect("rows");

        let parsed = parse_window_spec(7, &spec);
        assert_eq!(parsed.id, 7);
        assert_eq!(parsed.kind, "list"); // defaulted when the module omits `kind`
        assert_eq!(parsed.title, "Pick a server");
        assert_eq!(parsed.rows.len(), 1);
        assert_eq!(parsed.rows[0].key, "restart");
        assert_eq!(parsed.rows[0].text, "Restart");
    }

    fn run_source(source: &str) -> Vec<ModuleItem> {
        let theme = [("accent".to_owned(), "#89b4fa".to_owned())];
        let lua = setup_lua(
            &theme,
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let value = lua.load(source).eval::<Value>().unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        run_module(&module.body)
    }

    #[test]
    fn footer_meter_track_is_optional() {
        let items = run_source(
            r##"return function()
                return {
                    bootty.ui.footer_meter({ icon = "openai", color = "#a6e3a1", fill = 0.5 }),
                    bootty.ui.footer_meter({ icon = "openai", color = "#a6e3a1", track = "#313244", fill = 0.5 }),
                }
            end"##,
        );
        let rect_fills = |item: &ModuleItem| {
            item.primitives
                .iter()
                .filter_map(|primitive| match primitive {
                    ModulePrimitive::Rect { fill, .. } => *fill,
                    _ => None,
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            rect_fills(&items[0]),
            vec![Color32::from_rgb(0xa6, 0xe3, 0xa1)]
        );
        assert_eq!(
            rect_fills(&items[1]),
            vec![
                Color32::from_rgb(0x31, 0x32, 0x44),
                Color32::from_rgb(0xa6, 0xe3, 0xa1),
            ]
        );
    }

    #[cfg(unix)]
    struct PathGuard(std::ffi::OsString);

    #[cfg(unix)]
    impl Drop for PathGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::set_var("PATH", &self.0);
            }
        }
    }

    #[cfg(unix)]
    fn prepend_path(path: &Path) -> PathGuard {
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let next_path = std::env::join_paths(
            std::iter::once(path.to_path_buf()).chain(std::env::split_paths(&old_path)),
        )
        .expect("join PATH");
        unsafe {
            std::env::set_var("PATH", next_path);
        }
        PathGuard(old_path)
    }

    #[test]
    fn built_in_luau_modules_load_without_user_files() {
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");

        assert_builtins_load(
            &lua,
            dir.path(),
            BUILTIN_STATUS_EXTENSIONS,
            &["windows", "clock", "session", "sysinfo"],
        );
        assert_builtins_load(
            &lua,
            dir.path(),
            BUILTIN_SIDEBAR_EXTENSIONS,
            &["sessions", "codexbar"],
        );
    }

    #[test]
    fn built_in_session_only_renders_when_sidebar_is_hidden() {
        let mux = Arc::new(RwLock::new(MuxView {
            session: Some("work/api".to_owned()),
            sidebar_visible: true,
            ..MuxView::default()
        }));
        let lua = setup_lua(
            &[("accent".to_owned(), "#89b4fa".to_owned())],
            Arc::clone(&mux),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .expect("setup lua");
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let session = modules
            .iter()
            .find(|module| module.name == "session")
            .expect("session module");

        assert!(run_module(&session.body).is_empty());
        mux.write().expect("mux").sidebar_visible = false;
        assert_eq!(run_module(&session.body)[0].text, "work/api");
    }

    #[test]
    fn forced_cached_render_does_not_delay_next_interval_refresh() {
        let now = Instant::now();
        let mut last_run = None;

        record_module_interval_run(true, &mut last_run, now);
        assert_eq!(last_run, None);

        record_module_interval_run(false, &mut last_run, now);
        assert_eq!(last_run, Some(now));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_shell_run_environment_uses_login_path_with_current_path_fallbacks() {
        let env = macos_shell_run_environment_from(
            [
                (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
                (OsString::from("HOME"), OsString::from("/Users/live")),
            ],
            Some(vec![
                ("PATH".to_owned(), "/opt/bin:/usr/bin".to_owned()),
                ("HOME".to_owned(), "/Users/login".to_owned()),
                ("BOOTTY_ENV_PROBE".to_owned(), "login".to_owned()),
            ]),
        );

        assert_eq!(
            env.get(&OsString::from("PATH")),
            Some(&OsString::from("/opt/bin:/usr/bin:/bin"))
        );
        assert_eq!(
            env.get(&OsString::from("HOME")),
            Some(&OsString::from("/Users/live"))
        );
        assert_eq!(
            env.get(&OsString::from("BOOTTY_ENV_PROBE")),
            Some(&OsString::from("login"))
        );
    }

    #[test]
    fn keep_awake_mux_change_forces_status_module_rerender() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("awake_probe.luau"),
            "return { interval = 60, render = function() return tostring(bootty.awake()) end }",
        )
        .expect("write awake probe module");
        let host = ExtensionHost::spawn_status(
            dir.path().to_path_buf(),
            egui::Context::default(),
            Vec::new(),
        );
        host.set_active(["awake_probe".to_owned()]);
        host.update_mux(MuxView {
            keep_awake: false,
            ..MuxView::default()
        });

        assert!(wait_for_host_text(
            &host,
            "awake_probe",
            "false",
            Duration::from_secs(2)
        ));

        host.update_mux(MuxView {
            keep_awake: true,
            ..MuxView::default()
        });

        assert!(
            wait_for_host_text(&host, "awake_probe", "true", Duration::from_millis(500)),
            "keep-awake changes should re-render without waiting for the module interval"
        );
    }

    #[test]
    fn dropping_extension_host_does_not_wait_for_blocked_run_callback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let gate = dir.path().join("gate");
        let done = dir.path().join("done");
        let command = blocking_file_command(&started, &gate, &done);
        std::fs::write(
            dir.path().join("blocker.luau"),
            format!(
                "return {{ interval = 0, render = function() return bootty.run({command:?}) end }}"
            ),
        )
        .expect("write blocking module");
        let host = ExtensionHost::spawn_status(
            dir.path().to_path_buf(),
            egui::Context::default(),
            Vec::new(),
        );
        host.set_active(["blocker".to_owned()]);

        assert!(wait_for_path(&started, Duration::from_secs(2)));
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(host);
            dropped_tx.send(()).expect("drop signal");
        });

        let dropped_before_gate = dropped_rx.recv_timeout(Duration::from_millis(100)).is_ok();
        std::fs::write(&gate, "").expect("open blocking command gate");

        assert!(
            dropped_before_gate,
            "ExtensionHost drop must not join a worker blocked in bootty.run"
        );
        #[cfg(target_os = "macos")]
        assert!(
            !wait_for_path(&done, Duration::from_millis(200)),
            "dropping the host should cancel macOS launchd shell jobs"
        );
        #[cfg(not(target_os = "macos"))]
        assert!(wait_for_path(&done, Duration::from_secs(2)));
    }

    #[cfg(unix)]
    fn shell_quote(path: &Path) -> String {
        format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
    }

    #[cfg(windows)]
    fn cmd_quote(path: &Path) -> String {
        platform_shell_quote(&path.display().to_string())
    }

    #[cfg(windows)]
    fn blocking_file_command(started: &Path, gate: &Path, done: &Path) -> String {
        format!(
            "type nul > {} & for /l %i in (0,0,1) do @if exist {} (type nul > {} & exit /b 0) else (ping -n 2 127.0.0.1 >nul)",
            cmd_quote(started),
            cmd_quote(gate),
            cmd_quote(done),
        )
    }

    #[cfg(not(windows))]
    fn blocking_file_command(started: &Path, gate: &Path, done: &Path) -> String {
        format!(
            "touch {}; while [ ! -f {} ]; do sleep 0.05; done; touch {}",
            shell_quote(started),
            shell_quote(gate),
            shell_quote(done),
        )
    }

    fn wait_for_path(path: &Path, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn wait_for_host_text(
        host: &ExtensionHost,
        module: &str,
        expected: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if host.items(module).iter().any(|item| item.text == expected) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }
    #[cfg(unix)]
    fn wait_for_cached_output(
        cache: &RunCache,
        cmd: &str,
        expected: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cache.cached(cmd).as_deref() == Some(expected) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    // Only the macOS-gated run_cache_refresh_keeps_shell_out_errors_visible calls this;
    // gate it identically so non-macOS targets don't see it as dead code.
    #[cfg(target_os = "macos")]
    fn wait_for_cached_output_containing(
        cache: &RunCache,
        needle: &str,
        timeout: Duration,
    ) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cache
                .entries
                .lock()
                .is_ok_and(|entries| entries.values().any(|entry| entry.output.contains(needle)))
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn codexbar_builtin_renders_a_row_per_configured_provider() {
        // Exercise the render against pre-seeded CodexBar server responses so the test is
        // deterministic and touches no PATH/launchd state shared with other tests. The builtin
        // must emit a 5h and a 7d row per entry in its PROVIDERS table, in order: guards the
        // multi-provider default (codex + claude) against a provider being dropped, mislabeled,
        // or misordered.
        let run_cache = Arc::new(RunCache::default());
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::clone(&run_cache),
        )
        .expect("setup lua");
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_SIDEBAR_EXTENSIONS);
        let codexbar = modules
            .iter()
            .find(|module| module.name == "codexbar")
            .expect("codexbar builtin loaded");

        const PROBE_JSON: &str = r#"[{"usage":{"primary":{"usedPercent":25,"windowMinutes":300},"secondary":{"usedPercent":50,"windowMinutes":10080}}}]"#;
        for provider in ["codex", "claude"] {
            run_cache.codexbar.set_mock_usage(provider, PROBE_JSON);
        }
        let items = run_module(&codexbar.body);
        let texts = items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            texts,
            vec!["codex 5h", "codex 7d", "claude 5h", "claude 7d"]
        );
        assert!(items.iter().all(|item| {
            item.primitives
                .iter()
                .filter(|primitive| matches!(primitive, ModulePrimitive::Rect { .. }))
                .count()
                == 1
        }));
    }

    #[test]
    fn codexbar_usage_returns_cached_value_without_waiting_for_refresh() {
        let run_cache = Arc::new(RunCache::default());
        run_cache
            .codexbar
            .entries
            .lock()
            .expect("codexbar entries")
            .insert(
                "claude".to_owned(),
                CodexBarEntry {
                    output: "cached".to_owned(),
                    refreshing: true,
                    last_refresh: None,
                },
            );

        assert_eq!(run_cache.codexbar_usage("claude").unwrap(), "cached");
    }

    #[test]
    fn codexbar_usage_does_not_refresh_during_cached_render() {
        let run_cache = Arc::new(RunCache::default());
        run_cache
            .codexbar
            .entries
            .lock()
            .expect("codexbar entries")
            .insert(
                "claude".to_owned(),
                CodexBarEntry {
                    output: "cached".to_owned(),
                    refreshing: false,
                    last_refresh: None,
                },
            );
        run_cache.set_mode(RunMode::Cached);

        assert_eq!(run_cache.codexbar_usage("claude").unwrap(), "cached");
        assert!(
            !run_cache
                .codexbar
                .entries
                .lock()
                .expect("codexbar entries")
                .get("claude")
                .expect("claude entry")
                .refreshing
        );
    }

    #[test]
    fn codexbar_refresh_is_throttled_per_provider() {
        let client = CodexBarClient::default();

        assert!(client.mark_refreshing("claude", CODEXBAR_REFRESH_INTERVAL));
        assert!(!client.mark_refreshing("claude", CODEXBAR_REFRESH_INTERVAL));
        assert!(client.mark_refreshing("codex", CODEXBAR_REFRESH_INTERVAL));
    }

    #[test]
    fn http_response_body_reads_content_length_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 15\r\n\r\n{\"ok\":true}\n";

        assert_eq!(http_response_body(response).unwrap(), "{\"ok\":true}\n");
    }

    #[test]
    fn http_response_body_decodes_chunked_response() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nwiki\r\n5\r\npedia\r\n0\r\n\r\n";

        assert_eq!(http_response_body(response).unwrap(), "wikipedia");
    }

    #[test]
    fn codexbar_provider_rejects_url_injection() {
        assert!(validate_codexbar_provider("claude").is_ok());
        assert!(validate_codexbar_provider("claude&provider=all").is_err());
    }

    #[test]
    fn codexbar_usage_shell_outs_are_reserved() {
        assert!(command_invokes_codexbar_usage(
            "codexbar usage --provider claude --format json"
        ));
        assert!(command_invokes_codexbar_usage(
            "out=$(/opt/homebrew/bin/codexbar usage --provider claude)"
        ));
        assert!(!command_invokes_codexbar_usage("printf codexbar usage"));
        assert!(!command_invokes_codexbar_usage("codexbar --version"));
    }

    #[test]
    fn bootty_run_rejects_codexbar_usage_before_refresh() {
        let run_cache = Arc::new(RunCache::default());
        let cmd = "codexbar usage --provider claude --format json";

        run_cache.set_mode(RunMode::Refresh);
        let error = run_cache.run(cmd).expect_err("codexbar shell-out rejected");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            run_cache
                .entries
                .lock()
                .expect("run entries")
                .get(cmd)
                .is_none()
        );
    }

    #[test]
    fn shell_run_output_returns_stdout_text() {
        assert_eq!(
            shell_run_output(
                stdout_command("bootty-run").as_str(),
                &PlatformRunJobs::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
            "bootty-run"
        );
    }

    #[cfg(windows)]
    fn stdout_command(text: &str) -> String {
        format!("echo|set /p x={text}")
    }

    #[cfg(not(windows))]
    fn stdout_command(text: &str) -> String {
        format!("printf {}", platform_shell_quote(text))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_run_output_captures_launchd_job_stderr() {
        assert_eq!(
            shell_run_output(
                "printf bootty-stderr >&2",
                &PlatformRunJobs::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
            "bootty-stderr"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn shell_run_output_waits_for_fast_launchd_jobs_to_start() {
        for index in 0..20 {
            assert_eq!(
                shell_run_output(
                    &format!("printf fast-{index}"),
                    &PlatformRunJobs::default(),
                    &AtomicBool::new(false),
                )
                .unwrap(),
                format!("fast-{index}")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn shell_run_output_preserves_path_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("bootty-path-probe");
        std::fs::write(&program, "#!/bin/sh\nprintf path-ok").expect("write path probe");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make path probe executable");
        let _path = prepend_path(dir.path());

        assert_eq!(
            shell_run_output(
                "bootty-path-probe",
                &PlatformRunJobs::default(),
                &AtomicBool::new(false),
            )
            .unwrap(),
            "path-ok"
        );
    }
    fn assert_builtins_load(
        lua: &Lua,
        dir: &Path,
        builtins: &'static [(&'static str, &'static str)],
        expected: &[&str],
    ) {
        let modules = load_modules(lua, dir, builtins);
        let names = modules
            .iter()
            .map(|module| module.name.as_str())
            .collect::<Vec<_>>();

        for expected_name in expected {
            assert!(
                names.contains(expected_name),
                "{expected_name} builtin should load"
            );
        }
        assert!(
            modules
                .iter()
                .filter(|module| expected.contains(&module.name.as_str()))
                .all(|module| matches!(&module.body, ModuleBody::Render(_))),
            "builtins should load as renderable modules"
        );
    }

    #[test]
    fn list_module_yields_styled_items_with_actions() {
        let items = run_source(
            "return function() return { \
                { text = 'a', fg = '#a6e3a1', action = 'activate-window:1' }, \
                { text = 'b' } \
            } end",
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "a");
        assert_eq!(items[0].fg, Some(Color32::from_rgb(0xa6, 0xe3, 0xa1)));
        assert_eq!(items[0].action.as_deref(), Some("activate-window:1"));
        assert_eq!(items[1].text, "b");
    }

    #[test]
    fn json_decode_host_fn_returns_lua_tables() {
        let items = run_source(
            r#"return function()
                local decoded = bootty.json.decode('{"label":"codex","values":[20,true,null]}')
                return { text = decoded.label .. ':' .. decoded.values[1] .. ':' .. tostring(decoded.values[2]) .. ':' .. tostring(decoded.values[3]) }
            end"#,
        );

        assert_eq!(items[0].text, "codex:20:true:nil");
    }

    #[test]
    fn ui_session_items_namespaces_child_row_keys() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![
                SessionView {
                    id: "$1".to_owned(),
                    name: "work/api".to_owned(),
                    color: Some("#89b4fa".to_owned()),
                    dim_color: Some("#455a7d".to_owned()),
                    ..SessionView::default()
                },
                SessionView {
                    id: "$2".to_owned(),
                    name: "work/ui".to_owned(),
                    color: Some("#a6e3a1".to_owned()),
                    dim_color: Some("#526f50".to_owned()),
                    ..SessionView::default()
                },
            ],
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                r#"return function()
                    return bootty.ui.session_items({
                        sessions = bootty.sessions(),
                        details = function(_, _) return { { key = 'process', icon = 'terminal', label = 'node' } } end,
                        progress = function(_, _) return { key = 'progress', value = 50 } end,
                    })
                end"#,
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        let keys = items
            .iter()
            .filter_map(|item| item.key.as_deref())
            .collect::<Vec<_>>();

        assert!(keys.contains(&"$1:process"));
        assert!(keys.contains(&"$2:process"));
        assert!(keys.contains(&"$1:progress"));
        assert!(keys.contains(&"$2:progress"));
    }

    #[test]
    fn builtin_sessions_keeps_branch_row_stable_without_status_progress_rows() {
        let cwd = std::env::current_dir()
            .expect("current dir")
            .to_string_lossy()
            .into_owned();
        let display_cwd = crate::strings::display_path(&cwd);
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![SessionView {
                id: "plain".to_owned(),
                name: "bootty".to_owned(),
                selected: true,
                cwd: Some(cwd.clone()),
                color: Some("#89b4fa".to_owned()),
                dim_color: Some("#455a7d".to_owned()),
                ports: vec![8080, 3000],
                ..SessionView::default()
            }],
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_SIDEBAR_EXTENSIONS);
        let sessions = modules
            .iter()
            .find(|module| module.name == "sessions")
            .expect("sessions builtin loaded");

        let first = run_module(&sessions.body);
        let rerendered = run_module(&sessions.body);
        let keys = rerendered
            .iter()
            .filter_map(|item| item.key.as_deref())
            .collect::<Vec<_>>();

        assert_eq!(first.len(), rerendered.len());
        assert!(keys.contains(&"plain:branch"));
        assert!(keys.contains(&"plain:cwd"));
        assert!(keys.contains(&"plain:ports"));
        assert_eq!(
            rerendered
                .iter()
                .find(|item| item.key.as_deref() == Some("plain:cwd"))
                .map(|item| item.text.as_str()),
            Some(display_cwd.as_str())
        );
        assert_eq!(
            rerendered
                .iter()
                .find(|item| item.key.as_deref() == Some("plain:ports"))
                .map(|item| item.text.as_str()),
            Some("8080, 3000")
        );
        assert!(!keys.contains(&"plain:status"));
        assert!(!keys.contains(&"plain:progress"));
    }

    // Drives the real `sessions` builtin through the Refresh-mode cache path (what the app uses),
    // re-rendering until the branch row settles. Returns the branch row's text, or None if the
    // row is absent. Synchronizes on the rendered value rather than sleeping a fixed duration.
    fn settle_branch_row(
        sessions: &LoadedModule,
        run_cache: &RunCache,
        want: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        while Instant::now() < deadline {
            run_cache.set_mode(RunMode::Refresh);
            let items = run_module(&sessions.body);
            run_cache.set_mode(RunMode::Live);
            last = items
                .iter()
                .find(|item| item.key.as_deref() == Some("plain:branch"))
                .map(|item| item.text.clone());
            if last.as_deref().is_some_and(&want) {
                return last;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        last
    }

    #[test]
    fn builtin_sessions_branch_row_tracks_live_head_through_changes() {
        fn git(repo: &std::path::Path, args: &[&str]) {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(repo)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }

        let repo = tempfile::tempdir().expect("tempdir");
        let path = repo.path();
        git(path, &["init", "-q", "-b", "alpha"]);
        git(path, &["config", "user.email", "t@t.t"]);
        git(path, &["config", "user.name", "t"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(path, &["commit", "-q", "--allow-empty", "-m", "two"]);

        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![SessionView {
                id: "plain".to_owned(),
                name: "bootty".to_owned(),
                selected: true,
                cwd: Some(path.to_string_lossy().into_owned()),
                ..SessionView::default()
            }],
            ..MuxView::default()
        }));
        let run_cache = Arc::new(RunCache::default());
        let lua = setup_lua(
            &[],
            mux,
            Arc::default(),
            Arc::default(),
            Arc::clone(&run_cache),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_SIDEBAR_EXTENSIONS);
        let sessions = modules
            .iter()
            .find(|module| module.name == "sessions")
            .expect("sessions builtin loaded");

        let timeout = Duration::from_secs(5);
        assert_eq!(
            settle_branch_row(sessions, &run_cache, |b| b == "alpha", timeout).as_deref(),
            Some("alpha"),
            "branch row should report the starting branch",
        );

        // Switch to another branch: the row must follow, not freeze on the first value.
        git(path, &["checkout", "-q", "-b", "beta"]);
        assert_eq!(
            settle_branch_row(sessions, &run_cache, |b| b == "beta", timeout).as_deref(),
            Some("beta"),
            "branch row must update when the live branch changes",
        );

        // Detach HEAD: the row must report detached, not the stale branch name.
        git(path, &["checkout", "-q", "HEAD~1"]);
        let detached =
            settle_branch_row(sessions, &run_cache, |b| b.starts_with("detached"), timeout);
        assert!(
            detached
                .as_deref()
                .is_some_and(|b| b.starts_with("detached")),
            "branch row must report detached when HEAD detaches, got {detached:?}",
        );
    }

    #[test]
    fn string_return_is_one_item() {
        let items = run_source("return function() return 'hi' end");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "hi");
    }

    #[test]
    fn icon_only_table_return_is_one_item() {
        let items = run_source(
            "return function() return { icon = 'plug-zap', action = 'toggle-caffeinate' } end",
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].icon.as_deref(), Some("plug-zap"));
        assert_eq!(items[0].action.as_deref(), Some("toggle-caffeinate"));
    }

    #[test]
    fn scalar_array_return_yields_no_items() {
        let items = run_source("return function() return { 1, 2, 3 } end");
        assert!(items.is_empty());
    }
    #[test]
    fn module_styles_from_theme_token() {
        let items =
            run_source("return function() return { text = 'x', fg = bootty.theme.accent } end");
        assert_eq!(items[0].fg, Some(Color32::from_rgb(0x89, 0xb4, 0xfa)));
    }

    #[test]
    fn module_globals_do_not_leak_between_modules() {
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("aaa.luau"),
            "leaked = 'bad'; return function() return 'aaa' end",
        )
        .expect("write first module");
        std::fs::write(
            dir.path().join("bbb.luau"),
            "return function() return tostring(leaked) end",
        )
        .expect("write second module");

        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let module = modules
            .iter()
            .find(|module| module.name == "bbb")
            .expect("second module should load");
        let items = run_module(&module.body);

        assert_eq!(items[0].text, "nil");
    }

    #[test]
    fn module_load_cannot_mutate_shared_theme() {
        let theme = [("text".to_owned(), "#cdd6f4".to_owned())];
        let lua = setup_lua(
            &theme,
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("mutator.luau"),
            "bootty.theme.text = '#000000'; return function() return 'bad' end",
        )
        .expect("write mutator module");

        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let module = modules
            .iter()
            .find(|module| module.name == "mutator")
            .expect("mutator module should surface an error");
        let ModuleBody::LoadError(message) = &module.body else {
            panic!("theme mutation should fail while loading the module");
        };
        let env = module_environment(&lua).unwrap();
        let text = lua
            .load("return bootty.theme.text")
            .set_environment(env)
            .eval::<String>()
            .unwrap();

        assert!(message.starts_with("mutator:"));
        assert_eq!(text, "#cdd6f4");
    }

    #[test]
    fn table_module_interval_is_read() {
        let theme: [(String, String); 0] = [];
        let lua = setup_lua(
            &theme,
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let value = lua
            .load("return { interval = 5, render = function() return 'x' end }")
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        assert_eq!(module.interval, Duration::from_secs(5));
    }

    #[test]
    fn windows_host_fn_exposes_mux_view() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            windows: vec![WindowView {
                id: "@1".to_owned(),
                index: 2,
                name: "edit".to_owned(),
                active: true,
                progress: Some(42),
                progress_indeterminate: false,
            }],
            session: Some("work".to_owned()),
            session_color: Some("#89b4fa".to_owned()),
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                "return function() local w = bootty.windows()[1] \
                 return { text = w.index .. ':' .. w.name .. ':' .. w.progress, action = 'activate-window:' .. w.id } end",
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        assert_eq!(items[0].text, "2:edit:42");
        assert_eq!(items[0].action.as_deref(), Some("activate-window:@1"));
    }

    #[test]
    fn sessions_host_fn_exposes_bootty_owned_sessions() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![SessionView {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                active: true,
                selected: true,
                cwd: Some("/tmp/work/api".to_owned()),
                color: Some("#89b4fa".to_owned()),
                dim_color: Some("#455a7d".to_owned()),
                progress: Some(42),
                progress_indeterminate: false,
                progresses: vec![
                    SessionProgressView {
                        process: "pi".to_owned(),
                        value: 50,
                        indeterminate: true,
                    },
                    SessionProgressView {
                        process: "cargo".to_owned(),
                        value: 42,
                        indeterminate: false,
                    },
                ],
                ports: vec![8040, 3000],
            }],
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                r#"return function() local s = bootty.sessions()[1]
                   local p = s.progresses
                   return { kind = 'session', text = s.name .. ':' .. s.cwd .. ':' .. p[1].process .. ':' .. p[2].value .. ':' .. s.ports[1],
                   session_id = s.id, fg = s.color } end"#,
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);

        assert_eq!(items[0].kind.as_deref(), Some("session"));
        assert_eq!(items[0].text, "work/api:/tmp/work/api:pi:42:8040");
        assert_eq!(items[0].session_id.as_deref(), Some("$1"));
        assert_eq!(items[0].fg, Some(Color32::from_rgb(0x89, 0xb4, 0xfa)));
    }

    #[test]
    fn on_reorder_routes_through_reorder_session_host_fn() {
        let reorders: Arc<RwLock<Vec<SessionReorder>>> = Arc::default();
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::clone(&reorders),
            Arc::default(),
        )
        .unwrap();
        let value = lua
            .load(
                "return { render = function() return {} end, \
                 on_reorder = function(source, before) bootty.reorder_session(source, before) end }",
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("sessions".to_owned(), value).unwrap();
        let handler = module
            .on_reorder
            .expect("on_reorder parsed from module table");

        handler
            .call::<()>(("agents".to_owned(), Some("bootty".to_owned())))
            .unwrap();
        handler
            .call::<()>(("solo".to_owned(), Option::<String>::None))
            .unwrap();

        assert_eq!(
            *reorders.read().unwrap(),
            vec![
                SessionReorder {
                    source: "agents".to_owned(),
                    before: Some("bootty".to_owned()),
                },
                SessionReorder {
                    source: "solo".to_owned(),
                    before: None,
                },
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_cache_refreshes_query_output_without_blocking_render() {
        // Render-mode `bootty.run` must not block the extension worker: it returns cached output
        // immediately and refreshes the command in the background. Live mode is still synchronous
        // for side-effecting calls such as `on_reorder` handlers.
        let dir = tempfile::tempdir().expect("tempdir");
        let counter = dir.path().join("n");
        let counter_arg = shell_quote(&counter);
        let cmd = format!(
            "n=$(cat {0} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {0}; printf %s $n",
            counter_arg
        );
        let run_cache = Arc::new(RunCache::default());
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::clone(&run_cache),
        )
        .unwrap();
        let run_cmd = cmd.clone();
        let run = move || {
            lua.load(format!("return bootty.run({run_cmd:?})"))
                .eval::<String>()
                .unwrap()
        };

        run_cache.set_mode(RunMode::Refresh);
        assert_eq!(
            run(),
            "",
            "first refresh returns the empty cache immediately"
        );
        assert!(wait_for_cached_output(
            &run_cache,
            &cmd,
            "1",
            Duration::from_secs(2)
        ));
        run_cache.set_mode(RunMode::Cached);
        assert_eq!(run(), "1", "cached serves background-refreshed output");
        assert_eq!(run(), "1", "cached keeps serving on repeat");
        run_cache.set_mode(RunMode::Refresh);
        assert_eq!(run(), "1", "refresh returns stale output while updating");
        assert!(wait_for_cached_output(
            &run_cache,
            &cmd,
            "2",
            Duration::from_secs(2)
        ));
        run_cache.set_mode(RunMode::Cached);
        assert_eq!(run(), "2", "cached sees completed background refresh");
        run_cache.set_mode(RunMode::Live);
        assert_eq!(run(), "3", "live always executes, ignoring the cache");
        assert_eq!(run(), "4", "live never serves a cached result");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_cache_refresh_keeps_shell_out_errors_visible() {
        let cmd = "printf ignored".to_owned();
        let run_cache = Arc::new(RunCache {
            shutdown: Arc::new(AtomicBool::new(true)),
            ..RunCache::default()
        });

        run_cache.set_mode(RunMode::Refresh);
        assert_eq!(run_cache.run(&cmd).unwrap(), "");

        assert!(wait_for_cached_output_containing(
            &run_cache,
            "bootty.run: extension host stopped",
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn windows_module_anchors_each_tab_to_its_window_id() {
        let theme = [
            ("accent".to_owned(), "#89b4fa".to_owned()),
            ("surface".to_owned(), "#313244".to_owned()),
            ("base".to_owned(), "#1e1e2e".to_owned()),
            ("subtext".to_owned(), "#a6adc8".to_owned()),
            ("text".to_owned(), "#cdd6f4".to_owned()),
        ];
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            windows: vec![
                WindowView {
                    id: "@1".to_owned(),
                    index: 1,
                    name: "edit".to_owned(),
                    active: true,
                    progress: None,
                    progress_indeterminate: false,
                },
                WindowView {
                    id: "@2".to_owned(),
                    index: 2,
                    name: "logs".to_owned(),
                    active: false,
                    progress: None,
                    progress_indeterminate: false,
                },
            ],
            ..MuxView::default()
        }));
        let lua = setup_lua(&theme, mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let windows = modules
            .iter()
            .find(|module| module.name == "windows")
            .expect("built-in windows module loaded");
        let items = run_module(&windows.body);
        let anchors: Vec<_> = items
            .iter()
            .filter_map(|item| item.reorder_anchor.clone())
            .collect();
        // Each window contributes two cells (index + name) sharing the window id anchor.
        assert_eq!(anchors, vec!["@1", "@1", "@2", "@2"]);
    }

    #[test]
    fn indeterminate_window_progress_bounces_across_its_track() {
        assert_eq!(windows_indeterminate_progress_offset(0.0), 0.0);
        assert_eq!(windows_indeterminate_progress_offset(0.75), 0.75);
        assert_eq!(windows_indeterminate_progress_offset(1.5), 0.0);
    }

    #[test]
    fn builtin_windows_items_reflects_active_window_without_worker_cache() {
        let theme = BuiltinWindowsTheme {
            accent: Color32::from_rgb(1, 2, 3),
            surface: Color32::from_rgb(4, 5, 6),
            base: Color32::from_rgb(7, 8, 9),
            subtext: Color32::from_rgb(10, 11, 12),
            text: Color32::from_rgb(13, 14, 15),
            border: Color32::from_rgb(16, 17, 18),
        };
        let view = MuxView {
            windows: vec![
                WindowView {
                    id: "@1".to_owned(),
                    index: 1,
                    name: "edit".to_owned(),
                    active: false,
                    progress: Some(42),
                    progress_indeterminate: false,
                },
                WindowView {
                    id: "@2".to_owned(),
                    index: 2,
                    name: "logs".to_owned(),
                    active: true,
                    progress: None,
                    progress_indeterminate: false,
                },
            ],
            session_color: Some("#89b4fa".to_owned()),
            ..MuxView::default()
        };

        let items = builtin_windows_items(&view, theme);
        let texts = items
            .iter()
            .map(|item| item.text.as_str())
            .collect::<Vec<_>>();

        assert_eq!(texts, vec!["1", "edit", "2", "logs"]);
        assert_eq!(items[0].fg, Some(theme.subtext));
        assert_eq!(items[1].fg, Some(theme.subtext));
        assert_eq!(items[2].fg, Some(theme.base));
        assert_eq!(items[3].fg, Some(theme.text));
        assert_eq!(items[2].action.as_deref(), Some("activate-window:@2"));
        assert_eq!(items[3].action.as_deref(), Some("activate-window:@2"));
        assert_eq!(items[2].reorder_anchor.as_deref(), Some("@2"));
        assert_eq!(items[3].reorder_anchor.as_deref(), Some("@2"));
        assert_eq!(items[1].pad_left, WINDOWS_WEDGE_PX);
        assert!(items[1].primitives.iter().any(|primitive| {
            matches!(
                primitive,
                ModulePrimitive::Rect {
                    fill: Some(fill),
                    w,
                    ..
                } if *fill == Color32::from_rgb(0x89, 0xb4, 0xfa)
                    && w.frac == 0.42
                    && w.px == 0.0
            )
        }));
        assert!(items[3].primitives.iter().any(|primitive| {
            matches!(
                primitive,
                ModulePrimitive::Polygon {
                    fill: Some(fill),
                    ..
                } if *fill == Color32::from_rgb(0x89, 0xb4, 0xfa)
            )
        }));
    }

    #[test]
    fn extension_host_detects_user_module_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let host = ExtensionHost::spawn_status(
            dir.path().to_path_buf(),
            egui::Context::default(),
            Vec::new(),
        );

        assert!(!host.has_user_module("windows"));
        std::fs::write(
            dir.path().join("windows.luau"),
            "return function() return '' end",
        )
        .expect("write user windows override");

        assert!(host.has_user_module("windows"));
        assert!(!host.has_user_module("window"));
    }

    #[test]
    fn refresh_metrics_probes_memory() {
        // Catches a wiring regression (no memory refresh, or used/total swapped);
        // total memory is non-zero on any real OS the tests run on.
        let metrics: Arc<RwLock<Metrics>> = Arc::default();
        let mut system = System::new();
        let battery = BatteryManager::new().ok();
        refresh_metrics(&mut system, battery.as_ref(), &metrics);
        let m = *metrics.read().unwrap();
        assert!(m.mem_total_bytes > 0, "total memory should be probed");
        assert!(
            (0.0..=100.0).contains(&m.mem_used_pct),
            "memory percent out of range: {}",
            m.mem_used_pct
        );
        // A battery may be absent on CI; when present, charge is a real percentage.
        if let Some(pct) = m.battery_percent {
            assert!(
                (0.0..=100.0).contains(&pct),
                "battery percent out of range: {pct}"
            );
        }
    }

    #[test]
    fn session_color_host_fn_exposes_mux_color() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            session_color: Some("#a6e3a1".to_owned()),
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load("return function() return { text = 's', fg = bootty.session_color() } end")
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        assert_eq!(items[0].fg, Some(Color32::from_rgb(0xa6, 0xe3, 0xa1)));
    }

    #[test]
    fn awake_host_fn_exposes_keep_awake_state() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            keep_awake: true,
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load("return function() return { text = tostring(bootty.awake()) } end")
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        assert_eq!(items[0].text, "true");
    }

    #[test]
    fn metrics_host_fn_exposes_battery_remaining_seconds() {
        let metrics: Arc<RwLock<Metrics>> = Arc::new(RwLock::new(Metrics {
            battery_percent: Some(78.0),
            on_ac: false,
            battery_time_to_empty_secs: Some(7_080.0),
            battery_time_to_full_secs: None,
            ..Metrics::default()
        }));
        let lua = setup_lua(&[], Arc::default(), metrics, Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                "return function() local m = bootty.metrics() \
                 return { text = m.battery .. ':' .. m.battery_time_to_empty } end",
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        assert_eq!(items[0].text, "78:7080");
    }

    fn built_in_sysinfo_items(metrics: Metrics) -> Vec<ModuleItem> {
        let theme = [
            ("base".to_owned(), "#1e1e2e".to_owned()),
            ("surface".to_owned(), "#313244".to_owned()),
            ("hover".to_owned(), "#45475a".to_owned()),
            ("success".to_owned(), "#a6e3a1".to_owned()),
            ("warning".to_owned(), "#f9e2af".to_owned()),
            ("subtext".to_owned(), "#a6adc8".to_owned()),
            ("text".to_owned(), "#cdd6f4".to_owned()),
        ];
        let metrics = Arc::new(RwLock::new(metrics));
        let lua = setup_lua(
            &theme,
            Arc::default(),
            metrics,
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let sysinfo = modules
            .iter()
            .find(|module| module.name == "sysinfo")
            .expect("built-in sysinfo module loaded");
        run_module(&sysinfo.body)
    }

    #[test]
    fn built_in_sysinfo_marks_charging_and_full_battery() {
        let charging = built_in_sysinfo_items(Metrics {
            cpu: 10.0,
            mem_used_pct: 25.0,
            battery_percent: Some(61.0),
            on_ac: true,
            battery_time_to_full_secs: Some(3_600.0),
            ..Metrics::default()
        });
        assert_eq!(
            charging.last().and_then(|item| item.icon.as_deref()),
            Some("plug")
        );

        let full = built_in_sysinfo_items(Metrics {
            cpu: 10.0,
            mem_used_pct: 25.0,
            battery_percent: Some(100.0),
            on_ac: true,
            ..Metrics::default()
        });
        assert_eq!(
            full.last().and_then(|item| item.icon.as_deref()),
            Some("battery-full")
        );
    }

    #[test]
    fn available_module_names_include_user_luau_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("custom.luau"),
            "return function() return 'ok' end",
        )
        .expect("write luau");
        std::fs::write(dir.path().join("ignored.txt"), "ignored").expect("write ignored file");
        std::fs::create_dir(dir.path().join("folder.luau")).expect("create ignored directory");

        let names = available_module_names(dir.path());

        assert!(names.contains(&"custom".to_owned()));
        assert!(!names.contains(&"ignored".to_owned()));
        assert!(!names.contains(&"folder".to_owned()));

        assert!(names.contains(&"clock".to_owned()));
        let sidebar_names = module_names(dir.path(), ModuleKind::Sidebar);
        assert!(sidebar_names.contains(&"custom".to_owned()));
        assert!(sidebar_names.contains(&"sessions".to_owned()));
    }

    #[test]
    fn module_names_reject_paths_and_accept_safe_stems() {
        assert!(valid_module_name("my-module_2"));
        assert!(!valid_module_name(""));
        assert!(!valid_module_name("../module"));
        assert!(!valid_module_name("module.lua"));
    }

    #[test]
    fn creating_user_module_makes_it_discoverable() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_module(
            dir.path(),
            ModuleKind::Status,
            "custom-status",
            "return { render = function() return {} end }",
        )
        .expect("create module");

        let source =
            module_source(dir.path(), ModuleKind::Status, "custom-status").expect("module source");
        assert!(source.customized);
        assert!(!source.has_builtin);
        assert!(module_names(dir.path(), ModuleKind::Status).contains(&"custom-status".to_owned()));
    }
    #[test]
    fn editing_builtin_sidebar_module_creates_a_user_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let builtin = module_source(dir.path(), ModuleKind::Sidebar, "sessions")
            .expect("builtin sessions source");
        assert!(!builtin.customized);

        let edited = format!("{}\n-- customized", builtin.source);
        let path = save_module(dir.path(), ModuleKind::Sidebar, "sessions", &edited)
            .expect("save sidebar override");
        let loaded = module_source(dir.path(), ModuleKind::Sidebar, "sessions")
            .expect("custom sessions source");

        assert_eq!(path, dir.path().join("sessions.luau"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), edited);
        assert!(loaded.customized);
        assert_eq!(loaded.source, edited);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_user_module_file_surfaces_load_error() {
        use std::os::unix::fs::PermissionsExt;
        struct PermissionGuard {
            path: std::path::PathBuf,
            mode: u32,
        }

        impl Drop for PermissionGuard {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(
                    &self.path,
                    std::fs::Permissions::from_mode(self.mode),
                );
            }
        }

        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unreadable.luau");
        std::fs::write(&path, "return function() return 'ok' end").expect("write module");
        let original_mode = std::fs::metadata(&path)
            .expect("stat module")
            .permissions()
            .mode();
        let _guard = PermissionGuard {
            path: path.clone(),
            mode: original_mode,
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .expect("deny module reads");
        if std::fs::read_to_string(&path).is_ok() {
            eprintln!("skipping unreadable-module assertion: chmod 000 file remains readable");
            return;
        }

        let modules = load_modules(&lua, dir.path(), BUILTIN_STATUS_EXTENSIONS);
        let module = modules
            .iter()
            .find(|module| module.name == "unreadable")
            .expect("unreadable module should be loaded as an error");
        let items = run_module(&module.body);

        assert_eq!(items[0].fg, Some(ERROR_COLOR));
        assert!(items[0].text.starts_with("unreadable:"));
    }

    #[test]
    fn reload_prunes_items_for_deleted_modules() {
        let items = RwLock::new(HashMap::from([
            (
                "clock".to_owned(),
                vec![ModuleItem {
                    text: "time".to_owned(),
                    ..ModuleItem::default()
                }],
            ),
            (
                "deleted".to_owned(),
                vec![ModuleItem {
                    text: "stale".to_owned(),
                    ..ModuleItem::default()
                }],
            ),
        ]));
        let module_names = BTreeSet::from(["clock".to_owned()]);

        prune_removed_items(&items, &module_names);
        let map = items.read().unwrap();
        assert!(map.contains_key("clock"));
        assert!(!map.contains_key("deleted"));
    }

    #[test]
    fn short_hex_color_expands_nibbles() {
        assert_eq!(
            parse_hex_color("#fff"),
            Some(Color32::from_rgb(255, 255, 255))
        );
        assert_eq!(parse_hex_color("#0a0"), Some(Color32::from_rgb(0, 170, 0)));
        assert_eq!(parse_hex_color("nope"), None);
    }
}
