use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    commands::{
        AppCommandSendError, ArgumentSchema, BoundAppCommandSender, Caller, CommandCancellation,
        CommandCatalog, CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema,
        ExtensionGenerationCandidate, ExtensionGenerationToken, MutationClass, ResourceKind,
        ValueType,
    },
    control::ControlPlane,
    extension_ui::items_from_value,
};
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Table, Value as LuaValue, VmState};
use serde_json::{Map, Value, json};

mod builtins;
mod facts;
mod identity;
mod processes;
mod storage;
mod surfaces;

pub use crate::extension_ui::{
    Metrics, ModuleCoord, ModuleItem, ModulePrimitive, MuxView, SessionProgressView,
    SessionReorder, SessionView, WindowView,
};
use facts::{ExtensionFactGeneration, ExtensionFacts};
pub use identity::ModuleIdentity;
use storage::ExtensionStorage;
pub use surfaces::{
    ExtensionUiAction, PublishedSurfaceItem, PublishedSurfaceSnapshot, SurfaceDeclaration,
    SurfacePlacement, SurfaceSnapshot,
};

const RELOAD_SCAN_INTERVAL: Duration = Duration::from_millis(500);
const SETUP_EXECUTION_LIMIT: Duration = Duration::from_millis(100);
const SETUP_RESPONSE_LIMIT: Duration = Duration::from_millis(250);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const INVOCATION_QUEUE_LIMIT: usize = 64;
const MODULE_LIMIT: usize = 32;
const MODULE_SOURCE_LIMIT: u64 = 1024 * 1024;
const MODULE_COMMAND_LIMIT: usize = 64;
const MODULE_TOPIC_LIMIT: usize = 64;
const MODULE_SURFACE_LIMIT: usize = 64;

struct Invocation {
    command: String,
    invocation: CommandInvocation,
    deadline: Instant,
    cancellation: CommandCancellation,
    response: mpsc::Sender<CommandOutcome>,
}

enum WorkerMessage {
    Invoke(Invocation),
    Render,
    Action(ExtensionUiAction),
}

#[derive(Clone)]
struct ActiveInvocation {
    deadline: Instant,
    cancellation: CommandCancellation,
}

struct WorkerControl {
    generation: ExtensionGenerationToken,
    setup_complete: AtomicBool,
    setup_deadline: Instant,
    active: Mutex<Option<ActiveInvocation>>,
}

struct ModuleWorker {
    control: Arc<WorkerControl>,
    sender: mpsc::SyncSender<WorkerMessage>,
    facts: ExtensionFacts,
    thread: Option<thread::JoinHandle<()>>,
}

impl ModuleWorker {
    fn retire(mut self) -> Option<thread::JoinHandle<()>> {
        self.control.generation.retire();
        self.facts.retire();
        self.thread.take()
    }
}

struct ActiveModule {
    generation: u64,
    fingerprint: u64,
    worker: ModuleWorker,
    storage: ExtensionStorage,
}

