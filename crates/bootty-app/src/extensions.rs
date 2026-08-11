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
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
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
pub mod runtime;
pub use runtime::*;

use crate::commands::CommandCancellation;
#[cfg(test)]
use codexbar::command_invokes_usage as command_invokes_codexbar_usage;
use codexbar::{
    reject_reserved_shell_command, resolve_program as resolve_codexbar_program,
    validate_provider as validate_codexbar_provider,
};
#[cfg(test)]
use http::response_body as http_response_body;
use http::{get_local as http_get_local, get_local_cancellable};

/// Default refresh cadence for a module that doesn't declare its own `interval`.
const EXTENSION_LUA_LOAD_TIMEOUT: Duration = Duration::from_millis(250);
const EXTENSION_LUA_RENDER_TIMEOUT: Duration = Duration::from_millis(100);
const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);
/// Background poll granularity; a module fires on the first tick at or after its interval elapses.
const TICK: Duration = Duration::from_millis(8);
/// How often extension dirs are re-scanned for edited/added/removed module files (hot reload).
const RELOAD_SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Bounded handoff from the worker hot-reload boundary to the UI/control event
/// publisher. A full queue is coalesced into an explicit rebase publication.
const RELOAD_EVENT_QUEUE_LIMIT: usize = 64;
/// Bounds the authoritative module inventory carried by a reload rebase.
const RELOAD_MODULE_LIMIT: usize = 256;
const RELOAD_MODULE_ID_BYTES: usize = 256;
const RELOAD_MODULE_SNAPSHOT_BYTES: usize = 64 * 1024;
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
///
/// The cache and its refresh workers are deliberately bounded. User-provided modules can choose
/// arbitrary command strings, so an unbounded map or one thread per key would otherwise turn a
/// render loop into a resource leak.
const RUN_CACHE_ENTRY_LIMIT: usize = 256;
const RUN_CACHE_REFRESH_LIMIT: usize = EXTENSION_PROCESS_LIMIT;
const RUN_CACHE_QUOTA_ERROR: &str = "extension refresh quota exhausted";

#[derive(Default)]
struct RunCache {
    entries: Mutex<HashMap<String, RunEntry>>,
    /// Refresh handles are retained until they are reaped, so a completed worker is joined
    /// rather than silently detached. A reserved `None` handle is an active spawn slot.
    refresh_jobs: Arc<Mutex<BTreeMap<u64, RefreshJob>>>,
    next_refresh_job: AtomicU64,
    next_access: AtomicU64,
    mode: AtomicU8,
    waker: Option<Arc<Waker>>,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
    codexbar: Arc<CodexBarClient>,
    /// Branch a settings preview should show. Previews render against example sessions whose paths
    /// do not exist, so a real `HEAD` read has nothing to find.
    preview_branch: Option<String>,
}

struct RefreshJob {
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RefreshJob {
    fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
    }
}

#[derive(Default)]
struct RunEntry {
    output: String,
    refreshing: bool,
    last_used: u64,
}

#[derive(Default)]
struct CodexBarEntry {
    output: String,
    refreshing: bool,
    last_refresh: Option<Instant>,
    last_used: u64,
}

impl RunCache {
    fn with_waker(
        waker: Arc<Waker>,
        run_jobs: Arc<PlatformRunJobs>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let mut cache = Self::default();
        cache.waker = Some(waker);
        cache.run_jobs = run_jobs;
        cache.shutdown = shutdown;
        cache
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
                self.refresh(command)?;
                Ok((cached.clone().unwrap_or_default(), cached.is_some()))
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

    fn cached(&self, key: &str) -> Option<String> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get_mut(key)?;
        entry.last_used = self.next_access.fetch_add(1, Ordering::Relaxed);
        Some(entry.output.clone())
    }

    fn refresh(self: &Arc<Self>, command: RunCommand) -> std::io::Result<()> {
        self.reap_finished_jobs();
        let key = command.cache_key().into_owned();
        let job_id = self.next_refresh_job.fetch_add(1, Ordering::Relaxed);

        {
            let mut jobs = self
                .refresh_jobs
                .lock()
                .map_err(|_| std::io::Error::other("extension refresh jobs poisoned"))?;

            let mut entries = self
                .entries
                .lock()
                .map_err(|_| std::io::Error::other("extension run cache poisoned"))?;
            if let Some(entry) = entries.get_mut(&key) {
                if entry.refreshing {
                    return Ok(());
                }
                if jobs.len() >= RUN_CACHE_REFRESH_LIMIT {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        RUN_CACHE_QUOTA_ERROR,
                    ));
                }
                entry.refreshing = true;
                entry.last_used = self.next_access.fetch_add(1, Ordering::Relaxed);
            } else {
                if jobs.len() >= RUN_CACHE_REFRESH_LIMIT {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        RUN_CACHE_QUOTA_ERROR,
                    ));
                }
                if entries.len() >= RUN_CACHE_ENTRY_LIMIT {
                    let Some(eviction_key) = entries
                        .iter()
                        .filter(|(_, entry)| !entry.refreshing)
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(key, _)| key.clone())
                    else {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            RUN_CACHE_QUOTA_ERROR,
                        ));
                    };
                    entries.remove(&eviction_key);
                }
                entries.insert(
                    key.clone(),
                    RunEntry {
                        refreshing: true,
                        last_used: self.next_access.fetch_add(1, Ordering::Relaxed),
                        ..RunEntry::default()
                    },
                );
            }
            jobs.insert(job_id, RefreshJob { handle: None });
        }

        let run_jobs = Arc::clone(&self.run_jobs);
        let shutdown = Arc::clone(&self.shutdown);
        let cache = Arc::downgrade(self);
        let thread_key = key.clone();
        let handle = match std::thread::Builder::new()
            .name("bootty-run-refresh".to_owned())
            .spawn(move || {
                let output = command
                    .output(&run_jobs, &shutdown)
                    .map(|output| output.trim().to_owned())
                    .unwrap_or_else(|error| format!("bootty.run: {error}"));
                let Some(cache) = cache.upgrade() else {
                    return;
                };
                if let Ok(mut entries) = cache.entries.lock()
                    && let Some(entry) = entries.get_mut(&thread_key)
                {
                    entry.output = output;
                    entry.refreshing = false;
                    entry.last_used = cache.next_access.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(waker) = &cache.waker {
                    waker.force();
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                if let Ok(mut entries) = self.entries.lock()
                    && let Some(entry) = entries.get_mut(&key)
                {
                    if entry.output.is_empty() {
                        entries.remove(&key);
                    } else {
                        entry.refreshing = false;
                    }
                }
                if let Ok(mut jobs) = self.refresh_jobs.lock() {
                    jobs.remove(&job_id);
                }
                return Err(error);
            }
        };
        if let Ok(mut jobs) = self.refresh_jobs.lock()
            && let Some(job) = jobs.get_mut(&job_id)
        {
            job.handle = Some(handle);
        } else {
            let _ = handle.join();
        }
        Ok(())
    }

    fn reap_finished_jobs(&self) {
        let finished = {
            let Ok(mut jobs) = self.refresh_jobs.lock() else {
                return;
            };
            let ids: Vec<_> = jobs
                .iter()
                .filter(|(_, job)| job.is_finished())
                .map(|(id, _)| *id)
                .collect();
            ids.into_iter()
                .filter_map(|id| jobs.remove(&id).and_then(|job| job.handle))
                .collect::<Vec<_>>()
        };
        for handle in finished {
            let _ = handle.join();
        }
    }

    #[cfg(test)]
    fn cache_state(&self) -> (usize, usize) {
        self.reap_finished_jobs();
        let entries = self
            .entries
            .lock()
            .map(|entries| entries.len())
            .unwrap_or(0);
        let active = self.refresh_jobs.lock().map(|jobs| jobs.len()).unwrap_or(0);
        (entries, active)
    }

    fn refresh_codexbar_usage(self: &Arc<Self>, provider: String) {
        self.reap_finished_jobs();
        let job_id = self.next_refresh_job.fetch_add(1, Ordering::Relaxed);
        let Ok(mut jobs) = self.refresh_jobs.lock() else {
            return;
        };
        if jobs.len() >= RUN_CACHE_REFRESH_LIMIT {
            return;
        }
        jobs.insert(job_id, RefreshJob { handle: None });
        drop(jobs);

        if !self
            .codexbar
            .mark_refreshing(&provider, CODEXBAR_REFRESH_INTERVAL)
        {
            if let Ok(mut jobs) = self.refresh_jobs.lock() {
                jobs.remove(&job_id);
            }
            return;
        }

        let cache = Arc::downgrade(self);
        let codexbar = Arc::clone(&self.codexbar);
        let shutdown = Arc::clone(&self.shutdown);
        let thread_provider = provider.clone();
        let handle = match std::thread::Builder::new()
            .name("bootty-codexbar-refresh".to_owned())
            .spawn(move || {
                let output = codexbar
                    .fetch_usage(&thread_provider, &shutdown)
                    .map(|output| output.trim().to_owned())
                    .ok();
                let Some(cache) = cache.upgrade() else {
                    return;
                };
                let changed = cache.codexbar.finish_refresh(&thread_provider, output);
                if changed && let Some(waker) = &cache.waker {
                    waker.force();
                }
            }) {
            Ok(handle) => handle,
            Err(error) => {
                self.codexbar.cancel_refresh(&provider);
                if let Ok(mut jobs) = self.refresh_jobs.lock() {
                    jobs.remove(&job_id);
                }
                let _ = error;
                return;
            }
        };
        if let Ok(mut jobs) = self.refresh_jobs.lock()
            && let Some(job) = jobs.get_mut(&job_id)
        {
            job.handle = Some(handle);
        } else {
            let _ = handle.join();
        }
    }
}

impl Drop for RunCache {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.run_jobs.cleanup();
        self.codexbar.stop_server();
        let jobs = self
            .refresh_jobs
            .lock()
            .map(|mut jobs| std::mem::take(&mut *jobs))
            .unwrap_or_default();
        let deadline = Instant::now() + Duration::from_millis(300);
        for (_, job) in jobs {
            let Some(handle) = job.handle else {
                continue;
            };
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(4));
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
type CodexBarFetch = Arc<dyn Fn(&str) -> std::io::Result<String> + Send + Sync + 'static>;

#[derive(Default)]
struct CodexBarClient {
    server: Mutex<CodexBarServerState>,
    entries: Mutex<HashMap<String, CodexBarEntry>>,
    next_access: AtomicU64,
    #[cfg(test)]
    mock_usage: Mutex<HashMap<String, String>>,
    #[cfg(test)]
    fetch_override: Mutex<Option<CodexBarFetch>>,
}

#[derive(Default)]
struct CodexBarServerState {
    port: Option<u16>,
    child: Option<Child>,
}

impl Drop for CodexBarClient {
    fn drop(&mut self) {
        self.stop_server();
    }
}
impl CodexBarClient {
    fn stop_server(&self) {
        if let Ok(mut server) = self.server.lock() {
            if let Some(mut child) = server.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            server.port = None;
        }
    }

