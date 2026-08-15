use std::{
    collections::{BTreeMap, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    commands::{
        ArgumentSchema, CommandCancellation, CommandCatalog, CommandDescriptor, CommandInvocation,
        CommandOutcome, CompactSchema, MutationClass, ResourceKind, ValueType,
    },
    control::ControlPlane,
};
use mlua::{Function, Lua, RegistryKey, Table, Value as LuaValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    id: String,
    entrypoint: PathBuf,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

struct Invocation {
    command: String,
    invocation: CommandInvocation,
    deadline: Instant,
    cancellation: CommandCancellation,
    response: mpsc::Sender<CommandOutcome>,
}

struct PackageWorker {
    id: String,
    generation: u64,
    thread: Option<thread::JoinHandle<()>>,
}

pub struct CommandExtensionHost {
    root: PathBuf,
    catalog: Arc<CommandCatalog>,
    plane: ControlPlane,
    workers: Vec<PackageWorker>,
    fingerprint: u64,
    next_check: Instant,
    next_generation: u64,
}

impl CommandExtensionHost {
    pub fn load(root: &Path, catalog: Arc<CommandCatalog>, plane: ControlPlane) -> Self {
        let mut host = Self {
            root: root.to_owned(),
            catalog,
            plane,
            workers: Vec::new(),
            fingerprint: 0,
            next_check: Instant::now(),
            next_generation: 1,
        };
        host.reload();
        host
    }

    pub fn refresh(&mut self, now: Instant) {
        if now < self.next_check {
            return;
        }
        self.next_check = now + Duration::from_millis(500);
        let fingerprint = package_fingerprint(&self.root);
        if fingerprint != self.fingerprint {
            self.reload();
        }
    }

    fn reload(&mut self) {
        self.clear_workers();
        self.fingerprint = package_fingerprint(&self.root);
        let Ok(entries) = fs::read_dir(&self.root) else {
            return;
        };
        let mut packages = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        packages.sort();
        for package in packages {
            if let Err(error) = self.load_package(&package) {
                eprintln!("failed to load extension {}: {error}", package.display());
            }
        }
    }

    fn load_package(&mut self, root: &Path) -> Result<(), String> {
        let manifest_path = root.join("extension.json");
        let manifest: PackageManifest =
            serde_json::from_slice(&fs::read(&manifest_path).map_err(|error| error.to_string())?)
                .map_err(|error| error.to_string())?;
        validate_package_id(&manifest.id)?;
        if !manifest.enabled {
            return Ok(());
        }
        let entrypoint = safe_entrypoint(root, &manifest.entrypoint)?;
        let source = fs::read_to_string(&entrypoint).map_err(|error| error.to_string())?;
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (tx, rx) = mpsc::sync_channel(64);
        let id = manifest.id.clone();
        let worker_id = id.clone();
        let plane = self.plane.clone();
        let thread = thread::Builder::new()
            .spawn(move || run_package(worker_id, source, rx, ready_tx, plane))
            .map_err(|error| error.to_string())?;
        let descriptors = ready_rx
            .recv()
            .map_err(|_| "extension worker stopped during load".to_owned())??;
        for descriptor in descriptors {
            let command = descriptor.id.clone();
            let sender = tx.clone();
            self.catalog.register_extension(
                &id,
                generation,
                descriptor,
                Arc::new(move |invocation, deadline, cancellation| {
                    let (response, receiver) = mpsc::channel();
                    let work = Invocation {
                        command: command.clone(),
                        invocation,
                        deadline,
                        cancellation,
                        response,
                    };
                    if sender.try_send(work).is_err() {
                        let (fallback, receiver) = mpsc::channel();
                        let _ = fallback.send(CommandOutcome::Failed {
                            code: "extension_busy".to_owned(),
                            message: "extension command queue is unavailable".to_owned(),
                        });
                        return receiver;
                    }
                    receiver
                }),
            )?;
        }
        self.workers.push(PackageWorker {
            id,
            generation,
            thread: Some(thread),
        });
        Ok(())
    }

    fn clear_workers(&mut self) {
        for worker in &mut self.workers {
            self.catalog
                .remove_extension_generation(&worker.id, worker.generation);
        }
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
        self.workers.clear();
    }
}

impl Drop for CommandExtensionHost {
    fn drop(&mut self) {
        self.clear_workers();
    }
}

fn package_fingerprint(root: &Path) -> u64 {
    let mut hasher = DefaultHasher::new();
    let Ok(entries) = fs::read_dir(root) else {
        return hasher.finish();
    };
    let mut packages = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();
    packages.sort();
    for package in packages {
        package.hash(&mut hasher);
        let manifest_path = package.join("extension.json");
        let Ok(manifest_bytes) = fs::read(&manifest_path) else {
            continue;
        };
        manifest_bytes.hash(&mut hasher);
        if let Ok(manifest) = serde_json::from_slice::<PackageManifest>(&manifest_bytes)
            && let Ok(entrypoint) = safe_entrypoint(&package, &manifest.entrypoint)
            && let Ok(source) = fs::read(entrypoint)
        {
            source.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn run_package(
    id: String,
    source: String,
    rx: mpsc::Receiver<Invocation>,
    ready: mpsc::SyncSender<Result<Vec<CommandDescriptor>, String>>,
    plane: ControlPlane,
) {
    let lua = Lua::new();
    let handlers = Arc::new(std::sync::Mutex::new(BTreeMap::<String, RegistryKey>::new()));
    let descriptors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let setup = install_host_interface(
        &lua,
        &id,
        Arc::clone(&handlers),
        Arc::clone(&descriptors),
        plane,
    )
    .and_then(|()| lua.load(&source).set_name(&id).exec());
    if let Err(error) = setup {
        let _ = ready.send(Err(error.to_string()));
        return;
    }
    let registered = descriptors
        .lock()
        .map(|mut descriptors| std::mem::take(&mut *descriptors))
        .map_err(|_| "extension descriptor lock poisoned".to_owned());
    if ready.send(registered).is_err() {
        return;
    }
    while let Ok(work) = rx.recv() {
        let outcome = invoke_handler(&lua, &handlers, work);
        let _ = outcome.0.send(outcome.1);
    }
}

fn install_host_interface(
    lua: &Lua,
    id: &str,
    handlers: Arc<std::sync::Mutex<BTreeMap<String, RegistryKey>>>,
    descriptors: Arc<std::sync::Mutex<Vec<CommandDescriptor>>>,
    plane: ControlPlane,
) -> mlua::Result<()> {
    let bootty = lua.create_table()?;
    let commands = lua.create_table()?;
    let package = id.to_owned();
    commands.set(
        "register",
        lua.create_function(move |lua, (spec, handler): (Table, Function)| {
            let descriptor = descriptor_from_table(&package, &spec)?;
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
    bootty.set("commands", commands)?;

    let events = lua.create_table()?;
    let event_package = id.to_owned();
    let event_plane = plane.clone();
    events.set(
        "register",
        lua.create_function(move |_, topic: String| {
            event_plane
                .register_extension_topic(&event_package, &topic)
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    let publish_package = id.to_owned();
    events.set(
        "publish",
        lua.create_function(move |_, (topic, payload): (String, LuaValue)| {
            let payload = lua_value(payload, 0)?;
            plane
                .publish_extension_event(&publish_package, &topic, payload)
                .map_err(mlua::Error::runtime)
        })?,
    )?;
    bootty.set("events", events)?;
    lua.globals().set("bootty", bootty)
}

fn invoke_handler(
    lua: &Lua,
    handlers: &std::sync::Mutex<BTreeMap<String, RegistryKey>>,
    work: Invocation,
) -> (mpsc::Sender<CommandOutcome>, CommandOutcome) {
    let outcome = if work.cancellation.is_cancelled() {
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
            Err(message) => CommandOutcome::Failed {
                code: "extension_failed".to_owned(),
                message,
            },
        }
    };
    (work.response, outcome)
}

fn descriptor_from_table(package: &str, spec: &Table) -> mlua::Result<CommandDescriptor> {
    let id: String = spec.get("id")?;
    if !id.starts_with(package) || !id[package.len()..].starts_with('.') {
        return Err(mlua::Error::runtime(
            "extension command must be namespaced by its package",
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

fn validate_package_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("extension package id is invalid".to_owned());
    }
    Ok(())
}

fn safe_entrypoint(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err("extension entrypoint must stay inside its package".to_owned());
    }
    Ok(root.join(relative))
}
