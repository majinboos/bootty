use std::hash::{Hash, Hasher};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    rc::Rc,
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use rustix::fs::{self as unix_fs, AtFlags, Mode, OFlags};

#[cfg(any(target_os = "macos", target_os = "linux"))]
use rustix::fs::RenameFlags;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use mlua::{Function, Lua, Table, Value as LuaValue, VmState};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    automation::{
        AutomationError, AutomationHub, EventDelivery, EventPublication, MetadataPublication,
        OwnerIdentity, TaskStatus,
    },
    commands::{
        Caller, CommandDescriptor, CommandInvocation, CommandOutcome, CommandTarget,
        ExtensionCommandRegistry, ResourceKind,
    },
    mux::controller::CommandCancellation,
};

pub const EXTENSION_COMMAND_QUEUE_LIMIT: usize = 64;
pub const EXTENSION_COMMAND_WORKERS: usize = 4;
pub const EXTENSION_LOG_LIMIT: usize = 256;
pub const EXTENSION_LOG_BYTES: usize = 128 * 1024;
pub const EXTENSION_FILE_BYTES: usize = 512 * 1024;
pub const EXTENSION_LIFECYCLE_DEDUP_LIMIT: usize = 256;
pub const EXTENSION_LIFECYCLE_PENDING_LIMIT: usize = 64;
pub const EXTENSION_PROCESS_LINES: usize = 256;
pub const EXTENSION_PROCESS_BYTES: usize = 128 * 1024;
pub const EXTENSION_PROCESS_LIMIT: usize = 16;
const EXTENSION_LUA_LOAD_TIMEOUT: Duration = Duration::from_millis(500);
const EXTENSION_LUA_CALL_POLL: Duration = Duration::from_millis(8);
pub const EXTENSION_SURFACE_LIMIT: usize = 64;
pub const EXTENSION_STORAGE_RECORD_LIMIT: usize = 256;
pub const EXTENSION_CLEANUP_RETRY_LIMIT: usize = 256;

#[cfg(any(target_os = "macos", target_os = "linux"))]
static NEXT_EXTENSION_FILE_TEMP: AtomicU64 = AtomicU64::new(1);
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
type ProcessSpawnHook = Arc<dyn Fn(&Arc<ProcessRecord>) + Send + Sync + 'static>;

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static FILE_TRANSACTION_PRE_COMMIT: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static FILE_TRANSACTION_POST_EXCHANGE: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static FILE_TRANSACTION_PRE_ROLLBACK: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static FILE_TRANSACTION_TEST_SERIAL: Mutex<()> = Mutex::new(());
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static PROCESS_SPAWN_BEFORE_COMMIT: Mutex<Option<ProcessSpawnHook>> = Mutex::new(None);
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
static PROCESS_SPAWN_TEST_SERIAL: Mutex<()> = Mutex::new(());
#[cfg(test)]
static TASK_START_BEFORE_COMMIT: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
#[cfg(test)]
static TASK_START_TEST_SERIAL: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtensionPackageId(pub String);

impl ExtensionPackageId {
    pub fn new(value: impl Into<String>) -> Result<Self, ExtensionError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || value.split('.').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            })
        {
            return Err(ExtensionError::new(
                "invalid_extension_id",
                "invalid extension package id",
            ));
        }
        Ok(Self(value))
    }
}

impl AsRef<str> for ExtensionPackageId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPackageManifest {
    #[serde(alias = "extension_id")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub entrypoint: Option<String>,
    #[serde(default)]
    pub storage_namespace: Option<String>,
    #[serde(default)]
    pub default_enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPackageInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub generation: u64,
    pub enabled: bool,
    pub linked: bool,
    pub source: Option<String>,
    pub commands: Vec<String>,
    pub events: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ExtensionGeneration {
    pub extension_id: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionLogEntry {
    pub timestamp_ms: u64,
    pub level: String,
    pub message: String,
    pub generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBatchRollbackError {
    pub path: String,
    pub code: String,
    pub message: String,
    pub conflict: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBatchApplyError {
    pub original_code: String,
    pub original_message: String,
    pub rolled_back: bool,
    pub rollback_errors: Vec<FileBatchRollbackError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionError {
    pub code: String,
    pub message: String,
    pub details: Option<Box<FileBatchApplyError>>,
}

impl ExtensionError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    fn file_batch_failure(original: Self, rollback_errors: Vec<FileBatchRollbackError>) -> Self {
        let rolled_back = rollback_errors.is_empty();
        let message = if rolled_back {
            format!(
                "{}: {}; all applied file actions were rolled back",
                original.code, original.message
            )
        } else {
            format!(
                "{}: {}; file batch rollback reported {} conflict(s)",
                original.code,
                original.message,
                rollback_errors.len()
            )
        };
        Self {
            code: "file_batch_apply_failed".to_owned(),
            message,
            details: Some(Box::new(FileBatchApplyError {
                original_code: original.code,
                original_message: original.message,
                rolled_back,
                rollback_errors,
            })),
        }
    }

    fn outcome(self) -> CommandOutcome {
        CommandOutcome::Failed {
            code: self.code,
            message: self.message,
        }
    }
}

impl std::fmt::Display for ExtensionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ExtensionError {}

pub type ExtensionCommandHandler =
    Arc<dyn Fn(ExtensionCommandContext) -> Result<Value, ExtensionError> + Send + Sync + 'static>;

#[derive(Clone)]
pub struct ExtensionCommandContext {
    invocation: CommandInvocation,
    deadline: Instant,
    cancellation: CommandCancellation,
    generation_cancellation: CommandCancellation,
    runtime: ExtensionRuntime,
    generation: ExtensionGeneration,
    owner: OwnerIdentity,
}

#[derive(Clone)]
pub struct ExtensionRuntimeCapabilities {
    runtime: ExtensionRuntime,
    generation: ExtensionGeneration,
    owner: OwnerIdentity,
    cancellation: CommandCancellation,
}

impl ExtensionCommandContext {
    #[must_use]
    pub fn invocation(&self) -> &CommandInvocation {
        &self.invocation
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.invocation.arguments
    }

    #[must_use]
    pub fn target(&self) -> Option<&CommandTarget> {
        self.invocation.target.as_ref()
    }

    #[must_use]
    pub fn caller(&self) -> Caller {
        self.invocation.caller
    }

    #[must_use]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancel_requested()
            || self.generation_cancellation.is_cancel_requested()
            || self.runtime.inner.shutdown.load(Ordering::Acquire)
            || Instant::now() >= self.deadline
    }

    pub fn check(&self) -> Result<(), ExtensionError> {
        if self.cancellation.is_cancel_requested()
            || self.generation_cancellation.is_cancel_requested()
            || self.runtime.inner.shutdown.load(Ordering::Acquire)
        {
            return Err(ExtensionError::new(
                "cancelled",
                "extension command was cancelled",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(ExtensionError::new(
                "deadline_exceeded",
                "extension command deadline expired",
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn runtime(&self) -> ExtensionRuntimeCapabilities {
        ExtensionRuntimeCapabilities {
            runtime: self.runtime.clone_internal(),
            generation: self.generation.clone(),
            owner: self.owner.clone(),
            cancellation: self.cancellation.clone(),
        }
    }

    pub fn progress(&self, value: Value) -> Result<(), ExtensionError> {
        self.check()?;
        self.runtime
            .publish_progress(&self.invocation.command, value)
    }
}

impl ExtensionRuntimeCapabilities {
    #[must_use]
    pub fn generation(&self) -> &ExtensionGeneration {
        &self.generation
    }

    pub fn start_task(&self) -> Result<TaskStatus, ExtensionError> {
        self.runtime.start_task(
            &self.generation.extension_id,
            self.generation.generation,
            self.owner.clone(),
            extension_scope(&self.generation.extension_id, self.generation.generation),
            self.cancellation.clone(),
        )
    }

    pub fn task_status(&self, task: &str) -> Result<TaskStatus, ExtensionError> {
        self.runtime.task_status(
            task,
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn cancel_task(&self, task: &str) -> Result<TaskStatus, ExtensionError> {
        self.runtime.cancel_task(
            task,
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn finish_task(&self, task: &str, outcome: &Value) -> Result<(), ExtensionError> {
        self.runtime.finish_task(
            task,
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            outcome,
        )
    }
    pub fn publish_event(
        &self,
        topic: &str,
        payload: Value,
        target: Option<CommandTarget>,
    ) -> Result<u64, ExtensionError> {
        self.runtime.publish_event(
            &self.generation.extension_id,
            self.generation.generation,
            topic,
            extension_scope(&self.generation.extension_id, self.generation.generation),
            payload,
            target,
        )
    }

    pub fn subscribe_event(&self, topic: &str) -> Result<(String, EventDelivery), ExtensionError> {
        self.runtime.subscribe_event(
            &self.generation.extension_id,
            self.generation.generation,
            self.owner.clone(),
            topic,
            extension_scope(&self.generation.extension_id, self.generation.generation),
        )
    }

    pub fn publish_metadata(
        &self,
        namespace: &str,
        key: &str,
        target: Option<CommandTarget>,
        value: Value,
        expires_at_ms: Option<u64>,
        provenance: Value,
    ) -> Result<(), ExtensionError> {
        self.runtime.publish_metadata(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            MetadataPublication::new(
                extension_scope(&self.generation.extension_id, self.generation.generation),
                namespace,
                key,
                target,
                value,
                expires_at_ms,
                provenance,
            ),
        )
    }

    pub fn metadata_get(
        &self,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
    ) -> Result<Option<crate::automation::MetadataRecord>, ExtensionError> {
        self.runtime.metadata_get(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            namespace,
            key,
            target,
        )
    }

    pub fn metadata_clear(
        &self,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
    ) -> Result<(), ExtensionError> {
        self.runtime.metadata_clear(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            namespace,
            key,
            target,
        )
    }

    pub fn metadata_list(&self) -> Result<Vec<crate::automation::MetadataRecord>, ExtensionError> {
        self.runtime.metadata_list(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn log(
        &self,
        level: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), ExtensionError> {
        self.runtime.log(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            level,
            message,
        )
    }

    pub fn logs(&self) -> Result<Vec<ExtensionLogEntry>, ExtensionError> {
        self.runtime.logs(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn storage_get(&self, key: &str) -> Result<Option<Value>, ExtensionError> {
        self.runtime.storage_get(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            key,
        )
    }

    pub fn storage_put(&self, key: &str, value: Value) -> Result<(), ExtensionError> {
        self.runtime.storage_put(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            key,
            value,
        )
    }

    pub fn storage_delete(&self, key: &str) -> Result<(), ExtensionError> {
        self.runtime.storage_delete(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            key,
        )
    }

    pub fn storage_list(&self) -> Result<Vec<String>, ExtensionError> {
        self.runtime.storage_list(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn observe(&self) -> Result<HostObservation, ExtensionError> {
        self.runtime.observe(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
        )
    }

    pub fn replace_observation(&self, observation: HostObservation) -> Result<(), ExtensionError> {
        self.runtime.replace_observation(
            &self.generation.extension_id,
            self.generation.generation,
            &self.owner,
            observation,
        )
    }
}

#[derive(Clone)]
struct ExtensionCommandRecord {
    package: ExtensionPackageId,
    generation: u64,
    handler: ExtensionCommandHandler,
    generation_cancellation: CommandCancellation,
}

struct WorkItem {
    record: ExtensionCommandRecord,
    invocation: CommandInvocation,
    deadline: Instant,
    cancellation: CommandCancellation,
    response: mpsc::Sender<CommandOutcome>,
}

#[derive(Clone, Debug)]
pub struct ExtensionEventRegistration {
    pub topic: String,
    pub generation: ExtensionGeneration,
}

#[derive(Clone, Debug)]
pub struct ExtensionSubscription {
    pub id: String,
    pub topic: String,
    pub scope: String,
    pub generation: ExtensionGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfacePresentation {
    Modal,
    Floating,
    Docked,
    Sidebar,
    Status,
    Edge,
    NativeWindow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceSpec {
    pub id: String,
    pub title: String,
    pub presentation: SurfacePresentation,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceLifecycleEvent {
    pub operation: String,
    pub surface: SurfaceSpec,
    pub generation: ExtensionGeneration,
}

#[derive(Clone, Debug)]
struct SurfaceRecord {
    spec: SurfaceSpec,
    generation: ExtensionGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpec {
    pub executable: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessLine {
    pub stream: String,
    pub line: String,
    pub sequence: u64,
    /// Parsed JSONL payload. A non-JSON line stays visible in `line` and carries
    /// a typed parse error rather than being silently discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct ProcessRecord {
    generation: ExtensionGeneration,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    output: Arc<Mutex<VecDeque<ProcessLine>>>,
    output_bytes: Arc<Mutex<usize>>,
    next_sequence: AtomicU64,
    readers: Mutex<Vec<thread::JoinHandle<()>>>,
}

struct LuaWorker {
    generation: ExtensionGeneration,
    cancellation: CommandCancellation,
    handle: Mutex<Option<thread::JoinHandle<()>>>,
}

impl LuaWorker {
    fn cancel_and_join(&self) {
        self.cancellation.request_cancel();
        join_thread_bounded(&self.handle, Duration::from_millis(250));
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessStatus {
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub generation: ExtensionGeneration,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTransactionPreview {
    pub path: String,
    pub existed: bool,
    pub before_bytes: usize,
    pub after_bytes: usize,
    pub changed: bool,
    pub destructive: bool,
}

#[cfg(unix)]
#[derive(Debug)]
struct FileRootCapability {
    directory: fs::File,
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct FileAccess {
    root: Arc<FileRootCapability>,
    relative: PathBuf,
}

#[cfg(not(unix))]
#[derive(Clone, Debug)]
struct FileAccess;

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileVersion {
    size: u64,
    modified_seconds: i64,
    modified_nanos: i64,
    changed_seconds: i64,
    changed_nanos: i64,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Debug)]
struct FileSnapshot {
    existed: bool,
    identity: Option<FileIdentity>,
    version: Option<FileVersion>,
    bytes: Vec<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileConfirmation {
    /// Opaque host-minted one-use token. Packages must never construct this value.
    pub token: String,
    pub transaction_id: u64,
    pub extension: ExtensionPackageId,
    pub generation: u64,
    pub digest: String,
    pub preview: FileTransactionPreview,
    #[serde(default)]
    pub previews: Vec<FileTransactionPreview>,
}

#[derive(Clone)]
struct FileConfirmationRecord {
    confirmation: FileConfirmation,
    transactions: Vec<FileTransaction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum FileTransactionOperation {
    Write,
    Remove,
}

#[derive(Clone, Debug)]
pub struct FileTransaction {
    runtime: ExtensionRuntime,
    extension: ExtensionPackageId,
    generation: u64,
    confirmation_id: u64,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    root: Arc<FileRootCapability>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    relative: PathBuf,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    parent_identity: Option<FileIdentity>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    snapshot: FileSnapshot,
    contents: Vec<u8>,
    operation: FileTransactionOperation,
    preview: FileTransactionPreview,
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Debug)]
struct FileTransactionCommit {
    transaction: FileTransaction,
    post_snapshot: FileSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostObservation {
    pub topology: Value,
    pub terminals: Value,
    pub metadata: Value,
}

struct PackageState {
    manifest: ExtensionPackageManifest,
    generation: u64,
    enabled: bool,
    linked: bool,
    source: Option<PathBuf>,
    generation_cancellation: CommandCancellation,

    commands: BTreeSet<String>,
    events: BTreeSet<String>,
}

struct DiscoveredPackage {
    info: ExtensionPackageInfo,
    should_load: bool,
}

struct DiscoveryOutcome {
    packages: Vec<DiscoveredPackage>,
    diagnostic: Option<ExtensionError>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PersistedPackageState {
    id: String,
    enabled: bool,
}

struct SubscriptionRecord {
    owner: OwnerIdentity,
    generation: ExtensionGeneration,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExtensionResourceBinding {
    generation: ExtensionGeneration,
    owner: OwnerIdentity,
    scope: String,
}

#[derive(Clone, Debug)]
struct TaskBinding {
    resource: ExtensionResourceBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MetadataBindingKey {
    resource: ExtensionResourceBinding,
    namespace: String,
    key: String,
    target: String,
}

#[derive(Clone, Debug)]
struct MetadataBinding {
    resource: ExtensionResourceBinding,
    namespace: String,
    key: String,
    target: Option<CommandTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct StorageBindingKey {
    resource: ExtensionResourceBinding,
    key: String,
}

struct RuntimeState {
    packages: BTreeMap<String, PackageState>,
    commands: BTreeMap<String, ExtensionCommandRecord>,
    events: BTreeMap<String, ExtensionEventRegistration>,
    subscriptions: BTreeMap<String, SubscriptionRecord>,
    tasks: BTreeMap<String, TaskBinding>,
    surfaces: BTreeMap<String, SurfaceRecord>,
    processes: BTreeMap<String, Arc<ProcessRecord>>,
    process_reservations: BTreeMap<(String, u64), usize>,
    logs: BTreeMap<ExtensionResourceBinding, VecDeque<ExtensionLogEntry>>,
    metadata: BTreeMap<MetadataBindingKey, MetadataBinding>,
    storage: BTreeMap<StorageBindingKey, Value>,
    observation_metadata: BTreeMap<ExtensionResourceBinding, Value>,
    observations: HostObservation,
}
#[derive(Clone, Debug)]
struct LifecyclePublication {
    extension_id: String,
    generation: u64,
    operation: String,
    snapshot: Value,
}

#[derive(Default)]
struct LifecyclePublicationState {
    pending: VecDeque<LifecyclePublication>,
    published: VecDeque<(String, u64, String)>,
}

impl LifecyclePublicationState {
    fn mark_published(&mut self, publication: &LifecyclePublication) {
        let key = (
            publication.extension_id.clone(),
            publication.generation,
            publication.operation.clone(),
        );
        if !self.published.contains(&key) {
            self.published.push_back(key);
        }
        while self.published.len() > EXTENSION_LIFECYCLE_DEDUP_LIMIT {
            let Some(index) = self.published.iter().position(|key| {
                !self.pending.iter().any(|pending| {
                    pending.extension_id == key.0
                        && pending.generation == key.1
                        && pending.operation == key.2
                })
            }) else {
                break;
            };
            self.published.remove(index);
        }
    }
}

struct RuntimeInner {
    next_file_confirmation: AtomicU64,
    file_confirmations: Mutex<BTreeMap<String, FileConfirmationRecord>>,
    state: RwLock<RuntimeState>,
    commands: ExtensionCommandRegistry,
    automation: AutomationHub,
    work_tx: SyncSender<WorkItem>,
    shutdown: AtomicBool,
    storage_root: RwLock<Option<PathBuf>>,
    file_roots: RwLock<Vec<PathBuf>>,
    file_transaction_lock: Mutex<()>,
    next_process: AtomicU64,
    owner: OwnerIdentity,
    event_reservations: Mutex<BTreeMap<String, ExtensionGeneration>>,
    command_reservations: Mutex<BTreeMap<String, ExtensionGeneration>>,
    task_reservations: Mutex<BTreeMap<String, ExtensionGeneration>>,
    lifecycle_publications: Mutex<LifecyclePublicationState>,
    lifecycle_operations: Mutex<()>,
    cleanup_retries: Mutex<BTreeMap<(String, u64), u8>>,
    lua_workers: Mutex<Vec<Arc<LuaWorker>>>,
    external_owners: AtomicUsize,
}
/// A public runtime owner. Internal worker/context handles are non-owning and
/// cannot delay shutdown after the last public owner is dropped.
pub struct ExtensionRuntime {
    inner: Arc<RuntimeInner>,
    owner: bool,
}
impl std::fmt::Debug for ExtensionRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionRuntime")
            .field("owner", &self.owner)
            .finish()
    }
}

impl Clone for ExtensionRuntime {
    fn clone(&self) -> Self {
        if self.owner {
            self.inner.external_owners.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            inner: Arc::clone(&self.inner),
            owner: self.owner,
        }
    }
}
impl ExtensionRuntime {
    #[must_use]
    pub fn new(automation: AutomationHub) -> Self {
        let (work_tx, work_rx) = mpsc::sync_channel(EXTENSION_COMMAND_QUEUE_LIMIT);
        let inner = Arc::new(RuntimeInner {
            state: RwLock::new(RuntimeState {
                packages: BTreeMap::new(),
                commands: BTreeMap::new(),
                events: BTreeMap::new(),
                subscriptions: BTreeMap::new(),
                tasks: BTreeMap::new(),
                surfaces: BTreeMap::new(),
                processes: BTreeMap::new(),
                process_reservations: BTreeMap::new(),
                logs: BTreeMap::new(),
                metadata: BTreeMap::new(),
                storage: BTreeMap::new(),
                observation_metadata: BTreeMap::new(),
                observations: HostObservation {
                    topology: Value::Null,
                    terminals: Value::Null,
                    metadata: Value::Null,
                },
            }),
            commands: ExtensionCommandRegistry::new(),
            automation,
            work_tx,
            shutdown: AtomicBool::new(false),
            storage_root: RwLock::new(None),
            file_confirmations: Mutex::new(BTreeMap::new()),
            file_transaction_lock: Mutex::new(()),
            next_file_confirmation: AtomicU64::new(1),
            file_roots: RwLock::new(Vec::new()),
            next_process: AtomicU64::new(1),
            owner: OwnerIdentity::current_process()
                .unwrap_or_else(|| OwnerIdentity::new(std::process::id(), 0)),
            event_reservations: Mutex::new(BTreeMap::new()),
            command_reservations: Mutex::new(BTreeMap::new()),
            task_reservations: Mutex::new(BTreeMap::new()),
            lifecycle_publications: Mutex::new(LifecyclePublicationState::default()),
            lifecycle_operations: Mutex::new(()),
            cleanup_retries: Mutex::new(BTreeMap::new()),
            lua_workers: Mutex::new(Vec::new()),
            external_owners: AtomicUsize::new(1),
        });
        let receiver = Arc::new(Mutex::new(work_rx));
        let weak = Arc::downgrade(&inner);
        for index in 0..EXTENSION_COMMAND_WORKERS {
            let receiver = Arc::clone(&receiver);
            let weak = Weak::clone(&weak);
            let _ = thread::Builder::new()
                .name(format!("bootty-extension-command-{index}"))
                .spawn(move || {
                    loop {
                        let item = match receiver.lock().ok().and_then(|rx| rx.recv().ok()) {
                            Some(item) => item,
                            None => break,
                        };
                        let Some(inner) = weak.upgrade() else {
                            break;
                        };
                        let runtime = ExtensionRuntime::internal(inner);
                        let _ = runtime.execute(item);
                        if runtime.inner.shutdown.load(Ordering::Acquire) {
                            break;
                        }
                    }
                });
        }
        Self { inner, owner: true }
    }

    fn internal(inner: Arc<RuntimeInner>) -> Self {
        Self {
            inner,
            owner: false,
        }
    }

    fn clone_internal(&self) -> Self {
        Self::internal(Arc::clone(&self.inner))
    }

    #[must_use]
    pub fn command_registry(&self) -> crate::commands::CommandRegistry {
        crate::commands::CommandRegistry::core()
            .with_extension_registry(self.inner.commands.clone())
    }

    pub fn set_storage_root(&self, root: impl Into<PathBuf>) -> Result<(), ExtensionError> {
        let root = root.into();
        fs::create_dir_all(&root)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        *self
            .inner
            .storage_root
            .write()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))? =
            Some(root.clone());
        self.restore_package_states(&root);
        Ok(())
    }

    fn restore_package_states(&self, root: &Path) {
        let Ok(bytes) = fs::read(root.join("packages.json")) else {
            return;
        };
        let Ok(saved) = serde_json::from_slice::<Vec<PersistedPackageState>>(&bytes) else {
            return;
        };
        if let Ok(mut state) = self.inner.state.write() {
            for package in saved {
                if let Some(current) = state.packages.get_mut(&package.id) {
                    current.enabled = package.enabled;
                }
            }
        }
    }

    fn persist_package_states_with_override(
        &self,
        override_state: Option<(&str, bool)>,
    ) -> Result<(), ExtensionError> {
        let root = self
            .inner
            .storage_root
            .read()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))?
            .clone();
        let Some(root) = root else {
            return Ok(());
        };
        let saved = self
            .inner
            .state
            .read()
            .map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?
            .packages
            .values()
            .map(|package| {
                let enabled = override_state
                    .filter(|(id, _)| *id == package.manifest.id.as_str())
                    .map_or(package.enabled, |(_, enabled)| enabled);
                PersistedPackageState {
                    id: package.manifest.id.clone(),
                    enabled,
                }
            })
            .collect::<Vec<_>>();
        let mut saved = saved;
        saved.sort_by(|left, right| left.id.cmp(&right.id));
        let bytes = serde_json::to_vec(&saved)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        atomic_write(&root.join("packages.json"), &bytes)?;
        #[cfg(unix)]
        if let Ok(metadata) = fs::metadata(root.join("packages.json")) {
            let mut permissions = metadata.permissions();
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o600);
            let _ = fs::set_permissions(root.join("packages.json"), permissions);
        }
        Ok(())
    }
    fn persisted_enabled(&self, id: &str) -> Option<bool> {
        let root = self.inner.storage_root.read().ok()?.clone()?;
        let bytes = fs::read(root.join("packages.json")).ok()?;
        serde_json::from_slice::<Vec<PersistedPackageState>>(&bytes)
            .ok()?
            .into_iter()
            .find(|package| package.id == id)
            .map(|package| package.enabled)
    }

    pub fn set_file_roots(&self, roots: impl IntoIterator<Item = PathBuf>) {
        if let Ok(mut current) = self.inner.file_roots.write() {
            *current = roots.into_iter().collect();
        }
    }

    pub fn install(
        &self,
        manifest: ExtensionPackageManifest,
    ) -> Result<ExtensionPackageInfo, ExtensionError> {
        let id = ExtensionPackageId::new(manifest.id.clone())?;
        let restored_enabled = self.persisted_enabled(&manifest.id);
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        if state.packages.contains_key(id.as_ref()) {
            return Err(ExtensionError::new(
                "extension_exists",
                "extension is already installed",
            ));
        }
        state.packages.insert(
            id.0.clone(),
            PackageState {
                manifest: manifest.clone(),
                generation: 1,
                enabled: restored_enabled.unwrap_or(manifest.default_enabled),
                linked: false,
                source: None,
                generation_cancellation: CommandCancellation::new(),
                commands: BTreeSet::new(),
                events: BTreeSet::new(),
            },
        );
        Ok(package_info(
            state.packages.get(id.as_ref()).expect("package inserted"),
        ))
    }

    pub fn discover(&self, root: &Path) -> Result<Vec<ExtensionPackageInfo>, ExtensionError> {
        let outcome = self.discover_root(root)?;
        if let Some(error) = outcome.diagnostic {
            return Err(error);
        }
        Ok(outcome
            .packages
            .into_iter()
            .map(|package| package.info)
            .collect())
    }

    fn discover_root(&self, root: &Path) -> Result<DiscoveryOutcome, ExtensionError> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DiscoveryOutcome {
                    packages: Vec::new(),
                    diagnostic: None,
                });
            }
            Err(error) => {
                return Err(ExtensionError::new(
                    "extension_discovery_failed",
                    error.to_string(),
                ));
            }
        };
        let mut discovered = Vec::new();
        let mut diagnostic = None;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    if diagnostic.is_none() {
                        diagnostic = Some(ExtensionError::new(
                            "extension_discovery_failed",
                            error.to_string(),
                        ));
                    }
                    continue;
                }
            };
            let path = entry.path();
            let manifest_path = if path.is_dir() {
                let extension = path.join("extension.json");
                if extension.exists() {
                    extension
                } else {
                    path.join("manifest.json")
                }
            } else {
                continue;
            };
            let Ok(bytes) = fs::read(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_slice::<ExtensionPackageManifest>(&bytes) else {
                continue;
            };
            let id = manifest.id.clone();
            let existing = match self.package(&id) {
                Ok(info) => Some(info),
                Err(error) if error.code == "unknown_extension" => None,
                Err(error) => {
                    if diagnostic.is_none() {
                        diagnostic = Some(error);
                    }
                    continue;
                }
            };
            let (info, should_load) = if let Some(info) = existing {
                if let Some(source) = info.source.as_deref() {
                    let incoming = Self::canonical_source_path(&path);
                    let existing = Self::canonical_source_path(Path::new(source));
                    if incoming != existing {
                        if diagnostic.is_none() {
                            diagnostic = Some(ExtensionError::new(
                                "extension_source_conflict",
                                format!(
                                    "extension {id} is already loaded from {}; ignoring {}",
                                    existing.display(),
                                    incoming.display()
                                ),
                            ));
                        }
                        continue;
                    }
                    let should_load = !self.generation_has_worker(&id, info.generation);
                    (info, should_load)
                } else {
                    let info = match self.link(&id, &path) {
                        Ok(info) => info,
                        Err(error) => {
                            if diagnostic.is_none() {
                                diagnostic = Some(error);
                            }
                            continue;
                        }
                    };
                    (info, true)
                }
            } else {
                if let Err(error) = self.install(manifest) {
                    if diagnostic.is_none() {
                        diagnostic = Some(error);
                    }
                    continue;
                }
                let info = match self.link(&id, &path) {
                    Ok(info) => info,
                    Err(error) => {
                        if diagnostic.is_none() {
                            diagnostic = Some(error);
                        }
                        continue;
                    }
                };
                (info, true)
            };
            discovered.push(DiscoveredPackage { info, should_load });
        }
        discovered.sort_by(|left, right| left.info.id.cmp(&right.info.id));
        Ok(DiscoveryOutcome {
            packages: discovered,
            diagnostic,
        })
    }

    fn canonical_source_path(path: &Path) -> PathBuf {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    }

    pub fn load_linked_package(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let _operation = self.inner.lifecycle_operations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle operation lock poisoned")
        })?;
        self.load_linked_package_locked(id)
    }

    fn load_linked_package_locked(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let (generation, enabled, linked, source, entrypoint) = {
            let state = self.inner.state.read().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            (
                package.generation,
                package.enabled,
                package.linked,
                package.source.clone(),
                package.manifest.entrypoint.clone(),
            )
        };
        if !enabled {
            return Err(ExtensionError::new(
                "extension_disabled",
                "extension is disabled",
            ));
        }
        if self.generation_has_worker(id, generation) {
            return self.package(id);
        }
        if !linked {
            return Err(ExtensionError::new(
                "extension_unlinked",
                "extension source is not linked",
            ));
        }
        let root = source
            .ok_or_else(|| {
                ExtensionError::new("extension_source_missing", "extension source is not linked")
            })?
            .canonicalize()
            .map_err(|error| ExtensionError::new("extension_source_missing", error.to_string()))?;
        let entrypoint = entrypoint.unwrap_or_else(|| "main.luau".to_owned());
        let relative = PathBuf::from(&entrypoint);
        if relative
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(ExtensionError::new(
                "extension_entrypoint_denied",
                "entrypoint traversal denied",
            ));
        }
        let path = root.join(relative).canonicalize().map_err(|error| {
            ExtensionError::new("extension_entrypoint_missing", error.to_string())
        })?;
        if !path.starts_with(&root) {
            return Err(ExtensionError::new(
                "extension_entrypoint_denied",
                "entrypoint escapes package root",
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            ExtensionError::new("extension_entrypoint_read_failed", error.to_string())
        })?;
        if bytes.len() > EXTENSION_FILE_BYTES {
            return Err(ExtensionError::new(
                "extension_entrypoint_too_large",
                "entrypoint exceeds host limit",
            ));
        }
        let source = String::from_utf8(bytes).map_err(|_| {
            ExtensionError::new("extension_entrypoint_invalid", "entrypoint is not UTF-8")
        })?;
        self.ensure_lifecycle_capacity(1)?;
        register_luau_package(self, id, generation, &source)?;
        let operation = if generation == 1 {
            "loaded"
        } else {
            "reloaded"
        };
        let snapshot = self.lifecycle_snapshot(None)?;
        if let Err(error) = self.publish_lifecycle(id, generation, operation, snapshot) {
            self.remove_pending_lifecycle(id, generation, operation);
            let cleanup_error = self.cleanup_generation(id, generation, None).err();
            return Err(cleanup_error.unwrap_or(error));
        }
        self.package(id)
    }
    fn lifecycle_snapshot(&self, excluded: Option<(&str, u64)>) -> Result<Value, ExtensionError> {
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        let modules = state
            .packages
            .iter()
            .filter(|(id, package)| {
                package.enabled && excluded != Some((id.as_str(), package.generation))
            })
            .map(|(id, package)| {
                json!({
                    "extension_id": id,
                    "generation": package.generation,
                })
            })
            .collect::<Vec<_>>();
        let commands = state
            .commands
            .iter()
            .filter(|(_, command)| {
                state
                    .packages
                    .get(command.package.as_ref())
                    .is_some_and(|package| {
                        package.enabled
                            && package.generation == command.generation
                            && excluded != Some((command.package.as_ref(), command.generation))
                    })
            })
            .map(|(id, command)| {
                json!({
                    "id": id,
                    "extension_id": command.package.as_ref(),
                    "generation": command.generation,
                })
            })
            .collect::<Vec<_>>();
        let events = state
            .events
            .iter()
            .filter(|(_, event)| {
                state
                    .packages
                    .get(&event.generation.extension_id)
                    .is_some_and(|package| {
                        package.enabled
                            && package.generation == event.generation.generation
                            && excluded
                                != Some((
                                    event.generation.extension_id.as_str(),
                                    event.generation.generation,
                                ))
                    })
            })
            .map(|(topic, event)| {
                json!({
                    "topic": topic,
                    "extension_id": event.generation.extension_id,
                    "generation": event.generation.generation,
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({
            "modules": modules,
            "commands": commands,
            "events": events,
        }))
    }

    fn flush_lifecycle_publications(&self) -> Result<(), ExtensionError> {
        let Some(scope) = self.inner.automation.instance_scope() else {
            return Ok(());
        };
        let mut lifecycle = self.inner.lifecycle_publications.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle publication lock poisoned")
        })?;
        while let Some(publication) = lifecycle.pending.front().cloned() {
            let result = self.inner.automation.publish_event_with_snapshot_source(
                EventPublication::new(
                    scope.clone(),
                    "extension.reloaded",
                    json!({
                        "extension_id": publication.extension_id,
                        "generation": publication.generation,
                    }),
                    None,
                    json!({
                        "extension_id": publication.extension_id,
                        "generation": publication.generation,
                        "operation": publication.operation,
                        "snapshot": publication.snapshot.clone(),
                    }),
                ),
                "runtime",
                publication.snapshot.clone(),
            );
            match result {
                Ok(_) => {
                    let published = lifecycle
                        .pending
                        .pop_front()
                        .expect("pending lifecycle publication");
                    lifecycle.mark_published(&published);
                }
                Err(error) => return Err(automation_error(error)),
            }
        }
        Ok(())
    }
    fn ensure_lifecycle_capacity(&self, required: usize) -> Result<(), ExtensionError> {
        let lifecycle = self.inner.lifecycle_publications.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle publication lock poisoned")
        })?;
        if lifecycle.pending.len().saturating_add(required) > EXTENSION_LIFECYCLE_PENDING_LIMIT {
            return Err(ExtensionError::new(
                "lifecycle_backpressure",
                "lifecycle publication retry queue is full",
            ));
        }
        Ok(())
    }
    fn remove_pending_lifecycle(&self, id: &str, generation: u64, operation: &str) {
        if let Ok(mut lifecycle) = self.inner.lifecycle_publications.lock() {
            lifecycle.pending.retain(|pending| {
                pending.extension_id != id
                    || pending.generation != generation
                    || pending.operation != operation
            });
        }
    }
    fn pending_lifecycle_exists(&self, id: &str, generation: u64, operation: &str) -> bool {
        self.inner
            .lifecycle_publications
            .lock()
            .map(|lifecycle| {
                lifecycle.pending.iter().any(|pending| {
                    pending.extension_id == id
                        && pending.generation == generation
                        && pending.operation == operation
                })
            })
            .unwrap_or(false)
    }

    pub fn discover_and_load(
        &self,
        root: &Path,
    ) -> Result<Vec<ExtensionPackageInfo>, ExtensionError> {
        let _operation = self.inner.lifecycle_operations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle operation lock poisoned")
        })?;
        let outcome = self.discover_root(root)?;
        let mut loaded = Vec::new();
        let mut diagnostic = outcome.diagnostic;
        for package in outcome.packages {
            if !package.info.enabled {
                continue;
            }
            if !package.should_load {
                loaded.push(package.info);
                continue;
            }
            match self.load_linked_package_locked(&package.info.id) {
                Ok(info) => loaded.push(info),
                Err(error) => {
                    if diagnostic.is_none() {
                        diagnostic = Some(error);
                    }
                }
            }
        }
        if let Some(error) = diagnostic {
            return Err(error);
        }
        Ok(loaded)
    }

    fn generation_has_worker(&self, id: &str, generation: u64) -> bool {
        self.inner.lua_workers.lock().is_ok_and(|workers| {
            workers.iter().any(|worker| {
                worker.generation.extension_id == id && worker.generation.generation == generation
            })
        })
    }

    fn refresh_extension_scopes(&self) {
        let scopes = self
            .inner
            .state
            .read()
            .map(|state| {
                state
                    .packages
                    .values()
                    .filter(|package| package.enabled)
                    .map(|package| extension_scope(&package.manifest.id, package.generation))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.inner
            .automation
            .events()
            .replace_live_extension_scopes(scopes);
    }

    fn publish_lifecycle(
        &self,
        id: &str,
        generation: u64,
        operation: &str,
        snapshot: Value,
    ) -> Result<(), ExtensionError> {
        self.refresh_extension_scopes();
        let publication = LifecyclePublication {
            extension_id: id.to_owned(),
            generation,
            operation: operation.to_owned(),
            snapshot,
        };
        if let Err(error) = self.flush_lifecycle_publications() {
            let mut lifecycle = self.inner.lifecycle_publications.lock().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "lifecycle publication lock poisoned")
            })?;
            let exists = lifecycle.pending.iter().any(|pending| {
                pending.extension_id == publication.extension_id
                    && pending.generation == publication.generation
                    && pending.operation == publication.operation
            });
            if !exists {
                if lifecycle.pending.len() >= EXTENSION_LIFECYCLE_PENDING_LIMIT {
                    return Err(ExtensionError::new(
                        "lifecycle_backpressure",
                        "lifecycle publication retry queue is full",
                    ));
                }
                lifecycle.pending.push_back(publication);
            }
            return Err(error);
        }
        if self.inner.automation.instance_scope().is_none() {
            let mut lifecycle = self.inner.lifecycle_publications.lock().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "lifecycle publication lock poisoned")
            })?;
            let key = (
                publication.extension_id.clone(),
                publication.generation,
                publication.operation.clone(),
            );
            if lifecycle.published.contains(&key) {
                return Ok(());
            }
            if let Some(pending) = lifecycle.pending.iter_mut().find(|pending| {
                pending.extension_id == publication.extension_id
                    && pending.generation == publication.generation
                    && pending.operation == publication.operation
            }) {
                pending.snapshot = publication.snapshot;
            } else {
                if lifecycle.pending.len() >= EXTENSION_LIFECYCLE_PENDING_LIMIT {
                    return Err(ExtensionError::new(
                        "lifecycle_backpressure",
                        "lifecycle publication retry queue is full",
                    ));
                }
                lifecycle.pending.push_back(publication);
            }
            return Ok(());
        }
        let scope = self
            .inner
            .automation
            .instance_scope()
            .expect("scope checked above");
        let mut lifecycle = self.inner.lifecycle_publications.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle publication lock poisoned")
        })?;
        let key = (
            publication.extension_id.clone(),
            publication.generation,
            publication.operation.clone(),
        );
        if lifecycle.published.contains(&key) {
            return Ok(());
        }
        let result = self.inner.automation.publish_event_with_snapshot_source(
            EventPublication::new(
                scope,
                "extension.reloaded",
                json!({
                    "extension_id": publication.extension_id,
                    "generation": publication.generation,
                }),
                None,
                json!({
                    "extension_id": publication.extension_id,
                    "generation": publication.generation,
                    "operation": publication.operation,
                    "snapshot": publication.snapshot.clone(),
                }),
            ),
            "runtime",
            publication.snapshot.clone(),
        );
        match result {
            Ok(_) => {
                lifecycle.mark_published(&publication);
                Ok(())
            }
            Err(error) => {
                if lifecycle.pending.len() >= EXTENSION_LIFECYCLE_PENDING_LIMIT {
                    return Err(ExtensionError::new(
                        "lifecycle_backpressure",
                        "lifecycle publication retry queue is full",
                    ));
                }
                lifecycle.pending.push_back(publication);
                Err(automation_error(error))
            }
        }
    }

    #[must_use]
    pub fn has_package(&self, id: &str) -> bool {
        self.inner
            .state
            .read()
            .is_ok_and(|state| state.packages.contains_key(id))
    }

    fn package_source_root(&self, id: &str) -> Result<PathBuf, ExtensionError> {
        self.inner
            .state
            .read()
            .map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?
            .packages
            .get(id)
            .and_then(|package| package.source.clone())
            .ok_or_else(|| {
                ExtensionError::new("extension_source_missing", "extension source is not linked")
            })
    }
    pub fn package(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        state
            .packages
            .get(id)
            .map(package_info)
            .ok_or_else(|| ExtensionError::new("unknown_extension", "extension is not installed"))
    }

    pub fn list(&self) -> Vec<ExtensionPackageInfo> {
        let mut packages = self
            .inner
            .state
            .read()
            .map(|state| {
                state
                    .packages
                    .values()
                    .map(package_info)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        packages.sort_by(|left, right| left.id.cmp(&right.id));
        packages
    }

    pub fn link(&self, id: &str, source: &Path) -> Result<ExtensionPackageInfo, ExtensionError> {
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        let package = state.packages.get_mut(id).ok_or_else(|| {
            ExtensionError::new("unknown_extension", "extension is not installed")
        })?;
        package.linked = true;
        package.source = Some(source.to_path_buf());
        Ok(package_info(package))
    }

    pub fn enable(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let _operation = self.inner.lifecycle_operations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle operation lock poisoned")
        })?;
        self.retry_cleanup_tombstones();
        if self.cleanup_pending(id) {
            return Err(ExtensionError::new(
                "cleanup_pending",
                "extension resources are awaiting authoritative cleanup",
            ));
        }
        let _ = self.flush_lifecycle_publications();
        self.ensure_lifecycle_capacity(1)?;
        let (already_enabled, linked) = {
            let state = self.inner.state.read().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            (package.enabled, package.linked)
        };
        if already_enabled {
            self.persist_package_states_with_override(Some((id, true)))?;
            return self.package(id);
        }
        self.persist_package_states_with_override(Some((id, true)))?;
        {
            let mut state = self.inner.state.write().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get_mut(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            package.enabled = true;
            package.generation_cancellation = CommandCancellation::new();
        }
        if linked {
            match self.load_linked_package_locked(id) {
                Ok(info) => Ok(info),
                Err(error) => {
                    let rollback = self.persist_package_states_with_override(Some((id, false)));
                    if let Ok(mut state) = self.inner.state.write()
                        && let Some(package) = state.packages.get_mut(id)
                    {
                        package.enabled = false;
                        package.generation_cancellation.cancel();
                    }
                    rollback?;
                    Err(error)
                }
            }
        } else {
            let generation = self.package(id)?.generation;
            let snapshot = self.lifecycle_snapshot(None)?;
            if let Err(error) =
                self.publish_lifecycle(id, generation, "enabled_pending_source", snapshot)
                && !self.pending_lifecycle_exists(id, generation, "enabled_pending_source")
            {
                return Err(error);
            }
            self.package(id)
        }
    }

    pub fn disable(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let _operation = self.inner.lifecycle_operations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle operation lock poisoned")
        })?;
        self.retry_cleanup_tombstones();
        let _ = self.flush_lifecycle_publications();
        self.ensure_lifecycle_capacity(1)?;
        let (generation, cancellation, enabled) = {
            let state = self.inner.state.read().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            (
                package.generation,
                package.generation_cancellation.clone(),
                package.enabled,
            )
        };
        if !enabled {
            self.persist_package_states_with_override(Some((id, false)))?;
            return self.package(id);
        }
        self.ensure_cleanup_retry_capacity(id, generation)?;
        self.persist_package_states_with_override(Some((id, false)))?;
        cancellation.cancel();
        let cleanup_error = self.cleanup_generation(id, generation, None).err();
        let info = {
            let mut state = self.inner.state.write().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get_mut(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            package.enabled = false;
            package_info(package)
        };
        let snapshot = self.lifecycle_snapshot(Some((id, generation)))?;
        if let Err(error) = self.publish_lifecycle(id, generation, "disabled", snapshot)
            && !self.pending_lifecycle_exists(id, generation, "disabled")
        {
            return Err(error);
        }
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        Ok(info)
    }

    pub fn reload(&self, id: &str) -> Result<ExtensionPackageInfo, ExtensionError> {
        let _operation = self.inner.lifecycle_operations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "lifecycle operation lock poisoned")
        })?;
        self.retry_cleanup_tombstones();
        if self.cleanup_pending(id) {
            return Err(ExtensionError::new(
                "cleanup_pending",
                "extension resources are awaiting authoritative cleanup",
            ));
        }
        let _ = self.flush_lifecycle_publications();
        self.ensure_lifecycle_capacity(2)?;
        let (old_generation, old_cancel, manifest, enabled, linked, source) = {
            let state = self.inner.state.read().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            (
                package.generation,
                package.generation_cancellation.clone(),
                package.manifest.clone(),
                package.enabled,
                package.linked,
                package.source.clone(),
            )
        };
        if enabled && linked {
            let root = source
                .clone()
                .ok_or_else(|| {
                    ExtensionError::new(
                        "extension_source_missing",
                        "extension source is not linked",
                    )
                })?
                .canonicalize()
                .map_err(|error| {
                    ExtensionError::new("extension_source_missing", error.to_string())
                })?;
            let entrypoint = manifest
                .entrypoint
                .clone()
                .unwrap_or_else(|| "main.luau".to_owned());
            let relative = PathBuf::from(&entrypoint);
            if relative
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(ExtensionError::new(
                    "extension_entrypoint_denied",
                    "entrypoint traversal denied",
                ));
            }
            let path = root.join(relative).canonicalize().map_err(|error| {
                ExtensionError::new("extension_entrypoint_missing", error.to_string())
            })?;
            if !path.starts_with(&root) {
                return Err(ExtensionError::new(
                    "extension_entrypoint_denied",
                    "entrypoint escapes package root",
                ));
            }
            let bytes = fs::read(&path).map_err(|error| {
                ExtensionError::new("extension_entrypoint_read_failed", error.to_string())
            })?;
            if bytes.len() > EXTENSION_FILE_BYTES {
                return Err(ExtensionError::new(
                    "extension_entrypoint_too_large",
                    "entrypoint exceeds host limit",
                ));
            }
            let source = String::from_utf8(bytes).map_err(|_| {
                ExtensionError::new("extension_entrypoint_invalid", "entrypoint is not UTF-8")
            })?;
            validate_luau_source(&source, id)?;
        }
        let new_generation = old_generation.checked_add(1).ok_or_else(|| {
            ExtensionError::new("generation_exhausted", "extension generation exhausted")
        })?;
        self.ensure_cleanup_retry_capacity(id, old_generation)?;
        old_cancel.cancel();
        let cleanup_error = self
            .cleanup_generation(id, old_generation, Some("retired"))
            .err();
        let info = {
            let mut state = self.inner.state.write().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get_mut(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            package.generation = new_generation;
            package.generation_cancellation = CommandCancellation::new();
            package.enabled = enabled;
            package.linked = linked;
            package.source = source;
            package.manifest = manifest;
            package_info(package)
        };
        if let Some(error) = cleanup_error {
            return Err(error);
        }
        if enabled && linked {
            return self.load_linked_package_locked(id);
        }
        if enabled {
            let snapshot = self.lifecycle_snapshot(None)?;
            if let Err(error) =
                self.publish_lifecycle(id, new_generation, "reloaded_pending_source", snapshot)
                && !self.pending_lifecycle_exists(id, new_generation, "reloaded_pending_source")
            {
                return Err(error);
            }
        }
        Ok(info)
    }

    /// Registers a handler for an extension generation.
    ///
    /// Handlers must not capture an owning [`ExtensionRuntime`] clone. Use the
    /// invocation context's internal capabilities (`ExtensionCommandContext::runtime`)
    /// for host operations so dropping the last public runtime owner can always
    /// shut down workers and child processes.
    pub fn register_command(
        &self,
        id: &str,
        generation: u64,
        mut descriptor: CommandDescriptor,
        handler: ExtensionCommandHandler,
    ) -> Result<(), ExtensionError> {
        let generation_cancellation = self.package_generation(id, generation)?;
        descriptor.id = descriptor.id.trim().to_owned();
        let command_id = descriptor.id.clone();
        let generation_ref = ExtensionGeneration {
            extension_id: id.to_owned(),
            generation,
        };
        {
            let state = self.inner.state.read().map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?;
            if state.commands.contains_key(&command_id) {
                return Err(ExtensionError::new(
                    "command_collision",
                    "command is already registered",
                ));
            }
        }
        let mut reservations = self.inner.command_reservations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "command reservation lock poisoned")
        })?;
        if reservations.contains_key(&command_id) {
            return Err(ExtensionError::new(
                "command_collision",
                "command is already registered",
            ));
        }
        reservations.insert(command_id.clone(), generation_ref.clone());
        if let Err(error) =
            self.command_registry()
                .register_extension_command(descriptor.clone(), id, generation)
        {
            reservations.remove(&command_id);
            return Err(command_outcome_error(error));
        }
        let package_id = match ExtensionPackageId::new(id.to_owned()) {
            Ok(package_id) => package_id,
            Err(error) => {
                let _ = self
                    .command_registry()
                    .unregister_extension_commands(id, generation);
                reservations.remove(&command_id);
                return Err(error);
            }
        };
        let record = ExtensionCommandRecord {
            package: package_id,
            generation,
            handler,
            generation_cancellation,
        };
        let mut state = match self.inner.state.write() {
            Ok(state) => state,
            Err(_) => {
                let _ = self
                    .command_registry()
                    .unregister_extension_commands(id, generation);
                reservations.remove(&command_id);
                return Err(ExtensionError::new(
                    "runtime_unavailable",
                    "extension state lock poisoned",
                ));
            }
        };
        let valid = state.packages.get(id).is_some_and(|package| {
            package.enabled
                && package.generation == generation
                && !package.generation_cancellation.is_cancelled()
        });
        let reserved = reservations.get(&command_id) == Some(&generation_ref);
        if state.commands.contains_key(&command_id) || !valid || !reserved {
            drop(state);
            let _ = self
                .command_registry()
                .unregister_extension_commands(id, generation);
            reservations.remove(&command_id);
            return Err(if valid && reserved {
                ExtensionError::new("command_collision", "command is already registered")
            } else {
                ExtensionError::new("stale_generation", "extension generation is stale")
            });
        }
        let package = state
            .packages
            .get_mut(id)
            .expect("package generation was checked");
        package.commands.insert(command_id.clone());
        state.commands.insert(command_id.clone(), record);
        reservations.remove(&command_id);
        drop(state);
        self.publish_registry_changed("registered", id, generation, &command_id);
        Ok(())
    }

    pub fn register_event(
        &self,
        id: &str,
        generation: u64,
        name: &str,
    ) -> Result<ExtensionEventRegistration, ExtensionError> {
        let _ = self.package_generation(id, generation)?;
        let topic = if name.starts_with(&format!("{id}.")) {
            name.to_owned()
        } else {
            format!("{id}.{name}")
        };
        let generation_ref = ExtensionGeneration {
            extension_id: id.to_owned(),
            generation,
        };
        let mut reservations = self.inner.event_reservations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "event reservation lock poisoned")
        })?;
        if reservations.contains_key(&topic) {
            return Err(ExtensionError::new(
                "event_collision",
                "event topic is already registered",
            ));
        }
        reservations.insert(topic.clone(), generation_ref.clone());
        {
            let state = match self.inner.state.read() {
                Ok(state) => state,
                Err(_) => {
                    reservations.remove(&topic);
                    return Err(ExtensionError::new(
                        "runtime_unavailable",
                        "extension state lock poisoned",
                    ));
                }
            };
            if state.events.contains_key(&topic) {
                reservations.remove(&topic);
                return Err(ExtensionError::new(
                    "event_collision",
                    "event topic is already registered",
                ));
            }
            let active = state.packages.get(id).is_some_and(|package| {
                package.enabled
                    && package.generation == generation
                    && !package.generation_cancellation.is_cancelled()
            });
            if !active {
                reservations.remove(&topic);
                return Err(ExtensionError::new(
                    "stale_generation",
                    "extension generation is stale",
                ));
            }
        }
        if let Err(error) = self.inner.automation.register_event_topic(&topic) {
            reservations.remove(&topic);
            return Err(automation_error(error));
        }
        let registration = ExtensionEventRegistration {
            topic: topic.clone(),
            generation: generation_ref,
        };
        let mut state = match self.inner.state.write() {
            Ok(state) => state,
            Err(_) => {
                let _ = self.inner.automation.unregister_event_topic(&topic);
                reservations.remove(&topic);
                return Err(ExtensionError::new(
                    "runtime_unavailable",
                    "extension state lock poisoned",
                ));
            }
        };
        let valid = state.packages.get(id).is_some_and(|package| {
            package.enabled
                && package.generation == generation
                && !package.generation_cancellation.is_cancelled()
        });
        let reserved = reservations.get(&topic) == Some(&registration.generation);
        if state.events.contains_key(&topic) || !valid || !reserved {
            drop(state);
            let _ = self.inner.automation.unregister_event_topic(&topic);
            reservations.remove(&topic);
            return Err(if valid && reserved {
                ExtensionError::new("event_collision", "event topic is already registered")
            } else {
                ExtensionError::new("stale_generation", "extension generation is stale")
            });
        }
        state.events.insert(topic.clone(), registration.clone());
        state
            .packages
            .get_mut(id)
            .expect("package generation was checked")
            .events
            .insert(topic);
        reservations.remove(&registration.topic);
        Ok(registration)
    }

