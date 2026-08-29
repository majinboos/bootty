use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::facts::{ExtensionFactGeneration, ExtensionFacts};
use crate::integrations::IntegrationDeclaration;
use crate::module_sources::ModuleSource;
use crate::queue::{
    self, ExtensionInvocationRequest, ExtensionInvocationSender, ExtensionWorkerMessage,
    ExtensionWorkerReceiver, ExtensionWorkerSender,
};
use crate::storage::ExtensionStorage;
use crate::surfaces::{SurfaceDeclaration, SurfacePlacement, SurfaceSnapshot};
use crate::{
    ExtensionCatalog, ExtensionEventSender, ExtensionGenerationToken, ExtensionUiAction,
    ModuleIdentity, items::items_from_value,
};
use bootty_command::{
    AppCommandSendError, ArgumentSchema, BoundAppCommandSender, Caller, CommandCancellation,
    CommandDescriptor, CommandInvocation, CommandOutcome, CompactSchema, MutationClass,
    ResourceKind, ValueType,
};
use bootty_config::config::ExtensionSettingValue;
use mlua::{Function, Lua, LuaSerdeExt, RegistryKey, Table, Value as LuaValue, VmState};
use serde_json::{Map, Value, json};

const SETUP_EXECUTION_LIMIT: Duration = Duration::from_millis(100);
const SETUP_RESPONSE_LIMIT: Duration = Duration::from_millis(250);
const MODULE_COMMAND_LIMIT: usize = 64;
const MODULE_TOPIC_LIMIT: usize = 64;
const MODULE_SURFACE_LIMIT: usize = 64;
const MODULE_SETTING_LIMIT: usize = 32;
const MODULE_INTEGRATION_LIMIT: usize = 8;

#[derive(Clone)]
pub(crate) struct ActiveInvocation {
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CommandCancellation,
}

pub(crate) struct WorkerControl {
    pub(crate) generation: ExtensionGenerationToken,
    pub(crate) setup_complete: AtomicBool,
    pub(crate) setup_deadline: OnceLock<Instant>,
    pub(crate) active: Mutex<Option<ActiveInvocation>>,
}

pub(crate) struct ModuleWorker {
    pub(crate) control: Arc<WorkerControl>,
    pub(crate) sender: ExtensionWorkerSender,
    facts: ExtensionFacts,
    thread: Option<thread::JoinHandle<()>>,
}

impl ModuleWorker {
    pub(crate) fn retire(mut self) -> Option<thread::JoinHandle<()>> {
        self.control.generation.retire();
        if let Ok(active) = self.control.active.lock()
            && let Some(active) = active.as_ref()
        {
            active.cancellation.cancel();
        }
        self.facts.retire();
        self.thread.take()
    }
}

pub(crate) struct ActiveModule {
    pub(crate) generation: u64,
    pub(crate) fingerprint: u64,
    pub(crate) worker: ModuleWorker,
    pub(crate) storage: ExtensionStorage,
    /// What this module declared through `bootty.settings.register`.
    pub(crate) settings: Vec<ExtensionSettingDeclaration>,
    /// What this module declared through `bootty.integration.register`.
    pub(crate) integrations: Vec<IntegrationDeclaration>,
}

/// A setting an extension declared for itself. The host stamps `module` from the module's own
/// identity, so a declaration can only ever land in that module's namespace.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionSettingDeclaration {
    pub module: String,
    pub key: String,
    pub label: String,
    pub help: String,
    /// The value shown, and used by the module, until the user sets one. Its shape also decides
    /// which control the settings UI renders.
    pub default: ExtensionSettingValue,
}

fn setting_declaration_from_table(spec: &Table) -> mlua::Result<ExtensionSettingDeclaration> {
    let key: String = spec.get("key")?;
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(mlua::Error::runtime(
            "a setting key must be non-empty and use only letters, digits, - or _",
        ));
    }
    let default = match spec.get::<LuaValue>("default")? {
        LuaValue::Boolean(value) => ExtensionSettingValue::Bool(value),
        LuaValue::Integer(value) => ExtensionSettingValue::Number(value as f64),
        LuaValue::Number(value) => ExtensionSettingValue::Number(value),
        LuaValue::String(value) => ExtensionSettingValue::Text(value.to_str()?.to_owned()),
        _ => {
            return Err(mlua::Error::runtime(
                "a setting default must be a boolean, number or string",
            ));
        }
    };
    Ok(ExtensionSettingDeclaration {
        // Filled in by the host, which knows the module's identity.
        module: String::new(),
        key,
        label: spec.get::<Option<String>>("label")?.unwrap_or_default(),
        help: spec.get::<Option<String>>("help")?.unwrap_or_default(),
        default,
    })
}