struct ModuleSource {
    identity: ModuleIdentity,
    namespace: String,
    source: String,
    fingerprint: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditableModuleSource {
    pub identity: ModuleIdentity,
    pub source: String,
    pub path: PathBuf,
    pub customized: bool,
    pub has_builtin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyExtensionModule {
    pub source_path: PathBuf,
    pub target_identity: ModuleIdentity,
    pub placement: SurfacePlacement,
    pub surface_id: String,
}

struct ModuleDeclarations {
    commands: Vec<CommandDescriptor>,
    topics: Vec<String>,
    surfaces: Vec<SurfaceSnapshot>,
}

struct PreparedModule {
    commands: Vec<(CommandDescriptor, crate::commands::ExtensionCommandHandler)>,
    topics: Vec<String>,
    surfaces: Vec<SurfaceSnapshot>,
    worker: ModuleWorker,
}

struct SurfaceHandler {
    render: RegistryKey,
    action: Option<RegistryKey>,
}

#[derive(Clone)]
struct ModuleHost {
    identity: ModuleIdentity,
    namespace: String,
    generation: u64,
    commands: BoundAppCommandSender,
    plane: ControlPlane,
    catalog: Arc<CommandCatalog>,
    control: Arc<WorkerControl>,
    storage: ExtensionStorage,
    facts: ExtensionFacts,
}

pub struct ExtensionHost {
    root: PathBuf,
    catalog: Arc<CommandCatalog>,
    commands: BoundAppCommandSender,
    plane: ControlPlane,
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
        catalog: Arc<CommandCatalog>,
        commands: BoundAppCommandSender,
        plane: ControlPlane,
    ) -> Self {
        Self::load_with_ui(root, catalog, commands, plane, Vec::new())
    }

    pub fn load_with_ui(
        root: &Path,
        catalog: Arc<CommandCatalog>,
        commands: BoundAppCommandSender,
        plane: ControlPlane,
        theme: Vec<(String, String)>,
    ) -> Self {
        let mut host = Self {
            root: root.to_owned(),
            catalog,
            commands,
            plane,
            active: BTreeMap::new(),
            retired: Vec::new(),
            next_check: Instant::now(),
            next_generation: 1,
            facts: ExtensionFacts::new(theme),
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
                    let _ = active.worker.sender.try_send(WorkerMessage::Render);
                }
            }
        }
        if now < self.next_check {
            return;
        }
        self.next_check = now + RELOAD_SCAN_INTERVAL;
        self.reconcile(false);
    }

    pub fn update_mux(&self, view: crate::extension_ui::MuxView) {
        if !self.facts.update_mux(view) {
            return;
        }
        for active in self.active.values() {
            let _ = active.worker.sender.try_send(WorkerMessage::Render);
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
            .extension_surfaces()
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
    pub fn surface_items(
        &self,
        placement: SurfacePlacement,
        name: &str,
    ) -> Vec<crate::extension_ui::ModuleItem> {
        self.surface(placement, name)
            .map(|surface| surface.snapshot.items)
            .unwrap_or_default()
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

    pub fn metrics(&self) -> crate::extension_ui::Metrics {
        self.facts.metrics()
    }

    pub fn take_session_reorders(&self) -> Vec<crate::extension_ui::SessionReorder> {
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
            .try_send(WorkerMessage::Action(action))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "extension command queue is full".to_owned(),
                mpsc::TrySendError::Disconnected(_) => {
                    "extension generation is no longer active".to_owned()
                }
            })
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
                self.plane.clone(),
                self.facts.for_generation(),
            ) {
                Ok(prepared) => prepared,
                Err(error) => {
                    eprintln!("failed to load extension {identity}: {error}");
                    continue;
                }
            };
            if let Err(error) =
                self.catalog
                    .publish_extension_generation(ExtensionGenerationCandidate {
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
                .remove_extension_generation(identity.as_str(), previous.generation);
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

pub fn module_identities(root: &Path) -> Result<Vec<ModuleIdentity>, String> {
    discover_modules(root).map(|modules| modules.into_keys().collect())
}

pub fn legacy_extension_modules(config_dir: &Path) -> Result<Vec<LegacyExtensionModule>, String> {
    let mut discovered = BTreeMap::<(SurfacePlacement, String), LegacyExtensionModule>::new();
    for placement in [
        SurfacePlacement::Status,
        SurfacePlacement::Sidebar,
        SurfacePlacement::Session,
    ] {
        let directory = config_dir.join(placement.as_str());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        let canonical_directory =
            fs::canonicalize(&directory).map_err(|error| error.to_string())?;
        let mut paths = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort_by_key(|path| path.extension().and_then(|value| value.to_str()) == Some("luau"));
        for path in paths {
            if !path.is_file() || !is_module_path(&path) {
                continue;
            }
            let canonical_path = fs::canonicalize(&path).map_err(|error| error.to_string())?;
            if !canonical_path.starts_with(&canonical_directory) {
                return Err("legacy extension module path escapes its source directory".to_owned());
            }
            let Some(surface_id) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let target_identity = legacy_target_identity(placement, &surface_id, &path)?;
            discovered.insert(
                (placement, surface_id.clone()),
                LegacyExtensionModule {
                    source_path: path,
                    target_identity,
                    placement,
                    surface_id,
                },
            );
        }
    }
    Ok(discovered.into_values().collect())
}

pub fn import_legacy_extension_module(
    config_dir: &Path,
    legacy: &LegacyExtensionModule,
    theme: Vec<(String, String)>,
) -> Result<ModuleIdentity, String> {
    let current = legacy_extension_modules(config_dir)?;
    let legacy = current
        .iter()
        .find(|candidate| candidate == &legacy)
        .ok_or_else(|| "legacy extension module is no longer available".to_owned())?;
    let source = fs::read_to_string(&legacy.source_path).map_err(|error| error.to_string())?;
    let source = wrap_legacy_surface_source(&legacy.surface_id, legacy.placement, source.as_str());
    let surfaces = preview_module_surfaces(&legacy.target_identity, &source, theme)?;
    if surfaces.len() != 1
        || surfaces[0].declaration.id != legacy.surface_id
        || surfaces[0].declaration.placement != legacy.placement
    {
        return Err("legacy extension import did not produce its declared surface".to_owned());
    }
    save_module_source(
        &config_dir.join("extensions"),
        &legacy.target_identity,
        &source,
    )
    .map_err(|error| error.to_string())?;
    Ok(legacy.target_identity.clone())
}

fn legacy_target_identity(
    placement: SurfacePlacement,
    surface_id: &str,
    source_path: &Path,
) -> Result<ModuleIdentity, String> {
    let builtin = builtins::modules()
        .into_iter()
        .any(|builtin| builtin.identity == surface_id && builtin.placement == placement.as_str());
    let extension = source_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("luau");
    if builtin {
        ModuleIdentity::parse(format!("{surface_id}.luau"))
    } else {
        ModuleIdentity::parse(format!(
            "legacy/{}/{surface_id}.{extension}",
            placement.as_str()
        ))
    }
}

fn wrap_legacy_surface_source(
    surface_id: &str,
    placement: SurfacePlacement,
    source: &str,
) -> String {
    format!(
        r#"-- Imported from a legacy Bootty extension directory.
local candidate = (function()
{source}
end)()
local render = candidate
local interval = nil
if type(candidate) == "table" then
    render = candidate.render
    interval = candidate.interval
end
bootty.ui.register({{ id = "{surface_id}", placement = "{}", interval = interval }}, render)
"#,
        placement.as_str()
    )
}

pub fn editable_module_source(
    root: &Path,
    identity: &ModuleIdentity,
) -> Option<EditableModuleSource> {
    let path = root.join(identity.as_ref());
    let builtin = builtin_source(identity);
    match fs::read_to_string(&path) {
        Ok(source) => Some(EditableModuleSource {
            identity: identity.clone(),
            source,
            path,
            customized: true,
            has_builtin: builtin.is_some(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            builtin.map(|source| EditableModuleSource {
                identity: identity.clone(),
                source,
                path,
                customized: false,
                has_builtin: true,
            })
        }
        Err(_) => None,
    }
}

pub fn save_module_source(
    root: &Path,
    identity: &ModuleIdentity,
    source: &str,
) -> std::io::Result<PathBuf> {
    let path = root.join(identity.as_ref());
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension target has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    crate::extension_source_writer::save_within(root, &path, source)?;
    Ok(path)
}

pub fn reset_module_source(root: &Path, identity: &ModuleIdentity) -> std::io::Result<()> {
    let path = root.join(identity.as_ref());
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn preview_module_surfaces(
    identity: &ModuleIdentity,
    source: &str,
    theme: Vec<(String, String)>,
) -> Result<Vec<SurfaceSnapshot>, String> {
    let lua = Lua::new();
    let bootty = lua.create_table().map_err(|error| error.to_string())?;
    let facts = ExtensionFacts::preview(theme);
    facts
        .install(&lua, &bootty, None)
        .map_err(|error| error.to_string())?;
    let handlers = Arc::new(Mutex::new(BTreeMap::new()));
    let declarations = Arc::new(Mutex::new(Vec::new()));
    install_surface_interface(
        &lua,
        &bootty,
        Arc::clone(&handlers),
        Arc::clone(&declarations),
        None,
    )
    .map_err(|error| error.to_string())?;
    install_preview_noop_tables(&lua, &bootty).map_err(|error| error.to_string())?;
    bootty.set_readonly(true);
    lua.globals()
        .set("bootty", bootty)
        .map_err(|error| error.to_string())?;
    lua.sandbox(true).map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_millis(50);
    lua.set_interrupt(move |_| {
        if Instant::now() >= deadline {
            Err(mlua::Error::runtime("extension preview exceeded 50 ms"))
        } else {
            Ok(VmState::Continue)
        }
    });
    lua.load(source)
        .set_name(identity.as_str())
        .exec()
        .map_err(|error| error.to_string())?;
    initial_surface_snapshots(&lua, &handlers, &declarations)
}

fn install_preview_noop_tables(lua: &Lua, bootty: &Table) -> mlua::Result<()> {
    let commands = lua.create_table()?;
    commands.set(
        "register",
        lua.create_function(|_, (_spec, _handler): (Table, Function)| Ok(()))?,
    )?;
    commands.set_readonly(true);
    bootty.set("commands", commands)?;

    let events = lua.create_table()?;
    events.set("register", lua.create_function(|_, _: String| Ok(()))?)?;
    events.set_readonly(true);
    bootty.set("events", events)?;

    let storage = lua.create_table()?;
    storage.set(
        "get",
        lua.create_function(|_, _: String| Ok(LuaValue::Nil))?,
    )?;
    storage.set_readonly(true);
    bootty.set("storage", storage)
}

fn builtin_source(identity: &ModuleIdentity) -> Option<String> {
    if let Some(builtin) =
        builtins::agent_modules().find(|builtin| identity.as_str() == builtin.identity)
    {
        return Some(builtin.source.to_owned());
    }
    builtins::modules()
        .into_iter()
        .find(|builtin| identity.as_str() == format!("{}.luau", builtin.identity))
        .map(|builtin| builtin_module_source(builtin.identity, builtin.placement, builtin.source))
}

impl Drop for ExtensionHost {
    fn drop(&mut self) {
        for (identity, active) in std::mem::take(&mut self.active) {
            self.catalog
                .remove_extension_generation(identity.as_str(), active.generation);
            let _ = active.worker.retire();
        }
    }
}

fn discover_modules(
    root: &Path,
) -> Result<BTreeMap<ModuleIdentity, Result<ModuleSource, String>>, String> {
    let mut modules = builtins::modules()
        .into_iter()
        .map(|builtin| {
            let identity = ModuleIdentity::parse(format!("{}.luau", builtin.identity))
                .expect("built-in extension identity");
            let source = builtin_module_source(builtin.identity, builtin.placement, builtin.source);
            let loaded = module_source_from_text(identity.clone(), source);
            (identity, loaded)
        })
        .collect::<BTreeMap<_, _>>();
    modules.extend(builtins::agent_modules().map(|builtin| {
        let identity = ModuleIdentity::parse(builtin.identity).expect("built-in agent identity");
        let loaded = module_source_from_text(identity.clone(), builtin.source.to_owned());
        (identity, loaded)
    }));
    if !root.exists() {
        return Ok(modules);
    }
    let mut paths = Vec::new();
    collect_module_paths(root, &mut paths)?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut canonical_paths = BTreeSet::new();
    for path in paths {
        let canonical_path = path.canonicalize().map_err(|error| error.to_string())?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err("extension module must stay inside the extension root".to_owned());
        }
        canonical_paths.insert(canonical_path);
    }
    let paths = canonical_paths.into_iter().collect::<Vec<_>>();
    if paths.len() + modules.len() > MODULE_LIMIT {
        return Err(format!(
            "extension module count exceeds the limit of {MODULE_LIMIT}"
        ));
    }
    for path in paths {
        let identity = module_identity(root, &path)?;
        let loaded = load_module_source(identity.clone(), &path);
        modules.insert(identity, loaded);
    }
    Ok(modules)
}

fn builtin_module_source(identity: &str, placement: &str, source: &str) -> String {
    format!(
        r#"
local candidate = (function()
{source}
end)()
local render = candidate
local interval = nil
if type(candidate) == "table" then
    render = candidate.render
    interval = candidate.interval
end
bootty.ui.register({{ id = "{identity}", placement = "{placement}", interval = interval }}, render)
"#
    )
}

fn collect_module_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            collect_module_paths(&path, paths)?;
        } else if (file_type.is_file() || path.is_file()) && is_module_path(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_module_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "lua" | "luau"))
}

fn module_identity(root: &Path, path: &Path) -> Result<ModuleIdentity, String> {
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_path = path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("extension module must stay inside the extension root".to_owned());
    }
    let relative = canonical_path
        .strip_prefix(&canonical_root)
        .map_err(|_| "extension module must stay inside the extension root".to_owned())?;
    let parts = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "extension module path must be valid UTF-8".to_owned()),
            _ => Err("extension module path is invalid".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    ModuleIdentity::parse(parts.join("/"))
}

fn load_module_source(identity: ModuleIdentity, path: &Path) -> Result<ModuleSource, String> {
    let size = fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > MODULE_SOURCE_LIMIT {
        return Err(format!(
            "extension source exceeds the limit of {MODULE_SOURCE_LIMIT} bytes"
        ));
    }
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    module_source_from_text(identity, source)
}

fn module_source_from_text(
    identity: ModuleIdentity,
    source: String,
) -> Result<ModuleSource, String> {
    let namespace = identity.namespace();
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    source.hash(&mut hasher);
    Ok(ModuleSource {
        identity,
        namespace,
        source,
        fingerprint: hasher.finish(),
    })
}

fn prepare_module(
    module: &ModuleSource,
    storage: ExtensionStorage,
    generation: u64,
    catalog: Arc<CommandCatalog>,
    commands: BoundAppCommandSender,
    plane: ControlPlane,
    facts: ExtensionFacts,
) -> Result<PreparedModule, String> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (tx, rx) = mpsc::sync_channel(INVOCATION_QUEUE_LIMIT);
    let control = Arc::new(WorkerControl {
        generation: ExtensionGenerationToken::new(),
        setup_complete: AtomicBool::new(false),
        setup_deadline: Instant::now() + SETUP_EXECUTION_LIMIT,
        active: Mutex::new(None),
    });
    let host = ModuleHost {
        identity: module.identity.clone(),
        namespace: module.namespace.clone(),
        generation,
        commands,
        plane,
        catalog,
        control: Arc::clone(&control),
        storage,
        facts: facts.clone(),
    };
    let source = module.source.clone();
    let thread_name = format!("bootty-extension-{}", host.namespace);
    let thread = thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_module_worker(host, source, rx, ready_tx))
        .map_err(|error| error.to_string())?;
    let worker = ModuleWorker {
        control: Arc::clone(&control),
        sender: tx.clone(),
        facts,
        thread: Some(thread),
    };
    let declarations = match ready_rx.recv_timeout(SETUP_RESPONSE_LIMIT) {
        Ok(Ok(declarations)) => declarations,
        Ok(Err(error)) => {
            let _ = worker.retire();
            return Err(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = worker.retire();
            return Err("extension setup exceeded 250 ms".to_owned());
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = worker.retire();
            return Err("extension worker stopped during load".to_owned());
        }
    };
    if declarations.commands.len() > MODULE_COMMAND_LIMIT {
        let _ = worker.retire();
        return Err(format!(
            "extension command count exceeds the limit of {MODULE_COMMAND_LIMIT}"
        ));
    }
    if declarations.topics.len() > MODULE_TOPIC_LIMIT {
        let _ = worker.retire();
        return Err(format!(
            "extension event topic count exceeds the limit of {MODULE_TOPIC_LIMIT}"
        ));
    }
    if declarations.surfaces.len() > MODULE_SURFACE_LIMIT {
        let _ = worker.retire();
        return Err(format!(
            "extension surface count exceeds the limit of {MODULE_SURFACE_LIMIT}"
        ));
    }
    let registrations = declarations
        .commands
        .into_iter()
        .map(|descriptor| {
            let command = descriptor.id.clone();
            let sender = tx.clone();
            let control = Arc::clone(&control);
            let handler = Arc::new(move |invocation, deadline, cancellation| {
                let (response, receiver) = mpsc::channel();
                if !control.generation.is_active() {
                    let _ = response.send(CommandOutcome::Failed {
                        code: "stale_extension_generation".to_owned(),
                        message: "extension generation is no longer active".to_owned(),
                    });
                    return receiver;
                }
                let work = Invocation {
                    command: command.clone(),
                    invocation,
                    deadline,
                    cancellation,
                    response,
                };
                match sender.try_send(WorkerMessage::Invoke(work)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full(WorkerMessage::Invoke(work))) => {
                        let _ = work.response.send(CommandOutcome::Failed {
                            code: "extension_busy".to_owned(),
                            message: "extension command queue is full".to_owned(),
                        });
                    }
                    Err(mpsc::TrySendError::Disconnected(WorkerMessage::Invoke(work))) => {
                        let _ = work.response.send(CommandOutcome::Failed {
                            code: "stale_extension_generation".to_owned(),
                            message: "extension generation is no longer active".to_owned(),
                        });
                    }
                    Err(
                        mpsc::TrySendError::Full(WorkerMessage::Render)
                        | mpsc::TrySendError::Disconnected(WorkerMessage::Render)
                        | mpsc::TrySendError::Full(WorkerMessage::Action(_))
                        | mpsc::TrySendError::Disconnected(WorkerMessage::Action(_)),
                    ) => unreachable!(),
                }
                receiver
            }) as crate::commands::ExtensionCommandHandler;
            (descriptor, handler)
        })
        .collect();
    Ok(PreparedModule {
        commands: registrations,
        topics: declarations.topics,
        surfaces: declarations.surfaces,
        worker,
    })
}

