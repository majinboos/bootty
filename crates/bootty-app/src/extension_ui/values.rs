use std::sync::OnceLock;
use std::time::Instant;

use eframe::egui::{self, Color32};

use super::items::parse_hex_color;

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

/// A session-order change a module requested via `bootty.reorder_session(source, before)`.
/// The app drains these each frame and applies them to the native session-order store.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionReorder {
    pub source: String,
    pub before: Option<String>,
}