struct ModuleDeclarations {
    commands: Vec<CommandDescriptor>,
    topics: Vec<String>,
    surfaces: Vec<SurfaceSnapshot>,
    settings: Vec<ExtensionSettingDeclaration>,
    integrations: Vec<IntegrationDeclaration>,
}

/// What every module gets from the host, identical for all of them: the shared catalog and
/// channels, plus where an integration installs its files.
#[derive(Clone)]
pub(crate) struct ModuleEnvironment {
    pub(crate) catalog: Arc<ExtensionCatalog>,
    pub(crate) commands: BoundAppCommandSender,
    pub(crate) events: ExtensionEventSender,
    pub(crate) integration_dir: PathBuf,
}

pub(crate) struct PreparedModule {
    pub(crate) commands: Vec<(CommandDescriptor, ExtensionInvocationSender)>,
    pub(crate) topics: Vec<String>,
    pub(crate) surfaces: Vec<SurfaceSnapshot>,
    pub(crate) settings: Vec<ExtensionSettingDeclaration>,
    pub(crate) integrations: Vec<IntegrationDeclaration>,
    pub(crate) worker: ModuleWorker,
}

struct SurfaceHandler {
    render: RegistryKey,
    action: Option<RegistryKey>,
}

#[derive(Default)]
struct ModuleRegistry {
    handlers: RefCell<BTreeMap<String, RegistryKey>>,
    settings: RefCell<Vec<ExtensionSettingDeclaration>>,
    integrations: RefCell<Vec<IntegrationDeclaration>>,
    descriptors: RefCell<Vec<CommandDescriptor>>,
    topics: RefCell<Vec<String>>,
    surface_handlers: RefCell<BTreeMap<String, SurfaceHandler>>,
    surface_declarations: RefCell<Vec<SurfaceDeclaration>>,
}

#[derive(Clone)]
struct ModuleHost {
    identity: ModuleIdentity,
    namespace: String,
    generation: u64,
    commands: BoundAppCommandSender,
    events: ExtensionEventSender,
    catalog: Arc<ExtensionCatalog>,
    control: Arc<WorkerControl>,
    storage: ExtensionStorage,
    facts: ExtensionFacts,
    /// Where `bootty.integration.register` files land. A module reads it to compose the absolute
    /// paths it writes into another tool's config.
    integration_dir: PathBuf,
}

/// Preview a built-in module by name, so a caller can show a draft module composed over the
/// built-in surface it decorates rather than on its own.
pub fn preview_builtin_surfaces(
    name: &str,
    theme: Vec<(String, String)>,
) -> Result<Vec<SurfaceSnapshot>, String> {
    let identity = ModuleIdentity::parse(format!("{name}.luau"))?;
    // The wrapped source, not the bare file: a built-in declares its surface through the wrapper
    // the host loads it with, so previewing the raw text registers nothing at all.
    let builtin = crate::module_sources::builtin_module(&identity)
        .ok_or_else(|| format!("no built-in module named `{name}`"))?;
    preview_module_surfaces(&identity, &builtin.source, theme)
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
    let registry = Rc::new(ModuleRegistry::default());
    install_surface_interface(&lua, &bootty, Rc::clone(&registry), None)
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
    initial_surface_snapshots(&lua, &registry, &facts)
}