fn run_module_worker(
    host: ModuleHost,
    source: String,
    rx: mpsc::Receiver<WorkerMessage>,
    ready: mpsc::SyncSender<Result<ModuleDeclarations, String>>,
) {
    let lua = Lua::new();
    let handlers = Arc::new(std::sync::Mutex::new(BTreeMap::<String, RegistryKey>::new()));
    let descriptors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let topics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let surface_handlers = Arc::new(std::sync::Mutex::new(
        BTreeMap::<String, SurfaceHandler>::new(),
    ));
    let surface_declarations = Arc::new(std::sync::Mutex::new(Vec::new()));
    let interrupt_control = Arc::clone(&host.control);
    lua.set_interrupt(move |_| worker_interrupt(&interrupt_control));
    let setup = install_host_interface(
        &lua,
        &host,
        Arc::clone(&handlers),
        Arc::clone(&descriptors),
        Arc::clone(&topics),
        Arc::clone(&surface_handlers),
        Arc::clone(&surface_declarations),
    )
    .and_then(|()| lua.sandbox(true))
    .and_then(|()| lua.load(&source).set_name(host.identity.as_str()).exec());
    if let Err(error) = setup {
        let _ = ready.send(Err(error.to_string()));
        return;
    }
    let commands = descriptors
        .lock()
        .map(|mut descriptors| std::mem::take(&mut *descriptors))
        .map_err(|_| "extension descriptor lock poisoned".to_owned());
    let topics = topics
        .lock()
        .map(|mut topics| std::mem::take(&mut *topics))
        .map_err(|_| "extension topic lock poisoned".to_owned());
    let surfaces = initial_surface_snapshots(&lua, &surface_handlers, &surface_declarations);
    let registered = commands.and_then(|commands| {
        topics.and_then(|topics| {
            surfaces.map(|surfaces| ModuleDeclarations {
                commands,
                topics,
                surfaces,
            })
        })
    });
    host.control.setup_complete.store(true, Ordering::Release);
    if ready.send(registered).is_err() {
        return;
    }
    let render_interval = surface_declarations
        .lock()
        .ok()
        .and_then(|declarations| declarations.iter().map(|surface| surface.interval).min());
    let mut next_render = render_interval.map(|interval| Instant::now() + interval);
    while host.control.generation.is_active() {
        match rx.recv_timeout(WORKER_POLL_INTERVAL) {
            Ok(WorkerMessage::Invoke(work)) => {
                let response = work.response.clone();
                let _ = response.send(invoke_handler(&lua, &handlers, &host.control, work));
            }
            Ok(WorkerMessage::Render) => {
                render_and_publish_surfaces(&lua, &host, &surface_handlers, &surface_declarations);
                next_render = render_interval.map(|interval| Instant::now() + interval);
            }
            Ok(WorkerMessage::Action(action)) => {
                run_surface_action(&lua, &host, &surface_handlers, action);
                render_and_publish_surfaces(&lua, &host, &surface_handlers, &surface_declarations);
                next_render = render_interval.map(|interval| Instant::now() + interval);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if next_render.is_some_and(|deadline| Instant::now() >= deadline) {
                    render_and_publish_surfaces(
                        &lua,
                        &host,
                        &surface_handlers,
                        &surface_declarations,
                    );
                    next_render = render_interval.map(|interval| Instant::now() + interval);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn worker_interrupt(control: &WorkerControl) -> mlua::Result<VmState> {
    if !control.generation.is_active() {
        return Err(mlua::Error::runtime("extension generation retired"));
    }
    let active = control
        .active
        .lock()
        .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
        .clone();
    if let Some(active) = active {
        if active.cancellation.is_cancelled() {
            return Err(mlua::Error::runtime("extension command was cancelled"));
        }
        if Instant::now() >= active.deadline {
            return Err(mlua::Error::runtime("extension command deadline expired"));
        }
    } else if !control.setup_complete.load(Ordering::Acquire)
        && Instant::now() >= control.setup_deadline
    {
        return Err(mlua::Error::runtime("extension setup exceeded 100 ms"));
    }
    Ok(VmState::Continue)
}

fn install_host_interface(
    lua: &Lua,
    host: &ModuleHost,
    handlers: Arc<std::sync::Mutex<BTreeMap<String, RegistryKey>>>,
    descriptors: Arc<std::sync::Mutex<Vec<CommandDescriptor>>>,
    topics: Arc<std::sync::Mutex<Vec<String>>>,
    surface_handlers: Arc<std::sync::Mutex<BTreeMap<String, SurfaceHandler>>>,
    surface_declarations: Arc<std::sync::Mutex<Vec<SurfaceDeclaration>>>,
) -> mlua::Result<()> {
    let bootty = lua.create_table()?;
    host.facts.install(
        lua,
        &bootty,
        Some(ExtensionFactGeneration {
            catalog: Arc::clone(&host.catalog),
            identity: host.identity.clone(),
            generation: host.generation,
            control: Arc::clone(&host.control),
        }),
    )?;
    let commands = lua.create_table()?;
    let command_namespace = host.namespace.clone();
    let command_setup = Arc::clone(&host.control);
    commands.set(
        "register",
        lua.create_function(move |lua, (spec, handler): (Table, Function)| {
            require_setup_phase(&command_setup)?;
            let descriptor = descriptor_from_table(&command_namespace, &spec)?;
            let key = lua.create_registry_value(handler)?;
            handlers
                .lock()
                .map_err(|_| mlua::Error::runtime("extension handler lock poisoned"))?
                .insert(descriptor.id.clone(), key);
            descriptors
                .lock()
                .map_err(|_| mlua::Error::runtime("extension descriptor lock poisoned"))?
                .push(descriptor);
            Ok(())
        })?,
    )?;
    let active = Arc::clone(&host.control);
    let app_commands = host.commands.clone();
    commands.set(
        "invoke",
        lua.create_function(move |lua, spec: Table| {
            let active = active
                .active
                .lock()
                .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
                .clone()
                .ok_or_else(|| {
                    mlua::Error::runtime(
                        "bootty.commands.invoke is available only inside a command handler",
                    )
                })?;
            let mut value = lua_value(LuaValue::Table(spec), 0)?;
            let object = value.as_object_mut().ok_or_else(|| {
                mlua::Error::runtime("bootty.commands.invoke needs a command table")
            })?;
            object.insert("caller".to_owned(), json!(Caller::Luau));
            let invocation = serde_json::from_value(value)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            let outcome = submit_app_command(&app_commands, invocation, active);
            lua.to_value(&outcome)
        })?,
    )?;
    bootty.set("commands", commands)?;

    let events = lua.create_table()?;
    let event_namespace = host.namespace.clone();
    let event_setup = Arc::clone(&host.control);
    events.set(
        "register",
        lua.create_function(move |_, topic: String| {
            require_setup_phase(&event_setup)?;
            if !crate::commands::is_namespaced(&topic, &event_namespace) {
                return Err(mlua::Error::runtime(
                    "extension event topic must be namespaced by its module",
                ));
            }
            topics
                .lock()
                .map_err(|_| mlua::Error::runtime("extension topic lock poisoned"))?
                .push(topic);
            Ok(())
        })?,
    )?;
    let publish_identity = host.identity.clone();
    let publish_generation = host.generation;
    let publish_control = Arc::clone(&host.control);
    let plane = host.plane.clone();
    let catalog = Arc::clone(&host.catalog);
    events.set(
        "publish",
        lua.create_function(move |_, (topic, payload): (String, LuaValue)| {
            if publish_control
                .active
                .lock()
                .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
                .is_none()
            {
                return Err(mlua::Error::runtime(
                    "bootty.events.publish is available only inside a command handler",
                ));
            }
            let payload = lua_value(payload, 0)?;
            plane
                .publish_extension_event(
                    &catalog,
                    publish_identity.as_str(),
                    publish_generation,
                    &topic,
                    payload,
                )
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    bootty.set("events", events)?;

    install_surface_interface(
        lua,
        &bootty,
        surface_handlers,
        surface_declarations,
        Some(Arc::clone(&host.control)),
    )?;

    let storage = lua.create_table()?;
    let read_storage = host.storage.clone();
    storage.set(
        "get",
        lua.create_function(move |lua, key: String| {
            read_storage
                .get(&key)
                .map_err(mlua::Error::runtime)?
                .map_or_else(|| Ok(LuaValue::Nil), |value| lua.to_value(&value))
        })?,
    )?;
    let write_storage = host.storage.clone();
    let write_control = Arc::clone(&host.control);
    let write_catalog = Arc::clone(&host.catalog);
    let write_identity = host.identity.clone();
    let write_generation = host.generation;
    storage.set(
        "set",
        lua.create_function(move |_, (key, value): (String, LuaValue)| {
            if write_control
                .active
                .lock()
                .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
                .is_none()
            {
                return Err(mlua::Error::runtime(
                    "bootty.storage.set is available only inside a command handler",
                ));
            }
            let value = lua_value(value, 0)?;
            write_catalog
                .with_active_extension_generation(write_identity.as_str(), write_generation, || {
                    write_storage.set(key, Some(value))
                })
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    let remove_storage = host.storage.clone();
    let remove_control = Arc::clone(&host.control);
    let remove_catalog = Arc::clone(&host.catalog);
    let remove_identity = host.identity.clone();
    let remove_generation = host.generation;
    storage.set(
        "remove",
        lua.create_function(move |_, key: String| {
            if remove_control
                .active
                .lock()
                .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
                .is_none()
            {
                return Err(mlua::Error::runtime(
                    "bootty.storage.remove is available only inside a command handler",
                ));
            }
            remove_catalog
                .with_active_extension_generation(
                    remove_identity.as_str(),
                    remove_generation,
                    || remove_storage.set(key, None),
                )
                .map_err(mlua::Error::runtime)?
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    storage.set_readonly(true);
    bootty.set("storage", storage)?;
    bootty.set_readonly(true);
    lua.globals().set("bootty", bootty)
}

fn install_surface_interface(
    lua: &Lua,
    bootty: &Table,
    surface_handlers: Arc<std::sync::Mutex<BTreeMap<String, SurfaceHandler>>>,
    surface_declarations: Arc<std::sync::Mutex<Vec<SurfaceDeclaration>>>,
    setup: Option<Arc<WorkerControl>>,
) -> mlua::Result<()> {
    let ui = bootty.get::<Table>("ui")?;
    ui.set(
        "register",
        lua.create_function(
            move |lua, (spec, render, on_action): (Table, Function, Option<Function>)| {
                if let Some(setup) = setup.as_ref() {
                    require_setup_phase(setup)?;
                }
                let id = spec.get::<String>("id")?;
                if id.is_empty()
                    || !id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')
                    })
                {
                    return Err(mlua::Error::runtime(
                        "extension surface identity is invalid",
                    ));
                }
                let placement = SurfacePlacement::parse(&spec.get::<String>("placement")?)
                    .map_err(mlua::Error::runtime)?;
                let order = spec.get::<Option<i32>>("order")?.unwrap_or_default();
                let interval = spec
                    .get::<Option<f64>>("interval")?
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .map_or(Duration::from_secs(1), Duration::from_secs_f64);
                let render = lua.create_registry_value(render)?;
                let action = on_action
                    .map(|handler| lua.create_registry_value(handler))
                    .transpose()?;
                surface_handlers
                    .lock()
                    .map_err(|_| mlua::Error::runtime("extension surface handler lock poisoned"))?
                    .insert(id.clone(), SurfaceHandler { render, action });
                surface_declarations
                    .lock()
                    .map_err(|_| mlua::Error::runtime("extension surface lock poisoned"))?
                    .push(SurfaceDeclaration {
                        id,
                        placement,
                        order,
                        interval,
                    });
                Ok(())
            },
        )?,
    )?;
    ui.set_readonly(true);
    bootty.set("ui", ui)
}

fn require_setup_phase(control: &WorkerControl) -> mlua::Result<()> {
    if control.setup_complete.load(Ordering::Acquire) {
        Err(mlua::Error::runtime(
            "extension declarations are available only during setup",
        ))
    } else {
        Ok(())
    }
}

fn initial_surface_snapshots(
    lua: &Lua,
    handlers: &std::sync::Mutex<BTreeMap<String, SurfaceHandler>>,
    declarations: &std::sync::Mutex<Vec<SurfaceDeclaration>>,
) -> Result<Vec<SurfaceSnapshot>, String> {
    let declarations = declarations
        .lock()
        .map_err(|_| "extension surface lock poisoned".to_owned())?
        .clone();
    let handlers = handlers
        .lock()
        .map_err(|_| "extension surface handler lock poisoned".to_owned())?;
    declarations
        .into_iter()
        .map(|declaration| {
            let handler = handlers
                .get(&declaration.id)
                .ok_or_else(|| "extension surface handler is missing".to_owned())?;
            let render = lua
                .registry_value::<Function>(&handler.render)
                .map_err(|error| error.to_string())?;
            let value = render
                .call::<LuaValue>(())
                .map_err(|error| error.to_string())?;
            Ok(SurfaceSnapshot {
                declaration,
                items: items_from_value(value),
            })
        })
        .collect()
}

fn render_and_publish_surfaces(
    lua: &Lua,
    host: &ModuleHost,
    handlers: &std::sync::Mutex<BTreeMap<String, SurfaceHandler>>,
    declarations: &std::sync::Mutex<Vec<SurfaceDeclaration>>,
) {
    if let Ok(mut active) = host.control.active.lock() {
        *active = Some(ActiveInvocation {
            deadline: Instant::now() + Duration::from_millis(50),
            cancellation: CommandCancellation::new(),
        });
    }
    let snapshots = initial_surface_snapshots(lua, handlers, declarations);
    if let Ok(mut active) = host.control.active.lock() {
        *active = None;
    }
    match snapshots {
        Ok(snapshots) => {
            let _ = host.catalog.publish_extension_surfaces(
                host.identity.as_str(),
                host.generation,
                snapshots,
            );
        }
        Err(error) => eprintln!(
            "failed to render extension {} generation {}: {error}",
            host.identity, host.generation
        ),
    }
}

fn run_surface_action(
    lua: &Lua,
    host: &ModuleHost,
    handlers: &std::sync::Mutex<BTreeMap<String, SurfaceHandler>>,
    action: ExtensionUiAction,
) {
    let result = (|| -> Result<(), String> {
        if action.module != host.identity.as_str() || action.generation != host.generation {
            return Err("extension generation is no longer active".to_owned());
        }
        let handlers = handlers
            .lock()
            .map_err(|_| "extension surface handler lock poisoned".to_owned())?;
        let handler = handlers
            .get(&action.surface)
            .and_then(|handler| handler.action.as_ref())
            .ok_or_else(|| "extension surface has no action handler".to_owned())?;
        let handler = lua
            .registry_value::<Function>(handler)
            .map_err(|error| error.to_string())?;
        let payload = lua
            .to_value(&action.payload)
            .map_err(|error| error.to_string())?;
        if let Ok(mut active) = host.control.active.lock() {
            *active = Some(ActiveInvocation {
                deadline: Instant::now() + Duration::from_millis(50),
                cancellation: CommandCancellation::new(),
            });
        }
        let called = handler
            .call::<()>((action.action, payload))
            .map_err(|error| error.to_string());
        if let Ok(mut active) = host.control.active.lock() {
            *active = None;
        }
        called
    })();
    if let Err(error) = result {
        eprintln!(
            "failed to run extension {} generation {} surface action: {error}",
            host.identity, host.generation
        );
    }
}

fn invoke_handler(
    lua: &Lua,
    handlers: &std::sync::Mutex<BTreeMap<String, RegistryKey>>,
    control: &WorkerControl,
    work: Invocation,
) -> CommandOutcome {
    if !control.generation.is_active() {
        CommandOutcome::Failed {
            code: "stale_extension_generation".to_owned(),
            message: "extension generation is no longer active".to_owned(),
        }
    } else if work.cancellation.is_cancelled() {
        CommandOutcome::Failed {
            code: "cancelled".to_owned(),
            message: "extension command was cancelled".to_owned(),
        }
    } else if Instant::now() >= work.deadline {
        CommandOutcome::Failed {
            code: "deadline_exceeded".to_owned(),
            message: "extension command deadline expired".to_owned(),
        }
    } else {
        let context = ActiveInvocation {
            deadline: work.deadline,
            cancellation: work.cancellation.clone(),
        };
        if let Ok(mut active) = control.active.lock() {
            *active = Some(context);
        }
        let result = handlers
            .lock()
            .map_err(|_| "extension handler lock poisoned".to_owned())
            .and_then(|handlers| {
                let key = handlers
                    .get(&work.command)
                    .ok_or_else(|| "extension command is not registered".to_owned())?;
                let handler = lua
                    .registry_value::<Function>(key)
                    .map_err(|error| error.to_string())?;
                let context = lua.create_table().map_err(|error| error.to_string())?;
                let arguments = lua
                    .create_sequence_from(work.invocation.arguments)
                    .map_err(|error| error.to_string())?;
                context
                    .set("arguments", arguments)
                    .map_err(|error| error.to_string())?;
                handler
                    .call::<LuaValue>(context)
                    .map_err(|error| error.to_string())
            });
        if let Ok(mut active) = control.active.lock() {
            *active = None;
        }
        if !control.generation.is_active() {
            CommandOutcome::Failed {
                code: "stale_extension_generation".to_owned(),
                message: "extension generation is no longer active".to_owned(),
            }
        } else {
            match result {
                Ok(value) => match lua_value(value, 0) {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(error) => CommandOutcome::Failed {
                        code: "extension_result_invalid".to_owned(),
                        message: error.to_string(),
                    },
                },
                Err(_) if work.cancellation.is_cancelled() => CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "extension command was cancelled".to_owned(),
                },
                Err(_) if Instant::now() >= work.deadline => CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "extension command deadline expired".to_owned(),
                },
                Err(message) => CommandOutcome::Failed {
                    code: "extension_failed".to_owned(),
                    message,
                },
            }
        }
    }
}

fn submit_app_command(
    commands: &BoundAppCommandSender,
    invocation: CommandInvocation,
    active: ActiveInvocation,
) -> CommandOutcome {
    let receiver = match commands.submit(invocation, active.deadline, active.cancellation.clone()) {
        Ok(receiver) => receiver,
        Err(error) => {
            return match error {
                AppCommandSendError::Overloaded => CommandOutcome::Failed {
                    code: "overloaded".to_owned(),
                    message: "application command queue is overloaded".to_owned(),
                },
                AppCommandSendError::Shutdown => CommandOutcome::Failed {
                    code: "shutdown".to_owned(),
                    message: "application command channel shut down".to_owned(),
                },
            };
        }
    };
    loop {
        if active.cancellation.is_cancelled() {
            return CommandOutcome::Failed {
                code: "cancelled".to_owned(),
                message: "command was cancelled".to_owned(),
            };
        }
        let remaining = active.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            active.cancellation.cancel();
            return CommandOutcome::Failed {
                code: "deadline_exceeded".to_owned(),
                message: "command deadline expired".to_owned(),
            };
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(5))) {
            Ok(outcome) => return outcome,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return CommandOutcome::Failed {
                    code: "shutdown".to_owned(),
                    message: "application command response channel closed".to_owned(),
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn descriptor_from_table(namespace: &str, spec: &Table) -> mlua::Result<CommandDescriptor> {
    let id: String = spec.get("id")?;
    if !crate::commands::is_namespaced(&id, namespace) {
        return Err(mlua::Error::runtime(
            "extension command must be namespaced by its module",
        ));
    }
    let mutation = match spec.get::<Option<String>>("mutation")?.as_deref() {
        None | Some("read") => MutationClass::Read,
        Some("write") => MutationClass::Write,
        Some("destructive") => MutationClass::Destructive,
        Some(value) => return Err(mlua::Error::runtime(format!("invalid mutation {value}"))),
    };
    let target = match spec.get::<Option<String>>("target")?.as_deref() {
        None => None,
        Some("application_window") => Some(ResourceKind::ApplicationWindow),
        Some("binding") => Some(ResourceKind::Binding),
        Some("session") => Some(ResourceKind::Session),
        Some("mux_window") => Some(ResourceKind::MuxWindow),
        Some("pane") => Some(ResourceKind::Pane),
        Some("terminal") => Some(ResourceKind::Terminal),
        Some(value) => return Err(mlua::Error::runtime(format!("invalid target {value}"))),
    };
    let mut arguments = Vec::new();
    if let Some(schema) = spec.get::<Option<Table>>("arguments")? {
        for argument in schema.sequence_values::<Table>() {
            let argument = argument?;
            arguments.push(ArgumentSchema {
                name: argument.get("name")?,
                value_type: value_type(&argument.get::<String>("type")?)?,
                required: argument.get::<Option<bool>>("required")?.unwrap_or(false),
                choices: argument
                    .get::<Option<Table>>("choices")?
                    .map(|choices| choices.sequence_values().collect())
                    .transpose()?
                    .unwrap_or_default(),
                minimum: argument.get("minimum")?,
                maximum: argument.get("maximum")?,
            });
        }
    }
    Ok(CommandDescriptor {
        id,
        title: spec.get("title")?,
        description: spec
            .get::<Option<String>>("description")?
            .unwrap_or_default(),
        mutation,
        arguments: CompactSchema { arguments },
        target,
        palette: spec.get::<Option<bool>>("palette")?.unwrap_or(false),
    })
}

fn value_type(value: &str) -> mlua::Result<ValueType> {
    match value {
        "string" => Ok(ValueType::String),
        "integer" => Ok(ValueType::Integer),
        "number" => Ok(ValueType::Number),
        other => Err(mlua::Error::runtime(format!(
            "invalid argument type {other}"
        ))),
    }
}

fn lua_value(value: LuaValue, depth: usize) -> mlua::Result<Value> {
    if depth >= 32 {
        return Err(mlua::Error::runtime(
            "extension value nesting limit exceeded",
        ));
    }
    match value {
        LuaValue::Nil => Ok(Value::Null),
        LuaValue::Boolean(value) => Ok(Value::Bool(value)),
        LuaValue::Integer(value) => Ok(json!(value)),
        LuaValue::Number(value) => Ok(json!(value)),
        LuaValue::String(value) => Ok(Value::String(value.to_string_lossy())),
        LuaValue::Table(table) => {
            let length = table.raw_len();
            let mut array = Vec::with_capacity(length);
            let mut sequence = true;
            for index in 1..=length {
                match table.raw_get::<LuaValue>(index)? {
                    LuaValue::Nil => {
                        sequence = false;
                        break;
                    }
                    value => array.push(lua_value(value, depth + 1)?),
                }
            }
            if sequence && table.clone().pairs::<LuaValue, LuaValue>().count() == length {
                return Ok(Value::Array(array));
            }
            let mut object = Map::new();
            for pair in table.pairs::<String, LuaValue>() {
                let (key, value) = pair?;
                object.insert(key, lua_value(value, depth + 1)?);
            }
            Ok(Value::Object(object))
        }
        _ => Err(mlua::Error::runtime(
            "extension values must be JSON-compatible",
        )),
    }
}
