use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::fact_values::{Metrics, MuxView, SessionReorder};
use crate::facts::ExtensionFacts;
use crate::module_runtime::{ActiveModule, prepare_module};
use crate::module_sources::{discover_modules, legacy_extension_modules};
use crate::storage::ExtensionStorage;
use crate::{
    ExtensionCatalog, ExtensionEventSender, ExtensionGenerationCandidate, ExtensionUiAction,
    ModuleIdentity, PublishedSurfaceSnapshot, SurfacePlacement, facts,
};
use bootty_command::BoundAppCommandSender;

const RELOAD_SCAN_INTERVAL: Duration = Duration::from_millis(500);

pub struct ExtensionHost {
    root: PathBuf,
    catalog: Arc<ExtensionCatalog>,
    commands: BoundAppCommandSender,
    events: ExtensionEventSender,
    active: BTreeMap<ModuleIdentity, ActiveModule>,
    retired: Vec<thread::JoinHandle<()>>,
    next_check: Instant,
    next_generation: u64,
    facts: ExtensionFacts,
    metrics_system: sysinfo::System,
    battery: Option<starship_battery::Manager>,
    next_metrics: Instant,
}

impl ExtensionHost {
    pub fn load(
        root: &Path,
        catalog: Arc<ExtensionCatalog>,
        commands: BoundAppCommandSender,
        events: ExtensionEventSender,
    ) -> Self {
        Self::load_with_ui(
            root,
            catalog,
            commands,
            events,
            Vec::new(),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    }

    pub fn load_with_ui(
        root: &Path,
        catalog: Arc<ExtensionCatalog>,
        commands: BoundAppCommandSender,
        events: ExtensionEventSender,
        theme: Vec<(String, String)>,
        home: Option<PathBuf>,
    ) -> Self {
        let mut host = Self {
            root: root.to_owned(),
            catalog,
            commands,
            events,
            active: BTreeMap::new(),
            retired: Vec::new(),
            next_check: Instant::now(),
            next_generation: 1,
            facts: ExtensionFacts::new(theme, home),
            metrics_system: sysinfo::System::new(),
            battery: starship_battery::Manager::new().ok(),
            next_metrics: Instant::now(),
        };
        if let Some(config_dir) = root.parent()
            && let Ok(legacy) = legacy_extension_modules(config_dir)
            && !legacy.is_empty()
        {
            eprintln!(
                "legacy extension modules are inactive; import them from Settings > Extensions"
            );
        }
        host.reconcile(false);
        host
    }

    pub fn refresh(&mut self, now: Instant) {
        if now >= self.next_metrics {
            self.next_metrics = now + Duration::from_secs(2);
            let metrics = facts::sample_metrics(&mut self.metrics_system, self.battery.as_ref());
            if self.facts.update_metrics(metrics) {
                for active in self.active.values() {
                    active.worker.sender.try_render();
                }
            }
        }
        if now < self.next_check {
            return;
        }
        self.next_check = now + RELOAD_SCAN_INTERVAL;
        self.reconcile(false);
    }

    pub fn update_mux(&self, view: MuxView) {
        if !self.facts.update_mux(view) {
            return;
        }
        for active in self.active.values() {
            active.worker.sender.try_render();
        }
    }

    pub fn update_theme(&mut self, theme: Vec<(String, String)>) {
        if self.facts.set_theme(theme) {
            self.reconcile(true);
        }
    }

    #[must_use]
    pub fn surfaces(&self, placement: SurfacePlacement) -> Vec<PublishedSurfaceSnapshot> {
        let mut surfaces = self
            .catalog
            .surfaces()
            .into_iter()
            .filter(|surface| surface.snapshot.declaration.placement == placement)
            .collect::<Vec<_>>();
        surfaces.sort_by(|left, right| {
            left.snapshot
                .declaration
                .order
                .cmp(&right.snapshot.declaration.order)
                .then_with(|| left.module.cmp(&right.module))
                .then_with(|| {
                    left.snapshot
                        .declaration
                        .id
                        .cmp(&right.snapshot.declaration.id)
                })
        });
        surfaces
    }

    #[must_use]
    pub fn surface(
        &self,
        placement: SurfacePlacement,
        name: &str,
    ) -> Option<PublishedSurfaceSnapshot> {
        self.surfaces(placement).into_iter().find(|surface| {
            surface.snapshot.declaration.id == name
                || Path::new(&surface.module)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    == Some(name)
        })
    }

    pub fn metrics(&self) -> Metrics {
        self.facts.metrics()
    }

    pub fn take_session_reorders(&self) -> Vec<SessionReorder> {
        self.facts.take_session_reorders()
    }

    pub fn submit_ui_action(&self, action: ExtensionUiAction) -> Result<(), String> {
        let active = self
            .active
            .get(action.module.as_str())
            .filter(|active| {
                active.generation == action.generation
                    && active.worker.control.generation.is_active()
            })
            .ok_or_else(|| "extension generation is no longer active".to_owned())?;
        active
            .worker
            .sender
            .try_action(action)
            .map_err(str::to_owned)
    }

    fn reconcile(&mut self, force: bool) {
        self.reap_retired();
        let discovered = match discover_modules(&self.root) {
            Ok(discovered) => discovered,
            Err(error) => {
                eprintln!("failed to scan extensions {}: {error}", self.root.display());
                return;
            }
        };
        let present = discovered.keys().cloned().collect::<BTreeSet<_>>();
        for (identity, source) in discovered {
            let source = match source {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("failed to load extension {identity}: {error}");
                    continue;
                }
            };
            if !force
                && self
                    .active
                    .get(&identity)
                    .is_some_and(|active| active.fingerprint == source.fingerprint)
            {
                continue;
            }
            let generation = self.next_generation;
            let Some(next_generation) = self.next_generation.checked_add(1) else {
                eprintln!("failed to load extension {identity}: extension generation exhausted");
                continue;
            };
            self.next_generation = next_generation;
            let storage = match self.active.get(&identity) {
                Some(active) => active.storage.clone(),
                None => match ExtensionStorage::open(&self.root, identity.as_str()) {
                    Ok(storage) => storage,
                    Err(error) => {
                        eprintln!("failed to load extension {identity}: {error}");
                        continue;
                    }
                },
            };
            let prepared = match prepare_module(
                &source,
                storage.clone(),
                generation,
                Arc::clone(&self.catalog),
                self.commands.clone(),
                self.events.clone(),
                self.facts.for_generation(),
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    eprintln!("failed to load extension {identity}: {error}");
                    continue;
                }
            };
            if let Err(error) = self
                .catalog
                .publish_generation(ExtensionGenerationCandidate {
                    identity: source.identity.clone(),
                    generation,
                    token: prepared.worker.control.generation.clone(),
                    commands: prepared.commands,
                    topics: prepared.topics,
                    surfaces: prepared.surfaces,
                })
            {
                if let Some(thread) = prepared.worker.retire() {
                    self.retired.push(thread);
                }
                eprintln!("failed to publish extension {identity}: {error}");
                continue;
            }
            let next = ActiveModule {
                generation,
                fingerprint: source.fingerprint,
                worker: prepared.worker,
                storage,
            };
            if let Some(previous) = self.active.insert(identity, next)
                && let Some(thread) = previous.worker.retire()
            {
                self.retired.push(thread);
            }
        }
        let removed = self
            .active
            .keys()
            .filter(|identity| !present.contains(*identity))
            .cloned()
            .collect::<Vec<_>>();
        for identity in removed {
            let Some(previous) = self.active.remove(&identity) else {
                continue;
            };
            self.catalog
                .remove_generation(identity.as_str(), previous.generation);
            if let Some(thread) = previous.worker.retire() {
                self.retired.push(thread);
            }
        }
        self.reap_retired();
    }

    fn reap_retired(&mut self) {
        let mut pending = Vec::new();
        for thread in self.retired.drain(..) {
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                pending.push(thread);
            }
        }
        self.retired = pending;
    }
}

impl Drop for ExtensionHost {
    fn drop(&mut self) {
        for (identity, active) in std::mem::take(&mut self.active) {
            self.catalog
                .remove_generation(identity.as_str(), active.generation);
            let _ = active.worker.retire();
        }
    }
}