    fn publish_event(
        &self,
        id: &str,
        generation: u64,
        topic: &str,
        scope: impl Into<String>,
        payload: Value,
        target: Option<CommandTarget>,
    ) -> Result<u64, ExtensionError> {
        let _ = self.package_generation(id, generation)?;
        let scope = scope.into();
        if scope != extension_scope(id, generation) {
            return Err(ExtensionError::new(
                "invalid_binding_scope",
                "extension event scope is host-owned",
            ));
        }
        validate_metadata_target(id, generation, target.as_ref())?;
        let registration = self
            .inner
            .state
            .read()
            .map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?
            .events
            .get(topic)
            .cloned()
            .ok_or_else(|| ExtensionError::new("unknown_event", "event topic is not registered"))?;
        if registration.generation.generation != generation
            || registration.generation.extension_id != id
        {
            return Err(ExtensionError::new(
                "stale_generation",
                "event generation is stale",
            ));
        }
        let bytes = serde_json::to_vec(&payload)
            .map_err(|error| ExtensionError::new("invalid_event", error.to_string()))?;
        if bytes.len() > crate::automation::hub::EVENT_QUEUE_BYTE_LIMIT {
            return Err(ExtensionError::new(
                "event_too_large",
                "event payload exceeds host limit",
            ));
        }
        self.inner
            .automation
            .publish_event(EventPublication::new(
                scope,
                topic,
                json!({"extension_id": id, "generation": generation}),
                target,
                payload,
            ))
            .map_err(automation_error)
    }