    #[cfg(test)]
    fn mock_usage(&self, provider: &str) -> Option<String> {
        self.mock_usage
            .lock()
            .ok()
            .and_then(|entries| entries.get(provider).cloned())
    }

    #[cfg(test)]
    fn set_mock_usage(&self, provider: &str, output: &str) {
        self.mock_usage
            .lock()
            .expect("codexbar mock usage")
            .insert(provider.to_owned(), output.to_owned());
    }

    fn cached(&self, provider: &str) -> Option<String> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get_mut(provider)?;
        entry.last_used = self.next_access.fetch_add(1, Ordering::Relaxed);
        Some(entry.output.clone())
    }

    fn mark_refreshing(&self, provider: &str, refresh_interval: Duration) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        if !entries.contains_key(provider) {
            if entries.len() >= RUN_CACHE_ENTRY_LIMIT {
                let Some(eviction_key) = entries
                    .iter()
                    .filter(|(_, entry)| !entry.refreshing)
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(key, _)| key.clone())
                else {
                    return false;
                };
                entries.remove(&eviction_key);
            }
            entries.insert(provider.to_owned(), CodexBarEntry::default());
        }
        let entry = entries.get_mut(provider).expect("codexbar entry inserted");
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
        entry.last_used = self.next_access.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn cancel_refresh(&self, provider: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            let remove = entries
                .get(provider)
                .is_some_and(|entry| entry.output.is_empty());
            if remove {
                entries.remove(provider);
            } else if let Some(entry) = entries.get_mut(provider) {
                entry.refreshing = false;
            }
        }
    }

    fn finish_refresh(&self, provider: &str, output: Option<String>) -> bool {
        let Ok(mut entries) = self.entries.lock() else {
            return false;
        };
        let Some(entry) = entries.get_mut(provider) else {
            return false;
        };
        entry.refreshing = false;
        let Some(output) = output else {
            return false;
        };
        if entry.output == output {
            return false;
        }
        entry.output = output;
        entry.last_used = self.next_access.fetch_add(1, Ordering::Relaxed);
        true
    }

    fn fetch_usage(&self, provider: &str, shutdown: &AtomicBool) -> std::io::Result<String> {
        #[cfg(test)]
        if let Some(fetch) = self
            .fetch_override
            .lock()
            .ok()
            .and_then(|fetch| fetch.clone())
        {
            return fetch(provider);
        }
        let port = self.ensure_server()?;
        get_local_cancellable(
            port,
            &format!("/usage?provider={provider}"),
            Duration::from_secs(35),
            shutdown,
        )
    }

    #[cfg(test)]
    fn set_fetch_override(&self, fetch: CodexBarFetch) {
        *self.fetch_override.lock().expect("codexbar fetch override") = Some(fetch);
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

/// The file state that determines whether an extension worker must reload a
/// source. Readability is distinct from mtime because a permission-only
/// transition must retire an already-active module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExtensionFileSignature {
    modified: Option<SystemTime>,
    readable: bool,
}

/// A file-backed Luau module lifecycle change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionReloadOperation {
    Loaded,
    Reloaded,
    Removed,
}

impl ExtensionReloadOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Loaded => "loaded",
            Self::Reloaded => "reloaded",
            Self::Removed => "removed",
        }
    }
}

/// One lifecycle change emitted at the extension worker's hot-reload boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionReloadEvent {
    pub extension_id: String,
    pub generation: u64,
    pub operation: ExtensionReloadOperation,
}

/// Atomically drained worker lifecycle events and the source's current state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionReloadDrain {
    pub events: Vec<ExtensionReloadEvent>,
    pub modules: Vec<ExtensionModuleGeneration>,
    pub inventory_revision: u64,
    pub requires_rebase: bool,
}

/// A file-backed module in an extension source's authoritative reload snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionModuleGeneration {
    pub extension_id: String,
    pub generation: u64,
}

#[derive(Default)]
struct ReloadEventQueue {
    events: VecDeque<ExtensionReloadEvent>,
    modules: BTreeMap<String, u64>,
    inventory_revision: u64,
    requires_rebase: bool,
}

impl ReloadEventQueue {
    fn publish(&mut self, event: ExtensionReloadEvent) {
        if self.requires_rebase {
            return;
        }
        if self.events.len() >= RELOAD_EVENT_QUEUE_LIMIT {
            self.events.clear();
            self.requires_rebase = true;
            return;
        }
        self.events.push_back(event);
    }
    fn mark_rebase(&mut self) {
        self.events.clear();
        self.requires_rebase = true;
    }

    fn set_modules(&mut self, modules: impl IntoIterator<Item = (String, u64)>) -> bool {
        let candidate = modules.into_iter().collect::<BTreeMap<_, _>>();
        if candidate.len() > RELOAD_MODULE_LIMIT
            || candidate
                .keys()
                .any(|extension_id| extension_id.len() > RELOAD_MODULE_ID_BYTES)
        {
            return false;
        }
        let encoded_bytes = candidate
            .keys()
            .map(|id| id.len() + std::mem::size_of::<u64>() + 32)
            .sum::<usize>();
        if encoded_bytes > RELOAD_MODULE_SNAPSHOT_BYTES {
            return false;
        }
        if candidate != self.modules {
            self.modules = candidate;
            self.inventory_revision = self.inventory_revision.saturating_add(1);
        }
        true
    }

    fn requeue(&mut self, drain: ExtensionReloadDrain) {
        let current_events = std::mem::take(&mut self.events);
        let mut merged_events = VecDeque::new();
        let mut seen_events = BTreeSet::new();
        let mut newest_generation = BTreeMap::new();
        for event in drain.events.into_iter().chain(current_events) {
            if newest_generation
                .get(&event.extension_id)
                .is_some_and(|generation| event.generation < *generation)
            {
                continue;
            }
            newest_generation
                .entry(event.extension_id.clone())
                .and_modify(|generation| *generation = (*generation).max(event.generation))
                .or_insert(event.generation);
            let key = (
                event.extension_id.clone(),
                event.generation,
                event.operation.as_str().to_owned(),
            );
            if seen_events.insert(key) {
                merged_events.push_back(event);
            }
        }
        self.events = merged_events;
        while self.events.len() > RELOAD_EVENT_QUEUE_LIMIT {
            self.events.pop_front();
            self.requires_rebase = true;
        }

        // `drain` snapshots the authoritative module inventory without removing it from
        // the worker queue. A newer scan may already have replaced that inventory (including
        // with an empty set), so never merge or restore the drained modules here.
        self.requires_rebase |= drain.requires_rebase;
    }

