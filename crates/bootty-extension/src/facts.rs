//! Read-only host facts and bounded platform jobs for one extension generation.

use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

use starship_battery::{Manager as BatteryManager, State as BatteryState, units::time::second};
use sysinfo::{MemoryRefreshKind, System};

use crate::fact_values::{
    Metrics, MuxView, SessionProgressView, SessionReorder, SessionView, WindowView,
};
use crate::module_runtime::WorkerControl;
use crate::{
    ExtensionCatalog, ExtensionGenerationToken, ModuleIdentity, processes::ManagedProcesses,
};

mod bindings;
mod run;
use run::{RunCache, preview_run_cache};

#[derive(Clone)]
pub(super) struct QueuedSessionReorder {
    pub(super) reorder: SessionReorder,
    pub(super) generation: Option<(ModuleIdentity, u64, ExtensionGenerationToken)>,
}

/// How long a macOS memory-pressure sample serves every host before another subprocess runs.
#[cfg(target_os = "macos")]
const MEMORY_PRESSURE_TTL: Duration = Duration::from_secs(5);

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
                process: Some("cargo test".to_owned()),
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

pub(crate) fn sample_metrics(system: &mut System, battery: Option<&BatteryManager>) -> Metrics {
    system.refresh_cpu_usage();
    system.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let load = System::load_average();
    let (battery_percent, on_ac, battery_time_to_empty_secs, battery_time_to_full_secs) =
        battery_status(battery);
    Metrics {
        cpu: system.global_cpu_usage(),
        load1: load.one,
        mem_used_pct: memory_used_percent(system),
        mem_total_bytes: system.total_memory(),
        battery_percent,
        on_ac,
        battery_time_to_empty_secs,
        battery_time_to_full_secs,
    }
}

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

#[derive(Clone)]
pub(crate) struct ExtensionFacts {
    home: Option<PathBuf>,
    theme: Arc<RwLock<Vec<(String, String)>>>,
    mux: Arc<RwLock<MuxView>>,
    metrics: Arc<RwLock<Metrics>>,
    session_reorders: Arc<RwLock<Vec<QueuedSessionReorder>>>,
    run_cache: Arc<RunCache>,
    processes: ManagedProcesses,
}

#[derive(Clone)]
pub(crate) struct ExtensionFactGeneration {
    pub(super) catalog: Arc<ExtensionCatalog>,
    pub(super) identity: ModuleIdentity,
    pub(super) generation: u64,
    pub(super) control: Arc<WorkerControl>,
}

impl ExtensionFacts {
    pub(crate) fn new(theme: Vec<(String, String)>, home: Option<PathBuf>) -> Self {
        Self {
            home,
            theme: Arc::new(RwLock::new(theme)),
            mux: Arc::default(),
            metrics: Arc::default(),
            session_reorders: Arc::default(),
            run_cache: Arc::default(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn preview(theme: Vec<(String, String)>) -> Self {
        Self {
            home: None,
            theme: Arc::new(RwLock::new(theme)),
            mux: Arc::new(RwLock::new(preview_mux_view())),
            metrics: Arc::new(RwLock::new(Metrics {
                cpu: 42.0,
                load1: 1.25,
                mem_used_pct: 68.0,
                mem_total_bytes: 16 * 1_073_741_824,
                battery_percent: Some(73.0),
                on_ac: false,
                battery_time_to_empty_secs: Some(9_000.0),
                battery_time_to_full_secs: None,
            })),
            session_reorders: Arc::default(),
            run_cache: preview_run_cache(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn for_generation(&self) -> Self {
        Self {
            home: self.home.clone(),
            theme: Arc::clone(&self.theme),
            mux: Arc::clone(&self.mux),
            metrics: Arc::clone(&self.metrics),
            session_reorders: Arc::clone(&self.session_reorders),
            run_cache: Arc::default(),
            processes: ManagedProcesses::default(),
        }
    }

    pub(crate) fn retire(&self) {
        self.run_cache.retire();
        self.processes.retire();
    }

    pub(crate) fn update_mux(&self, view: MuxView) -> bool {
        let Ok(mut current) = self.mux.write() else {
            return false;
        };
        if *current == view {
            return false;
        }
        *current = view;
        true
    }

    pub(crate) fn theme(&self) -> Vec<(String, String)> {
        self.theme
            .read()
            .map(|theme| theme.clone())
            .unwrap_or_default()
    }

    pub(crate) fn set_theme(&self, theme: Vec<(String, String)>) -> bool {
        let Ok(mut current) = self.theme.write() else {
            return false;
        };
        if *current == theme {
            return false;
        }
        *current = theme;
        true
    }

    pub(crate) fn update_metrics(&self, metrics: Metrics) -> bool {
        let Ok(mut current) = self.metrics.write() else {
            return false;
        };
        if *current == metrics {
            return false;
        }
        *current = metrics;
        true
    }

    pub(crate) fn take_session_reorders(&self) -> Vec<SessionReorder> {
        self.session_reorders
            .write()
            .map(|mut queue| {
                std::mem::take(&mut *queue)
                    .into_iter()
                    .filter(|queued| {
                        queued
                            .generation
                            .as_ref()
                            .is_none_or(|(_, _, token)| token.is_active())
                    })
                    .map(|queued| queued.reorder)
                    .collect()
            })
            .unwrap_or_default()
    }
}