    fn subscribe_event(
        &self,
        id: &str,
        generation: u64,
        owner: OwnerIdentity,
        topic: &str,
        scope: String,
    ) -> Result<(String, EventDelivery), ExtensionError> {
        let _ = self.package_generation(id, generation)?;
        if scope != extension_scope(id, generation) {
            return Err(ExtensionError::new(
                "invalid_binding_scope",
                "extension event scope is host-owned",
            ));
        }
        let _event_reservation = self.inner.event_reservations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "event reservation lock poisoned")
        })?;
        let registration = self
            .inner
            .state
            .read()
            .map_err(|_| {
                ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
            })?
            .events
            .get(topic)
            .cloned()
            .ok_or_else(|| ExtensionError::new("unknown_event", "event topic is not registered"))?;
        if registration.generation.generation != generation
            || registration.generation.extension_id != id
        {
            return Err(ExtensionError::new(
                "stale_generation",
                "event generation is stale",
            ));
        }
        let topics = BTreeSet::from([topic.to_owned()]);
        let delivery = self
            .inner
            .automation
            .events()
            .subscribe(owner.clone(), topics, scope.clone())
            .map_err(automation_error)?;
        let subscription = delivery.subscription.clone();
        let mut state = match self.inner.state.write() {
            Ok(state) => state,
            Err(_) => {
                let _ = self
                    .inner
                    .automation
                    .events()
                    .unsubscribe(&subscription, &owner);
                return Err(ExtensionError::new(
                    "runtime_unavailable",
                    "extension state lock poisoned",
                ));
            }
        };
        let active = state.packages.get(id).is_some_and(|package| {
            package.enabled
                && package.generation == generation
                && !package.generation_cancellation.is_cancel_requested()
        });
        if !active {
            drop(state);
            let _ = self
                .inner
                .automation
                .events()
                .unsubscribe(&subscription, &owner);
            return Err(ExtensionError::new(
                "stale_generation",
                "event generation is stale",
            ));
        }
        state.subscriptions.insert(
            subscription.clone(),
            SubscriptionRecord {
                owner,
                generation: registration.generation,
            },
        );
        Ok((subscription, delivery))
    }

    pub fn poll_event(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
        cursor: u64,
    ) -> Result<EventDelivery, ExtensionError> {
        self.inner
            .automation
            .events()
            .poll(subscription, owner, cursor)
            .map_err(automation_error)
    }

    pub fn unsubscribe_event(
        &self,
        subscription: &str,
        owner: &OwnerIdentity,
    ) -> Result<(), ExtensionError> {
        self.inner
            .automation
            .events()
            .unsubscribe(subscription, owner)
            .map_err(automation_error)?;
        if let Ok(mut state) = self.inner.state.write() {
            state.subscriptions.remove(subscription);
        }
        Ok(())
    }

    pub fn start_task(
        &self,
        id: &str,
        generation: u64,
        owner: OwnerIdentity,
        scope: String,
        cancellation: CommandCancellation,
    ) -> Result<TaskStatus, ExtensionError> {
        let binding = self.resource_binding(id, generation, &owner)?;
        if scope != binding.scope {
            return Err(ExtensionError::new(
                "invalid_binding_scope",
                "extension task scope is host-owned",
            ));
        }
        self.ensure_running()?;
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        self.ensure_running()?;
        let mut reservations = self.inner.task_reservations.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "task reservation lock poisoned")
        })?;
        if reservations.contains_key(id) {
            return Err(ExtensionError::new(
                "task_busy",
                "extension task start is already reserved",
            ));
        }
        reservations.insert(id.to_owned(), binding.generation.clone());
        if let Err(error) = self.ensure_running() {
            reservations.remove(id);
            return Err(error);
        }
        let task_owner = owner.clone();
        let status =
            match self
                .inner
                .automation
                .tasks()
                .start(owner, cancellation, binding.scope.clone())
            {
                Ok(status) => status,
                Err(error) => {
                    reservations.remove(id);
                    return Err(automation_error(error));
                }
            };
        #[cfg(test)]
        invoke_task_start_before_commit_hook();
        let valid = !self.inner.shutdown.load(Ordering::Acquire)
            && validate_package_state(&state, id, generation).is_ok();
        if !valid {
            let _ = self
                .inner
                .automation
                .tasks()
                .terminate_force_checked(&status.id, &task_owner);
            reservations.remove(id);
            let code = if self.inner.shutdown.load(Ordering::Acquire) {
                "extension_stopped"
            } else {
                "stale_generation"
            };
            let message = if code == "extension_stopped" {
                "extension runtime has been shut down"
            } else {
                "extension generation is stale"
            };
            return Err(ExtensionError::new(code, message));
        }
        state
            .tasks
            .insert(status.id.clone(), TaskBinding { resource: binding });
        reservations.remove(id);
        Ok(status)
    }

    pub fn finish_task(
        &self,
        task: &str,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        outcome: &Value,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        validate_task_binding(&state, task, &binding)?;
        self.inner
            .automation
            .tasks()
            .finish(task, outcome)
            .map_err(automation_error)?;
        state.tasks.remove(task);
        Ok(())
    }

    pub fn task_status(
        &self,
        task: &str,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<TaskStatus, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        validate_task_binding(&state, task, &binding)?;
        self.inner
            .automation
            .tasks()
            .status(task, owner)
            .map_err(automation_error)
    }

    pub fn cancel_task(
        &self,
        task: &str,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<TaskStatus, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        validate_task_binding(&state, task, &binding)?;
        self.inner
            .automation
            .tasks()
            .cancel(task, owner)
            .map_err(automation_error)
    }

    pub fn publish_metadata(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        publication: MetadataPublication,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        if publication.scope != binding.scope {
            return Err(ExtensionError::new(
                "invalid_binding_scope",
                "extension metadata scope is host-owned",
            ));
        }
        validate_metadata_target(id, generation, publication.target.as_ref())?;
        validate_metadata_provenance(id, generation, &publication.provenance)?;
        let namespace = namespaced_metadata_namespace(id, &publication.namespace)?;
        let key = MetadataBindingKey {
            resource: binding.clone(),
            namespace: namespace.clone(),
            key: publication.key.clone(),
            target: serde_json::to_string(&publication.target)
                .map_err(|error| ExtensionError::new("invalid_metadata", error.to_string()))?,
        };
        let host_publication = MetadataPublication::new(
            binding.scope.clone(),
            namespace,
            publication.key.clone(),
            publication.target.clone(),
            publication.value,
            publication.expires_at_ms,
            json!({
                "extension_id": id,
                "generation": generation,
                "owner_pid": owner.pid(),
                "owner_generation": owner.generation(),
                "provenance": publication.provenance,
            }),
        );
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        self.inner
            .automation
            .metadata()
            .publish(host_publication)
            .map_err(automation_error)?;
        state.metadata.insert(
            key,
            MetadataBinding {
                resource: binding,
                namespace: publication.namespace,
                key: publication.key,
                target: publication.target,
            },
        );
        Ok(())
    }

    pub fn metadata_get(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
    ) -> Result<Option<crate::automation::MetadataRecord>, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        validate_metadata_target(id, generation, target)?;
        let namespace = namespaced_metadata_namespace(id, namespace)?;
        let target_key = serde_json::to_string(&target)
            .map_err(|error| ExtensionError::new("invalid_metadata", error.to_string()))?;
        let record_key = MetadataBindingKey {
            resource: binding.clone(),
            namespace: namespace.clone(),
            key: key.to_owned(),
            target: target_key,
        };
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        if !state.metadata.contains_key(&record_key) {
            return Ok(None);
        }
        let Some(mut record) = self
            .inner
            .automation
            .metadata()
            .get(&binding.scope, &namespace, key, target)
            .map_err(automation_error)?
        else {
            return Ok(None);
        };
        record.namespace = record
            .namespace
            .strip_prefix(&format!("{id}:"))
            .unwrap_or(&record.namespace)
            .to_owned();
        Ok(Some(record))
    }

    pub fn metadata_clear(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        namespace: &str,
        key: &str,
        target: Option<&CommandTarget>,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        validate_metadata_target(id, generation, target)?;
        let namespace = namespaced_metadata_namespace(id, namespace)?;
        let target_key = serde_json::to_string(&target)
            .map_err(|error| ExtensionError::new("invalid_metadata", error.to_string()))?;
        let record_key = MetadataBindingKey {
            resource: binding.clone(),
            namespace: namespace.clone(),
            key: key.to_owned(),
            target: target_key,
        };
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        self.inner
            .automation
            .metadata()
            .clear(
                &binding.scope,
                &namespace,
                key,
                target,
                json!({
                    "extension_id": id,
                    "generation": generation,
                    "owner_pid": owner.pid(),
                    "owner_generation": owner.generation(),
                }),
            )
            .map_err(automation_error)?;
        state.metadata.remove(&record_key);
        Ok(())
    }

    pub fn metadata_list(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<Vec<crate::automation::MetadataRecord>, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        let known = state
            .metadata
            .values()
            .filter(|record| record.resource == binding)
            .map(|record| {
                (
                    format!("{id}:{}", record.namespace),
                    record.key.clone(),
                    serde_json::to_string(&record.target).unwrap_or_default(),
                )
            })
            .collect::<BTreeSet<_>>();
        let prefix = format!("{id}:");
        Ok(self
            .inner
            .automation
            .metadata()
            .list(&binding.scope)
            .map_err(automation_error)?
            .into_iter()
            .filter_map(|mut record| {
                let internal_namespace = record.namespace.clone();
                let key = (
                    internal_namespace.clone(),
                    record.key.clone(),
                    serde_json::to_string(&record.target).ok()?,
                );
                if !known.contains(&key) {
                    return None;
                }
                record.namespace = internal_namespace
                    .strip_prefix(&prefix)
                    .unwrap_or(&internal_namespace)
                    .to_owned();
                Some(record)
            })
            .collect())
    }

    pub fn log(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        level: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let message = message.into();
        let level = level.into();
        if message.len() > EXTENSION_LOG_BYTES {
            return Err(ExtensionError::new(
                "log_too_large",
                "extension log entry exceeds host limit",
            ));
        }
        let entry = ExtensionLogEntry {
            timestamp_ms: unix_time_ms(),
            level,
            message,
            generation,
        };
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        let entries = state.logs.entry(binding).or_default();
        entries.push_back(entry);
        while entries.len() > EXTENSION_LOG_LIMIT
            || entries
                .iter()
                .map(|entry| entry.message.len())
                .sum::<usize>()
                > EXTENSION_LOG_BYTES
        {
            let _ = entries.pop_front();
        }
        Ok(())
    }

    pub fn logs(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<Vec<ExtensionLogEntry>, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        Ok(state
            .logs
            .get(&binding)
            .cloned()
            .map(|entries| entries.into_iter().collect())
            .unwrap_or_default())
    }

    pub fn storage_get(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        key: &str,
    ) -> Result<Option<Value>, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        validate_storage_key(key)?;
        let state = self
            .inner
            .state
            .read()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))?;
        validate_package_state(&state, id, generation)?;
        if let Some(value) = state.storage.get(&StorageBindingKey {
            resource: binding,
            key: key.to_owned(),
        }) {
            return Ok(Some(value.clone()));
        }
        let namespace = package_storage_namespace(
            state
                .packages
                .get(id)
                .expect("package validated before storage lookup"),
        )?;
        let Some(namespace) = namespace else {
            return Ok(None);
        };
        let Some(root) = self
            .inner
            .storage_root
            .read()
            .ok()
            .and_then(|root| root.clone())
        else {
            return Ok(None);
        };
        let path = storage_path(&root, id, &namespace, key)?;
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| ExtensionError::new("storage_invalid", error.to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(ExtensionError::new(
                "storage_unavailable",
                error.to_string(),
            )),
        }
    }

    pub fn storage_put(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        key: &str,
        value: Value,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        validate_storage_key(key)?;
        let bytes = serde_json::to_vec(&value)
            .map_err(|error| ExtensionError::new("storage_invalid", error.to_string()))?;
        if bytes.len() > EXTENSION_FILE_BYTES {
            return Err(ExtensionError::new(
                "storage_too_large",
                "storage value exceeds host limit",
            ));
        }
        let mut state = self
            .inner
            .state
            .write()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))?;
        validate_package_state(&state, id, generation)?;
        let storage_key = StorageBindingKey {
            resource: binding,
            key: key.to_owned(),
        };
        let owned_records = state
            .storage
            .keys()
            .filter(|entry| entry.resource.generation.extension_id == id)
            .count();
        if owned_records >= EXTENSION_STORAGE_RECORD_LIMIT
            && !state.storage.contains_key(&storage_key)
        {
            return Err(ExtensionError::new(
                "storage_limit",
                "extension storage record limit reached",
            ));
        }
        let namespace = package_storage_namespace(
            state
                .packages
                .get(id)
                .expect("package validated before storage write"),
        )?;
        let path = if let Some(namespace) = namespace {
            self.inner
                .storage_root
                .read()
                .ok()
                .and_then(|root| root.clone())
                .map(|root| storage_path(&root, id, &namespace, key))
                .transpose()?
        } else {
            None
        };
        if let Some(path) = path {
            atomic_write(&path, &bytes)?;
        }
        state.storage.insert(storage_key, value);
        Ok(())
    }

    pub fn storage_delete(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        key: &str,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        validate_storage_key(key)?;
        let mut state = self
            .inner
            .state
            .write()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))?;
        validate_package_state(&state, id, generation)?;
        let storage_key = StorageBindingKey {
            resource: binding,
            key: key.to_owned(),
        };
        let namespace = package_storage_namespace(
            state
                .packages
                .get(id)
                .expect("package validated before storage delete"),
        )?;
        let path = if let Some(namespace) = namespace {
            self.inner
                .storage_root
                .read()
                .ok()
                .and_then(|root| root.clone())
                .map(|root| storage_path(&root, id, &namespace, key))
                .transpose()?
        } else {
            None
        };
        if let Some(path) = path {
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ExtensionError::new(
                        "storage_unavailable",
                        error.to_string(),
                    ));
                }
            }
        }
        state.storage.remove(&storage_key);
        Ok(())
    }

    pub fn storage_list(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<Vec<String>, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self
            .inner
            .state
            .read()
            .map_err(|_| ExtensionError::new("storage_unavailable", "storage lock poisoned"))?;
        validate_package_state(&state, id, generation)?;
        let mut keys = state
            .storage
            .keys()
            .filter(|entry| entry.resource == binding)
            .map(|entry| entry.key.clone())
            .collect::<BTreeSet<_>>();
        let namespace = package_storage_namespace(
            state
                .packages
                .get(id)
                .expect("package validated before storage list"),
        )?;
        if let Some(namespace) = namespace
            && let Some(root) = self
                .inner
                .storage_root
                .read()
                .ok()
                .and_then(|root| root.clone())
        {
            let directory = storage_directory(&root, id, &namespace)?;
            if let Ok(entries) = fs::read_dir(directory) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                        continue;
                    }
                    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
                        && validate_storage_key(stem).is_ok()
                    {
                        keys.insert(stem.to_owned());
                    }
                }
            }
        }
        Ok(keys.into_iter().collect())
    }

    pub fn file_read(&self, id: &str, path: &Path) -> Result<Vec<u8>, ExtensionError> {
        self.ensure_running()?;
        #[cfg(unix)]
        {
            let access = self.ensure_file_access(id, path)?;
            let (parent, name) = open_file_parent(&access.root, &access.relative)
                .map_err(|error| ExtensionError::new("file_read_failed", error.to_string()))?;
            let file = open_file_at(&parent, &name)
                .map_err(|error| ExtensionError::new("file_read_failed", error.to_string()))?;
            read_file_contents(file)
        }
        #[cfg(not(unix))]
        {
            let _ = (id, path);
            Err(unsupported_file_api_error())
        }
    }

    pub fn file_exists(&self, id: &str, path: &Path) -> Result<bool, ExtensionError> {
        self.ensure_running()?;
        #[cfg(unix)]
        {
            let access = self.ensure_file_access(id, path)?;
            let (parent, name) = open_file_parent(&access.root, &access.relative)
                .map_err(|error| ExtensionError::new("file_stat_failed", error.to_string()))?;
            match open_file_at(&parent, &name) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(ExtensionError::new("file_stat_failed", error.to_string())),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (id, path);
            Err(unsupported_file_api_error())
        }
    }

    pub fn file_stat(&self, id: &str, path: &Path) -> Result<Value, ExtensionError> {
        self.ensure_running()?;
        #[cfg(unix)]
        {
            let access = self.ensure_file_access(id, path)?;
            let (parent, name) = open_file_parent(&access.root, &access.relative)
                .map_err(|error| ExtensionError::new("file_stat_failed", error.to_string()))?;
            let file = open_file_at(&parent, &name)
                .map_err(|error| ExtensionError::new("file_stat_failed", error.to_string()))?;
            let metadata = file
                .metadata()
                .map_err(|error| ExtensionError::new("file_stat_failed", error.to_string()))?;
            let identity = file_identity(&metadata);
            let version = file_version(&metadata);
            Ok(json!({
                "size": version.size,
                "device": identity.device,
                "inode": identity.inode,
                "modified_seconds": version.modified_seconds,
                "modified_nanos": version.modified_nanos,
                "changed_seconds": version.changed_seconds,
                "changed_nanos": version.changed_nanos,
            }))
        }
        #[cfg(not(unix))]
        {
            let _ = (id, path);
            Err(unsupported_file_api_error())
        }
    }
    pub fn file_transaction(
        &self,
        id: &str,
        generation: u64,
        path: &Path,
        contents: Vec<u8>,
    ) -> Result<FileTransaction, ExtensionError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let _ = self.package_generation(id, generation)?;
            let access = self.ensure_file_access(id, path)?;
            if contents.len() > EXTENSION_FILE_BYTES {
                return Err(ExtensionError::new(
                    "file_too_large",
                    "file exceeds host write limit",
                ));
            }
            let (parent, name) = open_file_parent(&access.root, &access.relative)
                .map_err(|error| ExtensionError::new("file_denied", error.to_string()))?;
            let parent_identity =
                Some(file_identity(&parent.metadata().map_err(|error| {
                    ExtensionError::new("file_denied", error.to_string())
                })?));
            let snapshot = match open_file_at(&parent, &name) {
                Ok(file) => snapshot_file(file).map_err(|error| {
                    let code = if error.kind() == std::io::ErrorKind::InvalidData {
                        "file_too_large"
                    } else {
                        "file_read_failed"
                    };
                    ExtensionError::new(code, error.to_string())
                })?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileSnapshot {
                    existed: false,
                    identity: None,
                    version: None,
                    bytes: Vec::new(),
                },
                Err(error) => {
                    return Err(ExtensionError::new("file_denied", error.to_string()));
                }
            };
            let preview = FileTransactionPreview {
                path: path.display().to_string(),
                existed: snapshot.existed,
                before_bytes: snapshot.bytes.len(),
                after_bytes: contents.len(),
                changed: snapshot.bytes != contents,
                destructive: snapshot.existed && snapshot.bytes != contents,
            };
            Ok(FileTransaction {
                runtime: self.clone_internal(),
                extension: ExtensionPackageId::new(id.to_owned())?,
                generation,
                confirmation_id: self
                    .inner
                    .next_file_confirmation
                    .fetch_add(1, Ordering::Relaxed),
                root: access.root,
                relative: access.relative,
                parent_identity,
                snapshot,
                contents,
                operation: FileTransactionOperation::Write,
                preview,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (id, generation, path, contents);
            Err(unsupported_file_api_error())
        }
    }
    pub fn file_removal_transaction(
        &self,
        id: &str,
        generation: u64,
        path: &Path,
    ) -> Result<FileTransaction, ExtensionError> {
        let mut transaction = self.file_transaction(id, generation, path, Vec::new())?;
        transaction.operation = FileTransactionOperation::Remove;
        transaction.preview.changed = transaction.preview.existed;
        transaction.preview.destructive = transaction.preview.existed;
        Ok(transaction)
    }
    /// Issues a host-owned confirmation bound to one exact file transaction.
    ///
    /// This is intentionally a runtime method rather than a capability method:
    /// package Luau can prepare a transaction and inspect its preview, but only
    /// the dispatcher/UI owner can authorize the destructive apply.
    pub fn confirm_file_transaction(
        &self,
        transaction: &FileTransaction,
    ) -> Result<FileConfirmation, ExtensionError> {
        self.confirm_file_transactions(std::slice::from_ref(transaction))
    }

    pub fn confirm_file_transactions(
        &self,
        transactions: &[FileTransaction],
    ) -> Result<FileConfirmation, ExtensionError> {
        let first = transactions.first().ok_or_else(|| {
            ExtensionError::new("invalid_confirmation", "file transaction batch is empty")
        })?;
        if !transactions.iter().all(|transaction| {
            Arc::ptr_eq(&self.inner, &transaction.runtime.inner)
                && transaction.extension == first.extension
                && transaction.generation == first.generation
        }) {
            return Err(ExtensionError::new(
                "invalid_confirmation",
                "file transaction batch crosses runtime or generation boundaries",
            ));
        }
        self.package_generation(first.extension.as_ref(), first.generation)?;
        let nonce = self
            .inner
            .next_file_confirmation
            .fetch_add(1, Ordering::Relaxed);
        let digest = file_transactions_digest(transactions);
        let token = format!("bootty-file-{nonce:016x}-{digest}");
        let previews = transactions
            .iter()
            .map(|transaction| transaction.preview.clone())
            .collect::<Vec<_>>();
        let confirmation = FileConfirmation {
            token: token.clone(),
            transaction_id: first.confirmation_id,
            extension: first.extension.clone(),
            generation: first.generation,
            digest,
            preview: first.preview.clone(),
            previews,
        };
        self.inner
            .file_confirmations
            .lock()
            .map_err(|_| {
                ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
            })?
            .insert(
                token,
                FileConfirmationRecord {
                    confirmation: confirmation.clone(),
                    transactions: transactions.to_vec(),
                },
            );
        Ok(confirmation)
    }

    pub fn validate_file_confirmation(
        &self,
        id: &str,
        generation: u64,
        expected: &Value,
        token: &str,
    ) -> Result<(), ExtensionError> {
        let record = self
            .inner
            .file_confirmations
            .lock()
            .map_err(|_| {
                ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
            })?
            .get(token)
            .map(|record| record.confirmation.clone())
            .ok_or_else(|| {
                ExtensionError::new(
                    "invalid_confirmation",
                    "file confirmation token is unknown or already consumed",
                )
            })?;
        let expected = expected.as_object().ok_or_else(|| {
            ExtensionError::new(
                "invalid_confirmation",
                "file confirmation expectation must be an object",
            )
        })?;
        let expected_transaction = expected
            .get("transaction_id")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                ExtensionError::new(
                    "invalid_confirmation",
                    "file confirmation transaction_id is required",
                )
            })?;
        let expected_digest = expected
            .get("digest")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ExtensionError::new(
                    "invalid_confirmation",
                    "file confirmation digest is required",
                )
            })?;
        let expected_preview = expected.get("preview").ok_or_else(|| {
            ExtensionError::new(
                "invalid_confirmation",
                "file confirmation preview is required",
            )
        })?;
        let actual_preview = serde_json::to_value(&record.preview)
            .map_err(|error| ExtensionError::new("invalid_confirmation", error.to_string()))?;
        let actual_previews = serde_json::to_value(&record.previews)
            .map_err(|error| ExtensionError::new("invalid_confirmation", error.to_string()))?;
        if record.extension.as_ref() != id
            || record.generation != generation
            || record.transaction_id != expected_transaction
            || record.digest != expected_digest
            || &actual_preview != expected_preview
            || expected
                .get("previews")
                .is_some_and(|value| value != &actual_previews)
        {
            return Err(ExtensionError::new(
                "invalid_confirmation",
                "file confirmation does not match the exact transaction preview",
            ));
        }
        self.package_generation(id, generation)?;
        Ok(())
    }
    pub fn apply_file_confirmation(
        &self,
        id: &str,
        generation: u64,
        actions: &Value,
        token: &str,
        _context: &Value,
    ) -> Result<Value, ExtensionError> {
        let entries = actions.as_array().ok_or_else(|| {
            ExtensionError::new("invalid_file_actions", "file actions must be an array")
        })?;
        let record = self
            .inner
            .file_confirmations
            .lock()
            .map_err(|_| {
                ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
            })?
            .get(token)
            .cloned()
            .ok_or_else(|| {
                ExtensionError::new(
                    "invalid_confirmation",
                    "file confirmation token is unknown or already consumed",
                )
            })?;
        if record.confirmation.extension.as_ref() != id
            || record.confirmation.generation != generation
            || entries.len() != record.transactions.len()
        {
            return Err(ExtensionError::new(
                "invalid_confirmation",
                "file confirmation does not match the exact file action batch",
            ));
        }
        for (entry, transaction) in entries.iter().zip(record.transactions.iter()) {
            let entry = entry.as_object().ok_or_else(|| {
                ExtensionError::new("invalid_file_actions", "file action must be an object")
            })?;
            let path = entry.get("path").and_then(Value::as_str).ok_or_else(|| {
                ExtensionError::new("invalid_file_actions", "file action path is required")
            })?;
            if path != transaction.preview.path {
                return Err(ExtensionError::new(
                    "invalid_confirmation",
                    "file action path does not match the exact transaction",
                ));
            }
            let operation = entry
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or("write");
            match transaction.operation {
                FileTransactionOperation::Write => {
                    if operation != "write" {
                        return Err(ExtensionError::new(
                            "invalid_file_actions",
                            "file action operation does not match the exact transaction",
                        ));
                    }
                    let content = entry.get("content").ok_or_else(|| {
                        ExtensionError::new(
                            "invalid_file_actions",
                            "write action content is required",
                        )
                    })?;
                    let contents = if let Some(content) = content.as_str() {
                        content.as_bytes().to_vec()
                    } else {
                        serde_json::to_vec(content).map_err(|error| {
                            ExtensionError::new("invalid_file_actions", error.to_string())
                        })?
                    };
                    if contents != transaction.contents {
                        return Err(ExtensionError::new(
                            "invalid_confirmation",
                            "file action content does not match the exact transaction",
                        ));
                    }
                }
                FileTransactionOperation::Remove => {
                    if operation != "remove" {
                        return Err(ExtensionError::new(
                            "invalid_file_actions",
                            "file action operation does not match the exact transaction",
                        ));
                    }
                }
            }
        }
        self.package_generation(id, generation)?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let _transaction_guard = self.inner.file_transaction_lock.lock().map_err(|_| {
                ExtensionError::new("file_unavailable", "file transaction lock poisoned")
            })?;
            for transaction in &record.transactions {
                transaction.validate_current()?;
            }
            let transactions = {
                let mut confirmations = self.inner.file_confirmations.lock().map_err(|_| {
                    ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
                })?;
                confirmations
                    .remove(token)
                    .ok_or_else(|| {
                        ExtensionError::new(
                            "invalid_confirmation",
                            "file confirmation token is unknown or already consumed",
                        )
                    })?
                    .transactions
            };
            let mut committed = Vec::with_capacity(transactions.len());
            for transaction in transactions {
                match transaction.apply_committed_unlocked() {
                    Ok(commit) => committed.push(commit),
                    Err(original) => {
                        let rollback_errors = rollback_file_transactions(committed);
                        return Err(ExtensionError::file_batch_failure(
                            original,
                            rollback_errors,
                        ));
                    }
                }
            }
            let previews = committed
                .into_iter()
                .map(|commit| commit.transaction.preview)
                .collect::<Vec<_>>();
            serde_json::to_value(previews)
                .map_err(|error| ExtensionError::new("invalid_file_actions", error.to_string()))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let transactions = {
                let mut confirmations = self.inner.file_confirmations.lock().map_err(|_| {
                    ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
                })?;
                confirmations
                    .remove(token)
                    .ok_or_else(|| {
                        ExtensionError::new(
                            "invalid_confirmation",
                            "file confirmation token is unknown or already consumed",
                        )
                    })?
                    .transactions
            };
            let previews = transactions
                .into_iter()
                .map(FileTransaction::apply_committed)
                .collect::<Result<Vec<_>, _>>()?;
            serde_json::to_value(previews)
                .map_err(|error| ExtensionError::new("invalid_file_actions", error.to_string()))
        }
    }

    pub fn open_surface(
        &self,
        id: &str,
        generation: u64,
        spec: SurfaceSpec,
    ) -> Result<SurfaceLifecycleEvent, ExtensionError> {
        let _ = self.package_generation(id, generation)?;
        if spec.id.is_empty() || spec.id.len() > 128 {
            return Err(ExtensionError::new(
                "invalid_surface",
                "surface id is invalid",
            ));
        }
        let state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        let package = state.packages.get(id).ok_or_else(|| {
            ExtensionError::new("unknown_extension", "extension is not installed")
        })?;
        if !package.enabled
            || package.generation != generation
            || package.generation_cancellation.is_cancel_requested()
        {
            return Err(ExtensionError::new(
                "stale_generation",
                "extension generation is stale",
            ));
        }
        if let Some(existing) = state.surfaces.get(&spec.id)
            && (existing.generation.extension_id != id
                || existing.generation.generation != generation)
        {
            return Err(ExtensionError::new(
                "surface_collision",
                "surface id belongs to another extension generation",
            ));
        }
        if state.surfaces.len() >= EXTENSION_SURFACE_LIMIT && !state.surfaces.contains_key(&spec.id)
        {
            return Err(ExtensionError::new(
                "surface_limit",
                "surface limit reached",
            ));
        }
        let generation = ExtensionGeneration {
            extension_id: id.to_owned(),
            generation,
        };
        let event = SurfaceLifecycleEvent {
            operation: "opened".to_owned(),
            surface: spec,
            generation,
        };
        drop(state);
        if let Err(error) =
            self.publish_surface_lifecycle("opened", &event.surface, &event.generation)
        {
            if let Ok(mut state) = self.inner.state.write() {
                state.surfaces.remove(&event.surface.id);
            }
            return Err(error);
        }
        Ok(event)
    }

    pub fn close_surface(
        &self,
        id: &str,
        generation: u64,
        surface: &str,
    ) -> Result<Option<SurfaceLifecycleEvent>, ExtensionError> {
        let _ = self.package_generation(id, generation)?;
        let state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        let Some(record) = state.surfaces.get(surface).cloned() else {
            return Ok(None);
        };
        if record.generation.extension_id != id || record.generation.generation != generation {
            return Err(ExtensionError::new(
                "stale_generation",
                "surface generation is stale",
            ));
        }
        let event = SurfaceLifecycleEvent {
            operation: "closed".to_owned(),
            surface: record.spec,
            generation: record.generation,
        };
        drop(state);
        self.publish_surface_lifecycle("closed", &event.surface, &event.generation)?;
        Ok(Some(event))
    }

    pub fn surfaces(&self, id: Option<&str>) -> Vec<SurfaceLifecycleEvent> {
        self.inner
            .state
            .read()
            .map(|state| {
                state
                    .surfaces
                    .values()
                    .filter(|surface| id.is_none_or(|id| surface.generation.extension_id == id))
                    .map(|surface| SurfaceLifecycleEvent {
                        operation: "active".to_owned(),
                        surface: surface.spec.clone(),
                        generation: surface.generation.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn spawn_process(
        &self,
        id: &str,
        generation: u64,
        spec: ProcessSpec,
    ) -> Result<ProcessStatus, ExtensionError> {
        self.package_generation(id, generation)?;
        if spec.executable.is_empty() || spec.executable.len() > 4096 {
            return Err(ExtensionError::new(
                "invalid_process",
                "executable is invalid",
            ));
        }
        let generation_ref = (id.to_owned(), generation);
        let mut retired = Vec::new();
        {
            let mut state = self.inner.state.write().map_err(|_| {
                ExtensionError::new("process_unavailable", "extension state lock poisoned")
            })?;
            let package = state.packages.get(id).ok_or_else(|| {
                ExtensionError::new("unknown_extension", "extension is not installed")
            })?;
            if !package.enabled {
                return Err(ExtensionError::new(
                    "extension_disabled",
                    "extension is disabled",
                ));
            }
            if package.generation != generation
                || package.generation_cancellation.is_cancel_requested()
            {
                return Err(ExtensionError::new(
                    "stale_generation",
                    "extension generation is stale",
                ));
            }
            let finished = state
                .processes
                .iter()
                .filter(|(_, process)| {
                    process.generation.extension_id == id
                        && process.generation.generation == generation
                        && process_is_finished(process)
                })
                .map(|(process_id, _)| process_id.clone())
                .collect::<Vec<_>>();
            for process_id in finished {
                if let Some(process) = state.processes.remove(&process_id) {
                    retired.push(process);
                }
            }
            let live = state
                .processes
                .values()
                .filter(|process| {
                    process.generation.extension_id == id
                        && process.generation.generation == generation
                })
                .count();
            let reserved = state
                .process_reservations
                .get(&generation_ref)
                .copied()
                .unwrap_or(0);
            if live + reserved >= EXTENSION_PROCESS_LIMIT {
                drop(state);
                for process in retired {
                    stop_process_record(&process);
                }
                return Err(ExtensionError::new(
                    "process_quota_exceeded",
                    "extension process quota is exhausted",
                ));
            }
            *state
                .process_reservations
                .entry(generation_ref.clone())
                .or_default() += 1;
        }
        for process in retired {
            stop_process_record(&process);
        }

        let mut command = Command::new(&spec.executable);
        command.args(&spec.arguments);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        command.envs(&spec.environment);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = match command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                self.release_process_reservation(&generation_ref);
                return Err(ExtensionError::new(
                    "process_spawn_failed",
                    error.to_string(),
                ));
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let output = Arc::new(Mutex::new(VecDeque::new()));
        let output_bytes = Arc::new(Mutex::new(0usize));
        let record = Arc::new(ProcessRecord {
            generation: ExtensionGeneration {
                extension_id: id.to_owned(),
                generation,
            },
            stdin: Mutex::new(child.stdin.take()),
            child: Mutex::new(child),
            output: Arc::clone(&output),
            output_bytes: Arc::clone(&output_bytes),
            next_sequence: AtomicU64::new(1),
            readers: Mutex::new(Vec::new()),
        });
        let stdout_reader = spawn_process_reader(stdout, "stdout", &record);
        let stderr_reader = spawn_process_reader(stderr, "stderr", &record);
        if let Ok(mut readers) = record.readers.lock() {
            if let Some(reader) = stdout_reader {
                readers.push(reader);
            }
            if let Some(reader) = stderr_reader {
                readers.push(reader);
            }
        }
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        invoke_process_spawn_before_commit_hook(&record);
        let process_id = format!(
            "process-{}",
            self.inner.next_process.fetch_add(1, Ordering::Relaxed)
        );
        let accepted = {
            let mut state = match self.inner.state.write() {
                Ok(state) => state,
                Err(_) => {
                    self.release_process_reservation(&generation_ref);
                    stop_process_record(&record);
                    return Err(ExtensionError::new(
                        "process_unavailable",
                        "extension state lock poisoned",
                    ));
                }
            };
            self.release_process_reservation_locked(&mut state, &generation_ref);
            let valid = !self.inner.shutdown.load(Ordering::Acquire)
                && state.packages.get(id).is_some_and(|package| {
                    package.enabled
                        && package.generation == generation
                        && !package.generation_cancellation.is_cancel_requested()
                });
            if valid {
                state
                    .processes
                    .insert(process_id.clone(), Arc::clone(&record));
            }
            valid
        };
        if !accepted {
            stop_process_record(&record);
            return Err(if self.inner.shutdown.load(Ordering::Acquire) {
                ExtensionError::new("extension_stopped", "extension runtime has been shut down")
            } else {
                ExtensionError::new("stale_generation", "extension generation is stale")
            });
        }
        Ok(ProcessStatus {
            id: process_id,
            running: true,
            exit_code: None,
            generation: record.generation.clone(),
        })
    }

    pub fn process_write(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
        bytes: &[u8],
    ) -> Result<(), ExtensionError> {
        if bytes.len() > EXTENSION_PROCESS_BYTES {
            return Err(ExtensionError::new(
                "process_input_too_large",
                "stdin payload exceeds host limit",
            ));
        }
        let record = self.process_for_owner(process, extension_id, generation)?;
        let mut stdin = record
            .stdin
            .lock()
            .map_err(|_| ExtensionError::new("process_unavailable", "stdin lock poisoned"))?;
        let Some(stdin) = stdin.as_mut() else {
            return Err(ExtensionError::new(
                "process_closed",
                "process stdin is closed",
            ));
        };
        stdin
            .write_all(bytes)
            .map_err(|error| ExtensionError::new("process_write_failed", error.to_string()))
    }

    pub fn process_read(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
        limit: usize,
    ) -> Result<Vec<ProcessLine>, ExtensionError> {
        let record = self.process_for_owner(process, extension_id, generation)?;
        let limit = limit.min(EXTENSION_PROCESS_LINES);
        let output = record
            .output
            .lock()
            .map_err(|_| ExtensionError::new("process_unavailable", "output lock poisoned"))?;
        Ok(output
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect())
    }

    /// Returns bounded JSONL records newer than `cursor`. The sequence number
    /// is the subscription cursor; callers can resume without replaying older
    /// stdout/stderr lines after a poll.
    pub fn process_read_since(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
        cursor: u64,
        limit: usize,
    ) -> Result<Vec<ProcessLine>, ExtensionError> {
        let record = self.process_for_owner(process, extension_id, generation)?;
        let limit = limit.min(EXTENSION_PROCESS_LINES);
        let output = record
            .output
            .lock()
            .map_err(|_| ExtensionError::new("process_unavailable", "output lock poisoned"))?;
        Ok(output
            .iter()
            .filter(|line| line.sequence > cursor)
            .take(limit)
            .cloned()
            .collect())
    }

    pub fn process_status(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
    ) -> Result<ProcessStatus, ExtensionError> {
        let record = self.process_for_owner(process, extension_id, generation)?;
        let status = {
            let mut child = record
                .child
                .lock()
                .map_err(|_| ExtensionError::new("process_unavailable", "child lock poisoned"))?;
            child
                .try_wait()
                .map_err(|error| ExtensionError::new("process_wait_failed", error.to_string()))?
        };
        let result = ProcessStatus {
            id: process.to_owned(),
            running: status.is_none(),
            exit_code: status.and_then(|status| status.code()),
            generation: record.generation.clone(),
        };
        if status.is_some() {
            self.remove_process_record(process, &record);
            stop_process_record(&record);
        }
        Ok(result)
    }

    pub fn process_signal(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
    ) -> Result<(), ExtensionError> {
        let record = self.process_for_owner(process, extension_id, generation)?;
        let exited = {
            let mut child = record
                .child
                .lock()
                .map_err(|_| ExtensionError::new("process_unavailable", "child lock poisoned"))?;
            child
                .try_wait()
                .map_err(|error| ExtensionError::new("process_wait_failed", error.to_string()))?
                .is_some()
        };
        if exited {
            self.remove_process_record(process, &record);
            stop_process_record(&record);
            return Err(ExtensionError::new(
                "process_closed",
                "process has already exited",
            ));
        }
        self.remove_process_record(process, &record);
        stop_process_record(&record);
        Ok(())
    }

    pub fn process_wait(
        &self,
        extension_id: &str,
        generation: u64,
        process: &str,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<ProcessStatus, ExtensionError> {
        self.process_for_owner(process, extension_id, generation)?;
        loop {
            if cancellation.is_cancel_requested() {
                return Err(ExtensionError::new(
                    "cancelled",
                    "process wait was cancelled",
                ));
            }
            if Instant::now() >= deadline {
                return Err(ExtensionError::new(
                    "deadline_exceeded",
                    "process wait deadline expired",
                ));
            }
            let status = self.process_status(extension_id, generation, process)?;
            if !status.running {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    pub fn invoke_async_exact(
        &self,
        invocation: CommandInvocation,
        extension_id: &str,
        generation: u64,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> Receiver<CommandOutcome> {
        let (response, receiver) = mpsc::channel();
        if let Err(error) = self.package_generation(extension_id, generation) {
            let _ = response.send(CommandOutcome::Failed {
                code: error.code,
                message: error.message,
            });
            return receiver;
        }
        let record = self
            .inner
            .state
            .read()
            .ok()
            .and_then(|state| state.commands.get(&invocation.command).cloned())
            .filter(|record| {
                record.package.as_ref() == extension_id && record.generation == generation
            });
        let Some(record) = record else {
            let _ = response.send(CommandOutcome::Failed {
                code: "stale_generation".to_owned(),
                message: "extension command generation is stale or reloaded".to_owned(),
            });
            return receiver;
        };
        if record.generation_cancellation.is_cancel_requested()
            || cancellation.is_cancel_requested()
        {
            let _ = response.send(CommandOutcome::Failed {
                code: "cancelled".to_owned(),
                message: "extension command was cancelled".to_owned(),
            });
            return receiver;
        }
        let item = WorkItem {
            record,
            invocation,
            deadline,
            cancellation,
            response,
        };
        match self.inner.work_tx.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(item)) => {
                let _ = item.response.send(CommandOutcome::Failed {
                    code: "extension_busy".to_owned(),
                    message: "extension command worker queue is full".to_owned(),
                });
            }
            Err(TrySendError::Disconnected(item)) => {
                let _ = item.response.send(CommandOutcome::Failed {
                    code: "extension_stopped".to_owned(),
                    message: "extension command workers are stopped".to_owned(),
                });
            }
        }
        receiver
    }

    pub fn observe(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<HostObservation, ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        Ok(HostObservation {
            topology: state.observations.topology.clone(),
            terminals: state.observations.terminals.clone(),
            metadata: state
                .observation_metadata
                .get(&binding)
                .cloned()
                .unwrap_or(Value::Null),
        })
    }

    pub fn replace_observation(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
        observation: HostObservation,
    ) -> Result<(), ExtensionError> {
        let binding = self.resource_binding(id, generation, owner)?;
        let size = serde_json::to_vec(&observation)
            .map_err(|error| ExtensionError::new("invalid_observation", error.to_string()))?
            .len();
        if size > EXTENSION_FILE_BYTES {
            return Err(ExtensionError::new(
                "observation_too_large",
                "host observation exceeds limit",
            ));
        }
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        validate_package_state(&state, id, generation)?;
        state
            .observation_metadata
            .insert(binding, observation.metadata);
        Ok(())
    }

    pub fn invoke_async(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> Receiver<CommandOutcome> {
        let (response, receiver) = mpsc::channel();
        if let Err(error) = self.ensure_running() {
            let _ = response.send(CommandOutcome::Failed {
                code: error.code,
                message: error.message,
            });
            return receiver;
        }
        let record = self.lookup_command(&invocation.command);
        let Some(record) = record else {
            let _ = response.send(CommandOutcome::Failed {
                code: "unknown_command".to_owned(),
                message: format!("unknown extension command {}", invocation.command),
            });
            return receiver;
        };
        if record.generation_cancellation.is_cancel_requested() {
            let _ = response.send(CommandOutcome::Failed {
                code: "stale_generation".to_owned(),
                message: "extension command generation is stale".to_owned(),
            });
            return receiver;
        }
        if cancellation.is_cancel_requested() {
            let _ = response.send(CommandOutcome::Failed {
                code: "cancelled".to_owned(),
                message: "extension command was cancelled".to_owned(),
            });
            return receiver;
        }
        let item = WorkItem {
            record,
            invocation,
            deadline,
            cancellation,
            response,
        };
        match self.inner.work_tx.try_send(item) {
            Ok(()) => {}
            Err(TrySendError::Full(item)) => {
                let _ = item.response.send(CommandOutcome::Failed {
                    code: "extension_busy".to_owned(),
                    message: "extension command worker queue is full".to_owned(),
                });
            }
            Err(TrySendError::Disconnected(item)) => {
                let _ = item.response.send(CommandOutcome::Failed {
                    code: "extension_stopped".to_owned(),
                    message: "extension command workers are stopped".to_owned(),
                });
            }
        }
        receiver
    }

    pub fn invoke_blocking(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> CommandOutcome {
        let cancellation_for_wait = cancellation.clone();
        let receiver = self.invoke_async(invocation, deadline, cancellation);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                cancellation_for_wait.request_cancel();
                return CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "extension command deadline expired".to_owned(),
                };
            }
            match receiver.recv_timeout(remaining.min(EXTENSION_LUA_CALL_POLL)) {
                Ok(outcome) => return outcome,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return CommandOutcome::Failed {
                        code: "extension_stopped".to_owned(),
                        message: "extension command worker stopped".to_owned(),
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout)
                    if cancellation_for_wait.is_cancel_requested() =>
                {
                    return CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message: "extension command was cancelled".to_owned(),
                    };
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn execute(&self, item: WorkItem) -> CommandOutcome {
        if item.cancellation.is_cancel_requested()
            || item.record.generation_cancellation.is_cancel_requested()
        {
            return self.send_outcome(
                item.response,
                CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "extension command was cancelled".to_owned(),
                },
            );
        }
        if Instant::now() >= item.deadline {
            return self.send_outcome(
                item.response,
                CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "extension command deadline expired".to_owned(),
                },
            );
        }
        if !item.cancellation.try_start() || item.cancellation.is_cancel_requested() {
            return self.send_outcome(
                item.response,
                CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "extension command was cancelled before execution".to_owned(),
                },
            );
        }
        if Instant::now() >= item.deadline {
            return self.send_outcome(
                item.response,
                CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "extension command deadline expired".to_owned(),
                },
            );
        }
        if item.record.generation_cancellation.is_cancel_requested() {
            return self.send_outcome(
                item.response,
                CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "extension generation was cancelled before execution".to_owned(),
                },
            );
        }
        let context = ExtensionCommandContext {
            invocation: item.invocation,
            deadline: item.deadline,
            cancellation: item.cancellation,
            generation_cancellation: item.record.generation_cancellation,
            runtime: self.clone_internal(),
            generation: ExtensionGeneration {
                extension_id: item.record.package.0.clone(),
                generation: item.record.generation,
            },
            owner: self.inner.owner.clone(),
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (item.record.handler)(context)
        }))
        .unwrap_or_else(|_| {
            Err(ExtensionError::new(
                "handler_panicked",
                "extension handler panicked",
            ))
        });
        let outcome = match result {
            Ok(value) => CommandOutcome::Success {
                value,
                warnings: Vec::new(),
            },
            Err(error) => error.outcome(),
        };
        self.send_outcome(item.response, outcome)
    }

    fn send_outcome(
        &self,
        response: mpsc::Sender<CommandOutcome>,
        outcome: CommandOutcome,
    ) -> CommandOutcome {
        let _ = response.send(outcome.clone());
        outcome
    }

    fn lookup_command(&self, id: &str) -> Option<ExtensionCommandRecord> {
        let state = self.inner.state.read().ok()?;
        let record = state.commands.get(id).cloned();
        if record.is_some() {
            return record;
        }
        let canonical = self
            .command_registry()
            .describe(id)
            .map(|descriptor| descriptor.id)?;
        state.commands.get(&canonical).cloned()
    }

    fn resource_binding(
        &self,
        id: &str,
        generation: u64,
        owner: &OwnerIdentity,
    ) -> Result<ExtensionResourceBinding, ExtensionError> {
        let _ = ExtensionPackageId::new(id.to_owned())?;
        let _ = self.package_generation(id, generation)?;
        Ok(ExtensionResourceBinding {
            generation: ExtensionGeneration {
                extension_id: id.to_owned(),
                generation,
            },
            owner: owner.clone(),
            scope: extension_scope(id, generation),
        })
    }

    fn ensure_running(&self) -> Result<(), ExtensionError> {
        if self.inner.shutdown.load(Ordering::Acquire) {
            Err(ExtensionError::new(
                "extension_stopped",
                "extension runtime has been shut down",
            ))
        } else {
            Ok(())
        }
    }

    fn package_generation(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<CommandCancellation, ExtensionError> {
        self.ensure_running()?;
        let state = self.inner.state.read().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        let package = state.packages.get(id).ok_or_else(|| {
            ExtensionError::new("unknown_extension", "extension is not installed")
        })?;
        if !package.enabled {
            return Err(ExtensionError::new(
                "extension_disabled",
                "extension is disabled",
            ));
        }
        if package.generation != generation || package.generation_cancellation.is_cancel_requested()
        {
            return Err(ExtensionError::new(
                "stale_generation",
                "extension generation is stale",
            ));
        }
        Ok(package.generation_cancellation.clone())
    }
    fn cleanup_pending(&self, id: &str) -> bool {
        self.inner
            .cleanup_retries
            .lock()
            .is_ok_and(|retries| retries.keys().any(|(pending_id, _)| pending_id == id))
    }

    /// Returns whether an extension generation is still enabled and uncancelled.
    ///
    /// AppState uses this immediately before forwarding a completed asynchronous
    /// outcome so a non-cooperative handler cannot publish success after reload.
    #[must_use]
    pub fn generation_is_active(&self, id: &str, generation: u64) -> bool {
        self.package_generation(id, generation).is_ok()
    }
    fn ensure_cleanup_retry_capacity(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<(), ExtensionError> {
        let retries = self.inner.cleanup_retries.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "cleanup retry lock poisoned")
        })?;
        if retries.contains_key(&(id.to_owned(), generation))
            || retries.len() < EXTENSION_CLEANUP_RETRY_LIMIT
        {
            Ok(())
        } else {
            Err(ExtensionError::new(
                "cleanup_retry_full",
                "extension cleanup retry capacity is exhausted",
            ))
        }
    }

    fn record_cleanup_retry(&self, id: &str, generation: u64) -> Result<(), ExtensionError> {
        let mut retries = self.inner.cleanup_retries.lock().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "cleanup retry lock poisoned")
        })?;
        let key = (id.to_owned(), generation);
        if let Some(attempts) = retries.get_mut(&key) {
            *attempts = attempts.saturating_add(1);
            return Ok(());
        }
        if retries.len() >= EXTENSION_CLEANUP_RETRY_LIMIT {
            return Err(ExtensionError::new(
                "cleanup_retry_full",
                "extension cleanup retry capacity is exhausted",
            ));
        }
        retries.insert(key, 1);
        Ok(())
    }

    fn retry_cleanup_tombstones(&self) {
        let tombstones = self
            .inner
            .cleanup_retries
            .lock()
            .map(|retries| retries.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for (id, generation) in tombstones {
            let _ = self.cleanup_generation(&id, generation, None);
        }
    }

    fn clear_cleanup_retry(&self, id: &str, generation: u64) {
        if let Ok(mut retries) = self.inner.cleanup_retries.lock() {
            retries.remove(&(id.to_owned(), generation));
        }
    }

    fn cleanup_generation(
        &self,
        id: &str,
        generation: u64,
        lifecycle_operation: Option<&str>,
    ) -> Result<(), ExtensionError> {
        if let Ok(mut confirmations) = self.inner.file_confirmations.lock() {
            confirmations.retain(|_, record| {
                record.confirmation.extension.as_ref() != id
                    || record.confirmation.generation != generation
            });
        }
        let mut command_reservations = self.inner.command_reservations.lock().ok();
        if let Some(reservations) = command_reservations.as_mut() {
            reservations.retain(|_, generation_ref| {
                generation_ref.extension_id != id || generation_ref.generation != generation
            });
        }
        let mut event_reservations = self.inner.event_reservations.lock().ok();
        if let Some(reservations) = event_reservations.as_mut() {
            reservations.retain(|_, generation_ref| {
                generation_ref.extension_id != id || generation_ref.generation != generation
            });
        }
        let command_ids = self
            .inner
            .state
            .read()
            .map(|state| {
                state
                    .commands
                    .iter()
                    .filter(|(_, command)| {
                        command.package.as_ref() == id && command.generation == generation
                    })
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let _ = self
            .command_registry()
            .unregister_extension_commands(id, generation);
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        if let Some(package) = state.packages.get_mut(id)
            && package.generation == generation
        {
            for command in &command_ids {
                package.commands.remove(command);
            }
        }
        for command in command_ids {
            state.commands.remove(&command);
        }
        let events = state
            .events
            .iter()
            .filter(|(_, registration)| {
                registration.generation.extension_id == id
                    && registration.generation.generation == generation
            })
            .map(|(topic, _)| topic.clone())
            .collect::<Vec<_>>();
        if let Some(package) = state.packages.get_mut(id)
            && package.generation == generation
        {
            for topic in &events {
                package.events.remove(topic);
            }
        }
        let subscriptions = state
            .subscriptions
            .iter()
            .filter(|(_, subscription)| {
                subscription.generation.extension_id == id
                    && subscription.generation.generation == generation
            })
            .map(|(id, subscription)| (id.clone(), subscription.owner.clone()))
            .collect::<Vec<_>>();
        let tasks = state
            .tasks
            .iter()
            .filter(|(_, binding)| {
                binding.resource.generation.extension_id == id
                    && binding.resource.generation.generation == generation
            })
            .map(|(task, binding)| (task.clone(), binding.resource.owner.clone()))
            .collect::<Vec<_>>();
        let metadata = state
            .metadata
            .iter()
            .filter(|(_, binding)| {
                binding.resource.generation.extension_id == id
                    && binding.resource.generation.generation == generation
            })
            .map(|(key, binding)| (key.clone(), binding.clone()))
            .collect::<Vec<_>>();
        let surfaces = state
            .surfaces
            .iter()
            .filter(|(_, surface)| {
                surface.generation.extension_id == id && surface.generation.generation == generation
            })
            .map(|(surface, record)| (surface.clone(), record.clone()))
            .collect::<Vec<_>>();
        let processes = state
            .processes
            .iter()
            .filter(|(_, process)| {
                process.generation.extension_id == id && process.generation.generation == generation
            })
            .map(|(process, record)| (process.clone(), Arc::clone(record)))
            .collect::<Vec<_>>();
        for topic in &events {
            state.events.remove(topic);
        }
        for (subscription, _) in &subscriptions {
            state.subscriptions.remove(subscription);
        }
        for (process, _) in &processes {
            state.processes.remove(process);
        }
        drop(state);
        for (_, process) in &processes {
            stop_process_record(process);
        }
        let workers = self
            .inner
            .lua_workers
            .lock()
            .map(|mut workers| {
                let mut selected = Vec::new();
                workers.retain(|worker| {
                    if worker.generation.extension_id == id
                        && worker.generation.generation == generation
                    {
                        selected.push(Arc::clone(worker));
                        false
                    } else {
                        true
                    }
                });
                selected
            })
            .unwrap_or_default();
        for worker in workers {
            worker.cancel_and_join();
        }
        for topic in events {
            let _ = self.inner.automation.unregister_event_topic(&topic);
        }
        for (subscription, owner) in subscriptions {
            let _ = self
                .inner
                .automation
                .events()
                .unsubscribe(&subscription, &owner);
        }
        let mut cleanup_error = None;
        for (_, record) in &metadata {
            let result = namespaced_metadata_namespace(id, &record.namespace)
                .map_err(|error| ExtensionError::new(error.code, error.message))
                .and_then(|namespace| {
                    self.inner
                        .automation
                        .metadata()
                        .clear(
                            &record.resource.scope,
                            &namespace,
                            &record.key,
                            record.target.as_ref(),
                            json!({
                                "extension_id": id,
                                "generation": generation,
                                "operation": "cleanup",
                            }),
                        )
                        .map(|_| ())
                        .map_err(automation_error)
                });
            if cleanup_error.is_none() {
                cleanup_error = result.err();
            }
        }
        for (task, owner) in &tasks {
            let result = if self.inner.shutdown.load(Ordering::Acquire) {
                self.inner
                    .automation
                    .tasks()
                    .terminate_force_checked(task, owner)
            } else {
                match self.inner.automation.tasks().status(task, owner) {
                    Ok(_) => self.inner.automation.tasks().terminate_checked(task),
                    Err(error) if error.code == -32602 => Ok(()),
                    Err(error) => Err(error),
                }
            };
            if let Err(error) = result
                && cleanup_error.is_none()
            {
                cleanup_error = Some(automation_error(error));
            }
        }
        if !self.inner.shutdown.load(Ordering::Acquire) {
            for (_, surface) in &surfaces {
                if let Err(error) =
                    self.publish_surface_lifecycle("closed", &surface.spec, &surface.generation)
                    && cleanup_error.is_none()
                {
                    cleanup_error = Some(error);
                }
            }
        }
        if let Some(error) = cleanup_error {
            self.record_cleanup_retry(id, generation)?;
            return Err(error);
        }
        let mut state = self.inner.state.write().map_err(|_| {
            ExtensionError::new("runtime_unavailable", "extension state lock poisoned")
        })?;
        for (task, _) in &tasks {
            state.tasks.remove(task);
        }
        for (metadata_key, _) in &metadata {
            state.metadata.remove(metadata_key);
        }
        for (surface, _) in &surfaces {
            state.surfaces.remove(surface);
        }
        state.storage.retain(|key, _| {
            key.resource.generation.extension_id != id
                || key.resource.generation.generation != generation
        });
        state.logs.retain(|resource, _| {
            resource.generation.extension_id != id || resource.generation.generation != generation
        });
        state.observation_metadata.retain(|resource, _| {
            resource.generation.extension_id != id || resource.generation.generation != generation
        });
        state
            .process_reservations
            .remove(&(id.to_owned(), generation));
        drop(state);
        if let Ok(mut reservations) = self.inner.task_reservations.lock() {
            reservations.retain(|_, generation_ref| {
                generation_ref.extension_id != id || generation_ref.generation != generation
            });
        }
        if !self.inner.shutdown.load(Ordering::Acquire)
            && let Some(operation) = lifecycle_operation
        {
            let snapshot = self.lifecycle_snapshot(Some((id, generation)))?;
            if let Err(error) = self.publish_lifecycle(id, generation, operation, snapshot)
                && !self.pending_lifecycle_exists(id, generation, operation)
            {
                return Err(error);
            }
        }
        self.clear_cleanup_retry(id, generation);
        Ok(())
    }

    fn remove_lua_worker(&self, worker: &Arc<LuaWorker>) {
        if let Ok(mut workers) = self.inner.lua_workers.lock() {
            workers.retain(|current| !Arc::ptr_eq(current, worker));
        }
    }

    fn process_for_owner(
        &self,
        process: &str,
        extension_id: &str,
        generation: u64,
    ) -> Result<Arc<ProcessRecord>, ExtensionError> {
        let record = self
            .inner
            .state
            .read()
            .map_err(|_| {
                ExtensionError::new("process_unavailable", "extension state lock poisoned")
            })?
            .processes
            .get(process)
            .cloned()
            .ok_or_else(|| ExtensionError::new("unknown_process", "process is not registered"))?;
        if record.generation.extension_id != extension_id
            || record.generation.generation != generation
        {
            return Err(ExtensionError::new(
                "stale_generation",
                "process belongs to a different extension generation",
            ));
        }
        self.package_generation(extension_id, generation)?;
        Ok(record)
    }

    fn remove_process_record(&self, process: &str, record: &Arc<ProcessRecord>) {
        if let Ok(mut state) = self.inner.state.write()
            && state
                .processes
                .get(process)
                .is_some_and(|current| Arc::ptr_eq(current, record))
        {
            state.processes.remove(process);
        }
    }

    fn release_process_reservation(&self, generation_ref: &(String, u64)) {
        if let Ok(mut state) = self.inner.state.write() {
            self.release_process_reservation_locked(&mut state, generation_ref);
        }
    }
    fn release_process_reservation_locked(
        &self,
        state: &mut RuntimeState,
        generation_ref: &(String, u64),
    ) {
        if let Some(reserved) = state.process_reservations.get_mut(generation_ref) {
            *reserved = reserved.saturating_sub(1);
            if *reserved == 0 {
                state.process_reservations.remove(generation_ref);
            }
        }
    }

    fn ensure_file_access(&self, _id: &str, path: &Path) -> Result<FileAccess, ExtensionError> {
        #[cfg(unix)]
        {
            let target = normalize_file_path(path)
                .map_err(|error| ExtensionError::new("file_denied", error.to_string()))?;
            let roots = self
                .inner
                .file_roots
                .read()
                .map_err(|_| ExtensionError::new("file_denied", "file roots lock poisoned"))?;
            if roots.is_empty() {
                return Err(ExtensionError::new(
                    "file_denied",
                    "no extension file root is configured",
                ));
            }
            for configured_root in roots.iter() {
                let Ok(root) = normalize_file_path(configured_root) else {
                    continue;
                };
                // The configured path is the lexical capability boundary.  Resolve that
                // boundary once for platforms such as macOS, where /var and /tmp are
                // symlinks, while keeping the requested path relative to the boundary.
                let canonical_root = fs::canonicalize(&root).ok();
                let relative = target.strip_prefix(&root).ok().or_else(|| {
                    canonical_root
                        .as_deref()
                        .and_then(|canonical| target.strip_prefix(canonical).ok())
                });
                let Some(relative) = relative else {
                    continue;
                };
                if relative.as_os_str().is_empty() {
                    continue;
                }
                let Ok(root) = open_file_root(&root) else {
                    continue;
                };
                return Ok(FileAccess {
                    root,
                    relative: relative.to_path_buf(),
                });
            }
            Err(ExtensionError::new(
                "file_denied",
                "path is not beneath an accessible extension file root",
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            Err(unsupported_file_api_error())
        }
    }

    fn publish_registry_changed(&self, operation: &str, id: &str, generation: u64, command: &str) {
        if let Some(scope) = self.inner.automation.instance_scope() {
            let _ = self.inner.automation.publish_event(EventPublication::new(
                scope,
                "command.registry_changed",
                json!({"extension_id": id, "generation": generation}),
                None,
                json!({"operation": operation, "command": command}),
            ));
        }
    }

    fn publish_surface_lifecycle(
        &self,
        operation: &str,
        spec: &SurfaceSpec,
        generation: &ExtensionGeneration,
    ) -> Result<(), ExtensionError> {
        let Some(scope) = self.inner.automation.instance_scope() else {
            return Ok(());
        };
        self.inner
            .automation
            .publish_event(EventPublication::new(
                scope,
                "extension.reloaded",
                json!({"extension_id": generation.extension_id, "generation": generation.generation}),
                None,
                json!({"surface": spec, "operation": operation}),
            ))
            .map(|_| ())
            .map_err(automation_error)
    }

    fn publish_progress(&self, command: &str, value: Value) -> Result<(), ExtensionError> {
        let Some(scope) = self.inner.automation.instance_scope() else {
            return Ok(());
        };
        self.inner
            .automation
            .publish_event(EventPublication::new(
                scope,
                "task.changed",
                json!({"command": command}),
                None,
                json!({"progress": value}),
            ))
            .map(|_| ())
            .map_err(automation_error)
    }
}

fn file_transaction_digest(transaction: &FileTransaction) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    transaction.confirmation_id.hash(&mut hasher);
    transaction.extension.as_ref().hash(&mut hasher);
    transaction.generation.hash(&mut hasher);
    transaction.preview.path.hash(&mut hasher);
    transaction.preview.existed.hash(&mut hasher);
    transaction.preview.before_bytes.hash(&mut hasher);
    transaction.preview.after_bytes.hash(&mut hasher);
    transaction.preview.changed.hash(&mut hasher);
    transaction.preview.destructive.hash(&mut hasher);
    transaction.contents.hash(&mut hasher);
    transaction.operation.hash(&mut hasher);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        transaction.snapshot.existed.hash(&mut hasher);
        if let Some(identity) = transaction.snapshot.identity {
            identity.device.hash(&mut hasher);
            identity.inode.hash(&mut hasher);
        }
        if let Some(version) = transaction.snapshot.version {
            version.size.hash(&mut hasher);
            version.modified_seconds.hash(&mut hasher);
            version.modified_nanos.hash(&mut hasher);
            version.changed_seconds.hash(&mut hasher);
            version.changed_nanos.hash(&mut hasher);
        }
        transaction.snapshot.bytes.hash(&mut hasher);
        if let Some(parent) = transaction.parent_identity {
            parent.device.hash(&mut hasher);
            parent.inode.hash(&mut hasher);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn file_transactions_digest(transactions: &[FileTransaction]) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for transaction in transactions {
        file_transaction_digest(transaction).hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

impl FileTransaction {
    #[must_use]
    pub fn preview(&self) -> &FileTransactionPreview {
        &self.preview
    }
    fn validate_current(&self) -> Result<(), ExtensionError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (parent, name) = open_file_parent(&self.root, &self.relative)
                .map_err(|error| file_conflict(format!("unable to open file: {error}")))?;
            match open_file_at(&parent, &name) {
                Ok(file) => {
                    let current =
                        snapshot_file(file).map_err(|error| file_conflict(error.to_string()))?;
                    if !self.snapshot.existed
                        || current.identity != self.snapshot.identity
                        || current.version != self.snapshot.version
                        || current.bytes != self.snapshot.bytes
                    {
                        return Err(file_conflict("file changed before batch apply"));
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::NotFound && !self.snapshot.existed => {}
                Err(error) => return Err(file_conflict(error.to_string())),
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(unsupported_file_api_error())
        }
    }

    pub fn apply(
        self,
        confirmation: FileConfirmation,
    ) -> Result<FileTransactionPreview, ExtensionError> {
        if confirmation.token.is_empty()
            || confirmation.transaction_id != self.confirmation_id
            || confirmation.extension != self.extension
            || confirmation.generation != self.generation
            || confirmation.preview != self.preview
            || confirmation.digest != file_transactions_digest(std::slice::from_ref(&self))
        {
            return Err(ExtensionError::new(
                "invalid_confirmation",
                "file confirmation does not match the exact transaction",
            ));
        }
        let record = self
            .runtime
            .inner
            .file_confirmations
            .lock()
            .map_err(|_| {
                ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
            })?
            .get(&confirmation.token)
            .cloned()
            .ok_or_else(|| {
                ExtensionError::new(
                    "invalid_confirmation",
                    "file confirmation token is unknown or already consumed",
                )
            })?;
        if record.confirmation != confirmation
            || record.transactions.len() != 1
            || record.transactions[0].confirmation_id != self.confirmation_id
        {
            return Err(ExtensionError::new(
                "invalid_confirmation",
                "file confirmation token does not match one exact transaction",
            ));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let transaction_runtime = self.runtime.clone_internal();
            let transaction_guard = transaction_runtime
                .inner
                .file_transaction_lock
                .lock()
                .map_err(|_| {
                    ExtensionError::new("file_unavailable", "file transaction lock poisoned")
                })?;
            self.validate_current()?;
            self.runtime
                .inner
                .file_confirmations
                .lock()
                .map_err(|_| {
                    ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
                })?
                .remove(&confirmation.token)
                .ok_or_else(|| {
                    ExtensionError::new(
                        "invalid_confirmation",
                        "file confirmation token is unknown or already consumed",
                    )
                })?;
            let commit = self.apply_committed_unlocked()?;
            let preview = commit.transaction.preview;
            drop(transaction_guard);
            Ok(preview)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            self.runtime
                .inner
                .file_confirmations
                .lock()
                .map_err(|_| {
                    ExtensionError::new("file_unavailable", "file confirmation lock poisoned")
                })?
                .remove(&confirmation.token)
                .ok_or_else(|| {
                    ExtensionError::new(
                        "invalid_confirmation",
                        "file confirmation token is unknown or already consumed",
                    )
                })?;
            self.apply_committed()
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn current_snapshot(&self) -> Result<FileSnapshot, ExtensionError> {
        let (parent, name) = open_file_parent(&self.root, &self.relative)
            .map_err(|error| file_conflict(format!("unable to open file: {error}")))?;
        let actual_parent =
            file_identity(&parent.metadata().map_err(|error| {
                file_conflict(format!("unable to validate file parent: {error}"))
            })?);
        if self.parent_identity != Some(actual_parent) {
            return Err(file_conflict("file parent changed before rollback"));
        }
        match open_file_at(&parent, &name) {
            Ok(file) => snapshot_file(file).map_err(|error| {
                file_conflict(format!("unable to read file for rollback: {error}"))
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileSnapshot {
                existed: false,
                identity: None,
                version: None,
                bytes: Vec::new(),
            }),
            Err(error) => Err(file_conflict(format!(
                "unable to inspect file for rollback: {error}"
            ))),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn apply_committed_unlocked(self) -> Result<FileTransactionCommit, ExtensionError> {
        self.runtime
            .package_generation(self.extension.as_ref(), self.generation)?;
        let post_snapshot = match self.operation {
            FileTransactionOperation::Write => apply_file_transaction(
                &self.root,
                &self.relative,
                self.parent_identity,
                &self.snapshot,
                &self.contents,
            )?,
            FileTransactionOperation::Remove => {
                let (parent, name) = open_file_parent(&self.root, &self.relative)
                    .map_err(|error| file_conflict(format!("unable to open file: {error}")))?;
                let Some(identity) = self.snapshot.identity else {
                    return Err(file_conflict("remove transaction target is missing"));
                };
                let Some(version) = self.snapshot.version else {
                    return Err(file_conflict("remove transaction version is missing"));
                };
                if !remove_file_if_matches(&parent, &name, identity, version, &self.snapshot.bytes)
                {
                    return Err(file_conflict("file changed before removal"));
                }
                unix_fs::fsync(&parent)
                    .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
                FileSnapshot {
                    existed: false,
                    identity: None,
                    version: None,
                    bytes: Vec::new(),
                }
            }
        };
        Ok(FileTransactionCommit {
            transaction: self,
            post_snapshot,
        })
    }
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl FileTransactionCommit {
    fn rollback(self) -> Result<(), ExtensionError> {
        let FileTransactionCommit {
            transaction,
            post_snapshot,
        } = self;
        let current = transaction.current_snapshot()?;
        if !snapshot_matches(&current, &post_snapshot) {
            return Err(file_conflict(
                "file changed concurrently; preserving current post-state",
            ));
        }
        if transaction.snapshot.existed {
            let _ = apply_file_transaction(
                &transaction.root,
                &transaction.relative,
                transaction.parent_identity,
                &post_snapshot,
                &transaction.snapshot.bytes,
            )?;
            Ok(())
        } else {
            let (parent, name) = open_file_parent(&transaction.root, &transaction.relative)
                .map_err(|error| file_conflict(format!("unable to open file: {error}")))?;
            let Some(identity) = post_snapshot.identity else {
                return Err(file_conflict("rollback post-state identity is missing"));
            };
            let Some(version) = post_snapshot.version else {
                return Err(file_conflict("rollback post-state version is missing"));
            };
            if !remove_file_if_matches(&parent, &name, identity, version, &post_snapshot.bytes) {
                return Err(file_conflict(
                    "file changed concurrently while removing rollback post-state",
                ));
            }
            unix_fs::fsync(&parent)
                .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
            Ok(())
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rollback_file_transactions(
    committed: Vec<FileTransactionCommit>,
) -> Vec<FileBatchRollbackError> {
    committed
        .into_iter()
        .rev()
        .filter_map(|commit| {
            let path = commit.transaction.preview.path.clone();
            commit.rollback().err().map(|error| FileBatchRollbackError {
                path,
                conflict: error.code == "file_conflict",
                code: error.code,
                message: error.message,
            })
        })
        .collect()
}

impl ExtensionRuntime {
    fn shutdown_and_cleanup(&self) {
        if self.inner.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        let generations = self
            .inner
            .state
            .write()
            .map(|state| {
                state
                    .packages
                    .iter()
                    .map(|(id, package)| {
                        let _ = package.generation_cancellation.request_cancel();
                        (id.clone(), package.generation)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (id, generation) in generations {
            let _ = self.cleanup_generation(&id, generation, None);
        }
        let workers = self
            .inner
            .lua_workers
            .lock()
            .map(|mut workers| std::mem::take(&mut *workers))
            .unwrap_or_default();
        for worker in workers {
            worker.cancel_and_join();
        }
        let processes = self
            .inner
            .state
            .write()
            .map(|mut state| {
                state.process_reservations.clear();
                std::mem::take(&mut state.processes)
                    .into_values()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for process in processes {
            stop_process_record(&process);
        }
    }
}

impl RuntimeInner {
    fn shutdown_and_cleanup(&self) {
        if self.shutdown.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut state) = self.state.write() {
            for package in state.packages.values() {
                let _ = package.generation_cancellation.request_cancel();
            }
            state.process_reservations.clear();
            std::mem::take(&mut state.processes)
                .into_values()
                .for_each(|process| stop_process_record(&process));
        }
        let workers = self
            .lua_workers
            .lock()
            .map(|mut workers| std::mem::take(&mut *workers))
            .unwrap_or_default();
        for worker in workers {
            worker.cancel_and_join();
        }
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        self.shutdown_and_cleanup();
    }
}

impl Drop for ExtensionRuntime {
    fn drop(&mut self) {
        if self.owner && self.inner.external_owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.shutdown_and_cleanup();
        }
    }
}

fn package_info(package: &PackageState) -> ExtensionPackageInfo {
    ExtensionPackageInfo {
        id: package.manifest.id.clone(),
        name: package.manifest.name.clone(),
        version: package.manifest.version.clone(),
        generation: package.generation,
        enabled: package.enabled,
        linked: package.linked,
        source: package
            .source
            .as_ref()
            .map(|path| path.display().to_string()),
        commands: package.commands.iter().cloned().collect(),
        events: package.events.iter().cloned().collect(),
    }
}

fn command_outcome_error(outcome: CommandOutcome) -> ExtensionError {
    match outcome {
        CommandOutcome::Failed { code, message } => ExtensionError::new(code, message),
        _ => ExtensionError::new(
            "command_registry",
            "extension command registration was rejected",
        ),
    }
}

fn automation_error(error: AutomationError) -> ExtensionError {
    ExtensionError::new(error.code.to_string(), error.message)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

fn extension_scope(id: &str, generation: u64) -> String {
    format!("extension:{id}:{generation}")
}

fn validate_package_state(
    state: &RuntimeState,
    id: &str,
    generation: u64,
) -> Result<(), ExtensionError> {
    let package = state
        .packages
        .get(id)
        .ok_or_else(|| ExtensionError::new("unknown_extension", "extension is not installed"))?;
    if !package.enabled {
        return Err(ExtensionError::new(
            "extension_disabled",
            "extension is disabled",
        ));
    }
    if package.generation != generation || package.generation_cancellation.is_cancelled() {
        return Err(ExtensionError::new(
            "stale_generation",
            "extension generation is stale",
        ));
    }
    Ok(())
}

fn validate_task_binding(
    state: &RuntimeState,
    task: &str,
    expected: &ExtensionResourceBinding,
) -> Result<(), ExtensionError> {
    let Some(binding) = state.tasks.get(task) else {
        return Err(ExtensionError::new(
            "unknown_task",
            "task is not an extension task",
        ));
    };
    if binding.resource.generation.extension_id != expected.generation.extension_id {
        return Err(ExtensionError::new(
            "task_owner_mismatch",
            "task belongs to another extension",
        ));
    }
    if binding.resource.generation.generation != expected.generation.generation {
        return Err(ExtensionError::new(
            "stale_generation",
            "task belongs to a stale extension generation",
        ));
    }
    if binding.resource.owner != expected.owner {
        return Err(ExtensionError::new(
            "task_owner_mismatch",
            "task belongs to another owner",
        ));
    }
    if binding.resource.scope != expected.scope {
        return Err(ExtensionError::new(
            "invalid_binding_scope",
            "task scope is not owned by this extension",
        ));
    }
    Ok(())
}

fn validate_metadata_target(
    id: &str,
    generation: u64,
    target: Option<&CommandTarget>,
) -> Result<(), ExtensionError> {
    if let Some(target) = target
        && (target.kind != ResourceKind::Extension
            || target.handle != id
            || target.generation != generation)
    {
        return Err(ExtensionError::new(
            "invalid_binding_target",
            "metadata target is not owned by this extension generation",
        ));
    }
    Ok(())
}

fn validate_metadata_provenance(
    id: &str,
    generation: u64,
    provenance: &Value,
) -> Result<(), ExtensionError> {
    let Some(object) = provenance.as_object() else {
        return Ok(());
    };
    if object
        .get("extension_id")
        .and_then(Value::as_str)
        .is_some_and(|extension_id| extension_id != id)
        || object
            .get("generation")
            .and_then(Value::as_u64)
            .is_some_and(|value| value != generation)
    {
        return Err(ExtensionError::new(
            "invalid_metadata_provenance",
            "metadata provenance does not match the invoking extension generation",
        ));
    }
    Ok(())
}

fn namespaced_metadata_namespace(id: &str, namespace: &str) -> Result<String, ExtensionError> {
    if namespace.is_empty()
        || namespace.len() > 96
        || namespace.contains('/')
        || namespace.contains('\\')
        || namespace == "."
        || namespace == ".."
    {
        return Err(ExtensionError::new(
            "invalid_metadata",
            "metadata namespace is invalid",
        ));
    }
    let namespace = format!("{id}:{namespace}");
    if namespace.len() > crate::automation::hub::METADATA_NAME_LIMIT {
        return Err(ExtensionError::new(
            "invalid_metadata",
            "metadata namespace is too long",
        ));
    }
    Ok(namespace)
}

fn package_storage_namespace(package: &PackageState) -> Result<Option<String>, ExtensionError> {
    package
        .manifest
        .storage_namespace
        .as_deref()
        .map(|namespace| {
            validate_storage_component(namespace)?;
            Ok(namespace.to_owned())
        })
        .transpose()
}

fn validate_storage_component(component: &str) -> Result<(), ExtensionError> {
    if component.is_empty()
        || component.len() > 128
        || Path::new(component).is_absolute()
        || component.contains('/')
        || component.contains('\\')
        || component == "."
        || component == ".."
        || !Path::new(component)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(ExtensionError::new(
            "invalid_storage_namespace",
            "storage namespace must be a single normal path component",
        ));
    }
    Ok(())
}

fn validate_storage_key(key: &str) -> Result<(), ExtensionError> {
    validate_storage_component(key).map_err(|_| {
        ExtensionError::new(
            "invalid_storage_key",
            "storage key must be a single normal path component",
        )
    })
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported_file_api_error() -> ExtensionError {
    ExtensionError::new(
        "file_unsupported",
        "extension file access is unavailable on this platform",
    )
}

#[cfg(unix)]
fn normalize_file_path(path: &Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension file paths must be absolute",
        ));
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "parent traversal is not allowed",
                ));
            }
            std::path::Component::Normal(component) => normalized.push(component),
        }
    }
    Ok(normalized)
}

#[cfg(unix)]
fn directory_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(unix)]
fn file_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

#[cfg(unix)]
fn file_from_owned_fd(file: std::os::fd::OwnedFd) -> fs::File {
    file.into()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn temporary_open_flags() -> OFlags {
    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC
}
#[cfg(unix)]
fn open_file_root(path: &Path) -> std::io::Result<Arc<FileRootCapability>> {
    // macOS exposes common temporary directories through symlinked aliases
    // (/var -> /private/var, /tmp -> /private/tmp).  Resolve only the
    // explicitly granted root; descendants are still opened with O_NOFOLLOW.
    let path = fs::canonicalize(path)?;
    let root = unix_fs::open(Path::new("/"), directory_open_flags(), Mode::empty())
        .map_err(std::io::Error::from)?;
    let mut directory = file_from_owned_fd(root);
    for component in path.components() {
        let std::path::Component::Normal(component) = component else {
            continue;
        };
        let child = unix_fs::openat(&directory, component, directory_open_flags(), Mode::empty())
            .map_err(std::io::Error::from)?;
        directory = file_from_owned_fd(child);
    }
    Ok(Arc::new(FileRootCapability { directory }))
}

#[cfg(unix)]
fn file_components(relative: &Path) -> std::io::Result<(Vec<OsString>, OsString)> {
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "extension file path contains a non-normal component",
            ));
        };
        components.push(component.to_os_string());
    }
    let name = components.pop().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension file path has no file name",
        )
    })?;
    Ok((components, name))
}

#[cfg(unix)]
fn open_file_parent(
    root: &FileRootCapability,
    relative: &Path,
) -> std::io::Result<(fs::File, OsString)> {
    let (components, name) = file_components(relative)?;
    let mut directory = root.directory.try_clone()?;
    for component in components {
        let child = unix_fs::openat(&directory, component, directory_open_flags(), Mode::empty())
            .map_err(std::io::Error::from)?;
        directory = file_from_owned_fd(child);
    }
    Ok((directory, name))
}

#[cfg(unix)]
fn open_file_at(parent: &fs::File, name: &OsString) -> std::io::Result<fs::File> {
    let file = unix_fs::openat(parent, name, file_open_flags(), Mode::empty())
        .map_err(std::io::Error::from)?;
    Ok(file_from_owned_fd(file))
}

#[cfg(unix)]
fn read_open_file(file: &mut fs::File) -> std::io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension file target is not a regular file",
        ));
    }
    let capacity = usize::try_from(metadata.len().min(EXTENSION_FILE_BYTES as u64 + 1))
        .unwrap_or(EXTENSION_FILE_BYTES + 1);
    let mut bytes = Vec::with_capacity(capacity);
    file.take(EXTENSION_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > EXTENSION_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds host read limit",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_file_contents(mut file: fs::File) -> Result<Vec<u8>, ExtensionError> {
    read_open_file(&mut file).map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            ExtensionError::new("file_too_large", error.to_string())
        } else {
            ExtensionError::new("file_read_failed", error.to_string())
        }
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_version(metadata: &fs::Metadata) -> FileVersion {
    FileVersion {
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanos: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanos: metadata.ctime_nsec(),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_file(mut file: fs::File) -> std::io::Result<FileSnapshot> {
    let before = file.metadata()?;
    if !before.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension file target is not a regular file",
        ));
    }
    let bytes = read_open_file(&mut file)?;
    let after = file.metadata()?;
    let before_identity = file_identity(&before);
    let after_identity = file_identity(&after);
    let before_version = file_version(&before);
    let after_version = file_version(&after);
    if before_identity != after_identity || before_version != after_version {
        return Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "extension file changed while it was being read",
        ));
    }
    Ok(FileSnapshot {
        existed: true,
        identity: Some(after_identity),
        version: Some(after_version),
        bytes,
    })
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_conflict(message: impl Into<String>) -> ExtensionError {
    ExtensionError::new("file_conflict", message)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn invoke_file_transaction_pre_commit_hook() {
    let hook = FILE_TRANSACTION_PRE_COMMIT
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn invoke_file_transaction_post_exchange_hook() {
    let hook = FILE_TRANSACTION_POST_EXCHANGE
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn invoke_file_transaction_pre_rollback_hook() {
    let hook = FILE_TRANSACTION_PRE_ROLLBACK
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        hook();
    }
}
#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
fn invoke_process_spawn_before_commit_hook(record: &Arc<ProcessRecord>) {
    let hook = PROCESS_SPAWN_BEFORE_COMMIT
        .lock()
        .ok()
        .and_then(|hook| hook.clone());
    if let Some(hook) = hook {
        hook(record);
    }
}

#[cfg(test)]
fn invoke_task_start_before_commit_hook() {
    let hook = TASK_START_BEFORE_COMMIT
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_committed_target(
    parent: &fs::File,
    name: &OsString,
    identity: FileIdentity,
    version: FileVersion,
    contents: &[u8],
) -> Result<(), ExtensionError> {
    let file = open_file_at(parent, name)
        .map_err(|error| file_conflict(format!("unable to validate committed file: {error}")))?;
    let current = snapshot_file(file)
        .map_err(|error| file_conflict(format!("unable to validate committed file: {error}")))?;
    if current.identity != Some(identity)
        || !current
            .version
            .is_some_and(|current| file_content_version_matches(&current, &version))
        || current.bytes != contents
    {
        return Err(file_conflict("target changed during file commit"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_displaced_target(
    parent: &fs::File,
    name: &OsString,
    expected: &FileSnapshot,
) -> Result<(), ExtensionError> {
    let file = open_file_at(parent, name)
        .map_err(|error| file_conflict(format!("unable to validate displaced file: {error}")))?;
    let current = snapshot_file(file)
        .map_err(|error| file_conflict(format!("unable to validate displaced file: {error}")))?;
    if !expected.existed
        || current.identity != expected.identity
        || !current
            .version
            .zip(expected.version)
            .is_some_and(|(current, expected)| file_content_version_matches(&current, &expected))
        || current.bytes != expected.bytes
    {
        return Err(file_conflict("file changed during atomic commit"));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn validate_snapshot(
    parent: &fs::File,
    name: &OsString,
    expected: &FileSnapshot,
) -> Result<(), ExtensionError> {
    match open_file_at(parent, name) {
        Ok(file) => {
            if !expected.existed {
                return Err(file_conflict("file was created after preview"));
            }
            let current = snapshot_file(file)
                .map_err(|error| file_conflict(format!("unable to validate file: {error}")))?;
            if current.identity != expected.identity
                || current.version != expected.version
                || current.bytes != expected.bytes
            {
                return Err(file_conflict("file changed after preview"));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !expected.existed => Ok(()),
        Err(error) => Err(file_conflict(format!("unable to validate file: {error}"))),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_file_at(parent: &fs::File, name: &OsString) -> Option<FileSnapshot> {
    open_file_at(parent, name)
        .ok()
        .and_then(|file| snapshot_file(file).ok())
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn snapshot_matches(left: &FileSnapshot, right: &FileSnapshot) -> bool {
    left.existed == right.existed
        && left.identity == right.identity
        && left.version == right.version
        && left.bytes == right.bytes
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn file_content_version_matches(left: &FileVersion, right: &FileVersion) -> bool {
    left.size == right.size
        && left.modified_seconds == right.modified_seconds
        && left.modified_nanos == right.modified_nanos
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn next_file_sidecar(prefix: &str, attempt: usize) -> OsString {
    let sequence = NEXT_EXTENSION_FILE_TEMP.fetch_add(1, Ordering::Relaxed);
    OsString::from(format!(
        ".bootty-{prefix}-{}-{sequence}-{attempt}",
        std::process::id()
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn remove_file_if_matches(
    parent: &fs::File,
    name: &OsString,
    identity: FileIdentity,
    version: FileVersion,
    contents: &[u8],
) -> bool {
    for attempt in 0..128 {
        let sidecar = next_file_sidecar("cleanup", attempt);
        match unix_fs::renameat_with(parent, name, parent, &sidecar, RenameFlags::NOREPLACE) {
            Ok(()) => {
                let matches = open_file_at(parent, &sidecar)
                    .ok()
                    .and_then(|file| snapshot_file(file).ok())
                    .is_some_and(|snapshot| {
                        snapshot.identity == Some(identity)
                            && snapshot.version.is_some_and(|current| {
                                file_content_version_matches(&current, &version)
                            })
                            && snapshot.bytes == contents
                    });
                if matches {
                    if unix_fs::unlinkat(parent, &sidecar, AtFlags::empty()).is_ok() {
                        return true;
                    }
                    let _ = unix_fs::renameat_with(
                        parent,
                        &sidecar,
                        parent,
                        name,
                        RenameFlags::NOREPLACE,
                    );
                    return false;
                }
                // A mismatching inode must never be left detached from its
                // original name. Restore it unless a concurrent writer has
                // already claimed that name; in that case the sidecar itself
                // is the durable preservation of the unknown file.
                let _ =
                    unix_fs::renameat_with(parent, &sidecar, parent, name, RenameFlags::NOREPLACE);
                return false;
            }
            Err(error) => {
                let error = std::io::Error::from(error);
                if error.kind() == std::io::ErrorKind::NotFound {
                    return true;
                }
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return false;
                }
            }
        }
    }
    false
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn preserve_unknown_file(parent: &fs::File, name: &OsString) -> bool {
    for attempt in 0..128 {
        let recovery = next_file_sidecar("recovered", attempt);
        match unix_fs::renameat_with(parent, name, parent, &recovery, RenameFlags::NOREPLACE) {
            Ok(()) => return true,
            Err(error) => {
                let error = std::io::Error::from(error);
                if error.kind() == std::io::ErrorKind::NotFound {
                    return true;
                }
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return false;
                }
            }
        }
    }
    false
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn create_temporary_file(
    parent: &fs::File,
    name: &OsString,
) -> std::io::Result<(fs::File, OsString)> {
    let stem = name.to_string_lossy();
    for _ in 0..128 {
        let sequence = NEXT_EXTENSION_FILE_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = OsString::from(format!(
            ".{stem}.bootty-{}-{sequence}.tmp",
            std::process::id()
        ));
        match unix_fs::openat(
            parent,
            &temporary,
            temporary_open_flags(),
            Mode::from_raw_mode(0o600),
        ) {
            Ok(file) => return Ok((file_from_owned_fd(file), temporary)),
            Err(error) => {
                let error = std::io::Error::from(error);
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "unable to allocate an extension file temporary",
    ))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn apply_file_transaction(
    root: &FileRootCapability,
    relative: &Path,
    expected_parent: Option<FileIdentity>,
    expected: &FileSnapshot,
    contents: &[u8],
) -> Result<FileSnapshot, ExtensionError> {
    let (parent, name) = open_file_parent(root, relative)
        .map_err(|error| file_conflict(format!("unable to open file parent: {error}")))?;
    let actual_parent = file_identity(
        &parent
            .metadata()
            .map_err(|error| file_conflict(format!("unable to validate file parent: {error}")))?,
    );
    if expected_parent != Some(actual_parent) {
        return Err(file_conflict("file parent changed after preview"));
    }
    validate_snapshot(&parent, &name, expected)?;

    let (mut temporary, temporary_name) = create_temporary_file(&parent, &name)
        .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
    let mut temporary_identity_for_cleanup: Option<(FileIdentity, FileVersion)> = None;
    let result = (|| {
        temporary
            .write_all(contents)
            .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
        unix_fs::fsync(&temporary)
            .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
        let temporary_metadata = temporary
            .metadata()
            .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
        let temporary_identity = file_identity(&temporary_metadata);
        let temporary_version = file_version(&temporary_metadata);
        temporary_identity_for_cleanup = Some((temporary_identity, temporary_version));
        drop(temporary);

        validate_snapshot(&parent, &name, expected)?;
        #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
        invoke_file_transaction_pre_commit_hook();
        let (reopened_parent, reopened_name) = open_file_parent(root, relative)
            .map_err(|error| file_conflict(format!("unable to revalidate file parent: {error}")))?;
        let reopened_identity = file_identity(&reopened_parent.metadata().map_err(|error| {
            file_conflict(format!("unable to revalidate file parent: {error}"))
        })?);
        if reopened_identity != actual_parent || reopened_name != name {
            return Err(file_conflict("file parent changed during commit"));
        }
        validate_committed_target(
            &parent,
            &temporary_name,
            temporary_identity,
            temporary_version,
            contents,
        )?;
        if expected.existed {
            unix_fs::renameat_with(
                &parent,
                &temporary_name,
                &parent,
                &name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
            #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
            invoke_file_transaction_post_exchange_hook();
            let displaced = validate_displaced_target(&parent, &temporary_name, expected);
            let committed = validate_committed_target(
                &parent,
                &name,
                temporary_identity,
                temporary_version,
                contents,
            );
            if let Err(error) = committed {
                if let (Some(identity), Some(version)) = (expected.identity, expected.version) {
                    let _ = remove_file_if_matches(
                        &parent,
                        &temporary_name,
                        identity,
                        version,
                        &expected.bytes,
                    );
                }
                return Err(error);
            }
            if let Err(error) = displaced {
                if let Err(revalidation_error) = validate_committed_target(
                    &parent,
                    &name,
                    temporary_identity,
                    temporary_version,
                    contents,
                ) {
                    if let (Some(identity), Some(version)) = (expected.identity, expected.version) {
                        let _ = remove_file_if_matches(
                            &parent,
                            &temporary_name,
                            identity,
                            version,
                            &expected.bytes,
                        );
                    }
                    return Err(revalidation_error);
                }
                #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
                invoke_file_transaction_pre_rollback_hook();
                unix_fs::renameat_with(
                    &parent,
                    &temporary_name,
                    &parent,
                    &name,
                    RenameFlags::EXCHANGE,
                )
                .map_err(|rollback_error| {
                    ExtensionError::new(
                        "file_write_failed",
                        format!("unable to restore file after conflict: {rollback_error}"),
                    )
                })?;

                let target_after_rollback = snapshot_file_at(&parent, &name);
                let temporary_after_rollback = snapshot_file_at(&parent, &temporary_name);
                let temporary_is_new = temporary_after_rollback.as_ref().is_some_and(|snapshot| {
                    snapshot.identity == Some(temporary_identity)
                        && snapshot.version.is_some_and(|current| {
                            file_content_version_matches(&current, &temporary_version)
                        })
                        && snapshot.bytes == contents
                });
                if !temporary_is_new {
                    if let (Some(target_before_second), Some(temporary_before_second)) = (
                        target_after_rollback.clone(),
                        temporary_after_rollback.clone(),
                    ) {
                        let target_stable =
                            snapshot_file_at(&parent, &name).is_some_and(|snapshot| {
                                snapshot_matches(&snapshot, &target_before_second)
                            });
                        let temporary_stable = snapshot_file_at(&parent, &temporary_name)
                            .is_some_and(|snapshot| {
                                snapshot_matches(&snapshot, &temporary_before_second)
                            });
                        if target_stable && temporary_stable {
                            if unix_fs::renameat_with(
                                &parent,
                                &temporary_name,
                                &parent,
                                &name,
                                RenameFlags::EXCHANGE,
                            )
                            .is_ok()
                            {
                                let temporary_after_second =
                                    snapshot_file_at(&parent, &temporary_name);
                                if let Some(snapshot) = temporary_after_second {
                                    let removed = expected
                                        .identity
                                        .zip(expected.version)
                                        .is_some_and(|(identity, version)| {
                                            snapshot.identity == Some(identity)
                                                && snapshot.version.is_some_and(|current| {
                                                    file_content_version_matches(&current, &version)
                                                })
                                                && snapshot.bytes == expected.bytes
                                                && remove_file_if_matches(
                                                    &parent,
                                                    &temporary_name,
                                                    identity,
                                                    version,
                                                    &expected.bytes,
                                                )
                                        })
                                        || (snapshot.identity == Some(temporary_identity)
                                            && snapshot.version.is_some_and(|current| {
                                                file_content_version_matches(
                                                    &current,
                                                    &temporary_version,
                                                )
                                            })
                                            && snapshot.bytes == contents
                                            && remove_file_if_matches(
                                                &parent,
                                                &temporary_name,
                                                temporary_identity,
                                                temporary_version,
                                                contents,
                                            ));
                                    if !removed && !preserve_unknown_file(&parent, &temporary_name)
                                    {
                                        return Err(file_conflict(
                                            "unable to preserve concurrent file during rollback",
                                        ));
                                    }
                                }
                            } else {
                                let _ = preserve_unknown_file(&parent, &temporary_name);
                            }
                        } else {
                            let _ = preserve_unknown_file(&parent, &temporary_name);
                        }
                    } else {
                        let _ = preserve_unknown_file(&parent, &temporary_name);
                    }
                }
                return Err(error);
            }
            if let (Some(identity), Some(version)) = (expected.identity, expected.version)
                && !remove_file_if_matches(
                    &parent,
                    &temporary_name,
                    identity,
                    version,
                    &expected.bytes,
                )
            {
                return Err(file_conflict("temporary file changed during commit"));
            }
        } else {
            unix_fs::renameat_with(
                &parent,
                &temporary_name,
                &parent,
                &name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                let error = std::io::Error::from(error);
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    file_conflict("file was created during commit")
                } else {
                    ExtensionError::new("file_write_failed", error.to_string())
                }
            })?;
        }
        unix_fs::fsync(&parent)
            .map_err(|error| ExtensionError::new("file_write_failed", error.to_string()))?;
        let post_snapshot = snapshot_file_at(&parent, &name)
            .ok_or_else(|| file_conflict("unable to snapshot committed file"))?;
        if !post_snapshot.existed
            || post_snapshot.identity != Some(temporary_identity)
            || !post_snapshot
                .version
                .is_some_and(|current| file_content_version_matches(&current, &temporary_version))
            || post_snapshot.bytes != contents
        {
            return Err(file_conflict("target changed during commit"));
        }
        Ok(post_snapshot)
    })();
    if result.is_err()
        && let Some((identity, version)) = temporary_identity_for_cleanup
    {
        let _ = remove_file_if_matches(&parent, &temporary_name, identity, version, contents);
    }
    result
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), ExtensionError> {
    let parent = path.parent().ok_or_else(|| {
        ExtensionError::new("storage_unavailable", "target has no parent directory")
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    let temporary = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        file.write_all(bytes)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        file.sync_all()
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        drop(file);
        fs::rename(&temporary, path)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn storage_directory(root: &Path, id: &str, namespace: &str) -> Result<PathBuf, ExtensionError> {
    let package_id = ExtensionPackageId::new(id.to_owned())?;
    validate_storage_component(namespace)?;
    fs::create_dir_all(root)
        .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
    let package_directory = canonical_root.join(package_id.as_ref());
    let namespace_directory = package_directory.join(namespace);
    fs::create_dir_all(&namespace_directory)
        .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
    let canonical_directory = namespace_directory
        .canonicalize()
        .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
    if !canonical_directory.starts_with(&canonical_root) {
        return Err(ExtensionError::new(
            "storage_unavailable",
            "storage path escapes the extension storage root",
        ));
    }
    let relative = canonical_directory
        .strip_prefix(&canonical_root)
        .map_err(|_| {
            ExtensionError::new("storage_unavailable", "storage path is outside the root")
        })?;
    let mut current = canonical_root.clone();
    for component in relative.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| ExtensionError::new("storage_unavailable", error.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(ExtensionError::new(
                "storage_unavailable",
                "symlink traversal is denied for extension storage",
            ));
        }
    }
    Ok(canonical_directory)
}

fn storage_path(
    root: &Path,
    id: &str,
    namespace: &str,
    key: &str,
) -> Result<PathBuf, ExtensionError> {
    validate_storage_key(key)?;
    let directory = storage_directory(root, id, namespace)?;
    let path = directory.join(format!("{key}.json"));
    if let Ok(metadata) = fs::symlink_metadata(&path)
        && metadata.file_type().is_symlink()
    {
        return Err(ExtensionError::new(
            "storage_unavailable",
            "symlink traversal is denied for extension storage",
        ));
    }
    Ok(path)
}

fn process_is_finished(record: &Arc<ProcessRecord>) -> bool {
    record
        .child
        .lock()
        .ok()
        .and_then(|mut child| child.try_wait().ok())
        .flatten()
        .is_some()
}

fn stop_process_record(record: &Arc<ProcessRecord>) {
    if let Ok(mut child) = record.child.lock() {
        terminate_process_tree(&mut child);
    }
    if let Ok(mut stdin) = record.stdin.lock() {
        stdin.take();
    }
    join_process_readers(record);
}

fn terminate_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status();
        let deadline = Instant::now() + Duration::from_millis(100);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(4));
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
        thread::sleep(Duration::from_millis(4));
    }
    let _ = child.try_wait();
}

fn join_process_readers(record: &Arc<ProcessRecord>) {
    let deadline = Instant::now() + Duration::from_millis(150);
    loop {
        let finished = record
            .readers
            .lock()
            .map(|readers| readers.iter().all(thread::JoinHandle::is_finished))
            .unwrap_or(true);
        if finished || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(4));
    }
    if let Ok(mut readers) = record.readers.lock() {
        for reader in std::mem::take(&mut *readers) {
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

fn join_thread_bounded(handle: &Mutex<Option<thread::JoinHandle<()>>>, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let finished = handle
            .lock()
            .ok()
            .and_then(|handle| handle.as_ref().map(thread::JoinHandle::is_finished))
            .unwrap_or(true);
        if finished || Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(4));
    }
    if let Ok(mut handle) = handle.lock()
        && handle.as_ref().is_some_and(thread::JoinHandle::is_finished)
        && let Some(handle) = handle.take()
    {
        let _ = handle.join();
    }
}

fn append_process_line(record: &Arc<ProcessRecord>, stream: &str, bytes: &[u8]) {
    let line = String::from_utf8_lossy(bytes)
        .trim_end_matches('\r')
        .to_owned();
    if line.len() > EXTENSION_PROCESS_BYTES {
        return;
    }
    let parsed = serde_json::from_str::<Value>(&line);
    let entry = ProcessLine {
        stream: stream.to_owned(),
        line: line.clone(),
        sequence: record.next_sequence.fetch_add(1, Ordering::Relaxed),
        value: parsed.as_ref().ok().cloned(),
        error: parsed.err().map(|error| error.to_string()),
    };
    if let Ok(mut queue) = record.output.lock()
        && let Ok(mut bytes) = record.output_bytes.lock()
    {
        *bytes += line.len();
        queue.push_back(entry);
        while queue.len() > EXTENSION_PROCESS_LINES || *bytes > EXTENSION_PROCESS_BYTES {
            if let Some(removed) = queue.pop_front() {
                *bytes = bytes.saturating_sub(removed.line.len());
            } else {
                break;
            }
        }
    }
}

fn spawn_process_reader<R: Read + Send + 'static>(
    reader: Option<R>,
    stream: &'static str,
    record: &Arc<ProcessRecord>,
) -> Option<thread::JoinHandle<()>> {
    let mut reader = reader?;
    let record = Arc::clone(record);
    Some(thread::spawn(move || {
        let mut chunk = [0_u8; 8192];
        let mut pending = Vec::new();
        loop {
            let read = match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => read,
            };
            pending.extend_from_slice(&chunk[..read]);
            while let Some(index) = pending.iter().position(|byte| *byte == b'\n') {
                let line = pending.drain(..=index).collect::<Vec<_>>();
                append_process_line(&record, stream, &line[..line.len().saturating_sub(1)]);
            }
            if !pending.is_empty() {
                append_process_line(&record, stream, &pending);
                pending.clear();
            }
        }
        if !pending.is_empty() {
            append_process_line(&record, stream, &pending);
        }
    }))
}

/// Register generic `bootty.commands.register` and `bootty.events.register`
/// helpers in a dedicated Luau VM. The returned generation is owned by the
/// package and command callbacks stay on this VM's worker thread.
struct LuaCall {
    invocation: CommandInvocation,
    deadline: Instant,
    cancellation: CommandCancellation,
    generation_cancellation: CommandCancellation,
    response: mpsc::Sender<Result<Value, ExtensionError>>,
}

/// Register generic `bootty.commands.register` and `bootty.events.register`
/// helpers in a dedicated Luau VM. The VM stays on its own worker thread;
/// command handlers communicate with it through a bounded message channel.
pub fn register_luau_package(
    runtime: &ExtensionRuntime,
    id: &str,
    generation: u64,
    source: &str,
) -> Result<(), ExtensionError> {
    let generation_cancellation = runtime.package_generation(id, generation)?;
    let module_root = runtime
        .package_source_root(id)?
        .canonicalize()
        .map_err(|error| ExtensionError::new("extension_source_missing", error.to_string()))?;
    let (registration_tx, registration_rx) =
        mpsc::sync_channel::<Result<(Vec<CommandDescriptor>, Vec<String>), ExtensionError>>(1);
    let (call_tx, call_rx) = mpsc::sync_channel::<LuaCall>(EXTENSION_COMMAND_QUEUE_LIMIT);
    let package_id = id.to_owned();
    let package_source = source.to_owned();
    let worker_cancellation = CommandCancellation::new();
    let worker = Arc::new(LuaWorker {
        generation: ExtensionGeneration {
            extension_id: id.to_owned(),
            generation,
        },
        cancellation: worker_cancellation.clone(),
        handle: Mutex::new(None),
    });
    if let Ok(mut workers) = runtime.inner.lua_workers.lock() {
        workers.push(Arc::clone(&worker));
    } else {
        return Err(ExtensionError::new(
            "luau_worker_failed",
            "worker registry lock poisoned",
        ));
    }
    let thread_cancellation = worker_cancellation.clone();
    let thread_generation_cancellation = generation_cancellation.clone();
    let host_runtime = runtime.clone_internal();
    let handle = match thread::Builder::new()
        .name(format!("bootty-luau-{id}"))
        .spawn(move || {
            let lua = Lua::new();
            let commands = match lua.create_table() {
                Ok(table) => table,
                Err(_) => return,
            };
            let events = match lua.create_table() {
                Ok(table) => table,
                Err(_) => return,
            };
            let registrations = Rc::new(RefCell::new(Vec::<(CommandDescriptor, Function)>::new()));
            let event_names = Arc::new(Mutex::new(Vec::<String>::new()));
            let registration_sink = Rc::clone(&registrations);
            let register = match lua.create_function(move |_, descriptor: Table| {
                let function = descriptor.get::<Function>("handler")?;
                let command_id = descriptor
                    .get::<String>("id")
                    .or_else(|_| descriptor.get::<String>("name"))?;
                let title = descriptor
                    .get::<String>("title")
                    .unwrap_or_else(|_| command_id.clone());
                let description = descriptor.get::<String>("description").unwrap_or_default();
                let parsed = lua_descriptor(&descriptor, command_id, title, description)?;
                registration_sink
                    .try_borrow_mut()
                    .map_err(|_| mlua::Error::external("registration storage borrowed"))?
                    .push((parsed, function));
                Ok(())
            }) {
                Ok(function) => function,
                Err(_) => return,
            };
            let event_sink = Arc::clone(&event_names);
            let event_register = match lua.create_function(move |_, name: String| {
                event_sink
                    .lock()
                    .map_err(|_| mlua::Error::external("event registration lock poisoned"))?
                    .push(name);
                Ok(())
            }) {
                Ok(function) => function,
                Err(_) => return,
            };
            if commands.set("register", register).is_err()
                || events.set("register", event_register).is_err()
            {
                return;
            }
            let bootty = match lua.create_table() {
                Ok(table) => table,
                Err(_) => return,
            };
            if bootty.set("commands", commands).is_err() || bootty.set("events", events).is_err() {
                let _ = registration_tx.send(Err(ExtensionError::new(
                    "luau_host_api_failed",
                    "unable to install command and event registration tables",
                )));
                return;
            }
            let host_runtime = host_runtime;
            if let Err(error) = install_lua_host_api(
                &lua,
                &bootty,
                host_runtime,
                &package_id,
                generation,
                thread_cancellation.clone(),
                thread_generation_cancellation.clone(),
            ) {
                let _ = registration_tx.send(Err(ExtensionError::new(
                    "luau_host_api_failed",
                    error.to_string(),
                )));
                return;
            }
            let module_root = module_root;
            let module_cache = Arc::new(Mutex::new(BTreeMap::<String, mlua::RegistryKey>::new()));
            let require =
                match lua.create_function(move |lua, name: String| -> mlua::Result<LuaValue> {
                    if name.is_empty() || name.len() > 128 {
                        return Err(mlua::Error::external("invalid module name"));
                    }
                    let relative = PathBuf::from(&name);
                    if relative
                        .components()
                        .any(|component| matches!(component, std::path::Component::ParentDir))
                    {
                        return Err(mlua::Error::external("module path traversal denied"));
                    }
                    {
                        let cache = module_cache
                            .lock()
                            .map_err(|_| mlua::Error::external("module cache lock poisoned"))?;
                        if let Some(key) = cache.get(&name) {
                            return lua.registry_value::<LuaValue>(key);
                        }
                    }
                    let mut path = module_root.join(&relative);
                    if path.extension().is_none() {
                        let luau = path.with_extension("luau");
                        let lua_path = path.with_extension("lua");
                        path = if luau.exists() { luau } else { lua_path };
                    }
                    let canonical = path
                        .canonicalize()
                        .map_err(|_| mlua::Error::external("module not found"))?;
                    if !canonical.starts_with(&module_root) {
                        return Err(mlua::Error::external("module path escapes package root"));
                    }
                    let bytes = fs::read(&canonical)
                        .map_err(|error| mlua::Error::external(error.to_string()))?;
                    if bytes.len() > EXTENSION_FILE_BYTES {
                        return Err(mlua::Error::external("module exceeds host read limit"));
                    }
                    let source = String::from_utf8(bytes)
                        .map_err(|_| mlua::Error::external("module is not valid UTF-8"))?;
                    let value: LuaValue = lua
                        .load(&source)
                        .set_name(canonical.to_string_lossy().into_owned())
                        .eval()?;
                    let key = lua.create_registry_value(value)?;
                    let result = lua.registry_value::<LuaValue>(&key)?;
                    module_cache
                        .lock()
                        .map_err(|_| mlua::Error::external("module cache lock poisoned"))?
                        .insert(name, key);
                    Ok(result)
                }) {
                    Ok(function) => function,
                    Err(_) => return,
                };
            if lua.globals().set("require", require).is_err() {
                return;
            }
            if lua.globals().set("bootty", bootty).is_err() {
                return;
            }
            let deadline = Instant::now() + EXTENSION_LUA_LOAD_TIMEOUT;
            let cancellation = thread_cancellation.clone();
            let generation_cancellation = thread_generation_cancellation.clone();
            lua.set_interrupt(move |_| {
                if cancellation.is_cancel_requested()
                    || generation_cancellation.is_cancel_requested()
                {
                    return Err(mlua::Error::external("cancelled"));
                }
                if Instant::now() >= deadline {
                    return Err(mlua::Error::external("deadline_exceeded"));
                }
                Ok(VmState::Continue)
            });
            let loaded = lua.load(&package_source).set_name(&package_id).exec();
            lua.remove_interrupt();
            if let Err(error) = loaded {
                let message = error.to_string();
                let code = if message.contains("deadline_exceeded") {
                    "luau_registration_timeout"
                } else if message.contains("cancelled") {
                    "cancelled"
                } else {
                    "luau_load_failed"
                };
                let _ = registration_tx.send(Err(ExtensionError::new(code, message)));
                return;
            }
            let registrations = match registrations.try_borrow_mut() {
                Ok(mut registrations) => std::mem::take(&mut *registrations),
                Err(_) => return,
            };
            let event_names = event_names
                .lock()
                .map(|names| names.clone())
                .unwrap_or_default();
            let descriptors = registrations
                .iter()
                .map(|(descriptor, _)| descriptor.clone())
                .collect::<Vec<_>>();
            if registration_tx
                .send(Ok((descriptors, event_names)))
                .is_err()
            {
                return;
            }
            let handlers = registrations
                .into_iter()
                .map(|(descriptor, function)| (descriptor.id, function))
                .collect::<BTreeMap<_, _>>();
            while !thread_cancellation.is_cancel_requested() {
                let call = match call_rx.recv_timeout(EXTENSION_LUA_CALL_POLL) {
                    Ok(call) => call,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                let deadline = call.deadline;
                let cancellation = call.cancellation.clone();
                let generation_cancellation = call.generation_cancellation.clone();
                let worker_cancellation = thread_cancellation.clone();
                if worker_cancellation.is_cancel_requested()
                    || call.cancellation.is_cancel_requested()
                    || call.generation_cancellation.is_cancel_requested()
                {
                    let _ = call.response.send(Err(ExtensionError::new(
                        "cancelled",
                        "Luau command was cancelled before execution",
                    )));
                    continue;
                }
                lua.set_interrupt(move |_| {
                    if worker_cancellation.is_cancel_requested()
                        || cancellation.is_cancel_requested()
                        || generation_cancellation.is_cancel_requested()
                    {
                        return Err(mlua::Error::external("cancelled"));
                    }
                    if Instant::now() >= deadline {
                        return Err(mlua::Error::external("deadline_exceeded"));
                    }
                    Ok(VmState::Continue)
                });
                let result = lua_invocation_context(&lua, &call.invocation, generation)
                    .map_err(|error| ExtensionError::new("luau_context_invalid", error.to_string()))
                    .and_then(|invocation| {
                        handlers
                            .get(&call.invocation.command)
                            .ok_or_else(|| {
                                ExtensionError::new(
                                    "unknown_command",
                                    "Luau command handler is not registered",
                                )
                            })
                            .and_then(|handler| {
                                handler
                                    .call::<LuaValue>(LuaValue::Table(invocation))
                                    .map_err(|error| {
                                        let message = error.to_string();
                                        let code = if message.contains("deadline_exceeded") {
                                            "deadline_exceeded"
                                        } else if message.contains("cancelled") {
                                            "cancelled"
                                        } else {
                                            "luau_handler_failed"
                                        };
                                        ExtensionError::new(code, message)
                                    })
                                    .and_then(|value| {
                                        lua_value_to_json(value).map_err(|error| {
                                            ExtensionError::new("luau_result_invalid", error)
                                        })
                                    })
                            })
                    });
                lua.remove_interrupt();
                let _ = call.response.send(result);
            }
        }) {
        Ok(handle) => handle,
        Err(error) => {
            runtime.remove_lua_worker(&worker);
            return Err(ExtensionError::new("luau_worker_failed", error.to_string()));
        }
    };
    if let Ok(mut slot) = worker.handle.lock() {
        *slot = Some(handle);
    } else {
        runtime.remove_lua_worker(&worker);
        worker.cancellation.request_cancel();
        return Err(ExtensionError::new(
            "luau_worker_failed",
            "worker handle lock poisoned",
        ));
    }
    let (descriptors, events) = match registration_rx.recv_timeout(EXTENSION_LUA_LOAD_TIMEOUT) {
        Ok(Ok(registration)) => registration,
        Ok(Err(error)) => {
            worker.cancel_and_join();
            runtime.remove_lua_worker(&worker);
            if error.code == "luau_registration_timeout" {
                let _ = runtime.cleanup_generation(id, generation, None);
            }
            return Err(error);
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            worker.cancel_and_join();
            runtime.remove_lua_worker(&worker);
            // Registration owns the whole generation boundary. If a top-level
            // evaluation times out, retire any earlier worker for this same
            // generation as well, rather than leaving a stale VM alive beside
            // the failed replacement.
            let _ = runtime.cleanup_generation(id, generation, None);
            return Err(ExtensionError::new(
                "luau_registration_timeout",
                "Luau package did not register in time",
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            worker.cancel_and_join();
            runtime.remove_lua_worker(&worker);
            return Err(ExtensionError::new(
                "luau_worker_stopped",
                "Luau registration worker stopped before publishing its descriptor",
            ));
        }
    };
    let mut registered_events = Vec::new();
    for topic in events {
        match runtime.register_event(id, generation, &topic) {
            Ok(registration) => registered_events.push(registration.topic),
            Err(error) => {
                worker.cancel_and_join();
                runtime.remove_lua_worker(&worker);
                if !registered_events.is_empty() {
                    let _ = runtime.cleanup_generation(id, generation, None);
                }
                return Err(error);
            }
        }
    }
    let mut registered_commands = Vec::new();
    for descriptor in descriptors {
        let registered_command_id = descriptor.id.clone();
        let sender = call_tx.clone();
        let handler: ExtensionCommandHandler = Arc::new(move |context| {
            let (response, receiver) = mpsc::channel();
            let mut call = LuaCall {
                invocation: context.invocation().clone(),
                deadline: context.deadline(),
                cancellation: context.cancellation.clone(),
                generation_cancellation: context.generation_cancellation.clone(),
                response,
            };
            loop {
                if context.cancellation.is_cancel_requested()
                    || context.generation_cancellation.is_cancel_requested()
                {
                    return Err(ExtensionError::new(
                        "cancelled",
                        "Luau command was cancelled",
                    ));
                }
                let remaining = context.deadline().saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    context.cancellation.request_cancel();
                    return Err(ExtensionError::new(
                        "deadline_exceeded",
                        "Luau command deadline expired",
                    ));
                }
                match sender.try_send(call) {
                    Ok(()) => break,
                    Err(TrySendError::Full(returned)) => {
                        call = returned;
                        thread::sleep(remaining.min(EXTENSION_LUA_CALL_POLL));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        return Err(ExtensionError::new(
                            "luau_worker_stopped",
                            "Luau worker stopped",
                        ));
                    }
                }
            }
            loop {
                let remaining = context.deadline().saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    context.cancellation.request_cancel();
                    return Err(ExtensionError::new(
                        "deadline_exceeded",
                        "Luau command deadline expired",
                    ));
                }
                match receiver.recv_timeout(remaining.min(EXTENSION_LUA_CALL_POLL)) {
                    Ok(result) => return result,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(ExtensionError::new(
                            "luau_worker_stopped",
                            "Luau worker stopped",
                        ));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout)
                        if context.cancellation.is_cancel_requested()
                            || context.generation_cancellation.is_cancel_requested() =>
                    {
                        return Err(ExtensionError::new(
                            "cancelled",
                            "Luau command was cancelled",
                        ));
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        match runtime.register_command(id, generation, descriptor, handler) {
            Ok(()) => registered_commands.push(registered_command_id),
            Err(error) => {
                worker.cancel_and_join();
                runtime.remove_lua_worker(&worker);
                if !registered_events.is_empty() || !registered_commands.is_empty() {
                    let _ = runtime.cleanup_generation(id, generation, None);
                }
                return Err(error);
            }
        }
    }
    Ok(())
}

fn validate_luau_source(source: &str, id: &str) -> Result<(), ExtensionError> {
    Lua::new()
        .load(source)
        .set_name(id)
        .into_function()
        .map(|_| ())
        .map_err(|error| {
            let message = error.to_string();
            let code = if message.contains("syntax") {
                "luau_syntax_invalid"
            } else {
                "luau_load_failed"
            };
            ExtensionError::new(code, message)
        })
}

fn install_lua_host_api(
    lua: &Lua,
    bootty: &Table,
    runtime: ExtensionRuntime,
    id: &str,
    generation: u64,
    cancellation: CommandCancellation,
    generation_cancellation: CommandCancellation,
) -> mlua::Result<()> {
    let extension_id = id.to_owned();
    let owner = runtime.inner.owner.clone();
    let scope = extension_scope(id, generation);

    let command_table = bootty.get::<Table>("commands")?;
    let command_runtime = runtime.clone_internal();
    let command_id = extension_id.clone();
    let command_cancel = cancellation.clone();
    let invoke_command_cancel = command_cancel.clone();
    let command_generation_cancel = generation_cancellation.clone();
    command_table.set(
        "invoke",
        lua.create_function(move |lua, specification: Table| {
            let command = specification.get::<String>("command")?;
            let arguments = lua_arguments(specification.get::<LuaValue>("arguments")?)
                .map_err(mlua::Error::external)?;
            let target = lua_target_from_value(specification.get::<LuaValue>("target")?)
                .map_err(mlua::Error::external)?;
            let invocation = CommandInvocation {
                command,
                arguments,
                caller: Caller::Luau,
                target,
                confirmation: None,
            };
            let outcome = command_runtime.invoke_blocking(
                invocation,
                Instant::now() + EXTENSION_LUA_LOAD_TIMEOUT,
                invoke_command_cancel.clone(),
            );
            match outcome {
                CommandOutcome::Success { value, .. } => json_to_lua(lua, value),
                CommandOutcome::Pending { .. } => {
                    Err(mlua::Error::external("pending: command is still pending"))
                }
                CommandOutcome::Unsupported { message } => {
                    Err(mlua::Error::external(format!("unsupported: {message}")))
                }
                CommandOutcome::Unavailable { message } => {
                    Err(mlua::Error::external(format!("unavailable: {message}")))
                }
                CommandOutcome::Denied { message } => {
                    Err(mlua::Error::external(format!("denied: {message}")))
                }
                CommandOutcome::StaleTarget { message } => {
                    Err(mlua::Error::external(format!("stale_target: {message}")))
                }
                CommandOutcome::Ambiguous { message, .. } => {
                    Err(mlua::Error::external(format!("ambiguous: {message}")))
                }
                CommandOutcome::ConfirmationRequired { .. } => Err(mlua::Error::external(
                    "confirmation_required: command requires confirmation",
                )),
                CommandOutcome::Failed { code, message } => {
                    Err(mlua::Error::external(format!("{code}: {message}")))
                }
            }
        })?,
    )?;
    let context_scope = scope.clone();
    command_table.set(
        "context",
        lua.create_function(move |lua, _: ()| {
            let table = lua.create_table()?;
            table.set("extension_id", command_id.clone())?;
            table.set("generation", generation)?;
            table.set("scope", context_scope.clone())?;
            table.set(
                "cancelled",
                command_generation_cancel.is_cancel_requested()
                    || command_cancel.is_cancel_requested(),
            )?;
            Ok(table)
        })?,
    )?;

    let events = bootty.get::<Table>("events")?;
    let events_runtime = runtime.clone_internal();
    let events_id = extension_id.clone();
    let events_owner = owner.clone();
    let events_scope = scope.clone();
    events.set(
        "publish",
        lua.create_function(move |_, specification: Table| {
            let topic = specification.get::<String>("topic")?;
            let payload = lua_value_to_json(specification.get::<LuaValue>("payload")?)
                .map_err(mlua::Error::external)?;
            let target = lua_target_from_value(specification.get::<LuaValue>("target")?)
                .map_err(mlua::Error::external)?;
            let sequence = events_runtime
                .publish_event(
                    &events_id,
                    generation,
                    &topic,
                    events_scope.clone(),
                    payload,
                    target,
                )
                .map_err(mlua::Error::external)?;
            Ok(sequence)
        })?,
    )?;
    let subscribe_runtime = runtime.clone_internal();
    let subscribe_id = extension_id.clone();
    let subscribe_owner = events_owner.clone();
    let subscribe_scope = scope.clone();
    events.set(
        "subscribe",
        lua.create_function(move |lua, topic: String| {
            let (id, delivery) = subscribe_runtime
                .subscribe_event(
                    &subscribe_id,
                    generation,
                    subscribe_owner.clone(),
                    &topic,
                    subscribe_scope.clone(),
                )
                .map_err(mlua::Error::external)?;
            let result = lua.create_table()?;
            result.set("id", id)?;
            result.set("topic", topic)?;
            result.set("cursor", delivery.cursor)?;
            result.set("revision", delivery.revision)?;
            Ok(result)
        })?,
    )?;
    let poll_runtime = runtime.clone_internal();
    let poll_owner = events_owner.clone();
    events.set(
        "poll",
        lua.create_function(move |lua, specification: Table| {
            let subscription = specification.get::<String>("subscription")?;
            let cursor = specification.get::<u64>("cursor").unwrap_or_default();
            let delivery = poll_runtime
                .poll_event(&subscription, &poll_owner, cursor)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(delivery).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let unsubscribe_runtime = runtime.clone_internal();
    let unsubscribe_owner = events_owner;
    events.set(
        "unsubscribe",
        lua.create_function(move |_, subscription: String| {
            unsubscribe_runtime
                .unsubscribe_event(&subscription, &unsubscribe_owner)
                .map_err(mlua::Error::external)
        })?,
    )?;

    let storage = lua.create_table()?;
    let storage_runtime = runtime.clone_internal();
    let storage_id = extension_id.clone();
    let storage_owner = owner.clone();
    storage.set(
        "get",
        lua.create_function(move |lua, key: String| {
            let value = storage_runtime
                .storage_get(&storage_id, generation, &storage_owner, &key)
                .map_err(mlua::Error::external)?;
            value.map_or(Ok(LuaValue::Nil), |value| json_to_lua(lua, value))
        })?,
    )?;
    let storage_runtime = runtime.clone_internal();
    let storage_id = extension_id.clone();
    let storage_owner = owner.clone();
    storage.set(
        "put",
        lua.create_function(move |_, (key, value): (String, LuaValue)| {
            let value = lua_value_to_json(value).map_err(mlua::Error::external)?;
            storage_runtime
                .storage_put(&storage_id, generation, &storage_owner, &key, value)
                .map_err(mlua::Error::external)
        })?,
    )?;
    let storage_runtime = runtime.clone_internal();
    let storage_id = extension_id.clone();
    let storage_owner = owner.clone();
    storage.set(
        "delete",
        lua.create_function(move |_, key: String| {
            storage_runtime
                .storage_delete(&storage_id, generation, &storage_owner, &key)
                .map_err(mlua::Error::external)
        })?,
    )?;
    let storage_runtime = runtime.clone_internal();
    let storage_id = extension_id.clone();
    let storage_owner = owner.clone();
    storage.set(
        "list",
        lua.create_function(move |lua, _: ()| {
            let keys = storage_runtime
                .storage_list(&storage_id, generation, &storage_owner)
                .map_err(mlua::Error::external)?;
            let result = lua.create_table()?;
            for (index, key) in keys.into_iter().enumerate() {
                result.set(index + 1, key)?;
            }
            Ok(result)
        })?,
    )?;
    bootty.set("storage", storage)?;

    let tasks = lua.create_table()?;
    let task_runtime = runtime.clone_internal();
    let task_id = extension_id.clone();
    let task_owner = owner.clone();
    let task_cancel = cancellation.clone();
    let task_scope = scope.clone();
    tasks.set(
        "start",
        lua.create_function(move |lua, _: ()| {
            let status = task_runtime
                .start_task(
                    &task_id,
                    generation,
                    task_owner.clone(),
                    task_scope.clone(),
                    task_cancel.clone(),
                )
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let task_runtime = runtime.clone_internal();
    let task_id = extension_id.clone();
    let task_owner = owner.clone();
    tasks.set(
        "status",
        lua.create_function(move |lua, task: String| {
            let status = task_runtime
                .task_status(&task, &task_id, generation, &task_owner)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let task_runtime = runtime.clone_internal();
    let task_id = extension_id.clone();
    let task_owner = owner.clone();
    tasks.set(
        "cancel",
        lua.create_function(move |lua, task: String| {
            let status = task_runtime
                .cancel_task(&task, &task_id, generation, &task_owner)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let task_runtime = runtime.clone_internal();
    let task_id = extension_id.clone();
    let task_owner = owner.clone();
    tasks.set(
        "finish",
        lua.create_function(move |_, specification: Table| {
            let task = specification.get::<String>("task")?;
            let outcome = lua_value_to_json(specification.get::<LuaValue>("outcome")?)
                .map_err(mlua::Error::external)?;
            task_runtime
                .finish_task(&task, &task_id, generation, &task_owner, &outcome)
                .map_err(mlua::Error::external)
        })?,
    )?;
    bootty.set("tasks", tasks)?;

    let process = lua.create_table()?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    process.set(
        "spawn",
        lua.create_function(move |lua, specification: Table| {
            let value = lua_table_to_json(&specification).map_err(mlua::Error::external)?;
            let spec = serde_json::from_value::<ProcessSpec>(value)
                .map_err(|error| mlua::Error::external(error.to_string()))?;
            let status = process_runtime
                .spawn_process(&process_id, generation, spec)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    process.set(
        "write",
        lua.create_function(move |_, specification: Table| {
            let process = specification.get::<String>("process")?;
            let value = specification.get::<LuaValue>("data")?;
            let bytes = match value {
                LuaValue::String(value) => value.as_bytes().to_vec(),
                value => {
                    serde_json::to_vec(&lua_value_to_json(value).map_err(mlua::Error::external)?)
                        .map_err(mlua::Error::external)?
                }
            };
            process_runtime
                .process_write(&process_id, generation, &process, &bytes)
                .map_err(mlua::Error::external)
        })?,
    )?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    process.set(
        "read",
        lua.create_function(move |lua, specification: Table| {
            let process = specification.get::<String>("process")?;
            let cursor = specification.get::<u64>("cursor").unwrap_or_default();
            let limit = specification
                .get::<usize>("limit")
                .unwrap_or(EXTENSION_PROCESS_LINES);
            let lines = process_runtime
                .process_read_since(&process_id, generation, &process, cursor, limit)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(lines).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    process.set(
        "status",
        lua.create_function(move |lua, process: String| {
            let status = process_runtime
                .process_status(&process_id, generation, &process)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    process.set(
        "signal",
        lua.create_function(move |_, process: String| {
            process_runtime
                .process_signal(&process_id, generation, &process)
                .map_err(mlua::Error::external)
        })?,
    )?;
    let process_runtime = runtime.clone_internal();
    let process_id = extension_id.clone();
    let process_cancel = cancellation.clone();
    process.set(
        "wait",
        lua.create_function(move |lua, process: String| {
            let status = process_runtime
                .process_wait(
                    &process_id,
                    generation,
                    &process,
                    Instant::now() + EXTENSION_LUA_LOAD_TIMEOUT,
                    &process_cancel,
                )
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(status).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    bootty.set("process", process)?;

    // IPC is intentionally local to this owner and uses the same bounded,
    // revisioned event stream as the generic event API.
    let ipc = lua.create_table()?;
    let ipc_runtime = runtime.clone_internal();
    let ipc_id = extension_id.clone();
    let ipc_scope = scope.clone();
    ipc.set(
        "publish",
        lua.create_function(move |_, specification: Table| {
            let topic = specification.get::<String>("topic")?;
            let payload = lua_value_to_json(specification.get::<LuaValue>("payload")?)
                .map_err(mlua::Error::external)?;
            let sequence = ipc_runtime
                .publish_event(
                    &ipc_id,
                    generation,
                    &topic,
                    ipc_scope.clone(),
                    payload,
                    None,
                )
                .map_err(mlua::Error::external)?;
            Ok(sequence)
        })?,
    )?;
    let ipc_runtime = runtime.clone_internal();
    let ipc_id = extension_id.clone();
    let ipc_owner = owner.clone();
    let ipc_scope = scope.clone();
    ipc.set(
        "subscribe",
        lua.create_function(move |lua, topic: String| {
            let (subscription, delivery) = ipc_runtime
                .subscribe_event(
                    &ipc_id,
                    generation,
                    ipc_owner.clone(),
                    &topic,
                    ipc_scope.clone(),
                )
                .map_err(mlua::Error::external)?;
            let result = lua.create_table()?;
            result.set("id", subscription)?;
            result.set("cursor", delivery.cursor)?;
            result.set("revision", delivery.revision)?;
            Ok(result)
        })?,
    )?;
    let ipc_runtime = runtime.clone_internal();
    let ipc_owner = owner.clone();
    ipc.set(
        "poll",
        lua.create_function(move |lua, specification: Table| {
            let subscription = specification.get::<String>("subscription")?;
            let cursor = specification.get::<u64>("cursor").unwrap_or_default();
            let delivery = ipc_runtime
                .poll_event(&subscription, &ipc_owner, cursor)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(delivery).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    bootty.set("ipc", ipc)?;

    let metadata = lua.create_table()?;
    let metadata_runtime = runtime.clone_internal();
    let metadata_id = extension_id.clone();
    let metadata_owner = owner.clone();
    metadata.set(
        "get",
        lua.create_function(move |lua, specification: Table| {
            let namespace = specification.get::<String>("namespace")?;
            let key = specification.get::<String>("key")?;
            let target = lua_target_from_value(specification.get::<LuaValue>("target")?)
                .map_err(mlua::Error::external)?;
            let value = metadata_runtime
                .metadata_get(
                    &metadata_id,
                    generation,
                    &metadata_owner,
                    &namespace,
                    &key,
                    target.as_ref(),
                )
                .map_err(mlua::Error::external)?;
            value.map_or(Ok(LuaValue::Nil), |value| {
                json_to_lua(
                    lua,
                    serde_json::to_value(value).map_err(mlua::Error::external)?,
                )
            })
        })?,
    )?;
    let metadata_runtime = runtime.clone_internal();
    let metadata_id = extension_id.clone();
    let metadata_owner = owner.clone();
    metadata.set(
        "publish",
        lua.create_function(move |_, specification: Table| {
            let namespace = specification.get::<String>("namespace")?;
            let key = specification.get::<String>("key")?;
            let value = lua_value_to_json(specification.get::<LuaValue>("value")?)
                .map_err(mlua::Error::external)?;
            let target = lua_target_from_value(specification.get::<LuaValue>("target")?)
                .map_err(mlua::Error::external)?;
            metadata_runtime
                .publish_metadata(
                    &metadata_id,
                    generation,
                    &metadata_owner,
                    MetadataPublication::new(
                        extension_scope(&metadata_id, generation),
                        namespace,
                        key,
                        target,
                        value,
                        specification.get("expires_at_ms").ok(),
                        json!({"extension_id": metadata_id, "generation": generation}),
                    ),
                )
                .map_err(mlua::Error::external)
        })?,
    )?;
    bootty.set("metadata", metadata)?;

    let host = lua.create_table()?;
    let observe_runtime = runtime.clone_internal();
    let observe_id = extension_id.clone();
    let observe_owner = owner.clone();
    host.set(
        "observe",
        lua.create_function(move |lua, _: ()| {
            let observation = observe_runtime
                .observe(&observe_id, generation, &observe_owner)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(observation).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    bootty.set("host", host)?;

    let notify = lua.create_table()?;
    notify.set(
        "show",
        lua.create_function(move |_, specification: Table| {
            let title = specification.get::<String>("title")?;
            let body = specification.get::<String>("body")?;
            crate::platform::show_desktop_notification(&title, &body)
                .map_err(|error| mlua::Error::external(error.to_string()))
        })?,
    )?;
    bootty.set("notify", notify)?;

    let files = lua.create_table()?;
    let pending_files = Rc::new(RefCell::new(BTreeMap::<String, FileTransaction>::new()));
    let next_file = Rc::new(std::cell::Cell::new(1_u64));
    let files_runtime = runtime.clone_internal();
    let files_id = extension_id.clone();
    let files_pending = Rc::clone(&pending_files);
    let files_next = Rc::clone(&next_file);
    let files_read_runtime = runtime.clone_internal();
    let files_read_id = extension_id.clone();
    files.set(
        "read",
        lua.create_function(move |lua, path: String| {
            let bytes = files_read_runtime
                .file_read(&files_read_id, Path::new(&path))
                .map_err(mlua::Error::external)?;
            lua.create_string(bytes)
        })?,
    )?;
    let files_exists_runtime = runtime.clone_internal();
    let files_exists_id = extension_id.clone();
    files.set(
        "exists",
        lua.create_function(move |_, path: String| {
            files_exists_runtime
                .file_exists(&files_exists_id, Path::new(&path))
                .map_err(mlua::Error::external)
        })?,
    )?;
    let files_stat_runtime = runtime.clone_internal();
    let files_stat_id = extension_id.clone();
    files.set(
        "stat",
        lua.create_function(move |lua, path: String| {
            let value = files_stat_runtime
                .file_stat(&files_stat_id, Path::new(&path))
                .map_err(mlua::Error::external)?;
            json_to_lua(lua, value)
        })?,
    )?;
    let files_validation_runtime = runtime.clone_internal();
    let files_validation_id = extension_id.clone();
    files.set(
        "validate_confirmation",
        lua.create_function(move |_, (expected, token): (Table, String)| {
            let expected = lua_table_to_json(&expected).map_err(mlua::Error::external)?;
            files_validation_runtime
                .validate_file_confirmation(&files_validation_id, generation, &expected, &token)
                .map_err(mlua::Error::external)?;
            Ok(true)
        })?,
    )?;
    let files_apply_runtime = runtime.clone_internal();
    let files_apply_id = extension_id.clone();
    files.set(
        "apply",
        lua.create_function(
            move |lua, (actions, token, context): (Table, String, Table)| {
                let actions = lua_table_to_json(&actions).map_err(mlua::Error::external)?;
                let context = lua_table_to_json(&context).map_err(mlua::Error::external)?;
                let result = files_apply_runtime
                    .apply_file_confirmation(
                        &files_apply_id,
                        generation,
                        &actions,
                        &token,
                        &context,
                    )
                    .map_err(mlua::Error::external)?;
                json_to_lua(lua, result)
            },
        )?,
    )?;
    files.set(
        "prepare",
        lua.create_function(move |lua, specification: Table| {
            let path = PathBuf::from(specification.get::<String>("path")?);
            let operation = specification
                .get::<Option<String>>("operation")?
                .unwrap_or_else(|| "write".to_owned());
            let transaction = if operation == "remove" {
                files_runtime
                    .file_removal_transaction(&files_id, generation, &path)
                    .map_err(mlua::Error::external)?
            } else if operation == "write" {
                let contents = match specification.get::<LuaValue>("contents")? {
                    LuaValue::String(value) => value.as_bytes().to_vec(),
                    value => serde_json::to_vec(
                        &lua_value_to_json(value).map_err(mlua::Error::external)?,
                    )
                    .map_err(mlua::Error::external)?,
                };
                files_runtime
                    .file_transaction(&files_id, generation, &path, contents)
                    .map_err(mlua::Error::external)?
            } else {
                return Err(mlua::Error::external("unsupported file operation"));
            };
            let id = format!("file-{}", files_next.get());
            files_next.set(files_next.get().saturating_add(1));
            let preview =
                serde_json::to_value(transaction.preview()).map_err(mlua::Error::external)?;
            files_pending.borrow_mut().insert(id.clone(), transaction);
            let result = lua.create_table()?;
            result.set("id", id)?;
            result.set("preview", json_to_lua(lua, preview)?)?;
            Ok(result)
        })?,
    )?;
    bootty.set("files", files)?;

    let ui = lua.create_table()?;
    let surface = lua.create_table()?;
    let surface_runtime = runtime.clone_internal();
    let surface_id = extension_id.clone();
    surface.set(
        "open",
        lua.create_function(move |lua, specification: Table| {
            let value = lua_table_to_json(&specification).map_err(mlua::Error::external)?;
            let spec = serde_json::from_value::<SurfaceSpec>(value)
                .map_err(|error| mlua::Error::external(error.to_string()))?;
            let event = surface_runtime
                .open_surface(&surface_id, generation, spec)
                .map_err(mlua::Error::external)?;
            json_to_lua(
                lua,
                serde_json::to_value(event).map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    let surface_runtime = runtime.clone_internal();
    let surface_id = extension_id.clone();
    surface.set(
        "close",
        lua.create_function(move |lua, surface: String| {
            let event = surface_runtime
                .close_surface(&surface_id, generation, &surface)
                .map_err(mlua::Error::external)?;
            event.map_or(Ok(LuaValue::Nil), |event| {
                json_to_lua(
                    lua,
                    serde_json::to_value(event).map_err(mlua::Error::external)?,
                )
            })
        })?,
    )?;
    let surface_runtime = runtime.clone_internal();
    let surface_id = extension_id;
    surface.set(
        "list",
        lua.create_function(move |lua, _: ()| {
            json_to_lua(
                lua,
                serde_json::to_value(surface_runtime.surfaces(Some(&surface_id)))
                    .map_err(mlua::Error::external)?,
            )
        })?,
    )?;
    ui.set("surface", surface)?;
    bootty.set("ui", ui)?;
    Ok(())
}

fn lua_arguments(value: LuaValue) -> Result<Vec<String>, String> {
    let LuaValue::Table(table) = value else {
        return Ok(Vec::new());
    };
    table
        .sequence_values::<LuaValue>()
        .map(|value| {
            let value = value.map_err(|error| error.to_string())?;
            match value {
                LuaValue::String(value) => value
                    .to_str()
                    .map(|value| value.to_owned())
                    .map_err(|error| error.to_string()),
                LuaValue::Integer(value) => Ok(value.to_string()),
                LuaValue::Number(value) => Ok(value.to_string()),
                LuaValue::Boolean(value) => Ok(value.to_string()),
                value => Err(format!("command argument must be scalar, got {value:?}")),
            }
        })
        .collect()
}

fn lua_table_to_json(table: &Table) -> Result<Value, String> {
    lua_value_to_json(LuaValue::Table(table.clone()))
}

fn lua_target_from_value(value: LuaValue) -> Result<Option<CommandTarget>, String> {
    let LuaValue::Table(table) = value else {
        return Ok(None);
    };
    let kind_name = table
        .get::<String>("kind")
        .map_err(|error| error.to_string())?;
    let kind = match kind_name.as_str() {
        "instance" => ResourceKind::Instance,
        "application_window" => ResourceKind::ApplicationWindow,
        "binding" => ResourceKind::Binding,
        "space" => ResourceKind::Space,
        "session" => ResourceKind::Session,
        "mux_window" | "window" => ResourceKind::MuxWindow,
        "pane" => ResourceKind::Pane,
        "terminal" => ResourceKind::Terminal,
        "client" => ResourceKind::Client,
        "directory" => ResourceKind::Directory,
        "worktree" => ResourceKind::Worktree,
        "task" => ResourceKind::Task,
        "subscription" => ResourceKind::Subscription,
        "surface" => ResourceKind::Surface,
        "extension" => ResourceKind::Extension,
        other => return Err(format!("unknown target kind {other}")),
    };
    let generation_value = table
        .get::<LuaValue>("generation")
        .map_err(|error| error.to_string())?;
    let generation = match generation_value {
        LuaValue::Integer(value) if value >= 0 => value as u64,
        LuaValue::Number(value) if value >= 0.0 => value as u64,
        LuaValue::String(value) => value
            .to_str()
            .map_err(|error| error.to_string())?
            .parse::<u64>()
            .map_err(|error| error.to_string())?,
        _ => return Err("target generation is required".to_owned()),
    };
    Ok(Some(CommandTarget {
        kind,
        handle: table
            .get::<String>("handle")
            .map_err(|error| error.to_string())?,
        generation,
    }))
}

fn lua_invocation_context(
    lua: &Lua,
    invocation: &CommandInvocation,
    generation: u64,
) -> mlua::Result<Table> {
    let context = lua.create_table()?;
    context.set("command", invocation.command.clone())?;
    context.set("caller", lua_caller_name(invocation.caller))?;
    context.set("arguments", {
        let arguments = lua.create_table()?;
        for (index, argument) in invocation.arguments.iter().enumerate() {
            arguments.set(index + 1, argument.clone())?;
        }
        arguments
    })?;
    for (index, argument) in invocation.arguments.iter().enumerate() {
        context.set(index + 1, argument.clone())?;
    }
    context.set(
        "target",
        invocation
            .target
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(mlua::Error::external)?
            .map_or(Ok(LuaValue::Nil), |target| json_to_lua(lua, target))?,
    )?;
    context.set(
        "confirmation",
        invocation
            .confirmation
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(mlua::Error::external)?
            .map_or(Ok(LuaValue::Nil), |confirmation| {
                json_to_lua(lua, confirmation)
            })?,
    )?;
    context.set(
        "extension_id",
        invocation.command.split('.').next().unwrap_or_default(),
    )?;
    context.set("generation", generation)?;
    Ok(context)
}

fn lua_caller_name(caller: Caller) -> &'static str {
    match caller {
        Caller::CommandPalette => "command_palette",
        Caller::Keybinding => "keybinding",
        Caller::BuiltinKeybinding => "builtin_keybinding",
        Caller::Cli => "cli",
        Caller::Socket => "socket",
        Caller::Luau => "luau",
        Caller::Internal => "internal",
    }
}

fn json_to_lua(lua: &Lua, value: Value) -> mlua::Result<LuaValue> {
    match value {
        Value::Null => Ok(LuaValue::Nil),
        Value::Bool(value) => Ok(LuaValue::Boolean(value)),
        Value::Number(value) => value
            .as_f64()
            .map(LuaValue::Number)
            .ok_or_else(|| mlua::Error::external("JSON number is not representable in Luau")),
        Value::String(value) => Ok(LuaValue::String(lua.create_string(&value)?)),
        Value::Array(values) => {
            let table = lua.create_table()?;
            for (index, value) in values.into_iter().enumerate() {
                table.set(index + 1, json_to_lua(lua, value)?)?;
            }
            Ok(LuaValue::Table(table))
        }
        Value::Object(values) => {
            let table = lua.create_table()?;
            for (key, value) in values {
                table.set(key, json_to_lua(lua, value)?)?;
            }
            Ok(LuaValue::Table(table))
        }
    }
}

fn lua_descriptor(
    table: &Table,
    id: String,
    title: String,
    description: String,
) -> mlua::Result<CommandDescriptor> {
    use crate::{
        automation::catalog::{BackendAvailability, CatalogPaletteMetadata},
        commands::{ArgumentSchema, CompactSchema, MutationClass},
    };
    let mutation = match table.get::<Option<String>>("mutation")? {
        None => MutationClass::Read,
        Some(value) => match value.as_str() {
            "read" => MutationClass::Read,
            "write" => MutationClass::Write,
            "destructive" => MutationClass::Destructive,
            _ => {
                return Err(mlua::Error::external(
                    "mutation must be read, write, or destructive",
                ));
            }
        },
    };
    let mut arguments = Vec::new();
    if let Some(args) = table.get::<Option<Table>>("arguments")? {
        for pair in args.sequence_values::<Table>() {
            let arg = pair?;
            let name = arg.get::<String>("name")?;
            if name.is_empty() || name.len() > 128 {
                return Err(mlua::Error::external("argument name is invalid"));
            }
            let value_type = lua_value_type(
                &arg.get::<Option<String>>("type")?
                    .unwrap_or_else(|| "string".to_owned()),
            )?;
            let choices = arg
                .get::<Option<Table>>("choices")?
                .map(|choices| {
                    choices
                        .sequence_values::<String>()
                        .collect::<mlua::Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            arguments.push(ArgumentSchema {
                name,
                value_type,
                required: arg.get::<Option<bool>>("required")?.unwrap_or(false),
                choices,
                minimum: arg.get::<Option<i64>>("minimum")?,
                maximum: arg.get::<Option<i64>>("maximum")?,
                default: arg.get::<Option<String>>("default")?,
                repeated: arg.get::<Option<bool>>("repeated")?.unwrap_or(false),
            });
        }
    }
    let aliases = table
        .get::<Option<Table>>("aliases")?
        .map(|aliases| {
            aliases
                .sequence_values::<String>()
                .collect::<mlua::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let result_schema = table
        .get::<Option<Table>>("result_schema")?
        .or(table.get::<Option<Table>>("result")?)
        .map(|schema| lua_result_schema(&schema))
        .transpose()?;
    let targets = table
        .get::<Option<Table>>("targets")?
        .map(|targets| {
            targets
                .sequence_values::<String>()
                .map(|target| target.and_then(|target| lua_catalog_target(&target)))
                .collect::<mlua::Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let availability = table
        .get::<Option<Table>>("availability")?
        .map(|availability| -> mlua::Result<BackendAvailability> {
            Ok(BackendAvailability {
                core: lua_catalog_availability(
                    &availability
                        .get::<Option<String>>("core")?
                        .unwrap_or_else(|| "available".to_owned()),
                )?,
                native: lua_catalog_availability(
                    &availability
                        .get::<Option<String>>("native")?
                        .unwrap_or_else(|| "available".to_owned()),
                )?,
                rmux: lua_catalog_availability(
                    &availability
                        .get::<Option<String>>("rmux")?
                        .unwrap_or_else(|| "available".to_owned()),
                )?,
                tmux: lua_catalog_availability(
                    &availability
                        .get::<Option<String>>("tmux")?
                        .unwrap_or_else(|| "available".to_owned()),
                )?,
            })
        })
        .transpose()?;
    let target = table
        .get::<Option<String>>("target")?
        .map(|target| lua_resource_kind(&target))
        .transpose()?;
    let palette_metadata = table
        .get::<Option<Table>>("palette_metadata")?
        .map(|palette| -> mlua::Result<CatalogPaletteMetadata> {
            Ok(CatalogPaletteMetadata {
                visible: palette.get::<Option<bool>>("visible")?.unwrap_or(true),
                category: palette
                    .get::<Option<String>>("category")?
                    .unwrap_or_else(|| "extensions".to_owned()),
                keywords: palette
                    .get::<Option<Table>>("keywords")?
                    .map(|keywords| {
                        keywords
                            .sequence_values::<String>()
                            .collect::<mlua::Result<Vec<_>>>()
                    })
                    .transpose()?
                    .unwrap_or_default(),
            })
        })
        .transpose()?;
    Ok(CommandDescriptor {
        id,
        title,
        description,
        aliases,
        origin: None,
        mutation,
        arguments: CompactSchema { arguments },
        result_schema,
        targets,
        availability,
        target,
        palette: table.get::<Option<bool>>("palette")?.unwrap_or(false),
        palette_metadata,
    })
}

fn lua_value_type(value: &str) -> mlua::Result<crate::commands::ValueType> {
    use crate::commands::ValueType;
    match value {
        "null" => Ok(ValueType::Null),
        "boolean" => Ok(ValueType::Boolean),
        "integer" => Ok(ValueType::Integer),
        "number" => Ok(ValueType::Number),
        "string" => Ok(ValueType::String),
        "enum" => Ok(ValueType::Enum),
        "array" => Ok(ValueType::Array),
        "object" => Ok(ValueType::Object),
        "resource_ref" => Ok(ValueType::ResourceRef),
        "json" => Ok(ValueType::Json),
        other => Err(mlua::Error::external(format!(
            "unknown argument type {other}"
        ))),
    }
}

fn lua_catalog_value_type(
    value: &str,
) -> mlua::Result<crate::automation::catalog::CatalogValueType> {
    use crate::automation::catalog::CatalogValueType;
    match value {
        "null" => Ok(CatalogValueType::Null),
        "boolean" => Ok(CatalogValueType::Boolean),
        "integer" => Ok(CatalogValueType::Integer),
        "number" => Ok(CatalogValueType::Number),
        "string" => Ok(CatalogValueType::String),
        "enum" => Ok(CatalogValueType::Enum),
        "array" => Ok(CatalogValueType::Array),
        "object" => Ok(CatalogValueType::Object),
        "resource_ref" => Ok(CatalogValueType::ResourceRef),
        "json" => Ok(CatalogValueType::Json),
        other => Err(mlua::Error::external(format!(
            "unknown result type {other}"
        ))),
    }
}

fn lua_result_schema(
    table: &Table,
) -> mlua::Result<crate::automation::catalog::CatalogResultSchema> {
    use crate::automation::catalog::CatalogResultSchema;
    let properties = table
        .get::<Option<Table>>("properties")?
        .map(|properties| {
            properties
                .pairs::<String, Table>()
                .map(|pair| {
                    let (name, schema) = pair?;
                    Ok((name, lua_result_schema(&schema)?))
                })
                .collect::<mlua::Result<BTreeMap<_, _>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let items = table
        .get::<Option<Table>>("items")?
        .map(|items| lua_result_schema(&items).map(Box::new))
        .transpose()?;
    Ok(CatalogResultSchema {
        value_type: lua_catalog_value_type(
            &table
                .get::<Option<String>>("type")?
                .unwrap_or_else(|| "json".to_owned()),
        )?,
        properties,
        items,
    })
}

fn lua_catalog_target(value: &str) -> mlua::Result<crate::automation::catalog::CatalogTarget> {
    use crate::automation::catalog::CatalogTarget;
    match value {
        "instance" => Ok(CatalogTarget::Instance),
        "application_window" => Ok(CatalogTarget::ApplicationWindow),
        "binding" => Ok(CatalogTarget::Binding),
        "space" => Ok(CatalogTarget::Space),
        "session" => Ok(CatalogTarget::Session),
        "window" | "mux_window" => Ok(CatalogTarget::Window),
        "pane" => Ok(CatalogTarget::Pane),
        "terminal" => Ok(CatalogTarget::Terminal),
        "client" => Ok(CatalogTarget::Client),
        "directory" => Ok(CatalogTarget::Directory),
        "worktree" => Ok(CatalogTarget::Worktree),
        "task" => Ok(CatalogTarget::Task),
        "subscription" => Ok(CatalogTarget::Subscription),
        "surface" => Ok(CatalogTarget::Surface),
        "extension" => Ok(CatalogTarget::Extension),
        other => Err(mlua::Error::external(format!(
            "unknown command target {other}"
        ))),
    }
}

fn lua_catalog_availability(
    value: &str,
) -> mlua::Result<crate::automation::catalog::CatalogAvailability> {
    use crate::automation::catalog::CatalogAvailability;
    match value {
        "available" => Ok(CatalogAvailability::Available),
        "conditional" => Ok(CatalogAvailability::Conditional),
        "unsupported" => Ok(CatalogAvailability::Unsupported),
        "unavailable" => Ok(CatalogAvailability::Unavailable),
        other => Err(mlua::Error::external(format!(
            "unknown backend availability {other}"
        ))),
    }
}

fn lua_resource_kind(value: &str) -> mlua::Result<ResourceKind> {
    match value {
        "instance" => Ok(ResourceKind::Instance),
        "application_window" => Ok(ResourceKind::ApplicationWindow),
        "binding" => Ok(ResourceKind::Binding),
        "space" => Ok(ResourceKind::Space),
        "session" => Ok(ResourceKind::Session),
        "mux_window" | "window" => Ok(ResourceKind::MuxWindow),
        "pane" => Ok(ResourceKind::Pane),
        "terminal" => Ok(ResourceKind::Terminal),
        "client" => Ok(ResourceKind::Client),
        "directory" => Ok(ResourceKind::Directory),
        "worktree" => Ok(ResourceKind::Worktree),
        "task" => Ok(ResourceKind::Task),
        "subscription" => Ok(ResourceKind::Subscription),
        "surface" => Ok(ResourceKind::Surface),
        "extension" => Ok(ResourceKind::Extension),
        other => Err(mlua::Error::external(format!(
            "unknown resource target {other}"
        ))),
    }
}

const LUA_RESULT_MAX_DEPTH: usize = 32;
const LUA_RESULT_MAX_NODES: usize = 4096;

fn lua_value_to_json(value: LuaValue) -> Result<Value, String> {
    let mut active = HashSet::new();
    let mut nodes = 0;
    lua_value_to_json_inner(value, 0, &mut nodes, &mut active)
}

fn lua_value_to_json_inner(
    value: LuaValue,
    depth: usize,
    nodes: &mut usize,
    active: &mut HashSet<usize>,
) -> Result<Value, String> {
    *nodes = nodes.saturating_add(1);
    if *nodes > LUA_RESULT_MAX_NODES {
        return Err("Luau result exceeds the node limit".to_owned());
    }
    if depth > LUA_RESULT_MAX_DEPTH {
        return Err("Luau result exceeds the nesting depth limit".to_owned());
    }
    match value {
        LuaValue::Nil => Ok(Value::Null),
        LuaValue::Boolean(value) => Ok(Value::Bool(value)),
        LuaValue::Integer(value) => Ok(json!(value)),
        LuaValue::Number(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| "Luau result contains a non-finite number".to_owned()),
        LuaValue::String(value) => value
            .to_str()
            .map(|value| Value::String(value.to_owned()))
            .map_err(|error| error.to_string()),
        LuaValue::Table(table) => {
            let identity = table.to_pointer() as usize;
            if !active.insert(identity) {
                return Err("Luau result contains a cyclic table".to_owned());
            }
            let result = (|| {
                let mut object = serde_json::Map::new();
                let mut array = BTreeMap::new();
                for pair in table.pairs::<LuaValue, LuaValue>() {
                    let (key, value) = pair.map_err(|error| error.to_string())?;
                    let json = lua_value_to_json_inner(value, depth + 1, nodes, active)?;
                    match key {
                        LuaValue::String(key) => {
                            object.insert(
                                key.to_str().map_err(|error| error.to_string())?.to_owned(),
                                json,
                            );
                        }
                        LuaValue::Integer(index) if index > 0 => {
                            let index = usize::try_from(index)
                                .map_err(|_| "Luau array index is too large".to_owned())?;
                            array.insert(index, json);
                        }
                        _ => {
                            return Err(
                                "Luau result table keys must be strings or positive integers"
                                    .to_owned(),
                            );
                        }
                    }
                }
                if !object.is_empty() && !array.is_empty() {
                    return Err("Luau result tables cannot mix string and array keys".to_owned());
                }
                if !object.is_empty() {
                    return Ok(Value::Object(object));
                }
                if array.is_empty() {
                    return Ok(Value::Array(Vec::new()));
                }
                let length = array.len();
                if array.keys().copied().ne(1..=length) {
                    return Err("Luau result array keys must be contiguous from 1".to_owned());
                }
                Ok(Value::Array(array.into_values().collect()))
            })();
            active.remove(&identity);
            result
        }
        _ => Err("unsupported Luau result value".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{ArgumentSchema, CompactSchema, MutationClass, ValueType};

    fn runtime() -> ExtensionRuntime {
        ExtensionRuntime::new(AutomationHub::new())
    }

    fn manifest(id: &str) -> ExtensionPackageManifest {
        ExtensionPackageManifest {
            id: id.to_owned(),
            name: id.to_owned(),
            version: "1".to_owned(),
            entrypoint: None,
            storage_namespace: None,
            default_enabled: false,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn apply_confirmed_transaction(
        runtime: &ExtensionRuntime,
        transaction: FileTransaction,
    ) -> Result<FileTransactionPreview, ExtensionError> {
        let confirmation = runtime.confirm_file_transaction(&transaction)?;
        transaction.apply(confirmation)
    }

    fn descriptor(id: &str) -> CommandDescriptor {
        CommandDescriptor {
            id: id.to_owned(),
            title: id.to_owned(),
            description: String::new(),
            aliases: Vec::new(),
            origin: None,
            mutation: MutationClass::Read,
            arguments: CompactSchema {
                arguments: vec![ArgumentSchema {
                    name: "value".to_owned(),
                    value_type: ValueType::String,
                    required: true,
                    choices: Vec::new(),
                    minimum: None,
                    maximum: None,
                    default: None,
                    repeated: false,
                }],
            },
            result_schema: None,
            targets: Vec::new(),
            availability: None,
            target: None,
            palette: true,
            palette_metadata: None,
        }
    }

    #[test]
    fn command_generation_reload_rejects_stale_invocations() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        runtime
            .register_command(
                "sample.extension",
                generation,
                descriptor("sample.extension.echo"),
                Arc::new(|context| Ok(json!(context.arguments()[0]))),
            )
            .unwrap();
        let cancellation = CommandCancellation::new();
        let invocation = CommandInvocation {
            command: "sample.extension.echo".to_owned(),
            arguments: vec!["ok".to_owned()],
            caller: Caller::Luau,
            target: None,
            confirmation: None,
        };
        assert!(matches!(
            runtime.invoke_blocking(
                invocation,
                Instant::now() + Duration::from_secs(1),
                cancellation
            ),
            CommandOutcome::Success { .. }
        ));
        let _ = runtime.reload("sample.extension").unwrap();
        let stale = runtime.register_command(
            "sample.extension",
            generation,
            descriptor("sample.extension.stale"),
            Arc::new(|_| Ok(Value::Null)),
        );
        assert!(stale.is_err());
    }

    #[test]
    fn concurrent_event_registration_has_one_reservation_winner() {
        let runtime = Arc::new(runtime());
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let first = Arc::clone(&runtime);
        let second = Arc::clone(&runtime);
        let first = std::thread::spawn(move || {
            first.register_event("sample.extension", generation, "shared.topic")
        });

        let second = std::thread::spawn(move || {
            second.register_event("sample.extension", generation, "shared.topic")
        });
        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_rejects_parent_directory_identity_swap() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let parent = root.path().join("nested");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("state.json");
        fs::write(&path, b"inside").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let moved = root.path().join("nested.original");
        fs::rename(&parent, &moved).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(parent.join("state.json"), b"replacement-dir").unwrap();

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(
            fs::read(parent.join("state.json")).unwrap(),
            b"replacement-dir"
        );
    }

    #[test]
    fn concurrent_command_registration_has_one_reservation_winner() {
        let runtime = Arc::new(runtime());
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let first = Arc::clone(&runtime);
        let second = Arc::clone(&runtime);
        let first = std::thread::spawn(move || {
            first.register_command(
                "sample.extension",
                generation,
                descriptor("sample.extension.shared"),
                Arc::new(|_| Ok(Value::Null)),
            )
        });
        let second = std::thread::spawn(move || {
            second.register_command(
                "sample.extension",
                generation,
                descriptor("sample.extension.shared"),
                Arc::new(|_| Ok(Value::Null)),
            )
        });

        let outcomes = [first.join().unwrap(), second.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_preserves_writer_during_rollback_exchange() {
        let _serial = match FILE_TRANSACTION_TEST_SERIAL.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("file transaction test lock poisoned"),
        };
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let post_root = root.path().to_path_buf();
        let post_hook: Box<dyn Fn() + Send> = Box::new(move || {
            let temporary = fs::read_dir(&post_root)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|candidate| {
                    candidate
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(".state.json."))
                })
                .expect("transaction temporary file");
            fs::write(temporary, b"changed-displaced").unwrap();
        });
        match FILE_TRANSACTION_POST_EXCHANGE.lock() {
            Ok(mut slot) => *slot = Some(post_hook),
            Err(_) => panic!("file transaction post-exchange hook lock poisoned"),
        }

        let replacement = root.path().join("rollback-writer");
        let target = path.clone();
        let pre_hook: Box<dyn Fn() + Send> = Box::new(move || {
            fs::write(&replacement, b"concurrent").unwrap();
            fs::rename(&replacement, &target).unwrap();
        });
        match FILE_TRANSACTION_PRE_ROLLBACK.lock() {
            Ok(mut slot) => *slot = Some(pre_hook),
            Err(_) => panic!("file transaction pre-rollback hook lock poisoned"),
        }

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"concurrent");
        let preserved = fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter_map(|candidate| fs::read(candidate).ok())
            .collect::<Vec<_>>();
        assert!(preserved.iter().any(|bytes| bytes == b"concurrent"));
        assert!(preserved.iter().any(|bytes| bytes == b"changed-displaced"));
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transactions_require_confirmation_for_destructive_changes() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, "old").unwrap();
        runtime.set_file_roots([directory.path().to_path_buf()]);
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction("sample.extension", generation, &path, b"new".to_vec())
            .unwrap();
        assert!(transaction.preview().destructive);
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();
        let mut invalid = confirmation.clone();
        invalid.token.clear();
        assert!(transaction.clone().apply(invalid).is_err());
        assert_eq!(transaction.apply(confirmation).unwrap().after_bytes, 3);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_batch_applies_mixed_write_and_remove_once() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let write_path = root.path().join("write.txt");
        let remove_path = root.path().join("remove.txt");
        fs::write(&write_path, b"before-write").unwrap();
        fs::write(&remove_path, b"before-remove").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let write = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &write_path,
                b"after-write".to_vec(),
            )
            .unwrap();
        let remove = runtime
            .file_removal_transaction("sample.extension", generation, &remove_path)
            .unwrap();
        let confirmation = runtime
            .confirm_file_transactions(&[write.clone(), remove.clone()])
            .unwrap();
        assert_eq!(confirmation.previews.len(), 2);
        let actions = json!([
            {
                "path": write.preview().path.clone(),
                "operation": "write",
                "content": "after-write",
            },
            {
                "path": remove.preview().path.clone(),
                "operation": "remove",
            },
        ]);

        let result = runtime
            .apply_file_confirmation(
                "sample.extension",
                generation,
                &actions,
                &confirmation.token,
                &json!({}),
            )
            .unwrap();
        assert_eq!(result.as_array().map(Vec::len), Some(2));
        assert_eq!(fs::read(&write_path).unwrap(), b"after-write");
        assert!(!remove_path.exists());
        assert!(
            runtime
                .apply_file_confirmation(
                    "sample.extension",
                    generation,
                    &actions,
                    &confirmation.token,
                    &json!({}),
                )
                .is_err()
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_batch_rolls_back_prior_actions_after_mid_batch_failure() {
        let _serial = FILE_TRANSACTION_TEST_SERIAL
            .lock()
            .expect("file transaction test lock poisoned");
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let write_path = root.path().join("write.txt");
        let remove_path = root.path().join("remove.txt");
        fs::write(&write_path, b"before-write").unwrap();
        fs::write(&remove_path, b"before-remove").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let write = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &write_path,
                b"after-write".to_vec(),
            )
            .unwrap();
        let remove = runtime
            .file_removal_transaction("sample.extension", generation, &remove_path)
            .unwrap();
        let confirmation = runtime
            .confirm_file_transactions(&[write.clone(), remove.clone()])
            .unwrap();
        let concurrent_path = remove_path.clone();
        let hook: Box<dyn Fn() + Send> = Box::new(move || {
            fs::write(&concurrent_path, b"concurrent").unwrap();
        });
        *FILE_TRANSACTION_POST_EXCHANGE
            .lock()
            .expect("file transaction post-exchange hook lock poisoned") = Some(hook);
        let actions = json!([
            {
                "path": write.preview().path.clone(),
                "operation": "write",
                "content": "after-write",
            },
            {
                "path": remove.preview().path.clone(),
                "operation": "remove",
            },
        ]);

        let error = runtime
            .apply_file_confirmation(
                "sample.extension",
                generation,
                &actions,
                &confirmation.token,
                &json!({}),
            )
            .unwrap_err();
        assert_eq!(error.code, "file_batch_apply_failed");
        let details = error.details.expect("file batch details");
        assert_eq!(details.original_code, "file_conflict");
        assert!(details.rolled_back);
        assert!(details.rollback_errors.is_empty());
        assert_eq!(fs::read(&write_path).unwrap(), b"before-write");
        assert_eq!(fs::read(&remove_path).unwrap(), b"concurrent");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_batch_preserves_concurrent_edit_during_rollback() {
        let _serial = FILE_TRANSACTION_TEST_SERIAL
            .lock()
            .expect("file transaction test lock poisoned");
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let remove_path = root.path().join("remove.txt");
        let write_path = root.path().join("write.txt");
        fs::write(&remove_path, b"before-remove").unwrap();
        fs::write(&write_path, b"before-write").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let remove = runtime
            .file_removal_transaction("sample.extension", generation, &remove_path)
            .unwrap();
        let write = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &write_path,
                b"after-write".to_vec(),
            )
            .unwrap();
        let confirmation = runtime
            .confirm_file_transactions(&[remove.clone(), write.clone()])
            .unwrap();
        let concurrent_remove_path = remove_path.clone();
        let concurrent_write_path = write_path.clone();
        let hook: Box<dyn Fn() + Send> = Box::new(move || {
            fs::write(&concurrent_remove_path, b"concurrent-remove").unwrap();
            fs::write(&concurrent_write_path, b"concurrent-write").unwrap();
        });
        *FILE_TRANSACTION_PRE_COMMIT
            .lock()
            .expect("file transaction pre-commit hook lock poisoned") = Some(hook);
        let actions = json!([
            {
                "path": remove.preview().path.clone(),
                "operation": "remove",
            },
            {
                "path": write.preview().path.clone(),
                "operation": "write",
                "content": "after-write",
            },
        ]);

        let error = runtime
            .apply_file_confirmation(
                "sample.extension",
                generation,
                &actions,
                &confirmation.token,
                &json!({}),
            )
            .unwrap_err();
        assert_eq!(error.code, "file_batch_apply_failed");
        let details = error.details.expect("file batch details");
        assert_eq!(details.original_code, "file_conflict");
        assert!(!details.rolled_back);
        assert_eq!(details.rollback_errors.len(), 1);
        assert_eq!(details.rollback_errors[0].path, remove.preview().path);
        assert!(details.rollback_errors[0].conflict);
        assert_eq!(fs::read(&remove_path).unwrap(), b"concurrent-remove");
        assert_eq!(fs::read(&write_path).unwrap(), b"concurrent-write");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_capability_access_reads_updates_and_creates_in_root() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let existing = root.path().join("existing.txt");
        fs::write(&existing, b"old").unwrap();
        assert_eq!(
            runtime.file_read("sample.extension", &existing).unwrap(),
            b"old"
        );
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction("sample.extension", generation, &existing, b"new".to_vec())
            .unwrap();
        apply_confirmed_transaction(&runtime, transaction).unwrap();
        assert_eq!(fs::read(&existing).unwrap(), b"new");

        let created = root.path().join("created.txt");
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &created,
                b"created".to_vec(),
            )
            .unwrap();
        assert!(!transaction.preview().existed);
        apply_confirmed_transaction(&runtime, transaction).unwrap();
        assert_eq!(fs::read(created).unwrap(), b"created");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_rejects_final_symlink_swap_without_touching_outside() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("state.json");
        let outside_path = outside.path().join("state.json");
        fs::write(&path, b"inside").unwrap();
        fs::write(&outside_path, b"outside").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let moved = root.path().join("state.original");
        fs::rename(&path, &moved).unwrap();
        std::os::unix::fs::symlink(&outside_path, &path).unwrap();

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(fs::read(outside_path).unwrap(), b"outside");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_rejects_parent_directory_swap_without_touching_outside() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let parent = root.path().join("nested");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("state.json");
        let outside_path = outside.path().join("state.json");
        fs::write(&path, b"inside").unwrap();
        fs::write(&outside_path, b"outside").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let moved = root.path().join("nested.original");
        fs::rename(&parent, &moved).unwrap();
        std::os::unix::fs::symlink(outside.path(), &parent).unwrap();

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(fs::read(outside_path).unwrap(), b"outside");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_rejects_concurrent_content_change() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        fs::write(&path, b"concurrent").unwrap();

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_rejects_writer_between_validation_and_commit() {
        let _serial = match FILE_TRANSACTION_TEST_SERIAL.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("file transaction test lock poisoned"),
        };
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let hook: Box<dyn Fn() + Send> = Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        match FILE_TRANSACTION_PRE_COMMIT.lock() {
            Ok(mut slot) => *slot = Some(hook),
            Err(_) => panic!("file transaction barrier lock poisoned"),
        }
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            entered_rx.recv().unwrap();
            fs::write(writer_path, b"writer").unwrap();
            release_tx.send(()).unwrap();
        });

        assert!(transaction.apply(confirmation).is_err());
        writer.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"writer");
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_preserves_post_exchange_replacement() {
        let _serial = match FILE_TRANSACTION_TEST_SERIAL.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("file transaction test lock poisoned"),
        };
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction(
                "sample.extension",
                generation,
                &path,
                b"replacement".to_vec(),
            )
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let replacement = root.path().join("post-exchange-writer");
        let hook_replacement = replacement.clone();
        let target = path.clone();
        let hook: Box<dyn Fn() + Send> = Box::new(move || {
            fs::write(&hook_replacement, b"concurrent").unwrap();
            fs::rename(&hook_replacement, &target).unwrap();
        });
        match FILE_TRANSACTION_POST_EXCHANGE.lock() {
            Ok(mut slot) => *slot = Some(hook),
            Err(_) => panic!("file transaction post-exchange hook lock poisoned"),
        }

        assert!(transaction.apply(confirmation).is_err());
        assert_eq!(fs::read(path).unwrap(), b"concurrent");
        assert!(!replacement.exists());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn file_transaction_absent_target_uses_no_replace_commit() {
        let _serial = match FILE_TRANSACTION_TEST_SERIAL.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("file transaction test lock poisoned"),
        };
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let path = root.path().join("created.json");
        let generation = runtime.package("sample.extension").unwrap().generation;
        let transaction = runtime
            .file_transaction("sample.extension", generation, &path, b"first".to_vec())
            .unwrap();
        let confirmation = runtime.confirm_file_transaction(&transaction).unwrap();

        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let hook: Box<dyn Fn() + Send> = Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        match FILE_TRANSACTION_PRE_COMMIT.lock() {
            Ok(mut slot) => *slot = Some(hook),
            Err(_) => panic!("file transaction barrier lock poisoned"),
        }
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            entered_rx.recv().unwrap();
            fs::write(writer_path, b"writer").unwrap();
            release_tx.send(()).unwrap();
        });

        assert!(transaction.apply(confirmation).is_err());
        writer.join().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"writer");
    }
    #[cfg(unix)]
    #[test]
    fn file_access_rejects_parent_traversal_and_symlink_targets() {
        let runtime = runtime();
        let _ = runtime.install(manifest("sample.extension")).unwrap();
        let _ = runtime.enable("sample.extension").unwrap();
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        runtime.set_file_roots([root.path().to_path_buf()]);
        let outside_path = outside.path().join("outside.txt");
        fs::write(&outside_path, b"outside").unwrap();
        let link = root.path().join("link.txt");
        std::os::unix::fs::symlink(&outside_path, &link).unwrap();

        assert!(
            runtime
                .file_read("sample.extension", &root.path().join("../outside.txt"))
                .is_err()
        );
        assert!(runtime.file_read("sample.extension", &link).is_err());
        assert_eq!(fs::read(outside_path).unwrap(), b"outside");
    }
    #[test]
    fn luau_top_level_eval_timeout_stops_worker() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let id = "top-level-loop.extension";
        let _ = runtime.install(manifest(id)).unwrap();
        let _ = runtime.link(id, directory.path()).unwrap();
        let _ = runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let started = Instant::now();
        let error =
            register_luau_package(&runtime, id, generation, "while true do end").unwrap_err();
        assert_eq!(error.code, "luau_registration_timeout");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            runtime
                .inner
                .lua_workers
                .lock()
                .is_ok_and(|workers| workers.is_empty())
        );
    }

    #[cfg(unix)]
    #[test]
    fn dropping_last_runtime_owner_cancels_handler_and_reaps_process() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let id = "drop-loop.extension";
        let _ = runtime.install(manifest(id)).unwrap();
        let _ = runtime.link(id, directory.path()).unwrap();
        let _ = runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let process = runtime
            .spawn_process(
                id,
                generation,
                ProcessSpec {
                    executable: "sh".to_owned(),
                    arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
                    cwd: None,
                    environment: BTreeMap::new(),
                },
            )
            .unwrap();
        let process_record = runtime
            .inner
            .state
            .read()
            .ok()
            .and_then(|state| state.processes.get(&process.id).cloned())
            .expect("spawned process record");
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (mutation_tx, mutation_rx) = mpsc::sync_channel(1);
        runtime
            .register_command(
                id,
                generation,
                descriptor("drop-loop.extension.blocked"),
                Arc::new(move |context| {
                    entered_tx.send(()).map_err(|_| {
                        ExtensionError::new("test_failed", "handler entry signal dropped")
                    })?;
                    loop {
                        match release_rx
                            .lock()
                            .map_err(|_| {
                                ExtensionError::new("test_failed", "release signal lock poisoned")
                            })?
                            .try_recv()
                        {
                            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                                let result = context.runtime().start_task();
                                let _ = mutation_tx
                                    .send(result.map(|_| ()).map_err(|error| error.code));
                                return Ok(Value::Null);
                            }
                            Err(mpsc::TryRecvError::Empty) => thread::yield_now(),
                        }
                    }
                }),
            )
            .unwrap();
        let internal = runtime.clone_internal();
        let weak = Arc::downgrade(&runtime.inner);
        let receiver = runtime.invoke_async(
            CommandInvocation {
                command: "drop-loop.extension.blocked".to_owned(),
                arguments: Vec::new(),
                caller: Caller::Luau,
                target: None,
                confirmation: None,
            },
            Instant::now() + Duration::from_secs(10),
            CommandCancellation::new(),
        );
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("blocked handler entered");
        drop(runtime);
        assert!(
            weak.upgrade()
                .is_some_and(|inner| { inner.shutdown.load(Ordering::Acquire) })
        );
        assert!(
            process_record.child.lock().is_ok_and(|mut child| child
                .try_wait()
                .ok()
                .flatten()
                .is_some())
        );
        assert!(
            internal
                .inner
                .lua_workers
                .lock()
                .is_ok_and(|workers| workers.is_empty())
        );
        assert!(
            internal
                .inner
                .state
                .read()
                .is_ok_and(|state| state.processes.is_empty())
        );
        let _ = release_tx.send(());
        assert_eq!(
            mutation_rx
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
                .unwrap_err(),
            "extension_stopped"
        );
        assert!(
            internal
                .inner
                .state
                .read()
                .is_ok_and(|state| state.tasks.is_empty())
        );
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(CommandOutcome::Success { .. })
        ));
        drop(internal);
    }

    #[test]
    fn task_start_race_with_last_owner_drop_compensates_exact_task() {
        let _serial = TASK_START_TEST_SERIAL.lock().unwrap();
        let runtime = runtime();
        let id = "task-drop-race.extension";
        runtime.install(manifest(id)).unwrap();
        let generation = runtime.enable(id).unwrap().generation;
        let scope = extension_scope(id, generation);
        runtime
            .inner
            .automation
            .tasks()
            .install_snapshot(&scope)
            .unwrap();
        let owner = OwnerIdentity::new(71, 1);
        let cancellation = CommandCancellation::new();
        let observed_cancellation = cancellation.clone();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *TASK_START_BEFORE_COMMIT.lock().unwrap() = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));

        let starter_runtime = runtime.clone_internal();
        let starter_id = id.to_owned();
        let starter_scope = scope.clone();
        let starter_owner = owner.clone();
        let starter = thread::spawn(move || {
            starter_runtime.start_task(
                &starter_id,
                generation,
                starter_owner,
                starter_scope,
                cancellation,
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task hub start reached final-commit barrier");

        let inspector = runtime.clone_internal();
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let dropper = thread::spawn(move || {
            drop(runtime);
            dropped_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !inspector.inner.shutdown.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(inspector.inner.shutdown.load(Ordering::Acquire));
        release_tx.send(()).unwrap();

        let error = starter.join().unwrap().unwrap_err();
        assert_eq!(error.code, "extension_stopped");
        dropper.join().unwrap();
        dropped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(observed_cancellation.is_cancel_requested());
        assert_eq!(
            inspector
                .inner
                .automation
                .tasks()
                .status("task-1", &owner)
                .unwrap_err()
                .code,
            -32602
        );
        assert!(inspector.inner.state.read().unwrap().tasks.is_empty());
        drop(inspector);
    }
    #[test]
    fn dropping_runtime_cleans_only_its_shared_extension_resources() {
        let hub = AutomationHub::new();
        let runtime = ExtensionRuntime::new(hub.clone());
        let extension_owner = OwnerIdentity::new(41, 1);
        let unrelated_owner = OwnerIdentity::new(42, 1);
        let extension_id = "drop-shared.extension";
        runtime.install(manifest(extension_id)).unwrap();
        runtime.enable(extension_id).unwrap();
        let generation = runtime.package(extension_id).unwrap().generation;
        let command_id = format!("{extension_id}.echo");
        runtime
            .register_command(
                extension_id,
                generation,
                descriptor(&command_id),
                Arc::new(|_| Ok(Value::Null)),
            )
            .unwrap();
        let event = runtime
            .register_event(extension_id, generation, "changed")
            .unwrap();
        let extension_scope = extension_scope(extension_id, generation);
        let extension_subscription = runtime
            .subscribe_event(
                extension_id,
                generation,
                extension_owner.clone(),
                &event.topic,
                extension_scope.clone(),
            )
            .unwrap()
            .0;
        let extension_task = runtime
            .start_task(
                extension_id,
                generation,
                extension_owner.clone(),
                extension_scope.clone(),
                CommandCancellation::new(),
            )
            .unwrap()
            .id;
        runtime
            .publish_metadata(
                extension_id,
                generation,
                &extension_owner,
                MetadataPublication::new(
                    extension_scope,
                    "state",
                    "key",
                    None,
                    json!({"value": 1}),
                    None,
                    json!({"extension_id": extension_id}),
                ),
            )
            .unwrap();
        let registry = runtime.command_registry();

        hub.register_event_topic("unrelated.event").unwrap();
        let unrelated_subscription = hub
            .events()
            .subscribe(
                unrelated_owner.clone(),
                BTreeSet::from(["unrelated.event".to_owned()]),
                "unrelated.scope".to_owned(),
            )
            .unwrap()
            .subscription;
        let unrelated_task = hub
            .tasks()
            .start(
                unrelated_owner.clone(),
                CommandCancellation::new(),
                "unrelated.scope".to_owned(),
            )
            .unwrap()
            .id;
        hub.metadata()
            .publish(MetadataPublication::new(
                "unrelated.scope",
                "state",
                "key",
                None,
                json!({"value": 2}),
                None,
                json!({"source": "test"}),
            ))
            .unwrap();

        drop(runtime);

        assert!(
            !registry
                .extension_commands()
                .iter()
                .any(|command| command.id == command_id)
        );
        assert!(!hub.events().topic_registered(&event.topic));
        assert!(
            hub.events()
                .poll(&extension_subscription, &extension_owner, 0)
                .is_err()
        );
        assert!(
            hub.tasks()
                .status(&extension_task, &extension_owner)
                .is_err()
        );
        assert!(
            hub.metadata()
                .get(
                    "extension:drop-shared.extension:1",
                    "drop-shared.extension:state",
                    "key",
                    None
                )
                .unwrap()
                .is_none()
        );

        assert!(hub.events().topic_registered("unrelated.event"));
        assert!(
            hub.events()
                .poll(&unrelated_subscription, &unrelated_owner, 0)
                .is_ok()
        );
        assert!(matches!(
            hub.tasks()
                .status(&unrelated_task, &unrelated_owner)
                .unwrap()
                .state,
            crate::automation::TaskState::Running
        ));
        assert!(
            hub.metadata()
                .get("unrelated.scope", "state", "key", None)
                .unwrap()
                .is_some()
        );
    }
    #[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn shutdown_during_process_spawn_reaps_uncommitted_child() {
        let _serial = PROCESS_SPAWN_TEST_SERIAL.lock().unwrap();
        let runtime = runtime();
        let id = "spawn-shutdown.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let inspection = runtime.clone_internal();
        let internal = runtime.clone_internal();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (record_tx, record_rx) = mpsc::sync_channel(1);
        let hook_id = id.to_owned();
        let hook: ProcessSpawnHook = Arc::new(move |record| {
            if record.generation.extension_id != hook_id {
                return;
            }
            record_tx.send(Arc::clone(record)).unwrap();
            entered_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        });
        *PROCESS_SPAWN_BEFORE_COMMIT.lock().unwrap() = Some(hook);
        let spawn = thread::spawn(move || {
            internal.spawn_process(
                id,
                generation,
                ProcessSpec {
                    executable: "sh".to_owned(),
                    arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
                    cwd: None,
                    environment: BTreeMap::new(),
                },
            )
        });
        let process_record = record_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn hook received child record");
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn hook entered");
        drop(runtime);
        assert!(inspection.inner.shutdown.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        let error = spawn.join().unwrap().unwrap_err();
        assert_eq!(error.code, "extension_stopped");
        assert!(
            process_record.child.lock().is_ok_and(|mut child| child
                .try_wait()
                .ok()
                .flatten()
                .is_some())
        );
        assert!(
            process_record
                .readers
                .lock()
                .is_ok_and(|readers| readers.is_empty())
        );
        assert!(inspection.inner.state.read().is_ok_and(|state| {
            state.processes.is_empty() && state.process_reservations.is_empty()
        }));
        *PROCESS_SPAWN_BEFORE_COMMIT.lock().unwrap() = None;
    }
    #[test]
    fn luau_require_is_rooted_cached_and_interruptible() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("module.luau"),
            "return {value = 'cached'}",
        )
        .unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let id = "sample.extension";
        let _ = runtime.install(manifest(id)).unwrap();
        let _ = runtime.link(id, directory.path()).unwrap();
        let _ = runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        register_luau_package(
            &runtime,
            id,
            generation,
            r#"
                bootty.commands.register({
                    id = "sample.extension.require",
                    handler = function()
                        local first = require("module")
                        local second = require("module")
                        return first.value .. second.value
                    end
                })
                bootty.commands.register({
                    id = "sample.extension.loop",
                    handler = function()
                        while true do end
                    end
                })
            "#,
        )
        .unwrap();
        let invocation = |command: &str| CommandInvocation {
            command: command.to_owned(),
            arguments: Vec::new(),
            caller: Caller::Luau,
            target: None,
            confirmation: None,
        };
        assert!(matches!(
            runtime.invoke_blocking(
                invocation("sample.extension.require"),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new()
            ),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "cachedcached"
        ));
        let deadline = Instant::now() + Duration::from_millis(100);
        let outcome = runtime.invoke_blocking(
            invocation("sample.extension.loop"),
            deadline,
            CommandCancellation::new(),
        );
        assert!(matches!(
            outcome,
            CommandOutcome::Failed { code, .. } if code == "deadline_exceeded"
        ));
        fs::write(directory.path().join("escape.luau"), "return 'escape'").unwrap();
        let traversal = ExtensionRuntime::new(AutomationHub::new());
        let _ = traversal.install(manifest("traversal.extension")).unwrap();
        let _ = traversal
            .link("traversal.extension", directory.path())
            .unwrap();
        let _ = traversal.enable("traversal.extension").unwrap();
        let generation = traversal.package("traversal.extension").unwrap().generation;
        register_luau_package(
            &traversal,
            "traversal.extension",
            generation,
            r#"
                bootty.commands.register({
                    id = "traversal.extension.escape",
                    handler = function()
                        return require("../escape")
                    end
                })
            "#,
        )
        .unwrap();
        let outcome = traversal.invoke_blocking(
            invocation("traversal.extension.escape"),
            Instant::now() + Duration::from_secs(1),
            CommandCancellation::new(),
        );
        assert!(matches!(
            outcome,
            CommandOutcome::Failed { code, .. } if code == "luau_handler_failed"
        ));
    }
    #[test]
    fn luau_registration_rolls_back_partial_commands_on_collision() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let id = "collision.extension";
        let _ = runtime.install(manifest(id)).unwrap();
        let _ = runtime.link(id, directory.path()).unwrap();
        let _ = runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let error = register_luau_package(
            &runtime,
            id,
            generation,
            r#"
                bootty.commands.register({
                    id = "collision.extension.first",
                    handler = function() return "first" end
                })
                bootty.commands.register({
                    id = "collision.extension.first",
                    handler = function() return "duplicate" end
                })
            "#,
        )
        .unwrap_err();
        assert_eq!(error.code, "command_collision");
        let outcome = runtime.invoke_blocking(
            CommandInvocation {
                command: "collision.extension.first".to_owned(),
                arguments: Vec::new(),
                caller: Caller::Luau,
                target: None,
                confirmation: None,
            },
            Instant::now() + Duration::from_secs(1),
            CommandCancellation::new(),
        );
        assert!(matches!(
            outcome,
            CommandOutcome::Failed { code, .. } if code == "unknown_command"
        ));
    }
    #[test]
    fn discover_and_load_registers_generic_commands_for_startup_invocation() {
        let runtime = runtime();
        let root = tempfile::tempdir().unwrap();
        let extension = root.path().join("agents");
        fs::create_dir_all(&extension).unwrap();
        fs::write(
            extension.join("manifest.json"),
            serde_json::to_vec(&json!({
                "id": "agents.extension",
                "name": "Agents",
                "version": "1",
                "entrypoint": "main.luau",
                "default_enabled": true
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            extension.join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "agents.list",
                    title = "List Agents",
                    handler = function() return { "fixture-agent" } end
                })
            "#,
        )
        .unwrap();
        let loaded = runtime.discover_and_load(root.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        let outcome = runtime.invoke_blocking(
            CommandInvocation {
                command: "agents.list".to_owned(),
                arguments: Vec::new(),
                caller: Caller::Cli,
                target: None,
                confirmation: None,
            },
            Instant::now() + Duration::from_secs(1),
            CommandCancellation::new(),
        );
        assert!(matches!(
            outcome,
            CommandOutcome::Success { value: Value::Array(values), .. }
                if values == vec![Value::String("fixture-agent".to_owned())]
        ));
    }

    #[test]
    fn bundled_agent_extension_loads_ingests_and_lists_through_runtime() {
        let runtime = ExtensionRuntime::new(AutomationHub::new());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("extensions");
        let loaded = runtime.discover_and_load(&root).unwrap();
        assert!(loaded.iter().any(|package| package.id == "bootty.agents"));
        let invoke = |command: &str, arguments: Vec<String>| {
            runtime.invoke_blocking(
                CommandInvocation {
                    command: command.to_owned(),
                    arguments,
                    caller: Caller::Cli,
                    target: None,
                    confirmation: None,
                },
                Instant::now() + Duration::from_secs(2),
                CommandCancellation::new(),
            )
        };
        let event = json!({
            "source": "hook",
            "event": "session_start",
            "sessionId": "pi-e2e-session",
            "cwd": "/work/bootty",
            "sequence": 1
        });
        assert!(matches!(
            invoke("agents.ingest", vec![event.to_string(), "pi".to_owned()]),
            CommandOutcome::Success { .. }
        ));
        assert!(matches!(
            invoke("agents.list", Vec::new()),
            CommandOutcome::Success { value: Value::Array(records), .. }
                if records.iter().any(|record| record["agent_id"] == "pi-e2e-session")
        ));
    }

    #[test]
    fn discover_missing_root_is_empty_success() {
        let runtime = runtime();
        let root = tempfile::tempdir().unwrap().path().join("missing");
        assert!(runtime.discover_and_load(&root).unwrap().is_empty());
    }

    #[test]
    fn discover_duplicate_sources_keep_first_and_continue_other_packages() {
        let runtime = runtime();
        let bundled_root = tempfile::tempdir().unwrap();
        let user_root = tempfile::tempdir().unwrap();
        let write_extension = |root: &std::path::Path, id: &str, command: &str, value: &str| {
            let extension = root.join(id);
            fs::create_dir_all(&extension).unwrap();
            fs::write(
                extension.join("manifest.json"),
                serde_json::to_vec(&json!({
                    "id": id,
                    "name": id,
                    "version": "1",
                    "entrypoint": "main.luau",
                    "default_enabled": true
                }))
                .unwrap(),
            )
            .unwrap();
            fs::write(
                extension.join("main.luau"),
                format!(
                    r#"
                        bootty.commands.register({{
                            id = "{command}",
                            handler = function() return "{value}" end
                        }})
                    "#
                ),
            )
            .unwrap();
        };
        write_extension(
            bundled_root.path(),
            "trusted.extension",
            "trusted.extension.source",
            "bundled",
        );
        write_extension(
            user_root.path(),
            "trusted.extension",
            "trusted.extension.source",
            "user",
        );
        write_extension(
            user_root.path(),
            "other.extension",
            "other.extension.source",
            "other",
        );

        let loaded = runtime.discover_and_load(bundled_root.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        let conflict = runtime.discover_and_load(user_root.path()).unwrap_err();
        assert_eq!(conflict.code, "extension_source_conflict");

        let invoke = |command: &str| {
            runtime.invoke_blocking(
                CommandInvocation {
                    command: command.to_owned(),
                    arguments: Vec::new(),
                    caller: Caller::Cli,
                    target: None,
                    confirmation: None,
                },
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
        };
        assert!(matches!(
            invoke("trusted.extension.source"),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "bundled"
        ));
        assert!(matches!(
            invoke("other.extension.source"),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "other"
        ));

        let idempotent = runtime.discover_and_load(bundled_root.path()).unwrap();
        assert_eq!(idempotent.len(), 1);
        assert!(matches!(
            invoke("trusted.extension.source"),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "bundled"
        ));
    }

    #[test]
    fn disable_then_enable_reloads_linked_commands() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        let id = "reenable.extension";
        fs::write(
            directory.path().join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "reenable.extension.echo",
                    handler = function() return "ready" end
                })
            "#,
        )
        .unwrap();
        let mut package_manifest = manifest(id);
        package_manifest.entrypoint = Some("main.luau".to_owned());
        let _ = runtime.install(package_manifest).unwrap();
        let _ = runtime.link(id, directory.path()).unwrap();
        runtime.enable(id).unwrap();
        let invocation = || CommandInvocation {
            command: "reenable.extension.echo".to_owned(),
            arguments: Vec::new(),
            caller: Caller::Cli,
            target: None,
            confirmation: None,
        };
        assert!(matches!(
            runtime.invoke_blocking(
                invocation(),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new()
            ),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "ready"
        ));
        runtime.disable(id).unwrap();
        assert!(matches!(
            runtime.invoke_blocking(
                invocation(),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new()
            ),
            CommandOutcome::Failed { code, .. } if code == "unknown_command"
        ));
        runtime.enable(id).unwrap();
        assert!(matches!(
            runtime.invoke_blocking(
                invocation(),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new()
            ),
            CommandOutcome::Success { value: Value::String(value), .. } if value == "ready"
        ));
    }

    #[test]
    fn package_enablement_persists_without_auto_enabling_disabled_packages() {
        let directory = tempfile::tempdir().unwrap();
        let first = runtime();
        first.set_storage_root(directory.path()).unwrap();
        let _ = first.install(manifest("persist.extension")).unwrap();
        let _ = first.enable("persist.extension").unwrap();
        drop(first);
        let second = runtime();
        second.set_storage_root(directory.path()).unwrap();
        let info = second.install(manifest("persist.extension")).unwrap();
        assert!(info.enabled);
        let _ = second.disable("persist.extension").unwrap();
        drop(second);
        let third = runtime();
        third.set_storage_root(directory.path()).unwrap();
        let info = third.install(manifest("persist.extension")).unwrap();
        assert!(!info.enabled);
    }
    #[cfg(unix)]
    #[test]
    fn process_quota_is_enforced_per_generation() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let _ = runtime.install(manifest("process.extension")).unwrap();
        let _ = runtime.link("process.extension", directory.path()).unwrap();
        let _ = runtime.enable("process.extension").unwrap();
        let generation = runtime.package("process.extension").unwrap().generation;
        let spec = ProcessSpec {
            executable: "sh".to_owned(),
            arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
            cwd: None,
            environment: BTreeMap::new(),
        };
        for _ in 0..EXTENSION_PROCESS_LIMIT {
            runtime
                .spawn_process("process.extension", generation, spec.clone())
                .unwrap();
        }
        assert_eq!(
            runtime
                .spawn_process("process.extension", generation, spec)
                .unwrap_err()
                .code,
            "process_quota_exceeded"
        );
        runtime.reload("process.extension").unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn concurrent_process_spawns_respect_reserved_quota() {
        let runtime = Arc::new(runtime());
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let _ = runtime
            .install(manifest("concurrent-process.extension"))
            .unwrap();
        let _ = runtime
            .link("concurrent-process.extension", directory.path())
            .unwrap();
        runtime.enable("concurrent-process.extension").unwrap();
        let generation = runtime
            .package("concurrent-process.extension")
            .unwrap()
            .generation;
        let spec = ProcessSpec {
            executable: "sh".to_owned(),
            arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
            cwd: None,
            environment: BTreeMap::new(),
        };
        let threads = (0..(EXTENSION_PROCESS_LIMIT * 2))
            .map(|_| {
                let runtime = Arc::clone(&runtime);
                let spec = spec.clone();
                std::thread::spawn(move || {
                    runtime.spawn_process("concurrent-process.extension", generation, spec)
                })
            })
            .collect::<Vec<_>>();
        let outcomes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            EXTENSION_PROCESS_LIMIT
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome
                    .as_ref()
                    .is_err_and(|error| error.code == "process_quota_exceeded"))
                .count(),
            EXTENSION_PROCESS_LIMIT
        );
        runtime.reload("concurrent-process.extension").unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn process_control_requires_exact_extension_and_generation() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let _ = runtime.install(manifest("owner-a.extension")).unwrap();
        let _ = runtime.install(manifest("owner-b.extension")).unwrap();
        let _ = runtime.link("owner-a.extension", directory.path()).unwrap();
        let _ = runtime.link("owner-b.extension", directory.path()).unwrap();
        runtime.enable("owner-a.extension").unwrap();
        runtime.enable("owner-b.extension").unwrap();
        let generation_a = runtime.package("owner-a.extension").unwrap().generation;
        let generation_b = runtime.package("owner-b.extension").unwrap().generation;
        let status = runtime
            .spawn_process(
                "owner-a.extension",
                generation_a,
                ProcessSpec {
                    executable: "sh".to_owned(),
                    arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
                    cwd: None,
                    environment: BTreeMap::new(),
                },
            )
            .unwrap();
        assert_eq!(
            runtime
                .process_status("owner-b.extension", generation_b, &status.id)
                .unwrap_err()
                .code,
            "stale_generation"
        );
        assert_eq!(
            runtime
                .process_signal("owner-a.extension", generation_a + 1, &status.id)
                .unwrap_err()
                .code,
            "stale_generation"
        );
        runtime.reload("owner-a.extension").unwrap();
        assert_eq!(
            runtime
                .process_status("owner-a.extension", generation_a, &status.id)
                .unwrap_err()
                .code,
            "unknown_process"
        );
    }
    #[cfg(unix)]
    #[test]
    fn process_reader_streams_bounded_partial_output_before_eof() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "").unwrap();
        let _ = runtime.install(manifest("stream.extension")).unwrap();
        let _ = runtime.link("stream.extension", directory.path()).unwrap();
        let _ = runtime.enable("stream.extension").unwrap();
        let generation = runtime.package("stream.extension").unwrap().generation;
        let status = runtime
            .spawn_process(
                "stream.extension",
                generation,
                ProcessSpec {
                    executable: "sh".to_owned(),
                    arguments: vec![
                        "-c".to_owned(),
                        "i=0; while [ $i -lt 200000 ]; do printf x; i=$((i+1)); done; sleep 10"
                            .to_owned(),
                    ],
                    cwd: None,
                    environment: BTreeMap::new(),
                },
            )
            .unwrap();
        thread::sleep(Duration::from_millis(100));
        let lines = runtime
            .process_read(
                "stream.extension",
                generation,
                &status.id,
                EXTENSION_PROCESS_LINES,
            )
            .unwrap();
        assert!(!lines.is_empty());
        assert!(
            lines
                .iter()
                .all(|line| line.line.len() <= EXTENSION_PROCESS_BYTES)
        );
        runtime.reload("stream.extension").unwrap();
    }
    #[test]
    fn exact_invocation_rejects_reloaded_generation() {
        let runtime = runtime();
        let _ = runtime.install(manifest("generation.extension")).unwrap();
        let _ = runtime.enable("generation.extension").unwrap();
        let generation = runtime.package("generation.extension").unwrap().generation;
        runtime
            .register_command(
                "generation.extension",
                generation,
                descriptor("generation.extension.echo"),
                Arc::new(|_| Ok(Value::String("ok".to_owned()))),
            )
            .unwrap();
        let _ = runtime.reload("generation.extension").unwrap();
        let invocation = CommandInvocation {
            command: "generation.extension.echo".to_owned(),
            arguments: vec!["x".to_owned()],
            caller: Caller::Luau,
            target: None,
            confirmation: None,
        };
        let outcome = runtime
            .invoke_async_exact(
                invocation,
                "generation.extension",
                generation,
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            outcome,
            CommandOutcome::Failed { code, .. } if code == "stale_generation"
        ));
    }

    #[test]
    fn lifecycle_reload_publishes_retirement_then_new_generation_snapshot() {
        let hub = AutomationHub::new();
        let scope = "instance:extension-lifecycle".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let runtime = ExtensionRuntime::new(hub.clone());
        let owner = OwnerIdentity::new(1, 1);
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "lifecycle.extension.echo",
                    handler = function() return "ok" end
                })
                bootty.events.register("changed")
            "#,
        )
        .unwrap();
        let _ = runtime.install(manifest("lifecycle.extension")).unwrap();
        let _ = runtime
            .link("lifecycle.extension", directory.path())
            .unwrap();
        runtime.enable("lifecycle.extension").unwrap();

        let loaded = hub.events().poll(&subscription, &owner, 0).unwrap();
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.events[0].payload["operation"], "loaded");
        assert_eq!(loaded.events[0].payload["generation"], 1);
        let loaded_snapshot = &loaded.events[0].payload["snapshot"];
        assert_eq!(loaded_snapshot["modules"][0]["generation"], 1);

        runtime.reload("lifecycle.extension").unwrap();
        runtime.load_linked_package("lifecycle.extension").unwrap();
        let reloaded = hub
            .events()
            .poll(&subscription, &owner, loaded.cursor)
            .unwrap();
        assert_eq!(reloaded.events.len(), 2);
        assert_eq!(reloaded.events[0].payload["operation"], "retired");
        assert_eq!(reloaded.events[0].payload["generation"], 1);
        assert_eq!(reloaded.events[1].payload["operation"], "reloaded");
        assert_eq!(reloaded.events[1].payload["generation"], 2);
        let snapshot = &reloaded.events[1].payload["snapshot"];
        assert_eq!(snapshot["modules"][0]["generation"], 2);
        assert_eq!(snapshot["commands"][0]["id"], "lifecycle.extension.echo");
        assert_eq!(snapshot["commands"][0]["generation"], 2);
        assert_eq!(
            snapshot["events"][0]["topic"],
            "lifecycle.extension.changed"
        );
        assert_eq!(snapshot["events"][0]["generation"], 2);
    }

    #[test]
    fn lifecycle_disable_publishes_disabled_for_retiring_generation() {
        let hub = AutomationHub::new();
        let scope = "instance:extension-disable".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let runtime = ExtensionRuntime::new(hub.clone());
        let owner = OwnerIdentity::new(1, 2);
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope.clone(),
            )
            .unwrap()
            .subscription;
        let _ = runtime.install(manifest("disable.extension")).unwrap();
        runtime.enable("disable.extension").unwrap();
        let loaded = hub.events().poll(&subscription, &owner, 0).unwrap();
        assert_eq!(
            loaded.events[0].payload["operation"],
            "enabled_pending_source"
        );

        runtime.disable("disable.extension").unwrap();
        let disabled = hub
            .events()
            .poll(&subscription, &owner, loaded.cursor)
            .unwrap();
        assert_eq!(disabled.events.len(), 1);
        assert_eq!(disabled.events[0].payload["operation"], "disabled");
        assert_eq!(disabled.events[0].payload["generation"], 1);
        let snapshot = &disabled.events[0].payload["snapshot"];
        assert!(snapshot["modules"].as_array().is_some_and(Vec::is_empty));
    }

    #[test]
    fn lifecycle_dedup_is_bounded_and_backpressure_precedes_reload_mutation() {
        let runtime = runtime();
        let id = "lifecycle-backpressure.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        {
            let mut lifecycle = runtime.inner.lifecycle_publications.lock().unwrap();
            lifecycle.pending.clear();
            for generation in 1..=(EXTENSION_LIFECYCLE_DEDUP_LIMIT as u64 + 32) {
                lifecycle.mark_published(&LifecyclePublication {
                    extension_id: id.to_owned(),
                    generation,
                    operation: "reloaded".to_owned(),
                    snapshot: Value::Null,
                });
            }
            assert!(lifecycle.published.len() <= EXTENSION_LIFECYCLE_DEDUP_LIMIT);
            for index in 0..(EXTENSION_LIFECYCLE_PENDING_LIMIT - 2) {
                lifecycle.pending.push_back(LifecyclePublication {
                    extension_id: format!("pending-{index}"),
                    generation: 1,
                    operation: "reloaded".to_owned(),
                    snapshot: Value::Null,
                });
            }
        }
        let info = runtime.reload(id).unwrap();
        assert_eq!(info.generation, 2);
        let error = runtime.reload(id).unwrap_err();
        assert_eq!(error.code, "lifecycle_backpressure");
        assert_eq!(runtime.package(id).unwrap().generation, 2);
    }

    #[test]
    fn linked_load_and_reload_serialize_generation_resources() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "load-reload-barrier.extension.echo",
                    handler = function() return "ok" end
                })
                bootty.events.register("changed")
            "#,
        )
        .unwrap();
        let runtime = Arc::new(runtime());
        let id = "load-reload-barrier.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.link(id, directory.path()).unwrap();
        {
            let mut state = runtime.inner.state.write().unwrap();
            state.packages.get_mut(id).unwrap().enabled = true;
        }
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let load_runtime = Arc::clone(&runtime);
        let load_barrier = Arc::clone(&barrier);
        let load = std::thread::spawn(move || {
            load_barrier.wait();
            load_runtime.load_linked_package(id)
        });
        let reload_runtime = Arc::clone(&runtime);
        let reload_barrier = Arc::clone(&barrier);
        let reload = std::thread::spawn(move || {
            reload_barrier.wait();
            reload_runtime.reload(id)
        });
        barrier.wait();
        let _ = load.join().unwrap();
        let _ = reload.join().unwrap();
        let package = runtime.package(id).unwrap();
        assert_eq!(package.generation, 2);
        let state = runtime.inner.state.read().unwrap();
        assert!(
            state
                .commands
                .values()
                .all(|command| command.generation == package.generation)
        );
        assert!(
            state
                .events
                .values()
                .all(|event| event.generation.generation == package.generation)
        );
    }

    #[test]
    fn unlinked_enable_then_discover_load_publishes_final_inventory() {
        let hub = AutomationHub::new();
        let scope = "instance:discover-after-enable".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let runtime = ExtensionRuntime::new(hub.clone());
        let owner = OwnerIdentity::new(7, 7);
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope,
            )
            .unwrap()
            .subscription;
        let id = "discover-after-enable.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();

        let root = tempfile::tempdir().unwrap();
        let package_root = root.path().join(id);
        fs::create_dir(&package_root).unwrap();
        fs::write(
            package_root.join("extension.json"),
            serde_json::to_vec(&manifest(id)).unwrap(),
        )
        .unwrap();
        fs::write(
            package_root.join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "discover-after-enable.extension.echo",
                    handler = function() return "ok" end
                })
            "#,
        )
        .unwrap();
        runtime.discover_and_load(root.path()).unwrap();
        let events = hub.events().poll(&subscription, &owner, 0).unwrap();
        assert_eq!(events.events.len(), 2);
        assert_eq!(
            events.events[0].payload["operation"],
            "enabled_pending_source"
        );
        assert_eq!(events.events[1].payload["operation"], "loaded");
        assert_eq!(
            events.events[1].payload["snapshot"]["commands"][0]["id"],
            "discover-after-enable.extension.echo"
        );
    }

    #[test]
    fn linked_startup_load_rejects_full_lifecycle_queue_without_handlers() {
        let runtime = runtime();
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("main.luau"),
            r#"
                bootty.commands.register({
                    id = "startup-capacity.extension.echo",
                    handler = function() return "ok" end
                })
            "#,
        )
        .unwrap();
        let id = "startup-capacity.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.link(id, directory.path()).unwrap();
        {
            let mut state = runtime.inner.state.write().unwrap();
            state.packages.get_mut(id).unwrap().enabled = true;
        }
        {
            let mut lifecycle = runtime.inner.lifecycle_publications.lock().unwrap();
            for index in 0..EXTENSION_LIFECYCLE_PENDING_LIMIT {
                lifecycle.pending.push_back(LifecyclePublication {
                    extension_id: format!("pending-{index}"),
                    generation: 1,
                    operation: "loaded".to_owned(),
                    snapshot: Value::Null,
                });
            }
        }
        let error = runtime.load_linked_package(id).unwrap_err();
        assert_eq!(error.code, "lifecycle_backpressure");
        assert!(
            !runtime
                .inner
                .state
                .read()
                .unwrap()
                .commands
                .contains_key("startup-capacity.extension.echo")
        );
        runtime
            .inner
            .lifecycle_publications
            .lock()
            .unwrap()
            .pending
            .clear();
        runtime.load_linked_package(id).unwrap();
        assert!(
            runtime
                .inner
                .state
                .read()
                .unwrap()
                .commands
                .contains_key("startup-capacity.extension.echo")
        );
    }

    #[test]
    fn lifecycle_publication_retry_preserves_order_and_deduplicates() {
        let hub = AutomationHub::new();
        let scope = "instance:extension-retry".to_owned();
        hub.bind_instance_scope(scope.clone()).unwrap();
        let runtime = ExtensionRuntime::new(hub.clone());
        let owner = OwnerIdentity::new(1, 3);
        let subscription = hub
            .events()
            .subscribe(
                owner.clone(),
                BTreeSet::from(["extension.reloaded".to_owned()]),
                scope,
            )
            .unwrap()
            .subscription;
        let oversized = Value::String("x".repeat(crate::automation::hub::EVENT_QUEUE_BYTE_LIMIT));
        assert!(
            runtime
                .publish_lifecycle("retry.extension", 1, "retired", oversized)
                .is_err()
        );
        {
            let mut lifecycle = match runtime.inner.lifecycle_publications.lock() {
                Ok(lifecycle) => lifecycle,
                Err(_) => panic!("lifecycle publication lock poisoned"),
            };
            lifecycle
                .pending
                .front_mut()
                .expect("pending retry publication")
                .snapshot = json!({
                "modules": [],
                "commands": [],
                "events": [],
            });
        }
        runtime
            .publish_lifecycle(
                "retry.extension",
                1,
                "retired",
                json!({"modules": [], "commands": [], "events": []}),
            )
            .unwrap();
        let events = hub.events().poll(&subscription, &owner, 0).unwrap();
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].payload["operation"], "retired");
        assert_eq!(events.events[0].payload["generation"], 1);
        runtime
            .publish_lifecycle(
                "retry.extension",
                1,
                "retired",
                json!({"modules": [], "commands": [], "events": []}),
            )
            .unwrap();
        let duplicate = hub
            .events()
            .poll(&subscription, &owner, events.cursor)
            .unwrap();
        assert!(duplicate.events.is_empty());
    }

    #[test]
    fn lifecycle_publish_failure_does_not_leave_disable_in_cancelled_state() {
        let hub = AutomationHub::new();
        hub.bind_instance_scope("instance:extension-disable-failure")
            .unwrap();
        let runtime = ExtensionRuntime::new(hub);
        let id = "disable-failure.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let oversized = Value::String("x".repeat(crate::automation::hub::EVENT_QUEUE_BYTE_LIMIT));
        assert!(
            runtime
                .publish_lifecycle(id, generation, "retired", oversized)
                .is_err()
        );

        let info = runtime.disable(id).unwrap();
        assert!(!info.enabled);
        assert!(!runtime.package(id).unwrap().enabled);
    }

    #[test]
    fn lifecycle_publish_failure_does_not_leave_reload_on_cancelled_generation() {
        let hub = AutomationHub::new();
        hub.bind_instance_scope("instance:extension-reload-failure")
            .unwrap();
        let runtime = ExtensionRuntime::new(hub);
        let id = "reload-failure.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let oversized = Value::String("x".repeat(crate::automation::hub::EVENT_QUEUE_BYTE_LIMIT));
        assert!(
            runtime
                .publish_lifecycle(id, generation, "retired", oversized)
                .is_err()
        );

        let info = runtime.reload(id).unwrap();
        assert_eq!(info.generation, generation + 1);
        assert!(info.enabled);
        assert!(runtime.generation_is_active(id, info.generation));
    }

    #[test]
    fn enable_persistence_failure_preserves_memory_and_reopen_state() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("packages.json")).unwrap();
        let runtime = runtime();
        runtime.set_storage_root(directory.path()).unwrap();
        let id = "enable-persistence.extension";
        runtime.install(manifest(id)).unwrap();
        assert!(runtime.enable(id).is_err());
        assert!(!runtime.package(id).unwrap().enabled);

        fs::remove_dir(directory.path().join("packages.json")).unwrap();
        runtime.enable(id).unwrap();
        drop(runtime);
        let reopened = self::runtime();
        reopened.set_storage_root(directory.path()).unwrap();
        assert!(reopened.install(manifest(id)).unwrap().enabled);
    }

    #[test]
    fn disable_persistence_failure_preserves_enabled_memory_until_retry() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime();
        runtime.set_storage_root(directory.path()).unwrap();
        let id = "disable-persistence.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        fs::remove_file(directory.path().join("packages.json")).unwrap();
        fs::create_dir(directory.path().join("packages.json")).unwrap();

        assert!(runtime.disable(id).is_err());
        assert!(runtime.package(id).unwrap().enabled);
        fs::remove_dir(directory.path().join("packages.json")).unwrap();
        runtime.disable(id).unwrap();
        drop(runtime);
        let reopened = self::runtime();
        reopened.set_storage_root(directory.path()).unwrap();
        assert!(!reopened.install(manifest(id)).unwrap().enabled);
    }

    #[test]
    fn cleanup_retry_capacity_failure_preserves_durable_enablement() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime();
        runtime.set_storage_root(directory.path()).unwrap();
        let id = "cleanup-capacity.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        {
            let mut retries = match runtime.inner.cleanup_retries.lock() {
                Ok(retries) => retries,
                Err(_) => panic!("cleanup retry lock poisoned"),
            };
            for index in 0..EXTENSION_CLEANUP_RETRY_LIMIT {
                retries.insert((format!("occupied-{index}.extension"), 1), 1);
            }
        }
        let error = runtime.ensure_cleanup_retry_capacity(id, 1).unwrap_err();
        assert_eq!(error.code, "cleanup_retry_full");
        assert!(runtime.package(id).unwrap().enabled);
        {
            let mut retries = match runtime.inner.cleanup_retries.lock() {
                Ok(retries) => retries,
                Err(_) => panic!("cleanup retry lock poisoned"),
            };
            retries.clear();
        }
        runtime.disable(id).unwrap();
        drop(runtime);
        let reopened = self::runtime();
        reopened.set_storage_root(directory.path()).unwrap();
        assert!(!reopened.install(manifest(id)).unwrap().enabled);
    }

    #[test]
    fn reload_blocks_until_failed_cleanup_tombstone_is_resolved() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime();
        runtime.set_storage_root(directory.path()).unwrap();
        let id = "reload-cleanup-pending.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let package_before = runtime.package(id).unwrap();
        let persisted_before = fs::read(directory.path().join("packages.json")).unwrap();
        let resource = ExtensionResourceBinding {
            generation: ExtensionGeneration {
                extension_id: id.to_owned(),
                generation,
            },
            owner: runtime.inner.owner.clone(),
            scope: "instance:cleanup-pending".to_owned(),
        };
        let metadata_key = MetadataBindingKey {
            resource: resource.clone(),
            namespace: "../invalid".to_owned(),
            key: "key".to_owned(),
            target: String::new(),
        };
        {
            let mut state = runtime.inner.state.write().unwrap();
            state.metadata.insert(
                metadata_key,
                MetadataBinding {
                    resource,
                    namespace: "../invalid".to_owned(),
                    key: "key".to_owned(),
                    target: None,
                },
            );
        }
        runtime.record_cleanup_retry(id, generation).unwrap();

        let error = runtime.reload(id).unwrap_err();
        assert_eq!(error.code, "cleanup_pending");
        let blocked = runtime.package(id).unwrap();
        assert_eq!(blocked.generation, generation);
        assert_eq!(blocked, package_before);
        assert_eq!(
            fs::read(directory.path().join("packages.json")).unwrap(),
            persisted_before
        );

        runtime.inner.state.write().unwrap().metadata.clear();
        let reloaded = runtime.reload(id).unwrap();
        assert_eq!(reloaded.generation, generation + 1);
    }

    #[test]
    fn linked_load_failure_returns_without_recursive_lifecycle_deadlock() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("main.luau"), "while true do end").unwrap();
        let runtime = Arc::new(runtime());
        let id = "linked-load-failure.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.link(id, directory.path()).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker_runtime = Arc::clone(&runtime);
        std::thread::spawn(move || {
            let result = worker_runtime.enable(id);
            sender
                .send(result.map(|_| ()).map_err(|error| error.code))
                .unwrap();
        });
        let result = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("linked load must return within the lifecycle deadline");
        assert!(result.is_err());
        assert!(!runtime.package(id).unwrap().enabled);
    }

    #[test]
    fn concurrent_reloads_serialize_generation_commit() {
        let runtime = Arc::new(runtime());
        let id = "concurrent-reload.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let first_runtime = Arc::clone(&runtime);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_runtime.reload(id).unwrap().generation
        });
        let second_runtime = Arc::clone(&runtime);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_runtime.reload(id).unwrap().generation
        });
        barrier.wait();
        let mut generations = vec![first.join().unwrap(), second.join().unwrap()];
        generations.sort_unstable();
        assert_eq!(generations, [2, 3]);
    }
    #[test]
    fn extension_tasks_bind_generation_and_owner_before_hub_access() {
        let runtime = runtime();
        let _ = runtime.install(manifest("first.extension")).unwrap();
        let _ = runtime.install(manifest("second.extension")).unwrap();
        let _ = runtime.enable("first.extension").unwrap();
        let _ = runtime.enable("second.extension").unwrap();
        let first_generation = runtime.package("first.extension").unwrap().generation;
        let second_generation = runtime.package("second.extension").unwrap().generation;
        let owner = OwnerIdentity::new(71, 11);
        let task = runtime
            .start_task(
                "first.extension",
                first_generation,
                owner.clone(),
                extension_scope("first.extension", first_generation),
                CommandCancellation::new(),
            )
            .unwrap();
        let cross_extension = runtime
            .task_status(&task.id, "second.extension", second_generation, &owner)
            .unwrap_err();
        assert_eq!(cross_extension.code, "task_owner_mismatch");
        let _ = runtime.reload("first.extension").unwrap();
        let stale = runtime
            .task_status(&task.id, "first.extension", first_generation, &owner)
            .unwrap_err();
        assert_eq!(stale.code, "stale_generation");
    }

    #[test]
    fn storage_namespace_rejects_traversal_and_symlink_components() {
        let directory = tempfile::tempdir().unwrap();
        assert!(storage_path(directory.path(), "../escape", "namespace", "key").is_err());
        assert!(storage_path(directory.path(), "/absolute", "namespace", "key").is_err());
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let package_dir = directory.path().join("safe.extension");
            fs::create_dir_all(directory.path()).unwrap();
            std::os::unix::fs::symlink(outside.path(), &package_dir).unwrap();
            assert!(storage_path(directory.path(), "safe.extension", "namespace", "key").is_err());
        }
    }
    #[test]
    fn storage_failure_preserves_cached_and_durable_value_until_retry() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = runtime();
        runtime.set_storage_root(directory.path()).unwrap();
        let id = "storage-failure.extension";
        let mut package = manifest(id);
        package.storage_namespace = Some("durable".to_owned());
        runtime.install(package).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let owner = runtime.inner.owner.clone();
        let key = "value";
        runtime
            .storage_put(id, generation, &owner, key, json!("old"))
            .unwrap();
        let path = storage_path(directory.path(), id, "durable", key).unwrap();
        assert_eq!(fs::read(&path).unwrap(), br#""old""#);

        {
            let mut state = runtime.inner.state.write().unwrap();
            state
                .packages
                .get_mut(id)
                .unwrap()
                .manifest
                .storage_namespace = Some("../invalid".to_owned());
        }
        assert!(
            runtime
                .storage_put(id, generation, &owner, key, json!("new"))
                .is_err()
        );
        assert_eq!(
            runtime.storage_get(id, generation, &owner, key).unwrap(),
            Some(json!("old"))
        );
        assert_eq!(fs::read(&path).unwrap(), br#""old""#);
        assert!(runtime.storage_delete(id, generation, &owner, key).is_err());
        assert_eq!(
            runtime.storage_get(id, generation, &owner, key).unwrap(),
            Some(json!("old"))
        );
        assert_eq!(fs::read(&path).unwrap(), br#""old""#);

        {
            let mut state = runtime.inner.state.write().unwrap();
            state
                .packages
                .get_mut(id)
                .unwrap()
                .manifest
                .storage_namespace = Some("durable".to_owned());
        }
        runtime
            .storage_put(id, generation, &owner, key, json!("new"))
            .unwrap();
        assert_eq!(fs::read(&path).unwrap(), br#""new""#);
        runtime.storage_delete(id, generation, &owner, key).unwrap();
        assert_eq!(
            runtime.storage_get(id, generation, &owner, key).unwrap(),
            None
        );
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_reaps_process_before_external_cleanup_retry() {
        let runtime = runtime();
        let id = "cleanup-process.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        let status = runtime
            .spawn_process(
                id,
                generation,
                ProcessSpec {
                    executable: "sh".to_owned(),
                    arguments: vec!["-c".to_owned(), "sleep 10".to_owned()],
                    cwd: None,
                    environment: BTreeMap::new(),
                },
            )
            .unwrap();
        let process = runtime
            .inner
            .state
            .read()
            .unwrap()
            .processes
            .get(&status.id)
            .cloned()
            .unwrap();
        let resource = ExtensionResourceBinding {
            generation: ExtensionGeneration {
                extension_id: id.to_owned(),
                generation,
            },
            owner: runtime.inner.owner.clone(),
            scope: extension_scope(id, generation),
        };
        let metadata_key = MetadataBindingKey {
            resource: resource.clone(),
            namespace: "../invalid".to_owned(),
            key: "key".to_owned(),
            target: String::new(),
        };
        runtime.inner.state.write().unwrap().metadata.insert(
            metadata_key.clone(),
            MetadataBinding {
                resource,
                namespace: "../invalid".to_owned(),
                key: "key".to_owned(),
                target: None,
            },
        );

        let error = runtime
            .cleanup_generation(id, generation, None)
            .unwrap_err();
        assert_eq!(error.code, "invalid_metadata");
        assert!(process.child.lock().unwrap().try_wait().unwrap().is_some());
        let state = runtime.inner.state.read().unwrap();
        assert!(!state.processes.contains_key(&status.id));
        assert!(state.metadata.contains_key(&metadata_key));
        drop(state);
        assert!(
            runtime
                .inner
                .cleanup_retries
                .lock()
                .unwrap()
                .contains_key(&(id.to_owned(), generation))
        );

        let mut state = runtime.inner.state.write().unwrap();
        state.metadata.get_mut(&metadata_key).unwrap().namespace = "valid".to_owned();
        drop(state);
        runtime.cleanup_generation(id, generation, None).unwrap();
        assert!(
            !runtime
                .inner
                .cleanup_retries
                .lock()
                .unwrap()
                .contains_key(&(id.to_owned(), generation))
        );
    }

    #[test]
    fn event_capabilities_reject_cross_extension_and_forged_scope_access() {
        let runtime = runtime();
        runtime.install(manifest("event-a.extension")).unwrap();
        runtime.install(manifest("event-b.extension")).unwrap();
        runtime.enable("event-a.extension").unwrap();
        runtime.enable("event-b.extension").unwrap();
        let generation_a = runtime.package("event-a.extension").unwrap().generation;
        let generation_b = runtime.package("event-b.extension").unwrap().generation;
        runtime
            .register_event("event-a.extension", generation_a, "topic")
            .unwrap();
        runtime
            .register_event("event-b.extension", generation_b, "topic")
            .unwrap();
        let owner = OwnerIdentity::new(900, 1);
        let capabilities = ExtensionRuntimeCapabilities {
            runtime: runtime.clone(),
            generation: ExtensionGeneration {
                extension_id: "event-a.extension".to_owned(),
                generation: generation_a,
            },
            owner: owner.clone(),
            cancellation: CommandCancellation::new(),
        };

        let publish_error = capabilities
            .publish_event("event-b.extension.topic", Value::Null, None)
            .unwrap_err();
        assert_eq!(publish_error.code, "stale_generation");
        let subscribe_error = capabilities
            .subscribe_event("event-b.extension.topic")
            .unwrap_err();
        assert_eq!(subscribe_error.code, "stale_generation");
        let forged_target = capabilities
            .publish_event(
                "event-a.extension.topic",
                Value::Null,
                Some(CommandTarget {
                    kind: ResourceKind::Extension,
                    handle: "event-b.extension".to_owned(),
                    generation: generation_b,
                }),
            )
            .unwrap_err();
        assert_eq!(forged_target.code, "invalid_binding_target");
        let forged_scope = runtime
            .publish_event(
                "event-a.extension",
                generation_a,
                "event-a.extension.topic",
                "instance:forged",
                Value::Null,
                None,
            )
            .unwrap_err();
        assert_eq!(forged_scope.code, "invalid_binding_scope");
        let forged_subscription_scope = runtime
            .subscribe_event(
                "event-a.extension",
                generation_a,
                owner,
                "event-a.extension.topic",
                "instance:forged".to_owned(),
            )
            .unwrap_err();
        assert_eq!(forged_subscription_scope.code, "invalid_binding_scope");
    }

    #[test]
    fn stale_event_capability_cannot_retain_subscription_after_reload() {
        let runtime = runtime();
        let id = "event-stale.extension";
        runtime.install(manifest(id)).unwrap();
        runtime.enable(id).unwrap();
        let generation = runtime.package(id).unwrap().generation;
        runtime.register_event(id, generation, "topic").unwrap();
        let owner = OwnerIdentity::new(901, 1);
        let capabilities = ExtensionRuntimeCapabilities {
            runtime: runtime.clone(),
            generation: ExtensionGeneration {
                extension_id: id.to_owned(),
                generation,
            },
            owner: owner.clone(),
            cancellation: CommandCancellation::new(),
        };
        let (subscription, _) = capabilities
            .subscribe_event("event-stale.extension.topic")
            .unwrap();
        runtime.reload(id).unwrap();
        let stale = capabilities
            .subscribe_event("event-stale.extension.topic")
            .unwrap_err();
        assert_eq!(stale.code, "stale_generation");
        assert!(
            !runtime
                .inner
                .state
                .read()
                .unwrap()
                .subscriptions
                .contains_key(&subscription)
        );
        assert!(
            runtime
                .inner
                .automation
                .events()
                .poll(&subscription, &owner, 0)
                .is_err()
        );
    }
}
