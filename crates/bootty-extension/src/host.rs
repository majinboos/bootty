use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::fact_values::{MuxView, SessionReorder};
use crate::facts::ExtensionFacts;
use crate::module_runtime::{
    ActiveModule, ExtensionSettingDeclaration, ModuleWorker, prepare_module,
};
use crate::module_sources::{
    LegacyExtensionModule, ModuleSourceOutcome, ModuleSourceRequest, ModuleSources,
    create_module_source, discover_modules, editable_module_source, import_legacy_extension_module,
    legacy_extension_modules, module_template, reset_module_source, save_module_source,
};
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
    /// Every module the last scan discovered, including ones that failed to load: the editor
    /// must be able to open a broken module to fix it.
    discovered: Vec<ModuleIdentity>,
    /// Inactive pre-module extension files awaiting import, refreshed by each scan.
    legacy: Vec<LegacyExtensionModule>,
    /// Settings the loaded modules declared for themselves, in module then declaration order.
    settings: Vec<ExtensionSettingDeclaration>,
    /// Bumped whenever `settings` changes, so the app rebuilds its schema only then.
    settings_revision: u64,
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
            discovered: Vec::new(),
            legacy: Vec::new(),
            settings: Vec::new(),
            settings_revision: 0,
            retired: Vec::new(),
            next_check: Instant::now(),
            next_generation: 1,
            facts: ExtensionFacts::new(theme, home),
            metrics_system: sysinfo::System::new(),
            battery: starship_battery::Manager::new().ok(),
            next_metrics: Instant::now(),
        };
        host.reconcile(false);
        if !host.legacy.is_empty() {
            eprintln!(
                "legacy extension modules are inactive; import them from Settings > Extensions"
            );
        }
        host
    }

    pub fn refresh(&mut self, now: Instant) {
        if now >= self.next_metrics {
            self.next_metrics = now + Duration::from_secs(2);
            let metrics = facts::sample_metrics(&mut self.metrics_system, self.battery.as_ref());
            if self.facts.update_metrics(metrics) {
                self.render_all();
            }
        }
        if now < self.next_check {
            return;
        }
        self.next_check = now + RELOAD_SCAN_INTERVAL;
        self.reconcile(false);
    }

    pub fn update_mux(&self, view: MuxView) {
        if self.facts.update_mux(view) {
            self.render_all();
        }
    }

    fn render_all(&self) {
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
        let mut surfaces = self.catalog.surfaces_for(placement);
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
        self.surfaces(placement)
            .into_iter()
            .find(|surface| surface.matches_name(name))
    }

    /// Whether `placement` has any published surface, without cloning one.
    #[must_use]
    pub fn has_surfaces(&self, placement: SurfacePlacement) -> bool {
        self.catalog.has_surfaces(placement)
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

    /// Settings the loaded extensions declared for themselves, and a revision that changes only
    /// when the set does.
    #[must_use]
    pub fn setting_declarations(&self) -> (&[ExtensionSettingDeclaration], u64) {
        (&self.settings, self.settings_revision)
    }

    /// Publish the accepted extension settings so a module reads the user's value. A module can
    /// only ever see its own table.
    pub fn update_settings(
        &self,
        settings: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, bootty_config::config::ExtensionSettingValue>,
        >,
    ) {
        if self.facts.set_extension_settings(settings) {
            self.render_all();
        }
    }

    /// Modules the settings editor may open, and the legacy files it may import. Both come
    /// from the periodic scan, so painting never walks the extension directory.
    #[must_use]
    pub fn module_sources(&self) -> ModuleSources<'_> {
        ModuleSources {
            identities: &self.discovered,
            legacy: &self.legacy,
        }
    }

    /// Run one editor request against the extension root. The periodic scan publishes the
    /// resulting source, so a save stays debounced instead of reloading per keystroke.
    pub fn apply_module_source_request(
        &mut self,
        request: ModuleSourceRequest,
    ) -> ModuleSourceOutcome {
        match request {
            ModuleSourceRequest::Load(identity) => {
                match editable_module_source(&self.root, &identity) {
                    Some(source) => ModuleSourceOutcome::Loaded {
                        source,
                        exists: true,
                    },
                    None => ModuleSourceOutcome::Loaded {
                        source: crate::module_sources::EditableModuleSource {
                            source: module_template(&identity),
                            path: self.root.join(identity.as_ref()),
                            identity,
                            customized: false,
                            has_builtin: false,
                        },
                        exists: false,
                    },
                }
            }
            ModuleSourceRequest::Create(value) => {
                let created = create_module_source(&self.root, &value);
                if created.is_ok() {
                    self.reconcile(false);
                }
                ModuleSourceOutcome::Created(created)
            }
            ModuleSourceRequest::Save { identity, source } => ModuleSourceOutcome::Saved(
                save_module_source(&self.root, &identity, &source)
                    .map_err(|error| error.to_string()),
            ),
            ModuleSourceRequest::Reset(identity) => {
                let reset = reset_module_source(&self.root, &identity)
                    .map(|()| identity)
                    .map_err(|error| error.to_string());
                if reset.is_ok() {
                    self.reconcile(false);
                }
                ModuleSourceOutcome::Reset(reset)
            }
            ModuleSourceRequest::ImportLegacy(legacy) => {
                let Some(config_dir) = self.root.parent() else {
                    return ModuleSourceOutcome::Imported(Err(
                        "extension root has no config directory".to_owned(),
                    ));
                };
                let imported =
                    import_legacy_extension_module(config_dir, &legacy, self.facts.theme());
                if imported.is_ok() {
                    self.reconcile(false);
                }
                ModuleSourceOutcome::Imported(imported)
            }
        }
    }

    fn reconcile(&mut self, force: bool) {
        self.reap_retired();
        self.legacy = self
            .root
            .parent()
            .and_then(|config_dir| legacy_extension_modules(config_dir).ok())
            .unwrap_or_default();
        let discovered = match discover_modules(&self.root) {
            Ok(discovered) => discovered,
            Err(error) => {
                eprintln!("failed to scan extensions {}: {error}", self.root.display());
                return;
            }
        };
        let present = discovered.keys().cloned().collect::<BTreeSet<_>>();
        self.discovered = present.iter().cloned().collect();
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
            let Some(next_generation) = self.next_generation.checked_add(1) else {
                eprintln!("failed to load extension {identity}: extension generation exhausted");
                continue;
            };
            let generation = std::mem::replace(&mut self.next_generation, next_generation);
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
                self.retire_worker(prepared.worker);
                eprintln!("failed to publish extension {identity}: {error}");
                continue;
            }
            let next = ActiveModule {
                generation,
                fingerprint: source.fingerprint,
                worker: prepared.worker,
                storage,
                settings: prepared.settings,
            };
            if let Some(previous) = self.active.insert(identity, next) {
                self.retire_worker(previous.worker);
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
            self.retire_worker(previous.worker);
        }
        self.refresh_setting_declarations();
        self.reap_retired();
    }

    /// Recompute the declaration list from the active modules, bumping the revision only when it
    /// actually changed.
    fn refresh_setting_declarations(&mut self) {
        let settings: Vec<ExtensionSettingDeclaration> = self
            .active
            .values()
            .flat_map(|active| active.settings.iter().cloned())
            .collect();
        if settings != self.settings {
            self.settings = settings;
            self.settings_revision = self.settings_revision.wrapping_add(1);
        }
    }

    fn retire_worker(&mut self, worker: ModuleWorker) {
        if let Some(thread) = worker.retire() {
            self.retired.push(thread);
        }
    }

    fn reap_retired(&mut self) {
        for thread in self.retired.extract_if(.., |thread| thread.is_finished()) {
            let _ = thread.join();
        }
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