fn install_preview_noop_tables(lua: &Lua, bootty: &Table) -> mlua::Result<()> {
    let settings = lua.create_table()?;
    settings.set("register", lua.create_function(|_, _: Table| Ok(()))?)?;
    settings.set(
        "get",
        lua.create_function(|_, _: String| Ok(LuaValue::Nil))?,
    )?;
    settings.set_readonly(true);
    bootty.set("settings", settings)?;

    let integration = lua.create_table()?;
    integration.set("dir", String::new())?;
    integration.set("register", lua.create_function(|_, _: Table| Ok(()))?)?;
    integration.set_readonly(true);
    bootty.set("integration", integration)?;

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

pub(crate) fn prepare_module(
    module: &ModuleSource,
    storage: ExtensionStorage,
    generation: u64,
    facts: ExtensionFacts,
    environment: ModuleEnvironment,
) -> Result<PreparedModule, String> {
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (tx, rx) = queue::worker_queue();
    let control = Arc::new(WorkerControl {
        generation: ExtensionGenerationToken::new(),
        setup_complete: AtomicBool::new(false),
        setup_deadline: OnceLock::new(),
        active: Mutex::new(None),
    });
    let host = ModuleHost {
        identity: module.identity.clone(),
        namespace: module.namespace.clone(),
        generation,
        commands: environment.commands,
        events: environment.events,
        catalog: environment.catalog,
        control: Arc::clone(&control),
        storage,
        facts: facts.clone(),
        integration_dir: environment.integration_dir,
    };
    let source = module.source.clone();
    let thread_name = format!("bootty-extension-{}", host.namespace);
    let thread = thread::Builder::new()
        .name(thread_name)
        .spawn(move || run_module_worker(&host, &source, &rx, &ready_tx))
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
    for (kind, count, limit) in [
        ("command", declarations.commands.len(), MODULE_COMMAND_LIMIT),
        ("event topic", declarations.topics.len(), MODULE_TOPIC_LIMIT),
        ("surface", declarations.surfaces.len(), MODULE_SURFACE_LIMIT),
        ("setting", declarations.settings.len(), MODULE_SETTING_LIMIT),
        (
            "integration",
            declarations.integrations.len(),
            MODULE_INTEGRATION_LIMIT,
        ),
    ] {
        if count > limit {
            let _ = worker.retire();
            return Err(format!(
                "extension {kind} count exceeds the limit of {limit}"
            ));
        }
    }
    let module_namespace = module.identity.namespace();
    let settings = declarations
        .settings
        .into_iter()
        .map(|declaration| ExtensionSettingDeclaration {
            module: module_namespace.clone(),
            ..declaration
        })
        .collect();
    let integrations = declarations
        .integrations
        .into_iter()
        .map(|declaration| IntegrationDeclaration {
            module: module_namespace.clone(),
            ..declaration
        })
        .collect();
    let registrations = declarations
        .commands
        .into_iter()
        .map(|descriptor| (descriptor, tx.invocation_sender()))
        .collect();
    Ok(PreparedModule {
        commands: registrations,
        topics: declarations.topics,
        surfaces: declarations.surfaces,
        settings,
        integrations,
        worker,
    })
}

fn run_module_worker(
    host: &ModuleHost,
    source: &str,
    rx: &ExtensionWorkerReceiver,
    ready: &mpsc::SyncSender<Result<ModuleDeclarations, String>>,
) {
    let lua = Lua::new();
    let registry = Rc::new(ModuleRegistry::default());
    let interrupt_control = Arc::clone(&host.control);
    lua.set_interrupt(move |_| worker_interrupt(&interrupt_control));
    let setup = install_host_interface(&lua, host, Rc::clone(&registry))
        .and_then(|()| lua.sandbox(true))
        .and_then(|()| {
            host.control
                .setup_deadline
                .set(Instant::now() + SETUP_EXECUTION_LIMIT)
                .map_err(|_| mlua::Error::runtime("extension setup deadline already started"))?;
            lua.load(source).set_name(host.identity.as_str()).exec()
        });
    if let Err(error) = setup {
        let _ = ready.send(Err(error.to_string()));
        return;
    }
    let surfaces = initial_surface_snapshots(&lua, &registry, &host.facts);
    let registered = surfaces.map(|surfaces| ModuleDeclarations {
        commands: std::mem::take(&mut *registry.descriptors.borrow_mut()),
        topics: std::mem::take(&mut *registry.topics.borrow_mut()),
        surfaces,
        settings: std::mem::take(&mut *registry.settings.borrow_mut()),
        integrations: std::mem::take(&mut *registry.integrations.borrow_mut()),
    });
    host.control.setup_complete.store(true, Ordering::Release);
    if ready.send(registered).is_err() {
        return;
    }
    let render_interval = registry
        .surface_declarations
        .borrow()
        .iter()
        .map(|surface| surface.interval)
        .min();
    let mut next_render = render_interval.map(|interval| Instant::now() + interval);
    while host.control.generation.is_active() {
        match rx.recv_until(next_render) {
            Ok(Some(ExtensionWorkerMessage::Invoke(work))) => {
                let response = work.response.clone();
                let _ = response.send(invoke_handler(&lua, &registry, &host.control, work));
            }

            Ok(Some(ExtensionWorkerMessage::Action(action))) => {
                run_surface_action(&lua, host, &registry, action);
                render_and_publish_surfaces(&lua, host, &registry);
                next_render = render_interval.map(|interval| Instant::now() + interval);
            }
            // A timer tick and an explicit render request do the same work; the host uses the
            // latter to wake a parked module that came back on screen.
            Ok(Some(ExtensionWorkerMessage::Render) | None) => {
                render_and_publish_surfaces(&lua, host, &registry);
                next_render = render_interval.map(|interval| Instant::now() + interval);
            }
            Err(_) => break,
        }
    }
    rx.drain_shutdown();
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
        && control
            .setup_deadline
            .get()
            .is_some_and(|deadline| Instant::now() >= *deadline)
    {
        return Err(mlua::Error::runtime("extension setup exceeded 100 ms"));
    }
    Ok(VmState::Continue)
}

fn install_host_interface(
    lua: &Lua,
    host: &ModuleHost,
    registry: Rc<ModuleRegistry>,
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
    let command_registry = Rc::clone(&registry);
    commands.set(
        "register",
        lua.create_function(move |lua, (spec, handler): (Table, Function)| {
            require_setup_phase(&command_setup)?;
            let descriptor = descriptor_from_table(&command_namespace, &spec)?;
            let key = lua.create_registry_value(handler)?;
            command_registry
                .handlers
                .borrow_mut()
                .insert(descriptor.id.clone(), key);
            command_registry.descriptors.borrow_mut().push(descriptor);
            Ok(())
        })?,
    )?;
    let active = Arc::clone(&host.control);
    let app_commands = host.commands.clone();
    commands.set(
        "invoke",
        lua.create_function(move |lua, spec: Table| {
            let active = require_active(
                &active,
                "bootty.commands.invoke is available only inside a command handler",
            )?;
            let mut value = lua_value(LuaValue::Table(spec), 0)?;
            let object = value.as_object_mut().ok_or_else(|| {
                mlua::Error::runtime("bootty.commands.invoke needs a command table")
            })?;
            object.insert("caller".to_owned(), json!(Caller::Luau));
            let invocation = serde_json::from_value(value)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            let outcome = submit_app_command(&app_commands, invocation, &active);
            let outcome = serde_json::to_value(outcome)
                .map_err(|error| mlua::Error::runtime(error.to_string()))?;
            lua.to_value(&outcome)
        })?,
    )?;
    bootty.set("commands", commands)?;

    // `bootty.settings`: a module declares its own settings during setup and reads their accepted
    // values any time. The module never names its namespace — the host derives it from the
    // module's identity — so one extension cannot declare or read another's settings.
    let settings = lua.create_table()?;
    let settings_setup = Arc::clone(&host.control);
    let settings_registry = Rc::clone(&registry);
    settings.set(
        "register",
        lua.create_function(move |_, spec: Table| {
            require_setup_phase(&settings_setup)?;
            let declaration = setting_declaration_from_table(&spec)?;
            settings_registry.settings.borrow_mut().push(declaration);
            Ok(())
        })?,
    )?;
    let settings_facts = host.facts.clone();
    let settings_module = host.identity.namespace();
    settings.set(
        "get",
        lua.create_function(move |lua, key: String| {
            match settings_facts.extension_setting(&settings_module, &key) {
                Some(ExtensionSettingValue::Bool(value)) => lua.pack(value),
                Some(ExtensionSettingValue::Number(value)) => lua.pack(value),
                Some(ExtensionSettingValue::Text(value)) => lua.pack(value),
                None => Ok(LuaValue::Nil),
            }
        })?,
    )?;
    settings.set_readonly(true);
    bootty.set("settings", settings)?;

    // `bootty.integration`: the adapter another tool needs before this module can see anything.
    // `dir` is where Bootty writes the files, so a module composes the absolute path it references
    // from its own `merge` value. The host knows only "write these files" and "merge this JSON" —
    // every tool-specific name and config path belongs to the module.
    let integration = lua.create_table()?;
    integration.set("dir", host.integration_dir.to_string_lossy().into_owned())?;
    let integration_setup = Arc::clone(&host.control);
    let integration_registry = Rc::clone(&registry);
    integration.set(
        "register",
        lua.create_function(move |_, spec: Table| {
            require_setup_phase(&integration_setup)?;
            let declaration = crate::integrations::declaration_from_table(&spec)?;
            integration_registry
                .integrations
                .borrow_mut()
                .push(declaration);
            Ok(())
        })?,
    )?;
    integration.set_readonly(true);
    bootty.set("integration", integration)?;

    let events = lua.create_table()?;
    let event_namespace = host.namespace.clone();
    let event_setup = Arc::clone(&host.control);
    let event_registry = Rc::clone(&registry);
    events.set(
        "register",
        lua.create_function(move |_, topic: String| {
            require_setup_phase(&event_setup)?;
            if !crate::catalog::is_namespaced(&topic, &event_namespace) {
                return Err(mlua::Error::runtime(
                    "extension event topic must be namespaced by its module",
                ));
            }
            event_registry.topics.borrow_mut().push(topic);
            Ok(())
        })?,
    )?;
    let publish_identity = host.identity.clone();
    let publish_generation = host.generation;
    let publish_control = Arc::clone(&host.control);
    let event_sender = host.events.clone();
    events.set(
        "publish",
        lua.create_function(move |_, (topic, payload): (String, LuaValue)| {
            require_active(
                &publish_control,
                "bootty.events.publish is available only inside a command handler",
            )?;
            let payload = lua_value(payload, 0)?;
            let active = publish_control
                .active
                .lock()
                .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
                .clone()
                .ok_or_else(|| mlua::Error::runtime("extension invocation is no longer active"))?;
            event_sender
                .publish(
                    publish_identity.clone(),
                    publish_generation,
                    topic,
                    payload,
                    active.deadline,
                    &active.cancellation,
                )
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    bootty.set("events", events)?;

    install_surface_interface(lua, &bootty, registry, Some(Arc::clone(&host.control)))?;

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
            require_active(
                &write_control,
                "bootty.storage.set is available only inside a command handler",
            )?;
            let value = lua_value(value, 0)?;
            write_catalog
                .with_active_generation(write_identity.as_str(), write_generation, || {
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
            require_active(
                &remove_control,
                "bootty.storage.remove is available only inside a command handler",
            )?;
            remove_catalog
                .with_active_generation(remove_identity.as_str(), remove_generation, || {
                    remove_storage.set(key, None)
                })
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
    registry: Rc<ModuleRegistry>,
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
                registry
                    .surface_handlers
                    .borrow_mut()
                    .insert(id.clone(), SurfaceHandler { render, action });
                registry
                    .surface_declarations
                    .borrow_mut()
                    .push(SurfaceDeclaration {
                        id,
                        placement,
                        order,
                        interval,
                        title: spec.get::<Option<String>>("title")?,
                        icon: spec.get::<Option<String>>("icon")?,
                        hint: spec.get::<Option<String>>("hint")?,
                    });
                Ok(())
            },
        )?,
    )?;
    ui.set_readonly(true);
    bootty.set("ui", ui)
}

/// Returns the invocation an extension host call runs inside, or `message` when there is none.
pub(crate) fn require_active(
    control: &WorkerControl,
    message: &str,
) -> mlua::Result<ActiveInvocation> {
    control
        .active
        .lock()
        .map_err(|_| mlua::Error::runtime("extension invocation lock poisoned"))?
        .clone()
        .ok_or_else(|| mlua::Error::runtime(message))
}

fn with_active<T>(
    control: &WorkerControl,
    invocation: ActiveInvocation,
    run: impl FnOnce() -> T,
) -> T {
    if let Ok(mut active) = control.active.lock() {
        *active = Some(invocation);
    }
    let result = run();
    if let Ok(mut active) = control.active.lock() {
        *active = None;
    }
    result
}

fn command_failure(code: &str, message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: code.to_owned(),
        message: message.into(),
    }
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
    registry: &ModuleRegistry,
    facts: &ExtensionFacts,
) -> Result<Vec<SurfaceSnapshot>, String> {
    let declarations = registry.surface_declarations.borrow().clone();
    let handlers = registry.surface_handlers.borrow();
    declarations
        .into_iter()
        .map(|declaration| {
            // Every surface on this worker re-renders on the shortest interval any of them asked
            // for, so a surface's own interval has to bound its shell-outs instead. Without it a
            // 60s module re-runs its command at the 50ms animation cadence.
            facts.set_run_refresh_gap(declaration.interval);
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

fn render_and_publish_surfaces(lua: &Lua, host: &ModuleHost, registry: &ModuleRegistry) {
    let snapshots = with_active(
        &host.control,
        ActiveInvocation {
            deadline: Instant::now() + Duration::from_millis(50),
            cancellation: CommandCancellation::new(),
        },
        || initial_surface_snapshots(lua, registry, &host.facts),
    );
    match snapshots {
        Ok(snapshots) => {
            let _ =
                host.catalog
                    .publish_surfaces(host.identity.as_str(), host.generation, snapshots);
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
    registry: &ModuleRegistry,
    action: ExtensionUiAction,
) {
    let result = (|| -> Result<(), String> {
        if action.module != host.identity.as_str()
            || action.generation != host.generation
            || !host.control.generation.is_active()
        {
            return Err("extension generation is no longer active".to_owned());
        }
        let handlers = registry.surface_handlers.borrow();
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
        with_active(
            &host.control,
            ActiveInvocation {
                deadline: Instant::now() + Duration::from_millis(50),
                cancellation: CommandCancellation::new(),
            },
            || {
                handler
                    .call::<()>((action.action, payload))
                    .map_err(|error| error.to_string())
            },
        )
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
    registry: &ModuleRegistry,
    control: &WorkerControl,
    work: ExtensionInvocationRequest,
) -> CommandOutcome {
    if !control.generation.is_active() {
        command_failure(
            "stale_extension_generation",
            "extension generation is no longer active",
        )
    } else if work.cancellation.is_cancelled() {
        command_failure("cancelled", "extension command was cancelled")
    } else if Instant::now() >= work.deadline {
        command_failure("deadline_exceeded", "extension command deadline expired")
    } else {
        let context = ActiveInvocation {
            deadline: work.deadline,
            cancellation: work.cancellation.clone(),
        };
        let result = with_active(control, context, || {
            if control.generation.is_active() {
                let handlers = registry.handlers.borrow();
                (|| {
                    let key = handlers
                        .get(&work.invocation.command)
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
                    let target = lua
                        .to_value(&work.invocation.target)
                        .map_err(|error| error.to_string())?;
                    context
                        .set("target", target)
                        .map_err(|error| error.to_string())?;
                    context
                        .set("target_supplied", work.target_supplied)
                        .map_err(|error| error.to_string())?;
                    handler
                        .call::<LuaValue>(context)
                        .map_err(|error| error.to_string())
                })()
            } else {
                let _ = work.cancellation.cancel();
                Err("extension generation retired".to_owned())
            }
        });
        if control.generation.is_active() {
            match result {
                Ok(value) => match lua_value(value, 0) {
                    Ok(value) => CommandOutcome::Success {
                        value,
                        warnings: Vec::new(),
                    },
                    Err(error) => command_failure("extension_result_invalid", error.to_string()),
                },
                Err(_) if work.cancellation.is_cancelled() => {
                    command_failure("cancelled", "extension command was cancelled")
                }
                Err(_) if Instant::now() >= work.deadline => {
                    command_failure("deadline_exceeded", "extension command deadline expired")
                }
                Err(message) => command_failure("extension_failed", message),
            }
        } else {
            command_failure(
                "stale_extension_generation",
                "extension generation is no longer active",
            )
        }
    }
}

fn submit_app_command(
    commands: &BoundAppCommandSender,
    invocation: CommandInvocation,
    active: &ActiveInvocation,
) -> CommandOutcome {
    let cancellation = CommandCancellation::new();
    let receiver = match commands.submit(invocation, active.deadline, cancellation.clone()) {
        Ok(receiver) => receiver,
        Err(error) => {
            return match error {
                AppCommandSendError::Overloaded => {
                    command_failure("overloaded", "application command queue is overloaded")
                }
                AppCommandSendError::Shutdown => {
                    command_failure("shutdown", "application command channel shut down")
                }
            };
        }
    };
    loop {
        if active.cancellation.is_cancelled() {
            cancellation.cancel();
            return command_failure("cancelled", "command was cancelled");
        }
        let remaining = active.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cancellation.cancel();
            return command_failure("deadline_exceeded", "command deadline expired");
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(5))) {
            Ok(outcome) => return outcome,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return command_failure("shutdown", "application command response channel closed");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn descriptor_from_table(namespace: &str, spec: &Table) -> mlua::Result<CommandDescriptor> {
    let id: String = spec.get("id")?;
    if !crate::catalog::is_namespaced(&id, namespace) {
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

pub(crate) fn lua_value(value: LuaValue, depth: usize) -> mlua::Result<Value> {
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
            if sequence && table.pairs::<LuaValue, LuaValue>().count() == length {
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
