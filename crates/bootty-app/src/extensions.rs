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

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32};
use mlua::{Function, Lua, Table, Value, VmState};
use starship_battery::{Manager as BatteryManager, State as BatteryState, units::time::second};
use sysinfo::{MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

mod codexbar;
mod http;

use codexbar::{
    reject_reserved_shell_command, resolve_program as resolve_codexbar_program,
    validate_provider as validate_codexbar_provider,
};
use http::get_local as http_get_local;

/// Default refresh cadence for a module that doesn't declare its own `interval`.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);
/// Background poll granularity; a module fires on the first tick at or after its interval elapses.
const TICK: Duration = Duration::from_millis(8);
/// How often extension dirs are re-scanned for edited/added/removed module files (hot reload).
const RELOAD_SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Cap on how many descendants `bootty.descendants` reports, so a runaway process tree cannot
/// stall a render.
const DESCENDANT_SCAN_LIMIT: usize = 256;
/// How long a macOS memory-pressure sample serves every host before another subprocess runs.
#[cfg(target_os = "macos")]
const MEMORY_PRESSURE_TTL: Duration = Duration::from_secs(5);
/// How long a machine-wide process listing serves `bootty.descendants` calls. Listing every process
/// costs a syscall per process, and the sidebar walks one session's tree every 500ms, so a TTL that
/// matched that cadence re-listed the machine for every single call. Four calls now share a listing;
/// the cost is that an agent started in the last couple of seconds is found on a later refresh.
const PROCESS_TREE_TTL: Duration = Duration::from_secs(2);
/// Slowest cadence any module runs at while the window is unfocused. Structural changes still
/// force a render, so this only slows animation and polling, not the response to real events.
const UNFOCUSED_INTERVAL_FLOOR: Duration = Duration::from_secs(1);
const CODEXBAR_SERVER_PORT: u16 = 17_613;
const CODEXBAR_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const ERROR_COLOR: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);
const EXTENSION_UI_PRELUDE: &str = include_str!("extension_ui.luau");
const SIDEBAR_FACTS_PRELUDE: &str = include_str!("sidebar_session_facts.luau");
const SESSION_MODULE_PREFIX: &str = "session:";

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

const BUILTIN_SESSION_EXTENSIONS: &[(&str, &str)] = &[
    ("diffs", include_str!("session_defaults/diffs.luau")),
    ("process", include_str!("session_defaults/process.luau")),
    ("agent", include_str!("session_defaults/agent.luau")),
    ("directory", include_str!("session_defaults/directory.luau")),
    ("branch", include_str!("session_defaults/branch.luau")),
    ("ports", include_str!("session_defaults/ports.luau")),
    ("progress", include_str!("session_defaults/progress.luau")),
];
pub fn session_module_key(name: &str) -> String {
    format!("{SESSION_MODULE_PREFIX}{name}")
}

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
    /// The backend's name, which is what every command and every membership record targets.
    pub name: String,
    /// The name bootty shows, free of any uniqueness suffix the backend name needed. Empty means
    /// bootty has no name of its own for this session; modules fall back to `name`.
    pub display_name: String,
    pub active: bool,
    pub selected: bool,
    pub cwd: Option<String>,
    /// The session's active pane, its process id, and the command running in it, as the last mux
    /// snapshot reported them. Modules read these instead of asking the backend again.
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub process: Option<String>,
    pub color: Option<String>,
    pub dim_color: Option<String>,
    pub progress: Option<u8>,
    pub progress_indeterminate: bool,
    pub progresses: Vec<SessionProgressView>,
    pub ports: Vec<u16>,
}

/// Mux state shared with the worker thread so modules can render it.
#[derive(Clone, Debug, PartialEq)]
pub struct MuxView {
    pub windows: Vec<WindowView>,
    pub sessions: Vec<SessionView>,
    /// Stable identity of the Space/backend binding that owns `sessions`.
    pub scope_key: String,
    pub session: Option<String>,
    pub sidebar_visible: bool,
    /// The active session's sidebar accent color as `#rrggbb`, so modules can
    /// match the bar to the session like the sidebar does.
    pub session_color: Option<String>,
    /// Whether Bootty is currently holding a keep-awake/caffeinate guard.
    pub keep_awake: bool,
    /// Whether the window has keyboard focus. Hosts run modules at [`UNFOCUSED_INTERVAL_FLOOR`]
    /// while it is false: a module that animates its rows otherwise repaints the whole window
    /// several times a second at nobody.
    pub focused: bool,
}

