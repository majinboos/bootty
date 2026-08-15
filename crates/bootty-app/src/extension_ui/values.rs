use eframe::egui::{self, Color32};

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
    /// Whether the window has keyboard focus. Hosts run modules at `UNFOCUSED_INTERVAL_FLOOR`
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

/// A session-order change a module requested via `bootty.reorder_session(source, before)`.
/// The app drains these each frame and applies them to the native session-order store.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionReorder {
    pub source: String,
    pub before: Option<String>,
}