    fn drain(&mut self) -> ExtensionReloadDrain {
        ExtensionReloadDrain {
            events: self.events.drain(..).collect(),
            modules: self
                .modules
                .iter()
                .map(|(extension_id, generation)| ExtensionModuleGeneration {
                    extension_id: extension_id.clone(),
                    generation: *generation,
                })
                .collect(),
            inventory_revision: self.inventory_revision,
            requires_rebase: std::mem::take(&mut self.requires_rebase),
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
    /// File-backed module lifecycle changes awaiting publication by the control-plane owner.
    reload_events: Arc<RwLock<ReloadEventQueue>>,
    waker: Arc<Waker>,
    run_jobs: Arc<PlatformRunJobs>,
    shutdown: Arc<AtomicBool>,
    worker_cancellation: CommandCancellation,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl ExtensionHost {
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
        let reload_events: Arc<RwLock<ReloadEventQueue>> = Arc::default();
        let next_window_id = Arc::new(AtomicU64::new(1));
        let waker: Arc<Waker> = Arc::default();
        let run_jobs = Arc::new(PlatformRunJobs::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_cancellation = CommandCancellation::new();
        let thread_cancellation = worker_cancellation.clone();
        let worker_handle = std::thread::Builder::new()
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
                let reload_events = Arc::clone(&reload_events);
                let next_window_id = Arc::clone(&next_window_id);
                let waker = Arc::clone(&waker);
                let shutdown = Arc::clone(&shutdown);
                let run_jobs = Arc::clone(&run_jobs);
                move || {
                    if !thread_cancellation.try_start() {
                        return;
                    }
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
                        &thread_cancellation,
                        &run_jobs,
                        &reload_events,
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
            reload_events,
            waker,
            shutdown,
            run_jobs,
            worker_cancellation,
            worker: Mutex::new(worker_handle),
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

    /// Drains lifecycle changes from the worker with its current authoritative
    /// file-backed-module snapshot. The UI publishes these through AutomationHub.
    #[must_use]
    pub fn take_reload_events(&self) -> ExtensionReloadDrain {
        self.reload_events
            .write()
            .map(|mut queue| queue.drain())
            .unwrap_or_default()
    }
    /// Returns a failed publication to the worker queue so transient control-plane
    /// failures cannot lose lifecycle state.
    pub fn requeue_reload_events(&self, drain: ExtensionReloadDrain) {
        if let Ok(mut queue) = self.reload_events.write() {
            queue.requeue(drain);
        }
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
        self.worker_cancellation.request_cancel();
        self.shutdown.store(true, Ordering::Release);
        self.waker.wake();
        self.run_jobs.cleanup();
        join_extension_worker(&self.worker, Duration::from_millis(300));
    }
}

fn join_extension_worker(handle: &Mutex<Option<std::thread::JoinHandle<()>>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let finished = handle
            .lock()
            .ok()
            .and_then(|handle| handle.as_ref().map(std::thread::JoinHandle::is_finished))
            .unwrap_or(true);
        if finished || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    if let Ok(mut handle) = handle.lock()
        && handle
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        && let Some(handle) = handle.take()
    {
        let _ = handle.join();
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
    cancellation: &CommandCancellation,
    run_jobs: &Arc<PlatformRunJobs>,
    reload_events: &RwLock<ReloadEventQueue>,
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
    let mut modules = load_catalog_modules(&lua, catalogs, cancellation, shutdown);
    let mut signature = catalog_signature(catalogs);
    let initial_signature = signature.clone();
    let mut reload_generations = BTreeMap::new();
    let mut active_extension_ids = BTreeSet::new();
    let rejected = reconcile_extension_reloads(
        catalogs,
        &[],
        &initial_signature,
        &successful_module_names(&modules),
        &mut reload_generations,
        &mut active_extension_ids,
        reload_events,
    );
    if !rejected.is_empty() {
        signature.retain(|(path, _)| !rejected.contains(&extension_id(path)));
    }
    retain_accepted_modules(
        &mut modules,
        catalogs,
        &initial_signature,
        &active_extension_ids,
    );
    let mut last_scan = Instant::now();
    let mut system = System::new();
    let battery = BatteryManager::new().ok();
    let mut last_metrics: Option<Instant> = None;
    while !shutdown.load(Ordering::Relaxed) && !cancellation.is_cancel_requested() {
        let now = Instant::now();
        // A structural mux change (reorder, session/window added or removed) forces a re-render
        // this tick, so the new layout shows immediately instead of after the poll interval.
        let force = waker.take_force();
        // Hot reload: re-evaluate when extension files are added, edited, or removed.
        if now.duration_since(last_scan) >= RELOAD_SCAN_INTERVAL {
            last_scan = now;
            let current = catalog_signature(catalogs);
            if current != signature {
                let previous = signature.clone();
                modules = load_catalog_modules(&lua, catalogs, cancellation, shutdown);
                let rejected = reconcile_extension_reloads(
                    catalogs,
                    &previous,
                    &current,
                    &successful_module_names(&modules),
                    &mut reload_generations,
                    &mut active_extension_ids,
                    reload_events,
                );
                signature = current
                    .iter()
                    .filter(|(path, _)| !rejected.contains(&extension_id(path)))
                    .cloned()
                    .collect();
                retain_accepted_modules(&mut modules, catalogs, &current, &active_extension_ids);
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
            install_lua_interrupt(
                &lua,
                cancellation,
                shutdown,
                Instant::now() + EXTENSION_LUA_RENDER_TIMEOUT,
            );
            let reorder_result = handler.call::<()>((request.source, request.before));
            lua.remove_interrupt();
            if let Err(error) = reorder_result
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
                install_lua_interrupt(
                    &lua,
                    cancellation,
                    shutdown,
                    Instant::now() + EXTENSION_LUA_RENDER_TIMEOUT,
                );
                let result = handler.call::<()>((key, value));
                lua.remove_interrupt();
                let _ = result;
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
                let produced = run_module_bounded(&lua, &module.body, cancellation, shutdown);
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

fn load_catalog_modules(
    lua: &Lua,
    catalogs: &[ModuleCatalog],
    cancellation: &CommandCancellation,
    shutdown: &Arc<AtomicBool>,
) -> Vec<LoadedModule> {
    catalogs
        .iter()
        .flat_map(|catalog| {
            load_modules_bounded(lua, &catalog.dir, catalog.builtins, cancellation, shutdown)
                .into_iter()
                .map(|mut module| {
                    module.name.insert_str(0, catalog.prefix);
                    module
                })
        })
        .collect()
}

fn catalog_signature(catalogs: &[ModuleCatalog]) -> Vec<(PathBuf, ExtensionFileSignature)> {
    let mut signature = catalogs
        .iter()
        .flat_map(|catalog| dir_signature(&catalog.dir))
        .collect::<Vec<_>>();
    signature.sort();
    signature
}

fn successful_module_names(modules: &[LoadedModule]) -> BTreeSet<String> {
    modules
        .iter()
        .filter(|module| !matches!(&module.body, ModuleBody::LoadError(_)))
        .map(|module| module.name.clone())
        .collect()
}

fn extension_id(path: &Path) -> String {
    format!("path:{}", path.to_string_lossy())
}

fn extension_signature_entries(
    signature: &[(PathBuf, ExtensionFileSignature)],
) -> BTreeMap<String, ExtensionFileSignature> {
    signature
        .iter()
        .map(|(path, modified)| (extension_id(path), *modified))
        .collect()
}

fn catalog_module_name(catalogs: &[ModuleCatalog], path: &Path) -> Option<String> {
    let catalog = catalogs
        .iter()
        .find(|catalog| path.parent() == Some(catalog.dir.as_path()))?;
    let name = path.file_stem()?.to_str()?;
    Some(format!("{}{}", catalog.prefix, name))
}

fn successful_extension_ids(
    catalogs: &[ModuleCatalog],
    signature: &[(PathBuf, ExtensionFileSignature)],
    successful_modules: &BTreeSet<String>,
) -> BTreeSet<String> {
    signature
        .iter()
        .filter_map(|(path, _)| {
            catalog_module_name(catalogs, path)
                .filter(|name| successful_modules.contains(name))
                .map(|_| extension_id(path))
        })
        .collect()
}

fn module_inventory_within_limits(active: &BTreeSet<String>) -> bool {
    active.len() <= RELOAD_MODULE_LIMIT
        && active
            .iter()
            .all(|extension_id| extension_id.len() <= RELOAD_MODULE_ID_BYTES)
        && active
            .iter()
            .map(|extension_id| extension_id.len() + std::mem::size_of::<u64>() + 32)
            .sum::<usize>()
            <= RELOAD_MODULE_SNAPSHOT_BYTES
}

fn next_extension_generation(generations: &mut BTreeMap<String, u64>, extension_id: &str) -> u64 {
    let generation = generations.entry(extension_id.to_owned()).or_insert(0);
    *generation = generation.saturating_add(1);
    *generation
}

fn reconcile_extension_reloads(
    catalogs: &[ModuleCatalog],
    previous_signature: &[(PathBuf, ExtensionFileSignature)],
    current_signature: &[(PathBuf, ExtensionFileSignature)],
    successful_modules: &BTreeSet<String>,
    generations: &mut BTreeMap<String, u64>,
    active_extensions: &mut BTreeSet<String>,
    reload_events: &RwLock<ReloadEventQueue>,
) -> BTreeSet<String> {
    let previous = extension_signature_entries(previous_signature);
    let current = extension_signature_entries(current_signature);
    let successful = successful_extension_ids(catalogs, current_signature, successful_modules);
    let mut retained = BTreeSet::new();
    let mut removed = Vec::new();

    // Remove failed and vanished modules before considering additions. This makes a
    // same-scan replacement fit at the limit instead of rejecting the replacement
    // because its predecessor still occupies a slot.
    for extension_id in active_extensions.iter() {
        if current.contains_key(extension_id) && successful.contains(extension_id) {
            retained.insert(extension_id.clone());
        } else {
            removed.push(extension_id.clone());
        }
    }

    let mut accepted = retained.clone();
    let mut rejected = BTreeSet::new();
    for extension_id in successful.iter() {
        if accepted.contains(extension_id) {
            continue;
        }
        let mut candidate = accepted.clone();
        candidate.insert(extension_id.clone());
        if module_inventory_within_limits(&candidate) {
            accepted.insert(extension_id.clone());
        } else {
            rejected.insert(extension_id.clone());
        }
    }

    let mut events = Vec::new();
    for extension_id in removed {
        active_extensions.remove(&extension_id);
        events.push(ExtensionReloadEvent {
            extension_id: extension_id.clone(),
            generation: next_extension_generation(generations, &extension_id),
            operation: ExtensionReloadOperation::Removed,
        });
    }

    for (extension_id, modified) in &current {
        if !successful.contains(extension_id) || !accepted.contains(extension_id) {
            continue;
        }
        let operation = match previous.get(extension_id) {
            None => Some(ExtensionReloadOperation::Loaded),
            Some(_) if !active_extensions.contains(extension_id) => {
                Some(ExtensionReloadOperation::Loaded)
            }
            Some(previous_modified) if previous_modified != modified => {
                Some(ExtensionReloadOperation::Reloaded)
            }
            Some(_) => None,
        };
        if let Some(operation) = operation {
            active_extensions.insert(extension_id.clone());
            events.push(ExtensionReloadEvent {
                extension_id: extension_id.clone(),
                generation: next_extension_generation(generations, extension_id),
                operation,
            });
        }
    }

    // Keep the accepted set authoritative even if a caller supplied stale state.
    *active_extensions = accepted;
    if let Ok(mut queue) = reload_events.write() {
        if !queue.set_modules(active_extensions.iter().filter_map(|extension_id| {
            generations
                .get(extension_id)
                .copied()
                .map(|generation| (extension_id.clone(), generation))
        })) {
            queue.mark_rebase();
        }
        if !rejected.is_empty() {
            // An over-limit source is deliberately not acknowledged in the accepted
            // signature by the worker. Force a bounded rebase publication so consumers
            // receive a diagnostic lifecycle boundary instead of a partial inventory.
            queue.mark_rebase();
        }
        for event in events {
            queue.publish(event);
        }
    }
    rejected
}

fn accepted_user_module_names(
    catalogs: &[ModuleCatalog],
    current_signature: &[(PathBuf, ExtensionFileSignature)],
    active_extensions: &BTreeSet<String>,
) -> BTreeSet<String> {
    current_signature
        .iter()
        .filter_map(|(path, _)| {
            let id = extension_id(path);
            active_extensions
                .contains(&id)
                .then(|| catalog_module_name(catalogs, path))
                .flatten()
        })
        .collect()
}

fn retain_accepted_modules(
    modules: &mut Vec<LoadedModule>,
    catalogs: &[ModuleCatalog],
    current_signature: &[(PathBuf, ExtensionFileSignature)],
    active_extensions: &BTreeSet<String>,
) {
    let user_module_names = current_signature
        .iter()
        .filter_map(|(path, _)| catalog_module_name(catalogs, path))
        .collect::<BTreeSet<_>>();
    let accepted_names = accepted_user_module_names(catalogs, current_signature, active_extensions);
    modules.retain(|module| {
        !user_module_names.contains(&module.name) || accepted_names.contains(&module.name)
    });
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
    let mut cache = RunCache::default();
    cache.preview_branch = Some("feature/module-previews".to_owned());
    let cache = Arc::new(cache);
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
                    last_used: cache.next_access.fetch_add(1, Ordering::Relaxed),
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
        let provider: Table = lua.load("return bootty.sidebar.session_facts()").eval()?;
        let records: Table = lua
            .load(
                r#"return {
                    {
                        agent_id = "preview/codex",
                        provider = "codex",
                        session_key = "preview:binding:$1",
                        display_name = "Codex",
                        lifecycle = "running",
                        activity = "working",
                    },
                }"#,
            )
            .eval()?;
        provider
            .get::<Function>("set_records")?
            .call::<()>(records)?;
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

/// Resolve one effective file per module name. `.luau` has the same precedence
/// as [`user_module_path`], so a lower-priority `.lua` sibling can never
/// masquerade as a successfully loaded version of a failed `.luau` override.
fn extension_module_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: BTreeMap<String, PathBuf> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_extension_module_file(&path) {
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let replace = match paths.get(name) {
            None => true,
            Some(current) => {
                extension_module_priority(&path) < extension_module_priority(current)
                    || (extension_module_priority(&path) == extension_module_priority(current)
                        && path < *current)
            }
        };
        if replace {
            paths.insert(name.to_owned(), path);
        }
    }
    paths.into_values().collect()
}

fn extension_module_priority(path: &Path) -> u8 {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("luau") => 0,
        Some("lua") => 1,
        _ => u8::MAX,
    }
}

/// Sorted `(path, mtime, readability)` state for effective module files. A
/// permission-only transition must trigger a reload so active stale state is
/// retired rather than silently retained.
fn dir_signature(dir: &Path) -> Vec<(PathBuf, ExtensionFileSignature)> {
    let mut signature = extension_module_paths(dir)
        .into_iter()
        .map(|path| {
            let modified = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok();
            let readable = std::fs::File::open(&path).is_ok();
            (path, ExtensionFileSignature { modified, readable })
        })
        .collect::<Vec<_>>();
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
    cancelled: AtomicBool,
}

impl PlatformRunJobs {
    fn register(&self, id: u64, child: Child, shutdown: &AtomicBool) -> std::io::Result<()> {
        let mut children = self
            .children
            .lock()
            .map_err(|_| std::io::Error::other("extension run jobs poisoned"))?;
        children.insert(id, child);
        if shutdown.load(Ordering::Acquire) || self.cancelled.load(Ordering::Acquire) {
            if let Some(mut child) = children.remove(&id) {
                terminate_platform_child(&mut child);
            }
            return Err(std::io::Error::other("extension host stopped"));
        }
        Ok(())
    }

    fn take(&self, id: u64) -> Option<Child> {
        self.children.lock().ok()?.remove(&id)
    }

    fn terminate(&self, id: u64) {
        if let Ok(mut children) = self.children.lock()
            && let Some(mut child) = children.remove(&id)
        {
            terminate_platform_child(&mut child);
        }
    }

    fn cleanup(&self) {
        self.cancelled.store(true, Ordering::Release);
        let Ok(mut children) = self.children.lock() else {
            return;
        };
        for (_, mut child) in std::mem::take(&mut *children) {
            terminate_platform_child(&mut child);
        }
    }
}

fn terminate_platform_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status();
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(4));
        }
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_millis(100);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    let _ = child.try_wait();
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
    shell_command_output(shell_command(cmd), run_jobs, shutdown)
}

/// Runs a configured platform-shell command with merged stdout and stderr.
fn shell_command_output(
    command: Command,
    run_jobs: &PlatformRunJobs,
    shutdown: &AtomicBool,
) -> std::io::Result<String> {
    // One pipe for both streams: a module's text keeps the interleaved output the old
    // single-file capture produced, and reading a single end cannot deadlock on a full buffer.
    run_output(command, true, run_jobs, shutdown)
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
    let (reader, writer) = std::io::pipe()?;
    command.stdin(Stdio::null());
    if capture_stderr {
        command.stderr(writer.try_clone()?);
    } else {
        command.stderr(Stdio::null());
    }
    command.stdout(writer);
    #[cfg(unix)]
    command.process_group(0);

    let id = RUN_JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let child = command.spawn()?;
    drop(command);
    run_jobs.register(id, child, shutdown)?;
    let (output_tx, output_rx) = std::sync::mpsc::sync_channel(8);
    let reader_handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => {
                    let _ = output_tx.send(Ok(None));
                    break;
                }
                Ok(size) => {
                    if output_tx.send(Ok(Some(chunk[..size].to_vec()))).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = output_tx.send(Err(error));
                    break;
                }
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut output = Vec::new();
    let result = loop {
        if shutdown.load(Ordering::Acquire) {
            run_jobs.terminate(id);
            break Err(std::io::Error::other("extension host stopped"));
        }
        if Instant::now() >= deadline {
            run_jobs.terminate(id);
            break Err(std::io::Error::other("extension command deadline expired"));
        }
        match output_rx.recv_timeout(Duration::from_millis(8)) {
            Ok(Ok(None)) => break Ok(()),
            Ok(Ok(Some(chunk))) => {
                if output.len().saturating_add(chunk.len()) > EXTENSION_PROCESS_BYTES {
                    run_jobs.terminate(id);
                    break Err(std::io::Error::other("extension output limit exceeded"));
                }
                output.extend_from_slice(&chunk);
            }
            Ok(Err(error)) => {
                run_jobs.terminate(id);
                break Err(error);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Err(std::io::Error::other("extension output reader stopped"));
            }
        }
    };
    drop(output_rx);
    if result.is_err() {
        run_jobs.terminate(id);
    }
    let mut output_closed_with_live_child = false;
    let mut cancelled_by_cleanup = false;
    if let Some(mut child) = run_jobs.take(id) {
        let wait_deadline = Instant::now() + Duration::from_millis(100);
        let mut exited = child.try_wait().ok().flatten().is_some();
        while !exited && Instant::now() < wait_deadline {
            std::thread::sleep(Duration::from_millis(4));
            exited = child.try_wait().ok().flatten().is_some();
        }
        if !exited {
            output_closed_with_live_child = true;
            terminate_platform_child(&mut child);
        }
    } else if result.is_ok() {
        // `cleanup` removes a child from the registry before killing it. If its pipe happens to
        // close cleanly first, treating the partial bytes as success would leak cancelled output
        // into the module cache.
        cancelled_by_cleanup = true;
    }
    let join_deadline = Instant::now() + Duration::from_millis(150);
    while !reader_handle.is_finished() && Instant::now() < join_deadline {
        std::thread::sleep(Duration::from_millis(4));
    }
    if reader_handle.is_finished() {
        let _ = reader_handle.join();
    }
    result?;
    if cancelled_by_cleanup {
        return Err(std::io::Error::other("extension command cancelled"));
    }
    if output_closed_with_live_child {
        return Err(std::io::Error::other(
            "process_still_running_after_output_closed",
        ));
    }
    String::from_utf8(output).map_err(|error| std::io::Error::other(error.to_string()))
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

fn preview_diff_counts(run_cache: &RunCache) -> Option<(u64, u64)> {
    let command = RunCommand::Exec(
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
    );
    let output = run_cache.cached(&command.cache_key())?;
    let mut totals: Option<(u64, u64)> = None;
    for line in output.lines() {
        let mut fields = line.split('\t');
        let added = fields.next()?.parse::<u64>().ok()?;
        let removed = fields.next()?.parse::<u64>().ok()?;
        let (total_added, total_removed) = totals.get_or_insert((0, 0));
        *total_added = (*total_added).saturating_add(added);
        *total_removed = (*total_removed).saturating_add(removed);
    }
    totals
}

fn enrich_session_facts(facts: &Table, session: &Table, run_cache: &RunCache) -> mlua::Result<()> {
    let cwd = session.get::<Option<String>>("cwd")?;
    let branch = run_cache
        .preview_branch
        .clone()
        .or_else(|| cwd.as_deref().and_then(crate::git::head_branch));
    facts.set("branch", branch.clone())?;
    facts.set("branch_status", branch.as_ref().map(|_| "current"))?;
    if run_cache.preview_branch.is_some() {
        let (added, removed) = preview_diff_counts(run_cache).unwrap_or((0, 0));
        facts.set("diff_added", added)?;
        facts.set("diff_removed", removed)?;
    } else {
        facts.set("diff_added", Option::<u64>::None)?;
        facts.set("diff_removed", Option::<u64>::None)?;
    }
    Ok(())
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
    let session_facts: Function = sidebar_table.get("session_facts")?;
    let session_facts_provider: Table = session_facts.call(())?;
    let original_get: Function = session_facts_provider.get("get")?;
    let facts_run_cache = Arc::clone(&run_cache);
    session_facts_provider.set(
        "get",
        lua.create_function(move |_, session: Table| {
            let facts: Table = original_get.call(session.clone())?;
            enrich_session_facts(&facts, &session, &facts_run_cache)?;
            Ok(facts)
        })?,
    )?;
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

fn load_modules_bounded(
    lua: &Lua,
    dir: &Path,
    builtins: &'static [(&'static str, &'static str)],
    cancellation: &CommandCancellation,
    shutdown: &Arc<AtomicBool>,
) -> Vec<LoadedModule> {
    let mut sources = builtins
        .iter()
        .map(|(name, source)| ((*name).to_owned(), Ok((*source).to_owned())))
        .collect::<BTreeMap<_, _>>();
    for path in extension_module_paths(dir) {
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let source = std::fs::read_to_string(&path).map_err(|error| first_line(&error.to_string()));
        sources.insert(name.to_owned(), source);
    }
    sources
        .into_iter()
        .map(|(name, source)| match source {
            Ok(code) => {
                install_lua_interrupt(
                    lua,
                    cancellation,
                    shutdown,
                    Instant::now() + EXTENSION_LUA_LOAD_TIMEOUT,
                );
                let result = module_environment(lua).and_then(|env| {
                    lua.load(&code)
                        .set_name(&name)
                        .set_environment(env)
                        .eval::<Value>()
                });
                lua.remove_interrupt();
                match result {
                    Ok(value) => {
                        loaded_module_from_value(name.clone(), value).unwrap_or_else(|| {
                            load_error(
                                name,
                                "must return a function or { render = ... }".to_owned(),
                            )
                        })
                    }
                    Err(error) => load_error(name, first_line(&error.to_string())),
                }
            }
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

fn install_lua_interrupt(
    lua: &Lua,
    cancellation: &CommandCancellation,
    shutdown: &Arc<AtomicBool>,
    deadline: Instant,
) {
    let cancellation = cancellation.clone();
    let shutdown = Arc::clone(shutdown);
    lua.set_interrupt(move |_| {
        if shutdown.load(Ordering::Acquire) || cancellation.is_cancel_requested() {
            return Err(mlua::Error::external("cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(mlua::Error::external("deadline_exceeded"));
        }
        Ok(VmState::Continue)
    });
}

fn run_module_bounded(
    lua: &Lua,
    body: &ModuleBody,
    cancellation: &CommandCancellation,
    shutdown: &Arc<AtomicBool>,
) -> Vec<ModuleItem> {
    install_lua_interrupt(
        lua,
        cancellation,
        shutdown,
        Instant::now() + EXTENSION_LUA_RENDER_TIMEOUT,
    );
    let produced = match body {
        ModuleBody::Render(render) => match render.call::<Value>(()) {
            Ok(value) => items_from_value(value),
            Err(error) => vec![error_item(&error.to_string())],
        },
        ModuleBody::LoadError(message) => vec![error_item(message)],
    };
    lua.remove_interrupt();
    produced
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

    fn load_modules(
        lua: &Lua,
        dir: &Path,
        builtins: &'static [(&'static str, &'static str)],
    ) -> Vec<LoadedModule> {
        let cancellation = CommandCancellation::new();
        let shutdown = Arc::new(AtomicBool::new(false));
        super::load_modules_bounded(lua, dir, builtins, &cancellation, &shutdown)
    }

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
    fn diff_summary_reserves_width_for_long_removed_count() {
        let items = run_source(
            r##"return function()
                return {
                    {
                        primitives = bootty.ui.diff_summary(1, 123456, {
                            success = "#a6e3a1",
                            destructive = "#f38ba8",
                        }),
                    },
                }
            end"##,
        );
        let x_offsets = items[0]
            .primitives
            .iter()
            .filter_map(|primitive| match primitive {
                ModulePrimitive::Text { x, .. } => Some(x.px),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(x_offsets.len(), 2);
        assert!(x_offsets[1] - x_offsets[0] >= 60.0);
    }

    #[test]
    fn module_preview_runs_source_with_example_host_data() {
        let items = preview_module_source(
            r#"return function()
                local metrics = bootty.metrics()
                local sessions = bootty.sessions()
                return {
                    {
                        text = string.format(
                            "%.0f%% · %s · %d",
                            metrics.cpu,
                            sessions[1].name,
                            sessions[1].ports[1]
                        )
                    }
                }
            end"#,
            "preview",
            &[("accent".to_owned(), "#89b4fa".to_owned())],
        );

        assert_eq!(items[0].text, "42% · work/api · 3000");
    }

    #[test]
    fn status_builtin_previews_produce_renderable_items() {
        let theme = crate::theme::theme_tokens(
            &crate::config::BoottyConfig::default(),
            crate::config::AppearanceVariant::Dark,
        );
        for (name, source) in BUILTIN_STATUS_EXTENSIONS {
            let items = preview_module_source(source, name, &theme);
            assert!(!items.is_empty(), "{name} preview was empty");
            assert!(
                items.iter().all(|item| item.fg != Some(ERROR_COLOR)),
                "{name} preview failed: {items:?}"
            );
        }
    }

    #[test]
    fn sidebar_and_session_previews_use_seeded_example_data() {
        let theme = crate::theme::theme_tokens(
            &crate::config::BoottyConfig::default(),
            crate::config::AppearanceVariant::Dark,
        );
        let usage = preview_builtin_module(ModuleKind::Sidebar, "codexbar", &theme);
        assert_eq!(
            usage
                .iter()
                .filter(|item| item.kind.as_deref() == Some("footer"))
                .count(),
            4
        );

        for (name, source) in BUILTIN_SESSION_EXTENSIONS {
            let items = preview_module_source(source, name, &theme);
            assert!(!items.is_empty(), "{name} preview was empty");
            assert!(
                items.iter().all(|item| item.fg != Some(ERROR_COLOR)),
                "{name} preview failed: {items:?}"
            );
            if *name == "process" {
                assert!(
                    items
                        .iter()
                        .any(|item| item.key.as_deref() == Some("$1:process")),
                    "dirty session process should remain visible"
                );
            }
        }
    }

    #[test]
    fn session_preview_composes_with_example_sidebar_sessions() {
        let theme = crate::theme::theme_tokens(
            &crate::config::BoottyConfig::default(),
            crate::config::AppearanceVariant::Dark,
        );
        let base = preview_builtin_module(ModuleKind::Sidebar, "sessions", &theme);
        let ports = preview_builtin_module(ModuleKind::Session, "ports", &theme);
        let items = crate::app::compose_session_module_items(base, ports);

        assert!(
            items
                .iter()
                .any(|item| { item.kind.as_deref() == Some("group") && item.text == "work" })
        );
        assert!(
            items
                .iter()
                .any(|item| item.key.as_deref() == Some("$1:ports"))
        );
    }

    #[test]
    fn module_preview_stops_runaway_source() {
        let started = Instant::now();
        let items =
            preview_module_source("return function() while true do end end", "preview", &[]);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(items[0].text.contains("preview exceeded 50 ms"));
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
        assert_builtins_load(
            &lua,
            dir.path(),
            BUILTIN_SESSION_EXTENSIONS,
            &[
                "diffs",
                "process",
                "agent",
                "directory",
                "branch",
                "ports",
                "progress",
            ],
        );
    }

    #[test]
    fn sidebar_facts_use_explicit_authority_and_scoped_correlation() {
        let run_cache = Arc::new(RunCache::default());
        run_cache.set_mode(RunMode::Cached);
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            run_cache,
        )
        .expect("lua host");
        let provider: Table = lua
            .load("return bootty.sidebar.session_facts()")
            .eval()
            .expect("facts provider");
        let records: Table = lua
            .load(
                r#"return {
					{
						agent_id = "opaque-native",
						session_key = "1:1:$1",
						terminal = { terminal_id = "%1" },
						lifecycle = "running",
					},
					{
						agent_id = "terminal-native",
						terminal = { terminal_id = "%2" },
						lifecycle = "waiting",
					},
				}"#,
            )
            .eval()
            .expect("authoritative agent records");
        provider
            .get::<Function>("set_records")
            .expect("set records")
            .call::<()>(records)
            .expect("authoritative agent provider");
        let refresh: Function = provider.get("refresh").expect("refresh");
        let get: Function = provider.get("get").expect("get");
        let session = |scope: &str, pane: &str, process: &str| {
            let value = lua.create_table().expect("session");
            value.set("id", "$1").expect("id");
            value.set("cache_key", scope).expect("scope");
            value.set("pane_id", pane).expect("pane");
            value.set("process", process).expect("process");
            value.set("selected", true).expect("selected");
            value
        };
        let refresh_one = |value: Table| {
            let sessions = lua.create_table().expect("sessions");
            sessions.set(1, value).expect("session entry");
            refresh.call::<()>(sessions).expect("refresh facts");
        };

        let first = session("1:1:$1", "%1", "codex");
        refresh_one(first.clone());
        let first_facts: Table = get.call(first).expect("first facts");
        assert!(first_facts.get::<bool>("authoritative").unwrap());
        assert_eq!(
            first_facts.get::<String>("agent_id").unwrap(),
            "opaque-native"
        );
        assert_eq!(first_facts.get::<String>("lifecycle").unwrap(), "running");
        assert!(
            first_facts
                .get::<Option<String>>("provider")
                .unwrap()
                .is_none()
        );
        assert!(
            first_facts
                .get::<Option<String>>("activity")
                .unwrap()
                .is_none()
        );

        let other_scope = session("2:2:$1", "%1", "codex");
        refresh_one(other_scope.clone());
        let other_facts: Table = get.call(other_scope).expect("other facts");
        assert!(!other_facts.get::<bool>("authoritative").unwrap());
        assert!(
            other_facts
                .get::<Option<String>>("agent_id")
                .unwrap()
                .is_none()
        );

        let replacement_pane = session("2:2:$1", "%2", "cargo");
        refresh_one(replacement_pane.clone());
        let replacement_facts: Table = get.call(replacement_pane).expect("replacement facts");
        assert!(replacement_facts.get::<bool>("authoritative").unwrap());
        assert_eq!(
            replacement_facts.get::<String>("agent_id").unwrap(),
            "terminal-native"
        );
        assert_eq!(
            replacement_facts.get::<String>("lifecycle").unwrap(),
            "waiting"
        );
    }

    #[test]
    fn sidebar_facts_never_shell_for_process_or_terminal_inference() {
        let mut cache = RunCache::default();
        cache.shutdown = Arc::new(AtomicBool::new(true));
        let run_cache = Arc::new(cache);
        run_cache.set_mode(RunMode::Refresh);
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::clone(&run_cache),
        )
        .expect("lua host");
        let provider: Table = lua
            .load("return bootty.sidebar.session_facts()")
            .eval()
            .expect("facts provider");
        let sessions = lua.create_table().expect("sessions");
        for index in 1..=3 {
            let session = lua.create_table().expect("session");
            session.set("id", format!("${index}")).expect("id");
            session
                .set("cache_key", format!("1:1:${index}"))
                .expect("cache key");
            session.set("pane_id", format!("%{index}")).expect("pane");
            session.set("process", "codex").expect("process");
            sessions.set(index, session).expect("session entry");
        }
        let refresh: Function = provider.get("refresh").expect("refresh");
        refresh
            .call::<()>(sessions.clone())
            .expect("first module refresh");
        refresh.call::<()>(sessions).expect("second module refresh");
        assert!(
            run_cache.entries.lock().expect("entries").is_empty(),
            "agent facts must not shell out to classify processes or scrape panes"
        );
    }

    #[test]
    fn sidebar_facts_preserve_only_host_session_context() {
        let run_cache = Arc::new(RunCache::default());
        run_cache.set_mode(RunMode::Cached);
        let lua = setup_lua(
            &[],
            Arc::default(),
            Arc::default(),
            Arc::default(),
            Arc::clone(&run_cache),
        )
        .expect("lua host");
        let provider: Table = lua
            .load("return bootty.sidebar.session_facts()")
            .eval()
            .expect("facts provider");
        let session = lua.create_table().expect("session");
        session.set("id", "$1").expect("id");
        session.set("cache_key", "1:1:$1").expect("cache key");
        session.set("cwd", "/missing/repo").expect("cwd");
        session.set("process", "zsh").expect("process");
        let sessions = lua.create_table().expect("sessions");
        sessions.set(1, session.clone()).expect("session entry");
        let refresh: Function = provider.get("refresh").expect("refresh");
        refresh.call::<()>(sessions).expect("refresh");
        let facts: Table = provider
            .get::<Function>("get")
            .expect("get")
            .call(session)
            .expect("facts");
        assert_eq!(facts.get::<String>("display_process").unwrap(), "zsh");
        assert!(facts.get::<Option<u32>>("diff_added").unwrap().is_none());
        assert!(
            run_cache.entries.lock().expect("entries").is_empty(),
            "agent fact providers must not derive repository facts as agent state"
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

    #[cfg(not(windows))]
    #[test]
    fn descendant_processes_reaches_a_grandchild_of_the_scanned_pid() {
        // The trailing command keeps the shell from `exec`ing `sleep`, so `sleep` really is a
        // grandchild of this test process and the walk has to go two levels deep to see it.
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30; true"])
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn shell");
        let mut tree = ProcessTree::default();

        let deadline = Instant::now() + Duration::from_secs(5);
        let found = loop {
            // A fresh listing each round, so the walk sees the grandchild as soon as it exists.
            tree.listed_at = None;
            let commands = descendant_processes(&mut tree, std::process::id())
                .into_iter()
                .any(|process| process.command.contains("sleep") || process.args.contains("sleep"));
            if commands || Instant::now() >= deadline {
                break commands;
            }
        };
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            found,
            "the walk should reach `sleep` under the shell it started"
        );
    }

    #[test]
    fn descendant_scan_caps_a_wide_process_tree() {
        let root = Pid::from_u32(1);
        let children = HashMap::from([(root, (2..=400).map(Pid::from_u32).collect::<Vec<_>>())]);

        let found = descendant_pids(&children, root);

        assert_eq!(found.len(), DESCENDANT_SCAN_LIMIT);
        assert_eq!(found.first().map(|pid| pid.as_u32()), Some(2));
        assert_eq!(found.last().map(|pid| pid.as_u32()), Some(257));
    }

    #[cfg(unix)]
    #[test]
    fn a_job_registered_after_shutdown_is_killed_immediately() {
        let jobs = PlatformRunJobs::default();
        let shutdown = AtomicBool::new(true);
        let child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn stand-in job");

        let error = jobs
            .register(7, child, &shutdown)
            .expect_err("shutdown must reject a late registration");

        assert_eq!(error.to_string(), "extension host stopped");
        assert!(jobs.children.lock().expect("jobs").is_empty());
    }

    #[test]
    fn shell_run_output_cancels_in_flight_commands_on_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let gate = dir.path().join("gate");
        let done = dir.path().join("done");
        let command = blocking_file_command(&started, &gate, &done);
        let run_jobs = Arc::new(PlatformRunJobs::default());
        let shutdown = Arc::new(AtomicBool::new(false));

        let worker = std::thread::spawn({
            let run_jobs = Arc::clone(&run_jobs);
            let shutdown = Arc::clone(&shutdown);
            move || shell_run_output(&command, &run_jobs, &shutdown).is_err()
        });
        assert!(
            wait_for_path(&started, Duration::from_secs(5)),
            "the command should start before cleanup cancels it"
        );

        run_jobs.cleanup();

        assert!(
            worker.join().expect("worker"),
            "a cancelled bootty.run must report failure, not a truncated result"
        );
        assert!(
            !wait_for_path(&done, Duration::from_millis(200)),
            "cleanup should stop the command before it reaches its gate"
        );
    }
    #[cfg(unix)]
    #[test]
    fn shell_run_output_terminates_background_pipe_holders() {
        let run_jobs = PlatformRunJobs::default();
        let shutdown = AtomicBool::new(false);
        let started = Instant::now();
        let result = shell_run_output("printf ready; sleep 30 &", &run_jobs, &shutdown);
        assert!(
            result.is_err(),
            "a background pipe holder must hit the bounded deadline"
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(run_jobs.children.lock().is_ok_and(|jobs| jobs.is_empty()));
        let stress_started = Instant::now();
        assert!(shell_run_output("yes", &run_jobs, &shutdown).is_err());
        assert!(stress_started.elapsed() < Duration::from_secs(1));
        assert!(run_jobs.children.lock().is_ok_and(|jobs| jobs.is_empty()));
        let closed_started = Instant::now();
        let error = shell_run_output("exec 1>&- 2>&-; sleep 30", &run_jobs, &shutdown).unwrap_err();
        assert_eq!(
            error.to_string(),
            "process_still_running_after_output_closed"
        );
        assert!(closed_started.elapsed() < Duration::from_secs(1));
        assert!(run_jobs.children.lock().is_ok_and(|jobs| jobs.is_empty()));
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
        assert!(
            !wait_for_path(&done, Duration::from_millis(200)),
            "dropping the host should cancel its in-flight shell commands"
        );
    }
    #[test]
    fn window_action_deadline_keeps_worker_usable() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("window_loop.luau"),
            r#"
                local opened = false
                local renders = 0
                return {
                    interval = 60,
                    render = function()
                        renders = renders + 1
                        if not opened then
                            opened = true
                            bootty.window.open({
                                title = "loop",
                                rows = {{ key = "go", text = "go" }},
                                on_action = function()
                                    while true do end
                                end
                            })
                        end
                        return tostring(renders)
                    end
                }
            "#,
        )
        .expect("write window module");
        let host = ExtensionHost::spawn_status(
            dir.path().to_path_buf(),
            egui::Context::default(),
            Vec::new(),
        );
        host.set_active(["window_loop".to_owned()]);
        let initial_deadline = Instant::now() + Duration::from_secs(2);
        let initial = loop {
            if let Some(item) = host.items("window_loop").into_iter().next() {
                break item.text;
            }
            assert!(
                Instant::now() < initial_deadline,
                "window module did not render"
            );
            std::thread::sleep(Duration::from_millis(8));
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let window = loop {
            if let Some(spec) =
                host.take_window_requests()
                    .into_iter()
                    .find_map(|request| match request {
                        WindowRequest::Open(spec) => Some(spec),
                        WindowRequest::Close => None,
                    })
            {
                break spec;
            }
            assert!(Instant::now() < deadline, "window request was not produced");
            std::thread::sleep(Duration::from_millis(8));
        };
        host.push_window_action(window.id, "go".to_owned(), None);
        std::thread::sleep(Duration::from_millis(150));
        host.update_mux(MuxView {
            keep_awake: true,
            ..MuxView::default()
        });
        let rerender_deadline = Instant::now() + Duration::from_millis(500);
        let rerendered = loop {
            if host
                .items("window_loop")
                .iter()
                .any(|item| item.text != initial)
            {
                break true;
            }
            if Instant::now() >= rerender_deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(8));
        };
        assert!(
            rerendered,
            "timed-out window callback must not strand the worker"
        );
        drop(host);
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
                    ..CodexBarEntry::default()
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
                    ..CodexBarEntry::default()
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
    fn codexbar_cache_bounds_unique_providers_with_lru_eviction() {
        let client = CodexBarClient::default();
        let mut providers = Vec::with_capacity(RUN_CACHE_ENTRY_LIMIT + 1);
        for index in 0..=RUN_CACHE_ENTRY_LIMIT {
            let provider = format!("provider-{index}");
            assert!(client.mark_refreshing(&provider, Duration::ZERO));
            assert!(client.finish_refresh(&provider, Some(format!("usage-{index}"))));
            providers.push(provider);
        }
        let entries = client.entries.lock().expect("codexbar entries");
        assert_eq!(entries.len(), RUN_CACHE_ENTRY_LIMIT);
        assert_eq!(
            entries
                .get(providers.last().expect("last provider"))
                .map(|entry| entry.output.as_str()),
            Some("usage-256")
        );
    }

    #[test]
    fn codexbar_refresh_rejects_new_provider_when_active_quota_is_full() {
        let run_cache = Arc::new(RunCache::default());
        for index in 0..RUN_CACHE_REFRESH_LIMIT {
            let provider = format!("active-{index}");
            assert!(
                run_cache
                    .codexbar
                    .mark_refreshing(&provider, Duration::ZERO)
            );
            run_cache
                .refresh_jobs
                .lock()
                .expect("refresh jobs")
                .insert(index as u64, RefreshJob { handle: None });
        }

        run_cache.refresh_codexbar_usage("overflow".to_owned());

        assert!(
            !run_cache
                .codexbar
                .entries
                .lock()
                .expect("codexbar entries")
                .contains_key("overflow")
        );
        assert_eq!(
            run_cache.refresh_jobs.lock().expect("refresh jobs").len(),
            RUN_CACHE_REFRESH_LIMIT
        );
    }

    #[test]
    fn dropping_codexbar_refresh_cancels_and_joins_injected_fetch() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let started = Arc::new(AtomicBool::new(false));
        let mut cache = RunCache::default();
        cache.shutdown = Arc::clone(&shutdown);
        let run_cache = Arc::new(cache);
        let jobs = Arc::clone(&run_cache.refresh_jobs);
        let fetch_shutdown = Arc::clone(&shutdown);
        let fetch_started = Arc::clone(&started);
        run_cache.codexbar.set_fetch_override(Arc::new(move |_| {
            fetch_started.store(true, Ordering::Release);
            while !fetch_shutdown.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(std::io::Error::other("cancelled"))
        }));
        run_cache.refresh_codexbar_usage("codex".to_owned());
        let deadline = Instant::now() + Duration::from_secs(2);
        while !started.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(started.load(Ordering::Acquire));

        drop(run_cache);

        assert!(
            jobs.lock().expect("refresh jobs").is_empty(),
            "drop must remove every tracked CodexBar refresh job"
        );
        assert!(shutdown.load(Ordering::Acquire));
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

    #[test]
    fn shell_run_output_captures_command_stderr() {
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

    #[cfg(unix)]
    #[test]
    fn exec_run_output_hands_arguments_to_the_program_unchanged() {
        // The shell these call sites used to go through would split the space, strip the quotes and
        // expand `$HOME`; every one of those is a wrong argument reaching git or tmux.
        let argv = ["/bin/echo", "a b", "it's", "$HOME"]
            .map(str::to_owned)
            .to_vec();

        assert_eq!(
            exec_run_output(&argv, &PlatformRunJobs::default(), &AtomicBool::new(false)).unwrap(),
            "a b it's $HOME\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_run_output_preserves_path_lookup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let program = dir.path().join("bootty-path-probe");
        std::fs::write(&program, "#!/bin/sh\nprintf path-ok").expect("write path probe");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("make path probe executable");

        let mut command = shell_command("bootty-path-probe");
        command.env(
            "PATH",
            std::env::join_paths([dir.path(), Path::new("/usr/bin"), Path::new("/bin")])
                .expect("test PATH"),
        );

        assert_eq!(
            shell_command_output(
                command,
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

    /// Sidebar labels and grouping read the name bootty shows, so a suffix the backend name needed to
    /// stay unique never reaches the sidebar — while anchors keep targeting the backend name.
    #[test]
    fn ui_session_items_label_and_group_by_display_name() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![
                SessionView {
                    id: "$1".to_owned(),
                    name: "work-2/api".to_owned(),
                    display_name: "work/api".to_owned(),
                    ..SessionView::default()
                },
                SessionView {
                    id: "$2".to_owned(),
                    name: "work/ui".to_owned(),
                    display_name: "work/ui".to_owned(),
                    ..SessionView::default()
                },
            ],
            scope_key: "preview:binding".to_owned(),
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                r#"return function()
                    return bootty.ui.session_items({ sessions = bootty.sessions() })
                end"#,
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);
        let sessions = items
            .iter()
            .filter(|item| item.kind.as_deref() == Some("session"))
            .collect::<Vec<_>>();

        assert_eq!(
            items
                .iter()
                .find(|item| item.kind.as_deref() == Some("group"))
                .map(|item| item.text.as_str()),
            Some("work"),
            "both sessions belong to the same project once the suffix is out of the way"
        );
        assert_eq!(
            sessions
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            ["api", "ui"],
        );
        assert_eq!(
            sessions
                .iter()
                .map(|item| item.reorder_anchor.as_deref())
                .collect::<Vec<_>>(),
            [Some("work-2/api"), Some("work/ui")],
            "anchors are identities and stay on the backend's names"
        );
    }

    #[test]
    fn ui_session_items_namespaces_child_row_keys() {
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            sessions: vec![
                SessionView {
                    id: "$1".to_owned(),
                    name: "work/api".to_owned(),
                    display_name: String::new(),
                    color: Some("#89b4fa".to_owned()),
                    dim_color: Some("#455a7d".to_owned()),
                    ..SessionView::default()
                },
                SessionView {
                    id: "$2".to_owned(),
                    name: "work/ui".to_owned(),
                    display_name: String::new(),
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
    fn builtin_session_modules_keep_rows_stable_without_progress() {
        let cwd = std::env::current_dir()
            .expect("current dir")
            .to_string_lossy()
            .into_owned();
        let display_cwd = crate::strings::display_path(&cwd);
        let mux: Arc<RwLock<MuxView>> = Arc::new(RwLock::new(MuxView {
            scope_key: "7:11".to_owned(),
            sessions: vec![SessionView {
                id: "plain".to_owned(),
                name: "bootty".to_owned(),
                display_name: String::new(),
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
        let modules = load_modules(&lua, dir.path(), BUILTIN_SESSION_EXTENSIONS);
        let render = || {
            modules
                .iter()
                .filter(|module| matches!(module.name.as_str(), "directory" | "branch" | "ports"))
                .flat_map(|module| run_module(&module.body))
                .collect::<Vec<_>>()
        };

        let first = render();
        let rerendered = render();
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

    // Drives the real `branch` module through the Refresh-mode cache path (what the app uses),
    // re-rendering until the branch row settles. Returns the branch row's text, or None if the
    // row is absent. Synchronizes on the rendered value rather than sleeping a fixed duration.
    fn settle_branch_row(
        branch: &LoadedModule,
        run_cache: &RunCache,
        want: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut last = None;
        while Instant::now() < deadline {
            run_cache.set_mode(RunMode::Refresh);
            let items = run_module(&branch.body);
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
    fn builtin_branch_module_tracks_live_head_through_changes() {
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
                display_name: String::new(),
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
        let modules = load_modules(&lua, dir.path(), BUILTIN_SESSION_EXTENSIONS);
        let branch = modules
            .iter()
            .find(|module| module.name == "branch")
            .expect("branch builtin loaded");

        let timeout = Duration::from_secs(5);
        assert_eq!(
            settle_branch_row(branch, &run_cache, |b| b == "alpha", timeout).as_deref(),
            Some("alpha"),
            "branch row should report the starting branch",
        );

        // Switch to another branch: the row must follow, not freeze on the first value.
        git(path, &["checkout", "-q", "-b", "beta"]);
        assert_eq!(
            settle_branch_row(branch, &run_cache, |b| b == "beta", timeout).as_deref(),
            Some("beta"),
            "branch row must update when the live branch changes",
        );

        // Detach HEAD: the row must report detached, not the stale branch name.
        git(path, &["checkout", "-q", "HEAD~1"]);
        let detached =
            settle_branch_row(branch, &run_cache, |b| b.starts_with("detached"), timeout);
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
            scope_key: "7:11".to_owned(),
            sessions: vec![SessionView {
                id: "$1".to_owned(),
                name: "work/api".to_owned(),
                display_name: String::new(),
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
                pane_id: Some("%7".to_owned()),
                pane_pid: Some(4242),
                process: Some("codex".to_owned()),
            }],
            ..MuxView::default()
        }));
        let lua = setup_lua(&[], mux, Arc::default(), Arc::default(), Arc::default()).unwrap();
        let value = lua
            .load(
                r#"return function() local s = bootty.sessions()[1]
                   local p = s.progresses
                   return { kind = 'session', text = s.cache_key .. ':' .. s.name .. ':' .. s.cwd .. ':' .. p[1].process .. ':' .. p[2].value .. ':' .. s.ports[1] .. ':' .. s.pane_id .. ':' .. s.pane_pid .. ':' .. s.process,
                   session_id = s.id, fg = s.color } end"#,
            )
            .eval::<Value>()
            .unwrap();
        let module = loaded_module_from_value("test".to_owned(), value).unwrap();
        let items = run_module(&module.body);

        assert_eq!(items[0].kind.as_deref(), Some("session"));
        assert_eq!(
            items[0].text,
            "7:11:$1:work/api:/tmp/work/api:pi:42:8040:%7:4242:codex"
        );
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

    #[cfg(unix)]
    #[test]
    fn run_cache_bounds_unique_keys_and_evicts_least_recent_completed_entry() {
        let run_cache = Arc::new(RunCache::default());
        let mut commands = Vec::with_capacity(RUN_CACHE_ENTRY_LIMIT + 1);
        for index in 0..=RUN_CACHE_ENTRY_LIMIT {
            let command = format!("printf key{index}");
            run_cache
                .refresh(RunCommand::Shell(command.clone()))
                .expect("refresh should reserve a bounded slot");
            assert!(wait_for_cached_output(
                &run_cache,
                &command,
                &format!("key{index}"),
                Duration::from_secs(2)
            ));
            let deadline = Instant::now() + Duration::from_secs(2);
            while run_cache.cache_state().1 != 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(2));
            }
            commands.push(command);
        }

        let (entries, active) = run_cache.cache_state();
        assert_eq!(entries, RUN_CACHE_ENTRY_LIMIT);
        assert_eq!(active, 0);
        assert!(
            run_cache.cached(&commands[0]).is_none(),
            "the least-recent completed entry should be evicted"
        );
        assert_eq!(
            run_cache.cached(commands.last().expect("last command")),
            Some(format!("key{RUN_CACHE_ENTRY_LIMIT}"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_cache_rejects_new_refresh_when_all_workers_are_active() {
        let run_cache = Arc::new(RunCache::default());
        for index in 0..RUN_CACHE_REFRESH_LIMIT {
            run_cache
                .refresh(RunCommand::Shell(format!(
                    "while true; do sleep 1; done # {index}"
                )))
                .expect("each active refresh should consume one quota slot");
        }
        assert_eq!(run_cache.cache_state().1, RUN_CACHE_REFRESH_LIMIT);

        let error = run_cache
            .refresh(RunCommand::Shell("printf extra".to_owned()))
            .expect_err("an extra active refresh must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), RUN_CACHE_QUOTA_ERROR);
        assert_eq!(run_cache.cache_state().1, RUN_CACHE_REFRESH_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_run_cache_cancels_and_joins_refresh_workers() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = dir.path().join("started");
        let gate = dir.path().join("gate");
        let done = dir.path().join("done");
        let command = blocking_file_command(&started, &gate, &done);
        let run_jobs = Arc::new(PlatformRunJobs::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut cache = RunCache::default();
        cache.run_jobs = Arc::clone(&run_jobs);
        cache.shutdown = Arc::clone(&shutdown);
        let run_cache = Arc::new(cache);
        let weak_cache = Arc::downgrade(&run_cache);
        run_cache
            .refresh(RunCommand::Shell(command))
            .expect("blocking refresh should start");
        assert!(wait_for_path(&started, Duration::from_secs(2)));

        drop(run_cache);

        assert!(
            weak_cache.upgrade().is_none(),
            "drop must release the cache"
        );
        assert!(
            run_jobs.children.lock().is_ok_and(|jobs| jobs.is_empty()),
            "drop must cancel every process group"
        );
        assert!(!wait_for_path(&done, Duration::from_millis(200)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn run_cache_refresh_keeps_shell_out_errors_visible() {
        let cmd = "printf ignored".to_owned();
        let mut cache = RunCache::default();
        cache.shutdown = Arc::new(AtomicBool::new(true));
        let run_cache = Arc::new(cache);

        run_cache.set_mode(RunMode::Refresh);
        // Empty and not an answer yet: a module told only "empty" cannot tell this from a command
        // that printed nothing, and would show a blank row until its next turn came round.
        assert_eq!(run_cache.run(&cmd).unwrap(), (String::new(), false));

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
        let session_names = module_names(dir.path(), ModuleKind::Session);
        assert!(session_names.contains(&"directory".to_owned()));
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

    #[test]
    fn effective_module_paths_prefer_luau_over_lua_siblings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let lua = directory.path().join("priority.lua");
        let luau = directory.path().join("priority.luau");
        std::fs::write(&lua, "return function() return 'lua' end").expect("write lua");
        std::fs::write(&luau, "return function() return 'luau' end").expect("write luau");

        assert_eq!(extension_module_paths(directory.path()), vec![luau]);
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
    fn reload_events_are_path_stable_and_invalidate_failed_modules() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("example.luau");
        let catalogs = [ModuleCatalog {
            dir: directory.path().to_owned(),
            builtins: &[],
            prefix: "",
        }];
        let initial = vec![(
            path.clone(),
            ExtensionFileSignature {
                modified: Some(SystemTime::UNIX_EPOCH),
                readable: true,
            },
        )];
        let changed = vec![(
            path.clone(),
            ExtensionFileSignature {
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
                readable: true,
            },
        )];
        let failed = vec![(
            path.clone(),
            ExtensionFileSignature {
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
                readable: false,
            },
        )];
        let restored_signature = vec![(
            path,
            ExtensionFileSignature {
                modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(3)),
                readable: true,
            },
        )];
        let successful_modules = BTreeSet::from(["example".to_owned()]);
        let reload_events = RwLock::new(ReloadEventQueue::default());
        let mut generations = BTreeMap::new();
        let mut active = BTreeSet::new();
        let take_reload_events = || {
            reload_events
                .write()
                .map(|mut queue| queue.drain())
                .unwrap_or_default()
        };

        reconcile_extension_reloads(
            &catalogs,
            &[],
            &initial,
            &successful_modules,
            &mut generations,
            &mut active,
            &reload_events,
        );
        let loaded = take_reload_events();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].operation, ExtensionReloadOperation::Loaded);
        assert_eq!(loaded.events[0].generation, 1);
        assert_eq!(loaded.modules.len(), 1);
        assert_eq!(loaded.modules[0].generation, 1);
        let extension_id = loaded.events[0].extension_id.clone();

        reconcile_extension_reloads(
            &catalogs,
            &initial,
            &changed,
            &successful_modules,
            &mut generations,
            &mut active,
            &reload_events,
        );
        let reloaded = take_reload_events();
        assert_eq!(reloaded.events.len(), 1);
        assert_eq!(reloaded.events[0].extension_id, extension_id);
        assert_eq!(
            reloaded.events[0].operation,
            ExtensionReloadOperation::Reloaded
        );
        assert_eq!(reloaded.events[0].generation, 2);
        assert_eq!(reloaded.modules[0].generation, 2);

        reconcile_extension_reloads(
            &catalogs,
            &changed,
            &failed,
            &BTreeSet::new(),
            &mut generations,
            &mut active,
            &reload_events,
        );
        let invalidated = take_reload_events();
        assert_eq!(invalidated.events.len(), 1);
        assert_eq!(invalidated.events[0].extension_id, extension_id);
        assert_eq!(
            invalidated.events[0].operation,
            ExtensionReloadOperation::Removed
        );
        assert_eq!(invalidated.events[0].generation, 3);
        assert!(invalidated.modules.is_empty());

        reconcile_extension_reloads(
            &catalogs,
            &failed,
            &restored_signature,
            &successful_modules,
            &mut generations,
            &mut active,
            &reload_events,
        );
        let restored = take_reload_events();
        assert_eq!(restored.events.len(), 1);
        assert_eq!(restored.events[0].extension_id, extension_id);
        assert_eq!(
            restored.events[0].operation,
            ExtensionReloadOperation::Loaded
        );
        assert_eq!(restored.events[0].generation, 4);
        assert_eq!(restored.modules.len(), 1);
        assert_eq!(restored.modules[0].generation, 4);

        reconcile_extension_reloads(
            &catalogs,
            &restored_signature,
            &[],
            &BTreeSet::new(),
            &mut generations,
            &mut active,
            &reload_events,
        );
        let removed = take_reload_events();
        assert_eq!(removed.events.len(), 1);
        assert_eq!(removed.events[0].extension_id, extension_id);
        assert_eq!(
            removed.events[0].operation,
            ExtensionReloadOperation::Removed
        );
        assert_eq!(removed.events[0].generation, 5);
        assert!(removed.modules.is_empty());
    }

    #[test]
    fn reload_event_queue_preserves_normal_delta_order() {
        let mut queue = ReloadEventQueue::default();
        for generation in [1, 2, 3] {
            queue.publish(ExtensionReloadEvent {
                extension_id: "path:/extensions/example.luau".to_owned(),
                generation,
                operation: ExtensionReloadOperation::Reloaded,
            });
        }

        let drain = queue.drain();

        assert!(!drain.requires_rebase);
        assert_eq!(
            drain
                .events
                .into_iter()
                .map(|event| event.generation)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn reload_event_queue_overflow_drops_retained_deltas_for_rebase() {
        let extension_id = "path:/extensions/example.luau".to_owned();
        let final_generation = RELOAD_EVENT_QUEUE_LIMIT as u64 + 1;
        let mut queue = ReloadEventQueue::default();
        for generation in 1..final_generation {
            queue.publish(ExtensionReloadEvent {
                extension_id: extension_id.clone(),
                generation,
                operation: ExtensionReloadOperation::Reloaded,
            });
        }
        queue.modules.insert(extension_id.clone(), final_generation);
        queue.publish(ExtensionReloadEvent {
            extension_id: extension_id.clone(),
            generation: final_generation,
            operation: ExtensionReloadOperation::Reloaded,
        });

        let drain = queue.drain();

        assert!(drain.requires_rebase);
        assert!(drain.events.is_empty());
        assert_eq!(
            drain.modules,
            vec![ExtensionModuleGeneration {
                extension_id,
                generation: final_generation,
            }]
        );
    }
    #[test]
    fn requeue_keeps_newer_generation_after_failed_older_publication() {
        let mut queue = ReloadEventQueue::default();
        let extension_id = "path:/extensions/interleaved.luau".to_owned();
        assert!(queue.set_modules([(extension_id.clone(), 1)]));
        queue.publish(ExtensionReloadEvent {
            extension_id: extension_id.clone(),
            generation: 1,
            operation: ExtensionReloadOperation::Loaded,
        });
        let failed = queue.drain();
        assert_eq!(failed.inventory_revision, 1);
        assert!(queue.set_modules([(extension_id.clone(), 2)]));
        queue.publish(ExtensionReloadEvent {
            extension_id: extension_id.clone(),
            generation: 2,
            operation: ExtensionReloadOperation::Reloaded,
        });
        queue.requeue(failed);
        let merged = queue.drain();
        assert_eq!(merged.inventory_revision, 2);
        assert_eq!(merged.modules[0].generation, 2);
        assert_eq!(
            merged
                .events
                .iter()
                .map(|event| event.generation)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn requeue_does_not_resurrect_authoritative_empty_inventory() {
        let mut queue = ReloadEventQueue::default();
        let extension_id = "path:/extensions/removed.luau".to_owned();
        assert!(queue.set_modules([(extension_id.clone(), 1)]));
        let failed = queue.drain();
        assert_eq!(failed.inventory_revision, 1);
        assert!(queue.set_modules(std::iter::empty::<(String, u64)>()));
        queue.publish(ExtensionReloadEvent {
            extension_id,
            generation: 2,
            operation: ExtensionReloadOperation::Removed,
        });
        queue.requeue(failed);
        let drained = queue.drain();
        assert!(drained.modules.is_empty());
        assert_eq!(drained.inventory_revision, 2);
        assert_eq!(drained.events.len(), 1);
        assert_eq!(
            drained.events[0].operation,
            ExtensionReloadOperation::Removed
        );
    }
    #[test]
    fn reload_module_inventory_rejects_overflow_and_publishes_complete_admitted_set() {
        let mut queue = ReloadEventQueue::default();
        assert!(
            !queue.set_modules(
                (0..(RELOAD_MODULE_LIMIT + 1))
                    .map(|index| { (format!("path:/extensions/{index:04}.luau"), index as u64) })
            )
        );
        assert!(queue.drain().modules.is_empty());
        assert!(
            queue.set_modules(
                (0..RELOAD_MODULE_LIMIT)
                    .map(|index| { (format!("path:/extensions/{index:04}.luau"), index as u64) })
            )
        );
        let drain = queue.drain();
        assert!(!drain.requires_rebase);
        assert_eq!(drain.modules.len(), RELOAD_MODULE_LIMIT);
        assert_eq!(drain.modules[0].generation, 0);
        assert_eq!(
            drain.modules.last().map(|module| module.generation),
            Some((RELOAD_MODULE_LIMIT - 1) as u64)
        );
    }

    #[test]
    fn reload_reconciliation_replaces_removed_module_at_inventory_limit() {
        let directory = tempfile::tempdir().expect("tempdir");
        let catalogs = [ModuleCatalog {
            dir: directory.path().to_owned(),
            builtins: &[],
            prefix: "",
        }];
        let paths = (0..=RELOAD_MODULE_LIMIT)
            .map(|index| directory.path().join(format!("module-{index:03}.luau")))
            .collect::<Vec<_>>();
        let signature = paths
            .iter()
            .map(|path| {
                (
                    path.clone(),
                    ExtensionFileSignature {
                        modified: Some(SystemTime::UNIX_EPOCH),
                        readable: true,
                    },
                )
            })
            .collect::<Vec<_>>();
        let successful = paths
            .iter()
            .map(|path| catalog_module_name(&catalogs, path).expect("module name"))
            .collect::<BTreeSet<_>>();
        let reload_events = RwLock::new(ReloadEventQueue::default());
        let mut generations = BTreeMap::new();
        let mut active = BTreeSet::new();

        let rejected = reconcile_extension_reloads(
            &catalogs,
            &[],
            &signature,
            &successful,
            &mut generations,
            &mut active,
            &reload_events,
        );
        assert_eq!(active.len(), RELOAD_MODULE_LIMIT);
        assert_eq!(rejected.len(), 1);
        let rejected_id = rejected.iter().next().expect("overflow module").clone();
        assert!(!active.contains(&rejected_id));
        let _ = reload_events.write().map(|mut queue| queue.drain());

        let accepted_signature = signature
            .iter()
            .filter(|(path, _)| extension_id(path) != rejected_id)
            .cloned()
            .collect::<Vec<_>>();
        let replacement_signature = signature.iter().skip(1).cloned().collect::<Vec<_>>();
        let replacement_successful = paths
            .iter()
            .skip(1)
            .map(|path| catalog_module_name(&catalogs, path).expect("module name"))
            .collect::<BTreeSet<_>>();
        let rejected = reconcile_extension_reloads(
            &catalogs,
            &accepted_signature,
            &replacement_signature,
            &replacement_successful,
            &mut generations,
            &mut active,
            &reload_events,
        );
        assert!(rejected.is_empty());
        assert_eq!(active.len(), RELOAD_MODULE_LIMIT);
        assert!(active.contains(&rejected_id));
        assert!(!active.contains(&extension_id(&paths[0])));
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