impl Default for MuxView {
    /// Focused by default: a host that has not been told otherwise should run at full cadence
    /// rather than start out throttled.
    fn default() -> Self {
        Self {
            windows: Vec::new(),
            sessions: Vec::new(),
            scope_key: String::new(),
            session: None,
            sidebar_visible: false,
            session_color: None,
            keep_awake: false,
            focused: true,
        }
    }
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

/// A command a module asked for: either shell text or an argument vector run directly.
enum RunCommand {
    Shell(String),
    Exec(Vec<String>),
}

impl RunCommand {
    /// Cache identity. Shell text keys on itself, so seeded preview output still matches. An argv
    /// joins on a separator no argument carries, keeping `{"a b"}` and `{"a", "b"}` apart.
    fn cache_key(&self) -> Cow<'_, str> {
        match self {
            Self::Shell(cmd) => Cow::Borrowed(cmd),
            Self::Exec(argv) => Cow::Owned(format!("exec\u{1f}{}", argv.join("\u{1f}"))),
        }
    }

    /// The command as one line, for guards that read command text.
    fn reserved_guard_text(&self) -> Cow<'_, str> {
        match self {
            Self::Shell(cmd) => Cow::Borrowed(cmd),
            Self::Exec(argv) => Cow::Owned(argv.join(" ")),
        }
    }

    fn output(&self, run_jobs: &PlatformRunJobs, shutdown: &AtomicBool) -> std::io::Result<String> {
        match self {
            Self::Shell(cmd) => shell_run_output(cmd, run_jobs, shutdown),
            Self::Exec(argv) => exec_run_output(argv, run_jobs, shutdown),
        }
    }
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
    /// Branch a settings preview should show. Previews render against example sessions whose paths
    /// do not exist, so a real `HEAD` read has nothing to find.
    preview_branch: Option<String>,
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

    fn run(self: &Arc<Self>, cmd: &str) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Shell(cmd.to_owned()))
    }

    fn exec(self: &Arc<Self>, argv: Vec<String>) -> std::io::Result<(String, bool)> {
        self.run_command(RunCommand::Exec(argv))
    }

    /// What a command last printed, without running it. This is how a module shows an answer the
    /// moment it lands: the command that produces it is started on its own schedule, and every
    /// render in between reads the result for free.
    fn read(&self, argv: Vec<String>) -> (String, bool) {
        let cached = self.cached(&RunCommand::Exec(argv).cache_key());
        (cached.clone().unwrap_or_default(), cached.is_some())
    }

    /// Returns the command's output and whether that output is an answer yet. During a render the
    /// first ask for a command only starts it, and an empty string is what a module gets back —
    /// indistinguishable from a command that legitimately printed nothing. The flag is that
    /// difference, so a module can ask again shortly instead of showing nothing until its next turn.
    fn run_command(self: &Arc<Self>, command: RunCommand) -> std::io::Result<(String, bool)> {
        reject_reserved_shell_command(&command.reserved_guard_text())?;
        match self.mode() {
            RunMode::Live => command
                .output(&self.run_jobs, &self.shutdown)
                .map(|output| (output.trim().to_owned(), true)),
            RunMode::Cached => {
                let cached = self.cached(&command.cache_key());
                Ok((cached.clone().unwrap_or_default(), cached.is_some()))
            }
            RunMode::Refresh => {
                let cached = self.cached(&command.cache_key());
                self.refresh(command);
                Ok((cached.clone().unwrap_or_default(), cached.is_some()))
            }
        }
    }

    fn codexbar_usage(self: &Arc<Self>, provider: &str) -> std::io::Result<String> {
        validate_codexbar_provider(provider)?;
        let output = self.codexbar.cached(provider).unwrap_or_default();
        if self.mode() != RunMode::Cached {
            self.refresh_codexbar_usage(provider.to_owned());
        }
        Ok(output)
    }

    fn cached(&self, key: &str) -> Option<String> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(key).map(|entry| entry.output.clone()))
    }

    fn refresh(self: &Arc<Self>, command: RunCommand) {
        let key = command.cache_key().into_owned();
        {
            let Ok(mut entries) = self.entries.lock() else {
                return;
            };
            let entry = entries.entry(key.clone()).or_default();
            if entry.refreshing {
                return;
            }
            entry.refreshing = true;
        }

        let cache = Arc::clone(self);
        std::thread::spawn(move || {
            let output = command
                .output(&cache.run_jobs, &cache.shutdown)
                .map(|output| output.trim().to_owned())
                .unwrap_or_else(|error| format!("bootty.run: {error}"));
            if let Ok(mut entries) = cache.entries.lock() {
                let entry = entries.entry(key).or_default();
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

#[derive(Clone)]
struct ModuleCatalog {
    dir: PathBuf,
    builtins: &'static [(&'static str, &'static str)],
    prefix: &'static str,
}

/// Owns the Luau worker thread, the shared item map the UI reads, and the mux snapshot the UI feeds.
pub struct ExtensionRuntime {
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

impl ExtensionRuntime {
    /// Spawns the status-bar extension worker. `ctx` is woken when module output changes.
    pub fn spawn_status(dir: PathBuf, ctx: egui::Context, theme: Vec<(String, String)>) -> Self {
        Self::spawn_with_modules(
            "bootty-status",
            vec![ModuleCatalog {
                dir,
                builtins: BUILTIN_STATUS_EXTENSIONS,
                prefix: "",
            }],
            ctx,
            theme,
        )
    }

    /// Spawns the sidebar extension worker. Overall sidebar modules and per-session modules
    /// use sibling directories and share one Lua worker/facts cache.
    pub fn spawn_sidebar(dir: PathBuf, ctx: egui::Context, theme: Vec<(String, String)>) -> Self {
        let session_dir = dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("session");
        Self::spawn_with_modules(
            "bootty-sidebar",
            vec![
                ModuleCatalog {
                    dir,
                    builtins: BUILTIN_SIDEBAR_EXTENSIONS,
                    prefix: "",
                },
                ModuleCatalog {
                    dir: session_dir,
                    builtins: BUILTIN_SESSION_EXTENSIONS,
                    prefix: SESSION_MODULE_PREFIX,
                },
            ],
            ctx,
            theme,
        )
    }

    fn spawn_with_modules(
        thread_name: &str,
        catalogs: Vec<ModuleCatalog>,
        ctx: egui::Context,
        theme: Vec<(String, String)>,
    ) -> Self {
        let module_dir = catalogs
            .first()
            .map(|catalog| catalog.dir.clone())
            .unwrap_or_default();
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
        let run_jobs = Arc::new(PlatformRunJobs::default());
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
                        &catalogs,
                        &ctx,
                        &theme,
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
    pub fn has_legacy_sessions_module(&self) -> bool {
        user_module_path(&self.dir, "sessions")
            .and_then(|path| std::fs::read_to_string(path).ok())
            .is_some_and(|source| source.contains("bootty.sidebar.session_facts"))
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

impl Drop for ExtensionRuntime {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.waker.wake();
        self.run_jobs.cleanup();
    }
}

#[allow(clippy::too_many_arguments)]
fn run_loop(
    catalogs: &[ModuleCatalog],
    ctx: &egui::Context,
    theme: &[(String, String)],
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
    let mut modules = load_catalog_modules(&lua, catalogs);
    let mut signature = catalog_signature(catalogs);
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
            let current = catalog_signature(catalogs);
            if current != signature {
                signature = current;
                modules = load_catalog_modules(&lua, catalogs);
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
        // An unfocused window animates at nobody, and every changed item repaints the whole
        // window, so hold modules to a slow floor until focus returns.
        let interval_floor = if mux.read().is_ok_and(|view| view.focused) {
            Duration::ZERO
        } else {
            UNFOCUSED_INTERVAL_FLOOR
        };
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
                || module.last_run.is_none_or(|last| {
                    now.duration_since(last) >= module.interval.max(interval_floor)
                })
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

fn load_catalog_modules(lua: &Lua, catalogs: &[ModuleCatalog]) -> Vec<LoadedModule> {
    catalogs
        .iter()
        .flat_map(|catalog| {
            load_modules(lua, &catalog.dir, catalog.builtins)
                .into_iter()
                .map(|mut module| {
                    module.name.insert_str(0, catalog.prefix);
                    module
                })
        })
        .collect()
}

fn catalog_signature(catalogs: &[ModuleCatalog]) -> Vec<(PathBuf, Option<SystemTime>)> {
    let mut signature = catalogs
        .iter()
        .flat_map(|catalog| dir_signature(&catalog.dir))
        .collect::<Vec<_>>();
    signature.sort();
    signature
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
    Session,
    Status,
}

impl ModuleKind {
    fn builtins(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Sidebar => BUILTIN_SIDEBAR_EXTENSIONS,
            Self::Session => BUILTIN_SESSION_EXTENSIONS,
            Self::Status => BUILTIN_STATUS_EXTENSIONS,
        }
    }
}
fn preview_run_cache() -> Arc<RunCache> {
    let cache = Arc::new(RunCache {
        preview_branch: Some("feature/module-previews".to_owned()),
        ..RunCache::default()
    });
    cache.set_mode(RunMode::Cached);
    let commands = [
        (
            RunCommand::Exec(
                ["tmux", "capture-pane", "-t", "%1", "-p", "-S", "-30"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
            "• Working on module previews".to_owned(),
        ),
        (
            RunCommand::Exec(
                [
                    "git",
                    "-C",
                    "/Users/demo/src/bootty",
                    "diff",
                    "HEAD",
                    "--numstat",
                ]
                .map(str::to_owned)
                .to_vec(),
            ),
            "12\t3\tcrates/bootty-app/src/ui/settings/modules.rs".to_owned(),
        ),
    ];
    if let Ok(mut entries) = cache.entries.lock() {
        for (command, output) in commands {
            entries.insert(
                command.cache_key().into_owned(),
                RunEntry {
                    output: output.to_owned(),
                    refreshing: false,
                },
            );
        }
    }
    if let Ok(mut entries) = cache.codexbar.entries.lock() {
        entries.insert(
            "codex".to_owned(),
            CodexBarEntry {
                output: r#"{"primary":{"usedPercent":38,"windowMinutes":300},"secondary":{"usedPercent":61,"windowMinutes":10080}}"#.to_owned(),
                ..CodexBarEntry::default()
            },
        );
        entries.insert(
            "claude".to_owned(),
            CodexBarEntry {
                output: r#"{"primary":{"usedPercent":17,"windowMinutes":300},"secondary":{"usedPercent":44,"windowMinutes":10080}}"#.to_owned(),
                ..CodexBarEntry::default()
            },
        );
    }
    cache
}

/// Runs one module source in an isolated Lua VM with deterministic example host data.
/// Shell-backed APIs serve seeded example output and never execute external commands.
pub fn preview_module_source(
    source: &str,
    name: &str,
    theme: &[(String, String)],
) -> Vec<ModuleItem> {
    let mux = Arc::new(RwLock::new(preview_mux_view()));
    let metrics = Arc::new(RwLock::new(Metrics {
        cpu: 42.0,
        load1: 1.25,
        mem_used_pct: 68.0,
        mem_total_bytes: 16 * 1_073_741_824,
        battery_percent: Some(73.0),
        on_ac: false,
        battery_time_to_empty_secs: Some(9_000.0),
        battery_time_to_full_secs: None,
    }));
    let run_cache = preview_run_cache();
    let result = setup_lua(theme, mux, metrics, Arc::default(), run_cache).and_then(|lua| {
        let deadline = Instant::now() + Duration::from_millis(50);
        lua.set_interrupt(move |_| {
            if Instant::now() >= deadline {
                Err(mlua::Error::RuntimeError(
                    "preview exceeded 50 ms".to_owned(),
                ))
            } else {
                Ok(VmState::Continue)
            }
        });
        let value = module_environment(&lua).and_then(|env| {
            lua.load(source)
                .set_name(name)
                .set_environment(env)
                .eval::<Value>()
        })?;
        loaded_module_from_value(name.to_owned(), value)
            .map(|module| run_module(&module.body))
            .ok_or_else(|| {
                mlua::Error::RuntimeError("must return a function or { render = ... }".to_owned())
            })
    });
    result.unwrap_or_else(|error| vec![error_item(&error.to_string())])
}

fn preview_mux_view() -> MuxView {
    MuxView {
        scope_key: "preview:binding".to_owned(),
        windows: vec![
            WindowView {
                id: "@1".to_owned(),
                index: 1,
                name: "editor".to_owned(),
                active: true,
                ..WindowView::default()
            },
            WindowView {
                id: "@2".to_owned(),
                index: 2,
                name: "tests".to_owned(),
                progress: Some(62),
                ..WindowView::default()
            },
            WindowView {
                id: "@3".to_owned(),
                index: 3,
                name: "server".to_owned(),
                progress_indeterminate: true,
                ..WindowView::default()
            },
        ],
        sessions: vec![
            SessionView {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                display_name: String::new(),
                active: true,
                selected: true,
                cwd: Some("/Users/demo/src/bootty".to_owned()),
                pane_id: Some("%1".to_owned()),
                pane_pid: Some(4242),
                // An agent as the pane command, so the agent and process previews show a live
                // session without a process-tree walk the preview cannot seed.
                process: Some("codex".to_owned()),
                color: Some("#89b4fa".to_owned()),
                dim_color: Some("#585b70".to_owned()),
                progress: Some(62),
                progresses: vec![SessionProgressView {
                    process: "cargo test".to_owned(),
                    value: 62,
                    indeterminate: false,
                }],
                ports: vec![3000, 8080],
                ..SessionView::default()
            },
            SessionView {
                id: "$2".to_owned(),
                name: "work/web".to_owned(),
                display_name: String::new(),
                active: true,
                cwd: Some("/Users/demo/src/web".to_owned()),
                color: Some("#a6e3a1".to_owned()),
                dim_color: Some("#585b70".to_owned()),
                ..SessionView::default()
            },
        ],
        session: Some("work/api".to_owned()),
        sidebar_visible: false,
        session_color: Some("#89b4fa".to_owned()),
        keep_awake: true,
        focused: true,
    }
}
pub fn preview_builtin_module(
    kind: ModuleKind,
    name: &str,
    theme: &[(String, String)],
) -> Vec<ModuleItem> {
    kind.builtins()
        .iter()
        .find(|(builtin, _)| *builtin == name)
        .map_or_else(Vec::new, |(_, source)| {
            preview_module_source(source, name, theme)
        })
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
///
/// Shared across extension hosts and held for [`MEMORY_PRESSURE_TTL`], since every host sampling
/// metrics on its own meant a subprocess per host per metrics tick.
#[cfg(target_os = "macos")]
fn macos_memory_pressure_used() -> Option<f64> {
    static CACHED: Mutex<Option<(Instant, f64)>> = Mutex::new(None);

    let mut cached = CACHED.lock().ok()?;
    if let Some((sampled_at, used)) = *cached
        && sampled_at.elapsed() < MEMORY_PRESSURE_TTL
    {
        return Some(used);
    }
    let used = macos_memory_pressure_sample()?;
    *cached = Some((Instant::now(), used));
    Some(used)
}

#[cfg(target_os = "macos")]
fn macos_memory_pressure_sample() -> Option<f64> {
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

static RUN_JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The shells started by in-flight `bootty.run` calls, keyed by job id.
///
/// A module renders on a worker thread that blocks until its command finishes, and dropping the
/// host must not join that thread, so cancellation kills the shell the worker is waiting on: the
/// command's pipe reaches EOF and the worker returns.
#[derive(Default)]
struct PlatformRunJobs {
    children: Mutex<BTreeMap<u64, Child>>,
}

impl PlatformRunJobs {
    fn register(&self, id: u64, child: Child, shutdown: &AtomicBool) -> std::io::Result<()> {
        let mut children = self
            .children
            .lock()
            .map_err(|_| std::io::Error::other("extension run jobs poisoned"))?;
        children.insert(id, child);
        // Drop can set shutdown and clean the registry between spawn and registration. Rechecking
        // while the registry is locked closes that gap: either cleanup sees this child, or this
        // path removes it itself.
        if shutdown.load(Ordering::Acquire) {
            let mut child = children.remove(&id).expect("registered child");
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::other("extension host stopped"));
        }
        Ok(())
    }

    /// Reclaim a finished job. `None` means [`Self::cleanup`] already killed it.
    fn take(&self, id: u64) -> Option<Child> {
        self.children.lock().ok()?.remove(&id)
    }

    fn cleanup(&self) {
        let Ok(mut children) = self.children.lock() else {
            return;
        };
        // ponytail: killing the shell orphans any grandchild it started; a process-group kill
        // needs `libc::killpg`, which the workspace's `unsafe_code = "deny"` rules out.
        for (_, mut child) in std::mem::take(&mut *children) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Run `cmd` through the platform shell and return its merged stdout/stderr.
///
/// The shell inherits Bootty's environment, which `shell_env::hydrate_from_login_shell` already
/// filled with the login shell's PATH at startup, so commands resolve the same tools the user's
/// terminal does.
fn shell_run_output(
    cmd: &str,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    // One pipe for both streams: a module's text keeps the interleaved output the old
    // single-file capture produced, and reading a single end cannot deadlock on a full buffer.
    run_output(shell_command(cmd), true, run_jobs, shutdown)
}

/// Run `argv` directly and return its stdout, leaving the platform shell out of it.
///
/// A module that needs no shell syntax — no pipes, no globbing, no redirects — spends two processes
/// per call going through one, and every argument has to survive quoting on the way. `argv` reaches
/// the program as written, and only the program is spawned. Errors go to the null device, matching
/// the `2>/dev/null` these call sites already asked the shell for.
fn exec_run_output(
    argv: &[String],
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| std::io::Error::other("bootty.exec needs a program to run"))?;
    let mut command = Command::new(program);
    command.args(args);
    run_output(command, false, run_jobs, shutdown)
}

fn run_output(
    mut command: Command,
    capture_stderr: bool,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    if shutdown.load(Ordering::Acquire) {
        return Err(std::io::Error::other("extension host stopped"));
    }
    let (mut reader, writer) = std::io::pipe()?;
    command.stdin(Stdio::null());
    if capture_stderr {
        command.stderr(writer.try_clone()?);
    } else {
        command.stderr(Stdio::null());
    }
    command.stdout(writer);

    let id = RUN_JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let child = command.spawn()?;
    // `command` holds the pipe's write end until it is dropped, and the read below ends only
    // once every writer is closed.
    drop(command);
    run_jobs.register(id, child, shutdown)?;

    let mut output = String::new();
    let read = std::io::Read::read_to_string(&mut reader, &mut output);
    let mut child = run_jobs
        .take(id)
        .ok_or_else(|| std::io::Error::other("extension host stopped"))?;
    let _ = child.wait();
    read?;
    Ok(output)
}

/// One process below a session's pane, as a module sees it.
struct DescendantProcess {
    /// Executable path when known, otherwise the process name.
    command: String,
    /// Full argument vector joined by spaces, `argv[0]` first, like `ps -o args=`.
    args: String,
}

/// The machine's process table, reused across `bootty.descendants` calls.
#[derive(Default)]
struct ProcessTree {
    system: System,
    listed_at: Option<Instant>,
}

/// Breadth-first walk of the processes below `root_pid`.
///
/// Breadth-first because callers want the shallowest interesting descendant (the agent CLI a pane
/// is running, not a tool that CLI spawned).
///
/// Two passes, because reading command lines is the expensive half: the machine-wide pass asks for
/// parent links only, and command lines are fetched for the handful of processes the walk actually
/// reaches. Asking for everything up front cost a `sysctl` per process on the machine.
fn descendant_processes(tree: &mut ProcessTree, root_pid: u32) -> Vec<DescendantProcess> {
    if tree
        .listed_at
        .is_none_or(|listed_at| listed_at.elapsed() >= PROCESS_TREE_TTL)
    {
        tree.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        tree.listed_at = Some(Instant::now());
    }

    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in tree.system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }

    let found = descendant_pids(&children, Pid::from_u32(root_pid));
    if found.is_empty() {
        return Vec::new();
    }

    tree.system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&found),
        false,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_exe(UpdateKind::Always),
    );
    found
        .into_iter()
        .filter_map(|pid| tree.system.process(pid))
        .map(|process| DescendantProcess {
            command: process
                .exe()
                .map(|exe| exe.to_string_lossy().into_owned())
                .unwrap_or_else(|| process.name().to_string_lossy().into_owned()),
            args: process
                .cmd()
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect()
}

fn descendant_pids(children: &HashMap<Pid, Vec<Pid>>, root_pid: Pid) -> Vec<Pid> {
    let mut found = Vec::new();
    let mut queue = std::collections::VecDeque::from([root_pid]);
    let mut visited = BTreeSet::new();
    while let Some(pid) = queue.pop_front() {
        if !visited.insert(pid) || found.len() >= DESCENDANT_SCAN_LIMIT {
            continue;
        }
        for child in children.get(&pid).into_iter().flatten() {
            if found.len() >= DESCENDANT_SCAN_LIMIT {
                break;
            }
            found.push(*child);
            queue.push_back(*child);
        }
    }
    found
}

#[cfg(not(windows))]
fn shell_command(cmd: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    command
}

#[cfg(windows)]
fn shell_command(cmd: &str) -> Command {
    use std::os::windows::process::CommandExt;

    let mut command = Command::new("cmd");
    command
        .creation_flags(windows_no_window_flag())
        .raw_arg(format!("/S /C {cmd}"));
    command
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
const fn windows_no_window_flag() -> u32 {
    0x0800_0000
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
    // Run a program directly from its argument vector: no shell process in front of it, no quoting
    // to get wrong. Use it for anything that needs no shell syntax; `bootty.run` covers the rest.
    let exec_cache = Arc::clone(&run_cache);
    bootty.set(
        "exec",
        lua.create_function(move |_, argv: Vec<String>| {
            exec_cache.exec(argv).map_err(mlua::Error::external)
        })?,
    )?;
    // Read what a command last printed without starting it. Pair with `bootty.exec` on a schedule:
    // exec keeps the answer current, read shows it as soon as it arrives and costs nothing.
    let read_cache = Arc::clone(&run_cache);
    bootty.set(
        "read",
        lua.create_function(move |_, argv: Vec<String>| Ok(read_cache.read(argv)))?,
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

    // Walk a process subtree natively. Modules used to shell out to `ps -axo` and rebuild the
    // whole machine's tree in Lua, which cost a full process listing several times a second.
    let process_tree = Mutex::new(ProcessTree::default());
    bootty.set(
        "descendants",
        lua.create_function(move |lua, root_pid: u32| {
            let table = lua.create_table()?;
            let Ok(mut tree) = process_tree.lock() else {
                return Ok(table);
            };
            for (index, descendant) in descendant_processes(&mut tree, root_pid)
                .into_iter()
                .enumerate()
            {
                let entry = lua.create_table()?;
                entry.set("command", descendant.command)?;
                entry.set("args", descendant.args)?;
                table.set(index + 1, entry)?;
            }
            Ok(table)
        })?,
    )?;

    let path_table = lua.create_table()?;
    path_table.set(
        "display",
        lua.create_function(|_, value: String| Ok(crate::strings::display_path(&value)))?,
    )?;
    path_table.set_readonly(true);
    bootty.set("path", path_table)?;

    let git_table = lua.create_table()?;
    let git_preview_branch = run_cache.preview_branch.clone();
    git_table.set(
        "branch",
        lua.create_function(move |_, cwd: String| {
            Ok(match &git_preview_branch {
                Some(branch) => Some(branch.clone()),
                None => crate::git::head_branch(&cwd),
            })
        })?,
    )?;
    // A counter for the working tree, bumped by the filesystem whenever something under it changes.
    // A module compares it against the value from its last `git` call to know whether asking again
    // could possibly say anything new. `0` means the tree is not watched and nothing can be assumed.
    let git_watch_previews = run_cache.preview_branch.is_some();
    git_table.set(
        "worktree_revision",
        lua.create_function(move |_, cwd: String| {
            Ok(if git_watch_previews {
                0
            } else {
                crate::git::worktree_revision(&cwd)
            })
        })?,
    )?;
    git_table.set_readonly(true);
    bootty.set("git", git_table)?;

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
                    entry.set("cache_key", format!("{}:{}", view.scope_key, session.id))?;
                    entry.set("name", session.name.as_str())?;
                    entry.set(
                        "display_name",
                        if session.display_name.is_empty() {
                            session.name.as_str()
                        } else {
                            session.display_name.as_str()
                        },
                    )?;
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
                    if let Some(value) = &session.pane_id {
                        entry.set("pane_id", value.as_str())?;
                    }
                    if let Some(value) = session.pane_pid {
                        entry.set("pane_pid", value)?;
                    }
                    if let Some(value) = &session.process {
                        entry.set("process", value.as_str())?;
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
