use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    hash::Hasher,
    net::{IpAddr, UdpSocket},
    path::PathBuf,
    sync::{
        Arc, LazyLock, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use bootty_config::config::{MultiplexerBackendConfig, MultiplexerConfig};
#[cfg(test)]
use bootty_config::config::{SshProfileConfig, SshRemoteConfig};
use eframe::egui::{self, Pos2, Rect};
use serde_json::{Value, json};

mod copy_mode;
#[cfg(debug_assertions)]
mod diagnostic_actions;
mod recorded_chord;
mod selection;

use copy_mode::{
    CopyModeKeyAction, copy_mode_action_for_egui_event, copy_mode_action_for_input,
    copy_mode_egui_key_may_emit_text, copy_mode_egui_key_should_pass_to_app,
    copy_mode_input_should_pass_to_app, copy_mode_key_input_present, copy_shortcut_pressed,
    direct_copy_shortcut_pressed,
};
#[cfg(test)]
use copy_mode::{CopyModeSearchRepeat, copy_mode_action_for_char, copy_mode_action_for_egui_key};
#[cfg(debug_assertions)]
use diagnostic_actions::{DiagnosticAction, DiagnosticActionDriver, DiagnosticRecord};
use recorded_chord::normalize_recorded_chord;
use selection::{TerminalSelectionAction, TerminalSelectionRouteContext, TerminalSelectionRouter};
#[cfg(test)]
use selection::{selection_drag_scroll_delta, terminal_selection_event_clamped};

use crate::{
    app_actions::{
        AppAction, AppKeyBindings, FontSizeAction, KeybindAction, MuxKeyAction, SidebarAction,
        SidebarKeyBindings, TerminalFindAction, TerminalScrollAction,
        builtin_app_invocation_for_direct_key, split_app_actions_for_bindings_with_modifier_sides,
    },
    automation::{
        AutomationError, AutomationHub, EventPublication, LaunchValidationError,
        NormalizedSessionLaunch, SessionLaunchDescriptor, TerminalOutputRead,
        directory::{
            BindingRef, ClaimOwner, ClaimantRef, DirectoryClaimSeverity, DirectoryClaims,
            DirectoryRef, InstanceRef, PaneRef, SessionRef, TerminalRef, WindowRef, WorktreeRef,
            WorktreeRemovalConfirmation,
        },
    },
    commands::{
        AppCommandReceiver, AppCommandSender, BoundAppCommandSender, Caller, CommandCancellation,
        CommandCompletionContext, CommandInvocation, CommandOutcome, CommandTarget, CommandWarning,
        Confirmation, CoreCommandExecutor, MutationClass, MuxCommandSpec, ResourceKind,
        app_command_channel_with_repaint, bounded_command_outcome,
    },
    config::{
        AppearanceMode, AppearanceVariant, BoottyConfig, ConfigState, WindowConfig,
        load_config_from_path, load_or_create_config_document,
    },
    config_reload::{CONFIG_HOT_RELOAD_INTERVAL, ConfigHotReload, new_session_only_config_changed},
    diagnostics::{
        STATUS_METRICS_SAMPLE_INTERVAL, StabilityTrace, StabilityTraceSample, StatusMetrics,
    },
    direct_input::{DirectKeyInput, ModifierSideState},
    geometry::{TerminalSurface, ViewTransform},
    git::{WorktreeCreateRequest, WorktreeRemoveRequest, WorktreeServiceError},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState,
        focus::InputFocus,
        router::{RoutedInput, route_events},
        terminal_input_commands_with_wheel_state,
    },
    input_binding::CopyToClipboard,
    layout::{Direction, Divider, PaneLayout, SplitDirection},
    modifier_remap::ModifierRemapSet,
    mux::{
        RepaintHandle,
        backend::{
            MuxAllocatedResources, MuxEvent, MuxEventPayload, MuxEventTarget, MuxEventTopic,
            MuxForegroundState, MuxOccupantIdentity, MuxPaneOption, MuxPaneState, MuxRebaseReason,
        },
        capability::{BindingOperation, BindingOperationOutcome},
        command::{MuxCommand, MuxPaneLaunchPlan, MuxSessionLaunchPlan, MuxSplitDirection},
        config::selected_backend,
        controller::{
            BindingId, BindingMuxController, MuxCommandCompletion, MuxCommandError,
            MuxCommandResult, MuxController, MuxEventObservation, MuxScope,
            SessionSelectorResolution, SpaceId, mux_session_refresh_interval,
        },
        native::NativeBackend,
        snapshot::{MuxPaneAnchor, MuxSession, MuxWindow, MuxWindowProgress},
        terminal::{ActiveTerminal, TerminalRuntime, decode_scoped_pane_id},
    },
    platform::{
        apply_macos_non_native_fullscreen_presentation, macos_handles_non_native_fullscreen_frame,
        read_clipboard_text, restore_macos_presentation, show_desktop_notification,
        write_clipboard_html, write_clipboard_text,
    },
    renderer::{RendererMetrics, TerminalRenderSource, TerminalWidget},
    scheduler::{RepaintScheduler, RepaintSignal},
    session_names::SessionNameStore,
    session_order::{BackendConnectionNamespace, SessionOrderStore, namespace_for_binding},
    terminal::{DrainStats, MouseButton, TerminalSearchDirection, TerminalSessionConfig},
    terminal_text::TerminalTextConfig,
    theme::theme_from_config,
    ui::{
        command_palette::{CommandPaletteDialog, CommandPaletteEvent},
        ditch::{DitchAction, DitchSessionDialog, DitchSessionEvent},
        keybind_help::{KeybindHelpDialog, KeybindHelpEvent},
        new_session_picker::{NewMuxSessionDialog, NewSessionPickerEvent},
        rename::{RenameSessionDialog, RenameSessionEvent, RenameTabDialog, RenameTabEvent},
        session_navigation::{BindingSessionGroup, ScopedSessionTarget},
        session_picker::{SessionPickerDialog, SessionPickerEvent},
        space::{SpaceEditorDialog, SpaceEditorEvent, default_space_icon},
        terminal_find::{TerminalFindDialog, TerminalFindEvent, TerminalFindResult},
        theme_picker::{ThemePickerDialog, ThemePickerEvent},
    },
    workspace::{
        PendingSessionRename, SessionRenamePersistenceState, SpaceMuxOverride, SpaceRemoteOverride,
        WorkspaceNamespaceUpdateContext, WorkspaceSpace, WorkspaceSpaceUpdate, WorkspaceStore,
        clear_pending_ditch, clear_pending_session_rename, delete_session_launch_plan,
        load_pending_ditches, load_pending_session_renames, load_session_launch_plans,
        persist_pending_ditch, persist_pending_session_rename, persist_session_launch_plan,
        rekey_session_launch_plan, remove_session_membership_and_launch_plan,
        rename_session_membership_and_launch_plans, session_rename_persistence_state,
        simple_session_launch_plan,
    },
};
use bootty_terminal::terminal_engine::{
    TerminalColorConfig, TerminalCopyModeAction, TerminalCursorConfig, TerminalFeatureConfig,
    TerminalSelectionFormat, TerminalSideEffect, TerminalSideEffectEvent,
    encode_iterm2_report_cell_size, encode_iterm2_report_variable, encode_osc52_response,
};

#[cfg(test)]
use crate::mux::controller::{
    MUX_SESSION_REFRESH_INTERVAL, MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED,
};
#[cfg(test)]
use crate::terminal::{KeyInput, TerminalKey};
#[cfg(test)]
use bootty_terminal::terminal_engine::TerminalCopyModeMotion;

const PRIMARY_WINDOW_STATE_KEY: &str = "main";
static NEXT_WINDOW_COMMAND_GENERATION: AtomicU64 = AtomicU64::new(1);
static NEXT_APP_COMMAND_RECONCILIATION_ID: AtomicU64 = AtomicU64::new(1);

fn next_app_command_reconciliation_id() -> u64 {
    NEXT_APP_COMMAND_RECONCILIATION_ID.fetch_add(1, Ordering::Relaxed)
}

fn process_command_handle() -> String {
    static HANDLE: OnceLock<String> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            format!("{}:{nanos:032x}", std::process::id())
        })
        .clone()
}

fn process_directory_claims(instance_id: &str) -> Result<DirectoryClaims> {
    static CLAIMS: OnceLock<Mutex<HashMap<String, std::result::Result<DirectoryClaims, String>>>> =
        OnceLock::new();
    let claims_by_instance = CLAIMS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut claims_by_instance = claims_by_instance
        .lock()
        .map_err(|_| anyhow::anyhow!("directory claims cache is poisoned"))?;
    match claims_by_instance
        .entry(instance_id.to_owned())
        .or_insert_with(|| {
            ClaimOwner::current(instance_id)
                .map_err(|error| error.to_string())
                .and_then(|owner| DirectoryClaims::open(owner).map_err(|error| error.to_string()))
        }) {
        Ok(claims) => Ok(claims.clone()),
        Err(error) => Err(anyhow::anyhow!(
            "could not initialize directory claims: {error}"
        )),
    }
}
#[derive(Clone)]
enum ClaimReleaseTarget {
    Window(WindowRef),
    Binding(BindingRef),
}

struct PendingWindowClaimRelease {
    claims: DirectoryClaims,
    target: ClaimReleaseTarget,
    automation: AutomationHub,
    scopes: Vec<String>,
    attempts: u32,
    next_attempt: Instant,
}

struct WindowClaimReleaseQueue {
    sender: mpsc::Sender<PendingWindowClaimRelease>,
}

static WINDOW_CLAIM_RELEASE_QUEUE: OnceLock<Arc<WindowClaimReleaseQueue>> = OnceLock::new();

fn window_claim_release_queue() -> Arc<WindowClaimReleaseQueue> {
    WINDOW_CLAIM_RELEASE_QUEUE
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            if let Err(error) = std::thread::Builder::new()
                .name("bootty-claim-release".to_owned())
                .spawn(move || window_claim_release_worker(receiver))
            {
                eprintln!("could not start window claim release worker: {error}");
            }
            Arc::new(WindowClaimReleaseQueue { sender })
        })
        .clone()
}

fn window_claim_release_backoff(attempts: u32) -> Duration {
    let seconds = 1_u64.checked_shl(attempts.min(5)).unwrap_or(32).min(30);
    Duration::from_secs(seconds)
}

fn publish_window_claim_release(item: &PendingWindowClaimRelease, revision: u64) {
    let mut payload = match &item.target {
        ClaimReleaseTarget::Window(window) => json!({
            "reason": "window_closed",
            "window_id": window.window_id.clone(),
        }),
        ClaimReleaseTarget::Binding(binding) => {
            let space_id = binding
                .space_id
                .parse::<u64>()
                .map(Value::from)
                .unwrap_or_else(|_| Value::String(binding.space_id.clone()));
            json!({
                "reason": "space_closed",
                "space_id": space_id,
            })
        }
    };
    payload["revision"] = Value::from(revision);
    for scope in &item.scopes {
        if let Err(error) = publish_directory_usage_changed_for_scope(
            &item.automation,
            &item.claims,
            scope.clone(),
            None,
            payload.clone(),
        ) {
            eprintln!("claim release succeeded but usage publication failed: {error}");
        }
    }
}

fn release_claims(item: &PendingWindowClaimRelease) -> std::result::Result<Option<u64>, String> {
    match &item.target {
        ClaimReleaseTarget::Window(window) => item
            .claims
            .release_window_claims(window)
            .map_err(|error| error.to_string()),
        ClaimReleaseTarget::Binding(binding) => item
            .claims
            .reconcile_live_claimants(binding, Vec::new())
            .map_err(|error| error.to_string()),
    }
}

fn claim_release_label(target: &ClaimReleaseTarget) -> String {
    match target {
        ClaimReleaseTarget::Window(window) => format!("window {}", window.window_id),
        ClaimReleaseTarget::Binding(binding) => format!("space {}", binding.space_id),
    }
}

fn fallback_window_claim_release(item: PendingWindowClaimRelease) {
    match release_claims(&item) {
        Ok(Some(revision)) => publish_window_claim_release(&item, revision),
        Ok(None) => {}
        Err(error) => eprintln!(
            "claim release worker unavailable for {}; synchronous fallback failed: {error}; \
             durable owner snapshot remains for authoritative reconciliation",
            claim_release_label(&item.target)
        ),
    }
}

fn coalesce_window_claim_release(
    pending: &mut Vec<PendingWindowClaimRelease>,
    item: PendingWindowClaimRelease,
) {
    if let Some(existing) = pending.iter_mut().find(|existing| {
        std::mem::discriminant(&existing.target) == std::mem::discriminant(&item.target)
            && match (&existing.target, &item.target) {
                (ClaimReleaseTarget::Window(left), ClaimReleaseTarget::Window(right)) => {
                    left == right
                }
                (ClaimReleaseTarget::Binding(left), ClaimReleaseTarget::Binding(right)) => {
                    left == right
                }
                _ => false,
            }
    }) {
        existing.next_attempt = existing.next_attempt.min(item.next_attempt);
        existing.attempts = existing.attempts.max(item.attempts);
    } else {
        pending.push(item);
    }
}

fn window_claim_release_worker(receiver: mpsc::Receiver<PendingWindowClaimRelease>) {
    let mut pending: Vec<PendingWindowClaimRelease> = Vec::new();
    loop {
        let now = Instant::now();
        if let Some(index) = pending.iter().position(|item| item.next_attempt <= now) {
            let mut item = pending.swap_remove(index);
            match release_claims(&item) {
                Ok(Some(revision)) => publish_window_claim_release(&item, revision),
                Ok(None) => {}
                Err(error) => {
                    item.attempts = item.attempts.saturating_add(1);
                    let attempts = item.attempts;
                    if attempts == 1 || attempts == 3 || attempts.is_multiple_of(8) {
                        eprintln!(
                            "claim release attempt {attempts} failed for {}: {error}; \
                             retrying with durable claims retained",
                            claim_release_label(&item.target)
                        );
                    }
                    item.next_attempt = Instant::now() + window_claim_release_backoff(attempts);
                    coalesce_window_claim_release(&mut pending, item);
                }
            }
            continue;
        }

        let wait = pending
            .iter()
            .map(|item| item.next_attempt.saturating_duration_since(now))
            .min();
        match wait {
            Some(wait) => match receiver.recv_timeout(wait) {
                Ok(item) => coalesce_window_claim_release(&mut pending, item),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    for item in pending.drain(..) {
                        fallback_window_claim_release(item);
                    }
                    return;
                }
            },
            None => match receiver.recv() {
                Ok(item) => coalesce_window_claim_release(&mut pending, item),
                Err(_) => return,
            },
        }
    }
}

fn enqueue_claim_release(item: PendingWindowClaimRelease) {
    match release_claims(&item) {
        Ok(Some(revision)) => publish_window_claim_release(&item, revision),
        Ok(None) => {}
        Err(error) => {
            eprintln!(
                "claim release failed for {}; retaining durable claims for retry: {error}",
                claim_release_label(&item.target)
            );
            let queue = window_claim_release_queue();
            if let Err(error) = queue.sender.send(item) {
                fallback_window_claim_release(error.0);
            }
        }
    }
}

fn enqueue_window_claim_release(
    claims: DirectoryClaims,
    window: WindowRef,
    automation: AutomationHub,
    scopes: Vec<String>,
) {
    enqueue_claim_release(PendingWindowClaimRelease {
        claims,
        target: ClaimReleaseTarget::Window(window),
        automation,
        scopes,
        attempts: 0,
        next_attempt: Instant::now(),
    });
}

fn enqueue_binding_claim_release(
    claims: DirectoryClaims,
    binding: BindingRef,
    automation: AutomationHub,
    scopes: Vec<String>,
) {
    enqueue_claim_release(PendingWindowClaimRelease {
        claims,
        target: ClaimReleaseTarget::Binding(binding),
        automation,
        scopes,
        attempts: 0,
        next_attempt: Instant::now(),
    });
}

fn next_window_command_generation() -> u64 {
    NEXT_WINDOW_COMMAND_GENERATION.fetch_add(1, Ordering::Relaxed)
}
/// Session-finder heading for sessions running in a backend that no Space has claimed.
const UNCLAIMED_SESSIONS_LABEL: &str = "No space";

/// How soon to wake up for the next session poll, for backends that only report through polling.
/// Native sessions live in-process and report themselves, so they schedule nothing.
fn mux_refresh_repaint_after(
    config: &crate::config::MultiplexerConfig,
    window_focused: bool,
) -> Option<Duration> {
    (selected_backend(config) != MultiplexerBackendConfig::Native)
        .then(|| mux_session_refresh_interval(window_focused))
}
/// Per-frame snapshot of everything the state machine needs from the host.
/// Captured once at frame start; `egui::Context` never enters this module.
#[derive(Clone, Debug)]
pub struct FrameInputs {
    pub now: Instant,
    pub stable_dt_ms: f32,
    pub events: Vec<egui::Event>,
    pub dropped_file_paths: Vec<PathBuf>,
    pub modifiers: egui::Modifiers,
    pub hover_pos: Option<Pos2>,
    pub pressed_mouse_button: Option<MouseButton>,
    pub viewport: ViewportSnapshot,
    /// Whether the window has focus. Background work that only someone watching would notice —
    /// polling the backend for sessions, animating chrome — backs off when it is false.
    pub window_focused: bool,
    pub renderer_metrics: RendererMetrics,
    pub terminal_cell_width: f32,
    pub terminal_cell_height: f32,
    pub terminal_scale_factor: f32,
    pub terminal_view_transform: ViewTransform,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalFileHandoff {
    Ready(String),
    Rejected(&'static str),
}

fn local_file_handoff(paths: &[PathBuf]) -> LocalFileHandoff {
    if paths.is_empty() {
        return LocalFileHandoff::Rejected("file handoff ignored: no local files");
    }
    if paths.iter().any(|path| !path.exists()) {
        return LocalFileHandoff::Rejected("file handoff rejected: local path is unavailable");
    }
    bootty_winit::file_paths::format_file_paths_for_paste(paths.iter().map(PathBuf::as_path))
        .map(LocalFileHandoff::Ready)
        .unwrap_or(LocalFileHandoff::Rejected(
            "file handoff rejected: unsupported local path",
        ))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewportSnapshot {
    pub fullscreen: bool,
    pub maximized: bool,
    pub content_height: f32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceSummary {
    pub id: SpaceId,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub active: bool,
    pub error: Option<String>,
}

/// Host actions requested by a frame update, applied by the eframe adapter.
#[derive(Clone, Debug, PartialEq)]
pub enum AppEffect {
    CloseWindow,
    QuitApplication,
    SetWindowTitle(String),
    SetFullscreen(bool),
    SetMaximized(bool),
    SetDecorations(bool),
    RequestCopy,
    RequestRepaint,
    Bell,
    RepaintAfter(Duration),
    SetTerminalTextConfig(TerminalTextConfig),
    SetTerminalCursorIcon(egui::CursorIcon),
    /// Reinstall egui's UI-chrome fonts (settings/sidebar/status) so a `font.ui-family` edit applies
    /// live, mirroring how `SetTerminalTextConfig` re-fonts the terminal.
    SetUiFonts(Vec<String>),
    SetWindowFocus,
    OpenUrl(String),
    OpenSettings,
    /// Open settings to the keybindings page focused on the given action name,
    /// adding an editable row for it if none exists yet.
    ConfigureKeybind(String),
}

fn invalid_opaque_target() -> CommandOutcome {
    CommandOutcome::Denied {
        message: "target is not a valid opaque resource reference".to_owned(),
    }
}

fn command_outcome_message(outcome: &CommandOutcome) -> Option<String> {
    match outcome {
        CommandOutcome::Success { .. } | CommandOutcome::Pending { .. } => None,
        CommandOutcome::Unsupported { message }
        | CommandOutcome::Unavailable { message }
        | CommandOutcome::Denied { message }
        | CommandOutcome::StaleTarget { message }
        | CommandOutcome::Ambiguous { message, .. }
        | CommandOutcome::Failed { message, .. } => Some(message.clone()),
        CommandOutcome::ConfirmationRequired { .. } => {
            Some("command requires confirmation".to_owned())
        }
    }
}

fn command_success<T: serde::Serialize>(value: T) -> CommandOutcome {
    match serde_json::to_value(value) {
        Ok(value) => CommandOutcome::Success {
            value,
            warnings: Vec::new(),
        },
        Err(error) => CommandOutcome::Failed {
            code: "result_serialization_failed".to_owned(),
            message: error.to_string(),
        },
    }
}

fn worktree_service_failure(error: WorktreeServiceError) -> CommandOutcome {
    match error {
        WorktreeServiceError::IdentityChanged { path } => CommandOutcome::StaleTarget {
            message: format!("worktree identity changed before completion: {path:?}"),
        },
        error => CommandOutcome::Failed {
            code: "worktree_operation_failed".to_owned(),
            message: error.to_string(),
        },
    }
}

fn caller_name(caller: Caller) -> &'static str {
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

fn worktree_removal_confirmation(
    command: String,
    path: String,
    force: bool,
    confirmation: WorktreeRemovalConfirmation,
) -> CommandOutcome {
    match serde_json::to_string(&confirmation) {
        Ok(encoded_confirmation) => CommandOutcome::ConfirmationRequired {
            confirmation: Box::new(Confirmation {
                command,
                arguments: vec![path, force.to_string(), encoded_confirmation],
                target: None,
            }),
        },
        Err(error) => CommandOutcome::Failed {
            code: "confirmation_serialization_failed".to_owned(),
            message: error.to_string(),
        },
    }
}

fn command_outcome_for_binding_operation(
    outcome: BindingOperationOutcome<()>,
) -> Option<CommandOutcome> {
    match outcome {
        BindingOperationOutcome::Supported(()) => None,
        BindingOperationOutcome::Unsupported => Some(CommandOutcome::Unsupported {
            message: "mux operation is unsupported".to_owned(),
        }),
        BindingOperationOutcome::Unavailable => Some(CommandOutcome::Unavailable {
            message: "mux operation is unavailable".to_owned(),
        }),
        BindingOperationOutcome::Denied => Some(CommandOutcome::Denied {
            message: "mux operation was denied".to_owned(),
        }),
        BindingOperationOutcome::Stale => Some(CommandOutcome::StaleTarget {
            message: "mux operation capability is stale".to_owned(),
        }),
    }
}

#[derive(Clone, Debug)]
struct ResolvedCommandTarget {
    target: CommandTarget,
    mux_scope: Option<MuxScope>,
}
struct ResolvedCommandContext<'a> {
    target: Option<&'a CommandTarget>,
    mux_scope: Option<MuxScope>,
    caller: Caller,
    viewport: ViewportSnapshot,
    execution: Option<(Instant, CommandCancellation)>,
    invocation: CommandInvocation,
}

pub(crate) enum CommandDispatch {
    Complete(CommandOutcome),
    Pending {
        command: MuxCommand,
        command_id: String,
        origin: MuxScope,
        binding_identity: BindingRef,
        binding_generation: u64,
        namespace: BackendConnectionNamespace,
        target: Option<CommandTarget>,
        deadline: Instant,
        cancellation: CommandCancellation,
        result: mpsc::Receiver<MuxCommandResult>,
    },
    ExtensionPending {
        invocation: CommandInvocation,
        extension_id: String,
        generation: u64,
        target: Option<CommandTarget>,
        deadline: Instant,
        cancellation: CommandCancellation,
        result: mpsc::Receiver<CommandOutcome>,
    },
}

struct MuxCompletionContext<'a> {
    command_id: &'a str,
    origin: MuxScope,
    binding_identity: &'a BindingRef,
    binding_generation: u64,
    namespace: &'a BackendConnectionNamespace,
    command: &'a MuxCommand,
    rename: Option<&'a PendingSessionRename>,
}

struct PendingAppCommand {
    request_id: u64,
    command: MuxCommand,
    command_id: String,
    origin: MuxScope,
    binding_identity: BindingRef,
    binding_generation: u64,
    namespace: BackendConnectionNamespace,
    target: Option<CommandTarget>,
    deadline: Instant,
    cancellation: CommandCancellation,
    response: Option<mpsc::Sender<CommandOutcome>>,
    completion: Option<CommandCompletionContext>,
    rename: Option<PendingSessionRename>,
    result: mpsc::Receiver<MuxCommandResult>,
}

struct PendingExtensionCommand {
    request_id: u64,
    invocation: CommandInvocation,
    extension_id: String,
    generation: u64,
    target: Option<CommandTarget>,
    deadline: Instant,
    cancellation: CommandCancellation,
    response: Option<mpsc::Sender<CommandOutcome>>,
    completion: Option<CommandCompletionContext>,
    result: mpsc::Receiver<CommandOutcome>,
}

const COMPLETION_PUBLICATION_QUEUE_LIMIT: usize = 64;
const COMPLETION_PUBLICATION_RETRY_LIMIT: usize = 64;
const SHUTDOWN_RECONCILIATION_GRACE: Duration = Duration::from_secs(30);
const COMPLETION_PUBLICATION_RETRY_BASE: Duration = Duration::from_millis(25);
const COMPLETION_PUBLICATION_RETRY_MAX: Duration = Duration::from_secs(30);

struct PendingCompletionPublication {
    request_id: u64,
    publication: EventPublication,
    automation: AutomationHub,
    fallback_scope: String,
    attempts: u32,
    next_attempt_at: Instant,
}

enum ShutdownReconciliationJob {
    Mux(ShutdownMuxReconciliation),
    Extension(ShutdownExtensionReconciliation),
    Publication(PendingCompletionPublication),
}

struct ShutdownMuxReconciliation {
    request_id: u64,
    command_id: String,
    command: MuxCommand,
    origin: MuxScope,
    binding_identity: BindingRef,
    binding_generation: u64,
    namespace: BackendConnectionNamespace,
    result: mpsc::Receiver<MuxCommandResult>,
    deadline: Instant,
    cancellation: CommandCancellation,
    target: Option<CommandTarget>,
    completion: Option<CommandCompletionContext>,
    reconciliation: mpsc::Sender<ShutdownReconciliationCompletion>,
    automation: AutomationHub,
    scope: String,
    fallback_scope: String,
}

struct ShutdownExtensionReconciliation {
    request_id: u64,
    command_id: String,
    invocation: CommandInvocation,
    extension_id: String,
    generation: u64,
    result: mpsc::Receiver<CommandOutcome>,
    deadline: Instant,
    cancellation: CommandCancellation,
    target: Option<CommandTarget>,
    completion: Option<CommandCompletionContext>,
    reconciliation: mpsc::Sender<ShutdownReconciliationCompletion>,
    automation: AutomationHub,
    scope: String,
    fallback_scope: String,
}

enum ShutdownReconciliationCompletion {
    Mux {
        request_id: u64,
        command_id: String,
        command: MuxCommand,
        origin: MuxScope,
        binding_identity: BindingRef,
        binding_generation: u64,
        namespace: BackendConnectionNamespace,
        target: Option<CommandTarget>,
        completion: Option<CommandCompletionContext>,
        result: Box<MuxCommandResult>,
    },
    Extension {
        request_id: u64,
        command_id: String,
        invocation: CommandInvocation,
        extension_id: String,
        generation: u64,
        target: Option<CommandTarget>,
        completion: Option<CommandCompletionContext>,
        result: CommandOutcome,
    },
}

static SHUTDOWN_RECONCILIATION_WORKER: LazyLock<mpsc::Sender<ShutdownReconciliationJob>> =
    LazyLock::new(|| {
        let (sender, receiver) = mpsc::channel::<ShutdownReconciliationJob>();
        std::thread::Builder::new()
            .name("bootty-command-reconciliation".to_owned())
            .spawn(move || run_shutdown_reconciliation_worker(receiver))
            .expect("command reconciliation worker must start");
        sender
    });

fn enqueue_shutdown_reconciliation(job: ShutdownReconciliationJob) {
    if let Err(error) = SHUTDOWN_RECONCILIATION_WORKER.send(job) {
        eprintln!("command reconciliation worker stopped; reconciling inline");
        run_shutdown_reconciliation_inline(error.0);
    }
}

fn run_shutdown_reconciliation_worker(receiver: mpsc::Receiver<ShutdownReconciliationJob>) {
    let mut jobs = VecDeque::new();
    loop {
        while let Ok(job) = receiver.try_recv() {
            jobs.push_back(job);
        }
        if jobs.is_empty() {
            match receiver.recv() {
                Ok(job) => jobs.push_back(job),
                Err(_) => return,
            }
            continue;
        }

        let round = jobs.len();
        for _ in 0..round {
            let Some(job) = jobs.pop_front() else {
                break;
            };
            match poll_shutdown_reconciliation_job(job) {
                ShutdownReconciliationPoll::Pending(job) => jobs.push_back(*job),
                ShutdownReconciliationPoll::Complete => {}
            }
        }
        if !jobs.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn run_shutdown_reconciliation_inline(mut job: ShutdownReconciliationJob) {
    loop {
        match poll_shutdown_reconciliation_job(job) {
            ShutdownReconciliationPoll::Pending(next) => {
                job = *next;
                std::thread::sleep(Duration::from_millis(5));
            }
            ShutdownReconciliationPoll::Complete => return,
        }
    }
}
enum ShutdownReconciliationPoll {
    Pending(Box<ShutdownReconciliationJob>),
    Complete,
}

fn pending_shutdown_reconciliation(job: ShutdownReconciliationJob) -> ShutdownReconciliationPoll {
    ShutdownReconciliationPoll::Pending(Box::new(job))
}

fn poll_shutdown_reconciliation_job(job: ShutdownReconciliationJob) -> ShutdownReconciliationPoll {
    match job {
        ShutdownReconciliationJob::Mux(job) => {
            if Instant::now() >= job.deadline {
                publish_shutdown_completion(
                    ShutdownCompletionContext {
                        automation: &job.automation,
                        scope: job.scope,
                        fallback_scope: job.fallback_scope,
                        request_id: job.request_id,
                        command_id: job.command_id,
                        target: job.target,
                        completion: job.completion,
                    },
                    shutdown_unknown_outcome(),
                    true,
                    durable_intent_for_mux_command(&job.command),
                );
                return ShutdownReconciliationPoll::Complete;
            }
            let _ = job.cancellation.is_cancelled();
            match job.result.try_recv() {
                Ok(result) => {
                    let completion = ShutdownReconciliationCompletion::Mux {
                        request_id: job.request_id,
                        command_id: job.command_id,
                        command: job.command,
                        origin: job.origin,
                        binding_identity: job.binding_identity,
                        binding_generation: job.binding_generation,
                        namespace: job.namespace,
                        target: job.target,
                        completion: job.completion,
                        result: Box::new(result),
                    };
                    match job.reconciliation.send(completion) {
                        Ok(()) => ShutdownReconciliationPoll::Complete,
                        Err(error) => {
                            let ShutdownReconciliationCompletion::Mux {
                                request_id,
                                command_id,
                                command,
                                target,
                                completion,
                                result,
                                ..
                            } = error.0
                            else {
                                unreachable!(
                                    "mux reconciliation sender returned an extension result"
                                );
                            };
                            publish_shutdown_completion(
                                ShutdownCompletionContext {
                                    automation: &job.automation,
                                    scope: job.scope,
                                    fallback_scope: job.fallback_scope,
                                    request_id,
                                    command_id,
                                    target,
                                    completion,
                                },
                                shutdown_mux_outcome(&command, *result),
                                false,
                                durable_intent_for_mux_command(&command),
                            );
                            ShutdownReconciliationPoll::Complete
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    publish_shutdown_completion(
                        ShutdownCompletionContext {
                            automation: &job.automation,
                            scope: job.scope,
                            fallback_scope: job.fallback_scope,
                            request_id: job.request_id,
                            command_id: job.command_id,
                            target: job.target,
                            completion: job.completion,
                        },
                        shutdown_unknown_outcome(),
                        true,
                        durable_intent_for_mux_command(&job.command),
                    );
                    ShutdownReconciliationPoll::Complete
                }
                Err(mpsc::TryRecvError::Empty) if Instant::now() >= job.deadline => {
                    publish_shutdown_completion(
                        ShutdownCompletionContext {
                            automation: &job.automation,
                            scope: job.scope,
                            fallback_scope: job.fallback_scope,
                            request_id: job.request_id,
                            command_id: job.command_id,
                            target: job.target,
                            completion: job.completion,
                        },
                        shutdown_unknown_outcome(),
                        true,
                        durable_intent_for_mux_command(&job.command),
                    );
                    ShutdownReconciliationPoll::Complete
                }
                Err(mpsc::TryRecvError::Empty) => {
                    pending_shutdown_reconciliation(ShutdownReconciliationJob::Mux(job))
                }
            }
        }
        ShutdownReconciliationJob::Extension(job) => {
            if Instant::now() >= job.deadline {
                publish_shutdown_completion(
                    ShutdownCompletionContext {
                        automation: &job.automation,
                        scope: job.scope,
                        fallback_scope: job.fallback_scope,
                        request_id: job.request_id,
                        command_id: job.command_id,
                        target: job.target,
                        completion: job.completion,
                    },
                    shutdown_unknown_outcome(),
                    true,
                    None,
                );
                return ShutdownReconciliationPoll::Complete;
            }
            let _ = job.cancellation.is_cancelled();
            match job.result.try_recv() {
                Ok(result) => {
                    let completion = ShutdownReconciliationCompletion::Extension {
                        request_id: job.request_id,
                        command_id: job.command_id,
                        invocation: job.invocation,
                        extension_id: job.extension_id,
                        generation: job.generation,
                        target: job.target,
                        completion: job.completion,
                        result,
                    };
                    match job.reconciliation.send(completion) {
                        Ok(()) => ShutdownReconciliationPoll::Complete,
                        Err(error) => {
                            let ShutdownReconciliationCompletion::Extension {
                                request_id,
                                command_id,
                                target,
                                completion,
                                result,
                                ..
                            } = error.0
                            else {
                                unreachable!(
                                    "extension reconciliation sender returned a mux result"
                                );
                            };
                            publish_shutdown_completion(
                                ShutdownCompletionContext {
                                    automation: &job.automation,
                                    scope: job.scope,
                                    fallback_scope: job.fallback_scope,
                                    request_id,
                                    command_id,
                                    target,
                                    completion,
                                },
                                result,
                                false,
                                None,
                            );
                            ShutdownReconciliationPoll::Complete
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    publish_shutdown_completion(
                        ShutdownCompletionContext {
                            automation: &job.automation,
                            scope: job.scope,
                            fallback_scope: job.fallback_scope,
                            request_id: job.request_id,
                            command_id: job.command_id,
                            target: job.target,
                            completion: job.completion,
                        },
                        shutdown_unknown_outcome(),
                        true,
                        None,
                    );
                    ShutdownReconciliationPoll::Complete
                }
                Err(mpsc::TryRecvError::Empty) if Instant::now() >= job.deadline => {
                    publish_shutdown_completion(
                        ShutdownCompletionContext {
                            automation: &job.automation,
                            scope: job.scope,
                            fallback_scope: job.fallback_scope,
                            request_id: job.request_id,
                            command_id: job.command_id,
                            target: job.target,
                            completion: job.completion,
                        },
                        shutdown_unknown_outcome(),
                        true,
                        None,
                    );
                    ShutdownReconciliationPoll::Complete
                }
                Err(mpsc::TryRecvError::Empty) => {
                    pending_shutdown_reconciliation(ShutdownReconciliationJob::Extension(job))
                }
            }
        }
        ShutdownReconciliationJob::Publication(mut pending) => {
            if Instant::now() < pending.next_attempt_at {
                return pending_shutdown_reconciliation(ShutdownReconciliationJob::Publication(
                    pending,
                ));
            }
            match pending
                .automation
                .publish_event(pending.publication.clone())
            {
                Ok(_) => ShutdownReconciliationPoll::Complete,
                Err(error) if publication_error_is_oversized(&error) => {
                    pending.publication = bounded_completion_publication(&pending, &error);
                    pending.attempts = 0;
                    pending.next_attempt_at = Instant::now();
                    pending_shutdown_reconciliation(ShutdownReconciliationJob::Publication(pending))
                }
                Err(error)
                    if publication_error_is_retired_scope(&error)
                        && pending.publication.scope != pending.fallback_scope =>
                {
                    pending.publication.scope = pending.fallback_scope.clone();
                    pending.attempts = 0;
                    pending.next_attempt_at = Instant::now();
                    pending_shutdown_reconciliation(ShutdownReconciliationJob::Publication(pending))
                }
                Err(error) => {
                    pending.attempts = pending.attempts.saturating_add(1);
                    pending.next_attempt_at =
                        Instant::now() + completion_publication_retry_delay(pending.attempts);
                    eprintln!(
                        "completion publication retry {} failed for request {}: {error}",
                        pending.attempts, pending.request_id
                    );
                    pending_shutdown_reconciliation(ShutdownReconciliationJob::Publication(pending))
                }
            }
        }
    }
}

fn publication_error_is_oversized(error: &AutomationError) -> bool {
    error.code == -32003
        && (error.message.contains("payload")
            || error.message.contains("size")
            || error.message.contains("serial"))
}

fn publication_error_is_retired_scope(error: &AutomationError) -> bool {
    error.code == -32006 && error.message.contains("scope")
}

fn completion_publication_retry_delay(attempts: u32) -> Duration {
    let shift = attempts.min(10);
    let multiplier = 1u64 << shift;
    let millis = (COMPLETION_PUBLICATION_RETRY_BASE.as_millis() as u64)
        .saturating_mul(multiplier)
        .min(COMPLETION_PUBLICATION_RETRY_MAX.as_millis() as u64);
    Duration::from_millis(millis)
}

fn bounded_completion_publication(
    pending: &PendingCompletionPublication,
    _error: &AutomationError,
) -> EventPublication {
    let command = pending
        .publication
        .payload
        .get("command")
        .cloned()
        .unwrap_or(Value::Null);
    let request_id = pending
        .publication
        .payload
        .get("request_id")
        .cloned()
        .unwrap_or(Value::Null);
    EventPublication::new(
        pending.fallback_scope.clone(),
        "command.completed",
        json!({
            "source": "completion_publication_fallback",
            "reconciled": true,
        }),
        None,
        json!({
            "command": command,
            "request_id": request_id,
            "reconciled": true,
            "actual_outcome_unknown": true,
            "outcome": {
                "status": "failed",
                "code": "result_too_large",
                "message": "completion publication exceeded the event payload limit",
            },
        }),
    )
}

fn shutdown_unknown_outcome() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "completion_indeterminate".to_owned(),
        message: "command completion is unknown; durable intent was retained for reconciliation"
            .to_owned(),
    }
}

fn durable_intent_for_mux_command(command: &MuxCommand) -> Option<Value> {
    match command {
        MuxCommand::CreateSession { plan } => serde_json::to_value(plan).ok(),
        MuxCommand::CreateProjectSession { session_id, .. }
        | MuxCommand::CreateWorktreeSession { session_id, .. } => {
            Some(json!({"session_id": session_id}))
        }
        MuxCommand::DitchSession { session_id } => {
            Some(json!({"operation": "ditch_session", "session_id": session_id}))
        }
        _ => None,
    }
}

fn shutdown_mux_outcome(command: &MuxCommand, result: MuxCommandResult) -> CommandOutcome {
    match result {
        Ok(completion) => {
            let mut value = serde_json::Map::new();
            if let MuxCommand::CreateSession { plan } = command {
                value.insert(
                    "launch".to_owned(),
                    serde_json::to_value(plan).unwrap_or(Value::Null),
                );
            }
            if let Some(allocated) = completion.allocated() {
                value.insert(
                    "allocated".to_owned(),
                    serde_json::to_value(allocated).unwrap_or(Value::Null),
                );
            }
            if let Some(target) = completion.resolved_target() {
                value.insert(
                    "resolved_target".to_owned(),
                    serde_json::to_value(target).unwrap_or(Value::Null),
                );
            }
            CommandOutcome::Success {
                value: Value::Object(value),
                warnings: Vec::new(),
            }
        }
        Err(error) => {
            let (code, message) = match error {
                MuxCommandError::Cancelled => ("cancelled", "mux command was cancelled".to_owned()),
                MuxCommandError::DeadlineExceeded => (
                    "deadline_exceeded",
                    "mux command deadline expired".to_owned(),
                ),
                MuxCommandError::Unsupported => {
                    ("unsupported", "mux operation is unsupported".to_owned())
                }
                MuxCommandError::Unavailable => {
                    ("unavailable", "mux operation is unavailable".to_owned())
                }
                MuxCommandError::Denied => ("denied", "mux operation was denied".to_owned()),
                MuxCommandError::Stale => {
                    ("stale_target", "mux operation target is stale".to_owned())
                }
                MuxCommandError::Failed(message) => ("execution_failed", message),
            };
            CommandOutcome::Failed {
                code: code.to_owned(),
                message,
            }
        }
    }
}
struct ShutdownCompletionContext<'a> {
    automation: &'a AutomationHub,
    scope: String,
    fallback_scope: String,
    request_id: u64,
    command_id: String,
    target: Option<CommandTarget>,
    completion: Option<CommandCompletionContext>,
}

fn publish_shutdown_completion(
    context: ShutdownCompletionContext<'_>,
    outcome: CommandOutcome,
    actual_outcome_unknown: bool,
    durable_intent: Option<Value>,
) {
    let ShutdownCompletionContext {
        automation,
        scope,
        fallback_scope,
        request_id,
        command_id,
        target: requested_target,
        completion,
    } = context;
    let target = completion
        .as_ref()
        .and_then(|completion| completion.target.as_ref())
        .or(requested_target.as_ref())
        .cloned();
    let outcome = bounded_command_outcome(outcome);
    let publication = EventPublication::new(
        scope,
        "command.completed",
        json!({
            "source": "shutdown_reconciliation",
            "reconciled": true,
            "request_id": request_id,
            "caller": completion.as_ref().map(|completion| completion.caller),
            "owner_pid": completion.as_ref().map(|completion| completion.owner_pid),
            "owner_generation": completion.as_ref().map(|completion| completion.owner_generation),
        }),
        target.clone(),
        json!({
            "command": command_id,
            "request_id": request_id,
            "reconciled": true,
            "actual_outcome_unknown": actual_outcome_unknown,
            "durable_intent": durable_intent,
            "target": target,
            "outcome": serde_json::to_value(outcome).unwrap_or_else(|_| json!({
                "status": "failed",
                "code": "result_too_large",
            })),
        }),
    );
    if let Err(error) = automation.publish_event(publication.clone()) {
        let mut pending = PendingCompletionPublication {
            request_id,
            publication,
            automation: automation.clone(),
            fallback_scope,
            attempts: 0,
            next_attempt_at: Instant::now(),
        };
        if !publication_error_is_oversized(&error) && !publication_error_is_retired_scope(&error) {
            pending.attempts = 1;
            pending.next_attempt_at =
                Instant::now() + completion_publication_retry_delay(pending.attempts);
        }
        eprintln!("shutdown completion publication queued after failure: {error}");
        enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Publication(pending));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalProgressState {
    Normal,
    Error,
    Indeterminate,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TerminalProgress {
    pub state: TerminalProgressState,
    pub value: Option<u8>,
}

impl TerminalProgress {
    fn from_conemu(state: &str, value: Option<u8>) -> Option<Self> {
        let state = match state {
            "normal" => TerminalProgressState::Normal,
            "error" => TerminalProgressState::Error,
            "indeterminate" => TerminalProgressState::Indeterminate,
            "warning" => TerminalProgressState::Warning,
            "inactive" => return None,
            _ => return None,
        };
        Some(Self { state, value })
    }

    fn from_mux(progress: &MuxWindowProgress) -> Option<Self> {
        Self::from_conemu(&progress.state, progress.percent)
    }

    pub(crate) fn fraction(self) -> Option<f32> {
        self.value
            .map(|value| f32::from(value) / 100.0)
            .or((self.state == TerminalProgressState::Indeterminate).then_some(0.5))
    }

    fn percent(self) -> Option<u8> {
        self.value
            .or((self.state == TerminalProgressState::Indeterminate).then_some(50))
    }
}
#[derive(Clone, Debug)]
struct PendingGeneratedName {
    cwd: String,
    /// The name asked of the backend, unique across the whole server.
    name: String,
    /// What bootty calls it, which drops any uniqueness suffix `name` had to carry.
    display_name: String,
    /// The display name to restore if an in-flight rename fails.
    previous_display_name: Option<String>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ScopedWindowId {
    scope: MuxScope,
    session_id: String,
    window_id: String,
}

impl ScopedWindowId {
    fn new(scope: MuxScope, session_id: String, window_id: String) -> Self {
        Self {
            scope,
            session_id,
            window_id,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ScopedPaneId {
    window: ScopedWindowId,
    pane_id: String,
}

struct NativeTerminalOwner {
    terminal: Box<ActiveTerminal>,
    terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
}

impl NativeTerminalOwner {
    fn new(config: &BoottyConfig, variant: AppearanceVariant, repaint: RepaintHandle) -> Self {
        let (terminal_side_effect_tx, terminal_side_effect_rx) = mpsc::channel();
        let session_config =
            terminal_session_config_with_side_effects(config, variant, &terminal_side_effect_tx);
        Self {
            terminal: Box::new(ActiveTerminal::new(
                TerminalWidget::initial_geometry(),
                &config.multiplexer,
                session_config,
                repaint,
            )),
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        }
    }

    fn replace_binding(binding: &mut BindingRuntime, replacement: Self) -> Self {
        Self {
            terminal: std::mem::replace(&mut binding.terminal, replacement.terminal),
            terminal_side_effect_tx: std::mem::replace(
                &mut binding.terminal_side_effect_tx,
                replacement.terminal_side_effect_tx,
            ),
            terminal_side_effect_rx: std::mem::replace(
                &mut binding.terminal_side_effect_rx,
                replacement.terminal_side_effect_rx,
            ),
        }
    }

    fn swap_with_binding(&mut self, binding: &mut BindingRuntime) {
        std::mem::swap(&mut self.terminal, &mut binding.terminal);
        std::mem::swap(
            &mut self.terminal_side_effect_tx,
            &mut binding.terminal_side_effect_tx,
        );
        std::mem::swap(
            &mut self.terminal_side_effect_rx,
            &mut binding.terminal_side_effect_rx,
        );
    }

    fn discard_side_effects(&mut self) {
        self.terminal_side_effect_rx.try_iter().for_each(drop);
    }

    fn drain_inactive(&mut self) {
        self.terminal.drain_native_window();
        self.discard_side_effects();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersistedSessionRestoreDecision {
    Wait,
    Restore,
}

fn persisted_session_restore_decision(
    backend: MultiplexerBackendConfig,
    refresh_completed: bool,
) -> PersistedSessionRestoreDecision {
    match backend {
        MultiplexerBackendConfig::Native => PersistedSessionRestoreDecision::Restore,
        MultiplexerBackendConfig::Rmux
        | MultiplexerBackendConfig::Tmux
        | MultiplexerBackendConfig::Zellij
            if !refresh_completed =>
        {
            PersistedSessionRestoreDecision::Wait
        }
        MultiplexerBackendConfig::Rmux
        | MultiplexerBackendConfig::Tmux
        | MultiplexerBackendConfig::Zellij => PersistedSessionRestoreDecision::Restore,
    }
}

/// One immutable leaf in a recursive persisted launch. Pane order is the normalized DFS order
/// exposed by authoritative backend allocations.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistedLaunchLeaf {
    window_index: usize,
    pane_index: usize,
    cwd: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingPersistedSessionLaunch {
    /// The current backend/session-order identity used to resolve the claim.
    session_id: String,
    plan: Arc<MuxSessionLaunchPlan>,
    leaves: Vec<PersistedLaunchLeaf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedPersistedSessionLaunchClaim {
    launch: PendingPersistedSessionLaunch,
    claimant: ClaimantRef,
    directory: DirectoryRef,
}
fn persisted_launch_leaves(plan: &MuxSessionLaunchPlan) -> Vec<PersistedLaunchLeaf> {
    let mut leaves = Vec::with_capacity(plan.pane_count());
    for (window_index, window) in plan.windows.iter().enumerate() {
        let mut pane_index = 0;
        collect_persisted_launch_leaves(&window.layout, window_index, &mut pane_index, &mut leaves);
    }
    leaves
}

fn collect_persisted_launch_leaves(
    layout: &MuxPaneLaunchPlan,
    window_index: usize,
    pane_index: &mut usize,
    leaves: &mut Vec<PersistedLaunchLeaf>,
) {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => {
            leaves.push(PersistedLaunchLeaf {
                window_index,
                pane_index: *pane_index,
                cwd: pane.cwd.clone(),
            });
            *pane_index += 1;
        }
        MuxPaneLaunchPlan::Split(split) => {
            collect_persisted_launch_leaves(&split.first, window_index, pane_index, leaves);
            collect_persisted_launch_leaves(&split.second, window_index, pane_index, leaves);
        }
    }
}

struct BindingRuntime {
    scope: MuxScope,
    label: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    /// Set while this binding's remote attach client is gone and bootty is waiting to start
    /// another. Per binding, not per window: one space's outage is not another's, and a reconnect
    /// pending here must not discard the pane of whichever space is active when it comes due.
    reattach: Option<RemoteReattach>,
    /// When this binding's current remote attach client was asked for, so an outage that keeps
    /// ending clients can be told from one connection that lasted and then dropped much later.
    remote_attach_started: Option<Instant>,
    workspace_config_path: PathBuf,
    multiplexer: crate::config::MultiplexerConfig,
    terminal: Box<ActiveTerminal>,
    mux: BindingMuxController,
    session_order: SessionOrderStore,
    session_names: SessionNameStore,
    pending_generated_names: HashMap<String, PendingGeneratedName>,
    generated_names_signature: Option<u64>,
    terminal_side_effect_tx: mpsc::Sender<TerminalSideEffectEvent>,
    terminal_side_effect_rx: mpsc::Receiver<TerminalSideEffectEvent>,
    pane_layouts: HashMap<ScopedWindowId, PaneLayout>,
    pending_pane_split_directions: HashMap<ScopedWindowId, SplitDirection>,
    custom_tab_names: HashSet<ScopedWindowId>,
    terminal_tab_titles: HashMap<ScopedWindowId, String>,
    terminal_progress: HashMap<ScopedPaneId, TerminalProgress>,
    unscoped_terminal_progress: Option<TerminalProgress>,
    terminal_ports: HashMap<ScopedPaneId, Vec<u16>>,
    unscoped_terminal_ports: Vec<u16>,
    persisted_sessions_restored: bool,
    pending_persisted_session_launches: Vec<PendingPersistedSessionLaunch>,
    /// Drained backend observations remain here until their claim and event
    /// publication succeeds, so one failure cannot discard later observations.
    pending_automation_events: VecDeque<MuxEventObservation>,
    /// Per-terminal authoritative state for each terminal lifecycle topic. The
    /// cache is rebuilt from the live backend inventory at every authoritative
    /// snapshot rebase and is updated before publishing each delta.
    automation_terminal_states: BTreeMap<AutomationTerminalStateKey, AutomationTerminalState>,
    /// Binding-level backend status retained independently for each backend
    /// lifecycle topic, so a status rebase never masquerades as topology.
    automation_backend_states: HashMap<&'static str, AutomationTerminalTopicState>,
    /// Generation whose lifecycle sources are currently installed in the
    /// automation hub. `None` makes a newly constructed binding purge any
    /// recoverable claim/output state from a prior runtime before bootstrap.
    automation_generation: Option<u64>,
    /// A source bundle is installed once at bootstrap and again only after an
    /// authoritative refresh or generation change.
    automation_sources_installed: bool,
    /// A topology or rebase observation holds the entire ordered queue until
    /// its authoritative source refresh and claim reconciliation complete.
    automation_event_refresh_pending: bool,
}

impl BindingRuntime {
    fn new(
        scope: MuxScope,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Self> {
        let mut binding = Self::new_with_backend_override(
            scope,
            config,
            None,
            SpaceRemoteOverride::Inherit,
            variant,
            repaint.clone(),
            true,
        )?;
        let refresh_completed =
            if selected_backend(&binding.multiplexer) == MultiplexerBackendConfig::Native {
                binding
                    .mux
                    .refresh_sessions(&repaint, &binding.multiplexer)
                    .is_none()
            } else {
                false
            };
        binding.restore_persisted_sessions(refresh_completed, &repaint)?;
        Ok(binding)
    }

    fn new_with_backend_override(
        scope: MuxScope,
        config: &BoottyConfig,
        backend_override: Option<MultiplexerBackendConfig>,
        remote_override: SpaceRemoteOverride,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
        register_namespace: bool,
    ) -> Result<Self> {
        let mut config = config.clone();
        let backend_override = match &remote_override {
            SpaceRemoteOverride::Profile(remote) => Some(remote.backend),
            _ => backend_override,
        };
        if let Some(backend) = backend_override {
            config.multiplexer.backend = backend;
        }
        config.multiplexer.remote_space_id = None;
        let remote_error = match &remote_override {
            SpaceRemoteOverride::Inherit => None,
            SpaceRemoteOverride::Local => {
                config.multiplexer.remote = None;
                None
            }
            SpaceRemoteOverride::Profile(remote) => {
                config.multiplexer.remote_space_id = Some(remote.remote_space_id.clone());
                if let Some(profile) = config.ssh_profiles.get(&remote.profile_id) {
                    config.multiplexer.remote = Some(profile.to_remote());
                    None
                } else {
                    config.multiplexer.remote = None;
                    Some(format!(
                        "SSH profile '{}' is unavailable",
                        remote.profile_id
                    ))
                }
            }
            SpaceRemoteOverride::Inline(remote) => {
                config.multiplexer.remote = Some(remote.clone());
                None
            }
        };
        if !config.multiplexer.backend.supports_remote() {
            config.multiplexer.remote = None;
        }
        let NativeTerminalOwner {
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
        } = NativeTerminalOwner::new(&config, variant, repaint);
        let mut mux = BindingMuxController::new(scope);
        // Bindings of one workspace share native sessions, separate workspaces cannot see each
        // other's, and reopening a window keeps its own. Native sessions live in this process rather
        // than in a server, so which state a binding reaches is a choice bootty has to make.
        let workspace = config.config_path.clone();
        let unavailable = remote_error.clone();
        mux.set_backend_factory(Arc::new(move |multiplexer| {
            if let Some(message) = &unavailable {
                return bootty_mux::config::unavailable_backend(message.clone());
            }
            bootty_mux::config::build_backend_for_workspace(multiplexer, Some(&workspace))
        }));
        let namespace = namespace_for_binding(scope, &config.multiplexer);
        let session_order = if register_namespace {
            SessionOrderStore::for_binding(
                &config.config_path,
                scope.binding_id().persistence_value(),
                namespace,
            )?
        } else {
            SessionOrderStore::for_binding_preflight(
                &config.config_path,
                scope.binding_id().persistence_value(),
                namespace,
            )?
        };
        let mut binding = Self {
            label: binding_label(scope, &config.multiplexer),
            backend_override,
            remote_override,
            reattach: None,
            remote_attach_started: None,
            workspace_config_path: config.config_path.clone(),
            multiplexer: config.multiplexer.clone(),
            scope,
            terminal,
            terminal_side_effect_tx,
            terminal_side_effect_rx,
            mux,
            session_order,
            session_names: SessionNameStore::for_binding(
                &config.config_path,
                scope.binding_id().persistence_value(),
            ),
            pending_generated_names: HashMap::new(),
            generated_names_signature: None,
            pane_layouts: HashMap::new(),
            pending_pane_split_directions: HashMap::new(),
            custom_tab_names: HashSet::new(),
            terminal_tab_titles: HashMap::new(),
            terminal_progress: HashMap::new(),
            terminal_ports: HashMap::new(),
            unscoped_terminal_ports: Vec::new(),
            unscoped_terminal_progress: None,
            persisted_sessions_restored: false,
            pending_persisted_session_launches: Vec::new(),
            pending_automation_events: VecDeque::new(),
            automation_terminal_states: BTreeMap::new(),
            automation_backend_states: unknown_automation_backend_states(),
            automation_generation: None,
            automation_sources_installed: false,
            automation_event_refresh_pending: false,
        };
        if let Some(error) = remote_error {
            binding.mux.set_availability_error(Some(error));
        }
        if selected_backend(&binding.multiplexer) == MultiplexerBackendConfig::Native {
            binding
                .terminal
                .set_native_event_backend(NativeBackend::for_workspace(&config.config_path));
        }
        Ok(binding)
    }

    fn register_native_persisted_launch(&mut self, plan: &MuxSessionLaunchPlan) -> Result<bool> {
        if selected_backend(&self.multiplexer) != MultiplexerBackendConfig::Native {
            return Ok(true);
        }
        let Some(session) = self
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == plan.session_id || session.name == plan.session_id)
        else {
            return Ok(false);
        };
        if session.id != plan.session_id || session.windows.len() != plan.windows.len() {
            return Ok(false);
        }
        let allocated = MuxAllocatedResources {
            session_id: session.id.clone(),
            windows: session
                .windows
                .iter()
                .map(|window| crate::mux::backend::MuxAllocatedWindow {
                    window_id: window.id.clone(),
                    pane_ids: window
                        .panes
                        .iter()
                        .filter_map(|pane| pane.pane_id.clone())
                        .collect(),
                })
                .collect(),
        };
        let terminal_ids = allocated
            .windows
            .iter()
            .map(|window| {
                window
                    .pane_ids
                    .iter()
                    .map(|pane_id| {
                        self.mux
                            .terminal_id_for_pane(&allocated.session_id, &window.window_id, pane_id)
                            .map(|terminal_id| (pane_id.clone(), terminal_id.to_owned()))
                    })
                    .collect::<Option<HashMap<_, _>>>()
                    .map(|pane_terminal_ids| (window.window_id.clone(), pane_terminal_ids))
            })
            .collect::<Option<HashMap<_, _>>>();
        let Some(terminal_ids) = terminal_ids else {
            return Ok(false);
        };
        self.terminal.register_native_session_launch(
            self.scope,
            plan,
            &allocated,
            &terminal_ids,
        )?;
        Ok(true)
    }
    fn reconcile_pending_ditches(&mut self) -> Result<()> {
        let pending = load_pending_ditches(
            &self.workspace_config_path,
            self.scope.binding_id().persistence_value(),
        )?;
        for session_id in pending {
            let live = self
                .mux
                .sessions()
                .iter()
                .any(|session| session.id == session_id || session.name == session_id);
            if live {
                clear_pending_ditch(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &session_id,
                )?;
            } else {
                remove_session_membership_and_launch_plan(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &session_id,
                    &[session_id.as_str()],
                )?;
                self.session_order.forget_session_cache(&session_id);
            }
        }
        Ok(())
    }

    fn reconcile_pending_session_renames(&mut self) -> Result<()> {
        let pending = load_pending_session_renames(
            &self.workspace_config_path,
            self.scope.binding_id().persistence_value(),
        )?;
        for (command_id, rename) in pending {
            let target = self
                .mux
                .sessions()
                .iter()
                .filter(|session| session.id == rename.session_id)
                .collect::<Vec<_>>();
            let new_live = target.iter().any(|session| session.name == rename.new_name);
            let old_live = target.iter().any(|session| session.name == rename.old_name);
            if new_live && old_live {
                return Err(anyhow::anyhow!(
                    "pending session rename {:?} has conflicting authoritative names",
                    rename.session_id
                ));
            }
            if new_live && !old_live {
                match session_rename_persistence_state(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &command_id,
                    &rename,
                )? {
                    SessionRenamePersistenceState::AlreadyCommitted => {
                        self.session_order
                            .rename_session_cache(&rename.old_name, &rename.new_name);
                        self.session_names.mark_explicit(
                            &rename.session_id,
                            &rename.new_name,
                            &rename.display_name,
                            &rename.cwd,
                        );
                        clear_pending_session_rename(
                            &self.workspace_config_path,
                            self.scope.binding_id().persistence_value(),
                            &command_id,
                        )?;
                        continue;
                    }
                    SessionRenamePersistenceState::Conflict => {
                        return Err(anyhow::anyhow!(
                            "pending session rename {:?} conflicts with committed destination",
                            rename.session_id
                        ));
                    }
                    SessionRenamePersistenceState::NotCommitted => {}
                }
            }
            if new_live {
                let plan_ids = [rename.old_name.as_str(), rename.session_id.as_str()];
                rename_session_membership_and_launch_plans(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &rename.old_name,
                    &rename.new_name,
                    &plan_ids,
                )?;
                self.session_order
                    .rename_session_cache(&rename.old_name, &rename.new_name);
                self.session_names.mark_explicit(
                    &rename.session_id,
                    &rename.new_name,
                    &rename.display_name,
                    &rename.cwd,
                );
                clear_pending_session_rename(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &command_id,
                )?;
            } else if old_live || target.is_empty() {
                clear_pending_session_rename(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    &command_id,
                )?;
            }
        }
        Ok(())
    }

    fn restore_persisted_sessions(
        &mut self,
        refresh_completed: bool,
        repaint: &RepaintHandle,
    ) -> Result<bool> {
        if self.persisted_sessions_restored {
            return Ok(false);
        }
        let decision = persisted_session_restore_decision(
            selected_backend(&self.multiplexer),
            refresh_completed,
        );
        match decision {
            PersistedSessionRestoreDecision::Wait => return Ok(false),
            PersistedSessionRestoreDecision::Restore => {}
        }
        if refresh_completed {
            self.reconcile_pending_session_renames()?;
            self.reconcile_pending_ditches()?;
        }
        let persisted = self
            .session_names
            .persisted_sessions(&self.session_order.session_names());
        let stored_plans = load_session_launch_plans(
            &self.workspace_config_path,
            self.scope.binding_id().persistence_value(),
        )?;
        let mut plans_by_key = stored_plans.into_iter().collect::<HashMap<_, _>>();
        let mut restores = Vec::with_capacity(persisted.len());
        for (session_id, name, cwd) in persisted {
            let plan = plans_by_key
                .remove(&session_id)
                .or_else(|| plans_by_key.remove(&name))
                .or_else(|| {
                    plans_by_key
                        .iter()
                        .find(|(_, plan)| plan.session_id == session_id || plan.session_id == name)
                        .map(|(key, _)| key.clone())
                        .and_then(|key| plans_by_key.remove(&key))
                })
                .or_else(|| {
                    let cwd_keys = plans_by_key
                        .iter()
                        .filter(|(_, plan)| plan.default_cwd.as_str() == cwd.as_str())
                        .map(|(key, _)| key.clone())
                        .collect::<Vec<_>>();
                    (cwd_keys.len() == 1).then(|| plans_by_key.remove(&cwd_keys[0]))?
                });
            let (plan, missing) = plan.map_or_else(
                || (simple_session_launch_plan(&session_id, &cwd), true),
                |plan| (plan, false),
            );
            plan.validate().map_err(|error| {
                anyhow::anyhow!(
                    "persisted session launch plan for {session_id:?} is invalid: {error}"
                )
            })?;
            restores.push((session_id, name, plan, missing));
        }
        // A launch is durable as soon as it is queued, before session membership is updated by
        // completion. Include such plans even when a crash interrupted that completion boundary.
        for (session_id, plan) in plans_by_key {
            plan.validate().map_err(|error| {
                anyhow::anyhow!(
                    "pending session launch plan for {session_id:?} is invalid: {error}"
                )
            })?;
            restores.push((session_id.clone(), session_id, plan, false));
        }

        // Persist synthesized legacy plans before issuing any backend mutation. A malformed or
        // otherwise unwritable record therefore cannot leave a partially restored topology.
        for (session_id, _, plan, missing) in &restores {
            if *missing {
                persist_session_launch_plan(
                    &self.workspace_config_path,
                    self.scope.binding_id().persistence_value(),
                    plan,
                )
                .map_err(|error| {
                    anyhow::anyhow!(
                        "persisting legacy session launch plan for {session_id:?} failed: {error}"
                    )
                })?;
            }
        }
        // Establish durable membership before asking a backend to create anything. This makes
        // an interrupted startup recoverable even when the backend has already committed.
        for (_, name, _, _) in &restores {
            self.session_order.add_session(name).map_err(|error| {
                anyhow::anyhow!("persisting restored session membership failed: {error}")
            })?;
        }
        let mut native_restore_ready = true;
        // Backend mux state can outlive an AppState or be rediscovered after refresh. Recursive
        // restoration is therefore an idempotent ensure: an already observed session is
        // rediscovered, while an absent one receives the exact immutable plan persisted for it.
        for (session_id, name, plan, _) in &restores {
            let (already_exists, rename_target) = {
                let existing = self.mux.sessions().iter().find(|session| {
                    session.id == *session_id
                        || session.name == *session_id
                        || session.id == *name
                        || session.name == *name
                        || session.id == plan.session_id
                        || session.name == plan.session_id
                });
                (
                    existing.is_some(),
                    existing
                        .map(|session| session.id.clone())
                        .unwrap_or_else(|| plan.session_id.clone()),
                )
            };
            let already_pending = self
                .pending_persisted_session_launches
                .iter()
                .any(|pending| pending.session_id == *session_id);
            if !already_exists && !already_pending {
                self.mux
                    .create_session(plan.clone(), repaint, &self.multiplexer);
            }
            if !already_pending && !self.register_native_persisted_launch(plan)? {
                native_restore_ready = false;
                continue;
            }
            let launch = PendingPersistedSessionLaunch {
                session_id: session_id.clone(),
                plan: Arc::new(plan.clone()),
                leaves: persisted_launch_leaves(plan),
            };
            if !already_pending {
                self.pending_persisted_session_launches.push(launch);
            }
            if name != session_id && !already_pending {
                self.mux
                    .rename_session(&rename_target, name.clone(), repaint, &self.multiplexer);
            }
        }
        if let Err(error) = self.sync_session_order() {
            self.persisted_sessions_restored = false;
            return Err(error);
        }
        let complete = native_restore_ready
            && restores.iter().all(|(session_id, name, plan, _)| {
                self.mux.sessions().iter().any(|session| {
                    session.id == *session_id
                        || session.name == *session_id
                        || session.id == *name
                        || session.name == *name
                        || session.id == plan.session_id
                        || session.name == plan.session_id
                })
            });
        self.persisted_sessions_restored = complete;
        Ok(complete)
    }

    /// Yields only claims whose persisted launch has an unambiguous, currently
    /// authoritative allocation. Any not-yet-observed allocation stays queued
    /// rather than guessing a pane identity.
    fn take_resolved_persisted_session_launch_claims(
        &mut self,
        context: &DirectoryClaimsContext,
    ) -> Vec<ResolvedPersistedSessionLaunchClaim> {
        if self.multiplexer.remote.is_some() {
            self.pending_persisted_session_launches.clear();
            return Vec::new();
        }

        let mut resolved = Vec::new();
        let mut pending = Vec::new();
        for launch in std::mem::take(&mut self.pending_persisted_session_launches) {
            let Some(session) = self.mux.sessions().iter().find(|session| {
                session.id == launch.session_id
                    || session.name == launch.session_id
                    || session.id == launch.plan.session_id
                    || session.name == launch.plan.session_id
            }) else {
                pending.push(launch);
                continue;
            };
            let mut unresolved = Vec::new();
            for leaf in &launch.leaves {
                let Some(directory) = DirectoryRef::resolve(&leaf.cwd).ok() else {
                    unresolved.push(leaf.clone());
                    continue;
                };
                let Some(window) = session.windows.get(leaf.window_index) else {
                    unresolved.push(leaf.clone());
                    continue;
                };
                let pane = if window.panes.is_empty() {
                    (leaf.pane_index == 0).then_some(&window.anchor)
                } else {
                    window.panes.get(leaf.pane_index)
                };
                let Some(pane) = pane else {
                    unresolved.push(leaf.clone());
                    continue;
                };
                let (Some(pane_id), Some(terminal_id)) =
                    (pane.pane_id.as_deref(), pane.terminal_id.as_deref())
                else {
                    unresolved.push(leaf.clone());
                    continue;
                };
                let Some(claimant) = directory_claimant_for_pane(
                    context,
                    self,
                    &session.id,
                    &window.id,
                    pane_id,
                    terminal_id,
                ) else {
                    unresolved.push(leaf.clone());
                    continue;
                };
                resolved.push(ResolvedPersistedSessionLaunchClaim {
                    launch: PendingPersistedSessionLaunch {
                        session_id: launch.session_id.clone(),
                        plan: Arc::clone(&launch.plan),
                        leaves: vec![leaf.clone()],
                    },
                    claimant,
                    directory,
                });
            }
            if !unresolved.is_empty() {
                pending.push(PendingPersistedSessionLaunch {
                    session_id: launch.session_id,
                    plan: launch.plan,
                    leaves: unresolved,
                });
            }
        }
        self.pending_persisted_session_launches = pending;
        resolved
    }

    fn resolve_empty_remote_after_attach_exit(&mut self, refresh_completed: bool) -> bool {
        if !refresh_completed || self.reattach.is_none() || !self.mux.sessions().is_empty() {
            return false;
        }
        self.reattach = None;
        self.remote_attach_started = None;
        self.mux.set_availability_error(None);
        true
    }

    /// The names bootty shows for `sessions`, in the same order.
    ///
    /// A backend name has to be unique across a whole shared server, so bootty's own name for a
    /// session can differ from it: creating `agents/main` while another Space (or a hand-made tmux
    /// session) already holds that name asks the backend for `agents/main-2`, and that suffix is the
    /// backend's business, not the sidebar's. Sessions bootty has no name for keep the backend name,
    /// and so do two members that would otherwise show the same name — there the suffix is the only
    /// thing telling them apart.
    fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        let mut counts = HashMap::<&str, usize>::new();
        let candidates = sessions
            .iter()
            .map(|session| {
                let display_name = self
                    .session_names
                    .display_name(&session.id)
                    .unwrap_or(session.name.as_str());
                *counts.entry(display_name).or_default() += 1;
                display_name
            })
            .collect::<Vec<_>>();
        sessions
            .iter()
            .zip(candidates)
            .map(|(session, display_name)| {
                if counts.get(display_name).copied().unwrap_or_default() > 1 {
                    session.name.clone()
                } else {
                    display_name.to_owned()
                }
            })
            .collect()
    }

    /// The same names keyed by session id, for the UI groups that carry sessions from several
    /// bindings at once.
    fn session_display_name_map(&self, sessions: &[MuxSession]) -> HashMap<String, String> {
        sessions
            .iter()
            .map(|session| session.id.clone())
            .zip(self.session_display_names(sessions))
            .collect()
    }

    fn sync_session_order(&mut self) -> Result<()> {
        let initial_membership = self.session_order.session_names();
        if !initial_membership.is_empty() {
            self.mux.apply_session_order(&initial_membership);
        }
        self.carry_renamed_members()?;
        if self.multiplexer.remote_space_id.is_some() {
            for session in self.mux.all_sessions() {
                self.session_order.add_session(&session.name)?;
            }
        }
        // A binding with no persisted membership may adopt live backend sessions on its first
        // authoritative view. Once membership exists, keep refreshes scoped to that durable view:
        // another Space can own a same-server session, and an all-sessions snapshot must not
        // silently adopt it here. Pending generated names remain live until their command settles.
        let persisted_names = self.session_order.session_names();
        let pending_names = self
            .pending_generated_names
            .values()
            .map(|pending| pending.name.clone())
            .collect::<Vec<_>>();
        let include_all_sessions = persisted_names.is_empty();
        let alive_names = self
            .mux
            .all_sessions()
            .iter()
            .filter(|session| {
                include_all_sessions
                    || persisted_names.iter().any(|name| name == &session.name)
                    || pending_names.iter().any(|name| name == &session.name)
            })
            .map(|session| session.name.clone())
            .chain(pending_names.iter().cloned())
            .collect::<Vec<_>>();
        let ordered_names = self
            .session_order
            .sync_sessions(alive_names.iter().map(String::as_str))?;
        self.mux.apply_session_order(&ordered_names);
        Ok(())
    }

    /// Carry membership across a session rename, using the name this binding last saw for that
    /// session id. Membership is keyed by session name, so once the backend starts reporting the new
    /// name the old entry prunes away and the new one belongs to nobody: the session vanishes from
    /// its Space while still running, reachable only through the session finder.
    fn carry_renamed_members(&mut self) -> Result<()> {
        let renames = self
            .mux
            .all_sessions()
            .iter()
            .filter_map(|session| {
                let previous = self.session_names.last_observed_name(&session.id)?;
                (previous != session.name).then(|| (previous.to_owned(), session.name.clone()))
            })
            .collect::<Vec<_>>();
        for (previous, current) in renames {
            self.session_order.rename_session(&previous, &current)?;
            rekey_session_launch_plan(
                &self.workspace_config_path,
                self.scope.binding_id().persistence_value(),
                &previous,
                &current,
            )?;
        }
        Ok(())
    }

    fn discard_terminal_side_effects(&mut self) {
        self.terminal_side_effect_rx.try_iter().for_each(drop);
    }

    fn window_id(&self, session_id: String, window_id: String) -> ScopedWindowId {
        ScopedWindowId::new(self.scope, session_id, window_id)
    }

    fn pane_id(&self, window: ScopedWindowId, pane_id: impl Into<String>) -> ScopedPaneId {
        ScopedPaneId {
            window,
            pane_id: pane_id.into(),
        }
    }

    fn degraded_error(&self) -> Option<String> {
        self.mux.last_error().map(str::to_owned).or_else(|| {
            self.reattach
                .map(|reattach| format!("reconnecting (attempt {})", reattach.attempts))
        })
    }
}

const AUTOMATION_BACKEND_EVENT_DRAIN_LIMIT: usize = 64;

/// Every lifecycle delta below is backed by the controller's complete binding
/// source view. A subscription can therefore bootstrap before its first delta.
const AUTOMATION_BINDING_SNAPSHOT_TOPICS: &[&str] = &[
    "topology.changed",
    "terminal.process_changed",
    "terminal.title_changed",
    "terminal.options_changed",
    "terminal.foreground_changed",
    "terminal.cwd_changed",
    "terminal.occupant_replaced",
    "terminal.closed",
    "backend.connection_changed",
    "backend.lagged",
    "backend.rebased",
];

const AUTOMATION_TERMINAL_STATE_TOPICS: &[&str] = &[
    "terminal.process_changed",
    "terminal.title_changed",
    "terminal.options_changed",
    "terminal.foreground_changed",
    "terminal.cwd_changed",
];
const AUTOMATION_UNKNOWN_STATE_REASON: &str = "backend_snapshot_field_unavailable";
const AUTOMATION_BACKEND_STATE_TOPICS: &[&str] = &[
    "backend.connection_changed",
    "backend.lagged",
    "backend.rebased",
];

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct AutomationTerminalStateKey {
    session_id: String,
    window_id: String,
    pane_id: String,
    terminal_id: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug)]
struct AutomationTerminalTopicState {
    available: bool,
    value: Value,
    reason: Option<&'static str>,
}
fn unknown_automation_backend_states() -> HashMap<&'static str, AutomationTerminalTopicState> {
    AUTOMATION_BACKEND_STATE_TOPICS
        .iter()
        .map(|&topic| (topic, unknown_automation_terminal_topic_state()))
        .collect()
}

#[derive(Clone, Debug)]
struct AutomationTerminalState {
    target: MuxEventTarget,
    generation: u64,
    topics: HashMap<&'static str, AutomationTerminalTopicState>,
    latest_pane_state: Option<MuxPaneState>,
    options: BTreeMap<String, String>,
    options_known: bool,
}

struct AutomationTargetContext {
    process: String,
    window_state_key: String,
    window_generation: u64,
}

struct AutomationTerminalIdentity<'a> {
    session_id: &'a str,
    window_id: &'a str,
    pane_id: &'a str,
    terminal_id: &'a str,
}

#[derive(Clone)]
struct DirectoryClaimsContext {
    instance: InstanceRef,
    window_id: String,
}

fn directory_binding_ref(context: &DirectoryClaimsContext, binding: &BindingRuntime) -> BindingRef {
    directory_binding_ref_for_generation(context, binding, binding.mux.binding_generation())
}

fn directory_binding_ref_for_generation(
    context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
    generation: u64,
) -> BindingRef {
    BindingRef {
        window: WindowRef {
            instance: context.instance.clone(),
            window_id: context.window_id.clone(),
        },
        space_id: binding.scope.space_id().persistence_value().to_string(),
        binding_id: binding.scope.binding_id().persistence_value().to_string(),
        generation,
    }
}

fn directory_claimant_for_pane(
    context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
    session_id: &str,
    window_id: &str,
    pane_id: &str,
    terminal_id: &str,
) -> Option<ClaimantRef> {
    let occupant_generation =
        binding
            .mux
            .terminal_generation(session_id, window_id, terminal_id)?;
    Some(directory_claimant_for_pane_at_generation(
        context,
        binding,
        session_id,
        pane_id,
        terminal_id,
        binding.mux.binding_generation(),
        occupant_generation,
    ))
}

fn directory_claimant_for_pane_at_generation(
    context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
    session_id: &str,
    pane_id: &str,
    terminal_id: &str,
    binding_generation: u64,
    occupant_generation: u64,
) -> ClaimantRef {
    let binding_ref = directory_binding_ref_for_generation(context, binding, binding_generation);
    ClaimantRef {
        session: SessionRef {
            binding: binding_ref.clone(),
            session_id: session_id.to_owned(),
        },
        pane: PaneRef {
            binding: binding_ref.clone(),
            pane_id: pane_id.to_owned(),
        },
        terminal: TerminalRef {
            binding: binding_ref,
            terminal_id: terminal_id.to_owned(),
            occupant_generation,
        },
    }
}

fn automation_terminal_target_for_generation(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    identity: AutomationTerminalIdentity<'_>,
    binding_generation: u64,
    generation: u64,
) -> CommandTarget {
    let binding_handle =
        automation_binding_handle_for_generation(binding, context, binding_generation);
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: serde_json::to_string(&[
            binding_handle.as_str(),
            identity.session_id,
            identity.window_id,
            identity.pane_id,
            identity.terminal_id,
        ])
        .expect("serialize terminal target"),
        generation,
    }
}

fn automation_terminal_target_from_observation(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    observation: &MuxEventObservation,
    generation: u64,
) -> Option<CommandTarget> {
    let target = observation.event.target.as_ref()?;
    let (Some(session_id), Some(window_id), Some(pane_id), Some(terminal_id)) = (
        target.session_id.as_deref(),
        target.window_id.as_deref(),
        target.pane_id.as_deref(),
        target.terminal_id.as_deref(),
    ) else {
        return None;
    };
    Some(automation_terminal_target_for_generation(
        binding,
        context,
        AutomationTerminalIdentity {
            session_id,
            window_id,
            pane_id,
            terminal_id,
        },
        observation.binding_generation,
        generation,
    ))
}

fn directory_claims_automation_error(error: impl std::fmt::Display) -> AutomationError {
    AutomationError {
        code: -32000,
        message: format!("directory claims update failed: {error}"),
        data: None,
    }
}

fn worktree_service_automation_error(error: impl std::fmt::Display) -> AutomationError {
    AutomationError {
        code: -32000,
        message: format!("worktree inventory update failed: {error}"),
        data: None,
    }
}

/// Publish the cross-instance inventory while its claims revision is still
/// protected, so a concurrent owner cannot overwrite a newer source snapshot.
fn publish_directory_usage_changed_for_scope(
    automation: &AutomationHub,
    claims: &DirectoryClaims,
    scope: String,
    target: Option<CommandTarget>,
    payload: Value,
) -> Result<(), AutomationError> {
    claims
        .with_live_snapshots(|snapshots| {
            let snapshot =
                serde_json::to_value(snapshots).map_err(directory_claims_automation_error)?;
            let publication = EventPublication::new(
                scope,
                "directory.usage_changed",
                json!({"source": "directory_claims"}),
                target,
                payload,
            );
            automation.publish_event_with_snapshot(publication, snapshot)?;
            Ok(())
        })
        .map_err(directory_claims_automation_error)?
}

fn publish_directory_usage_changed(
    automation: &AutomationHub,
    claims: &DirectoryClaims,
    binding: &BindingRuntime,
    target: Option<CommandTarget>,
    payload: Value,
) -> Result<(), AutomationError> {
    publish_directory_usage_changed_for_scope(
        automation,
        claims,
        automation_event_scope(binding.scope),
        target,
        payload,
    )
}

fn worktree_inventory_snapshot(
    service: &crate::git::WorktreeService,
    worktree: &WorktreeRef,
) -> Result<Value, AutomationError> {
    service
        .list_repository(&worktree.repository)
        .map_err(worktree_service_automation_error)
        .and_then(|inventory| {
            serde_json::to_value(inventory).map_err(worktree_service_automation_error)
        })
}

fn publish_worktree_changed(
    automation: &AutomationHub,
    service: &crate::git::WorktreeService,
    binding: &BindingRuntime,
    worktree: &WorktreeRef,
    payload: Value,
) -> Result<(), AutomationError> {
    let snapshot = worktree_inventory_snapshot(service, worktree)?;
    let publication = EventPublication::new(
        automation_event_scope(binding.scope),
        "worktree.changed",
        json!({"source": "worktree_service"}),
        None,
        payload,
    );
    automation.publish_event_with_snapshot(publication, snapshot)?;
    Ok(())
}

fn automation_terminal_state_topic(topic: MuxEventTopic) -> Option<&'static str> {
    match topic {
        MuxEventTopic::PaneStateChanged => Some("terminal.process_changed"),
        MuxEventTopic::PaneTitleChanged => Some("terminal.title_changed"),
        MuxEventTopic::PaneOptionsChanged => Some("terminal.options_changed"),
        MuxEventTopic::PaneForegroundChanged => Some("terminal.foreground_changed"),
        MuxEventTopic::PaneCwdChanged => Some("terminal.cwd_changed"),
        _ => None,
    }
}

fn unknown_automation_terminal_topic_state() -> AutomationTerminalTopicState {
    AutomationTerminalTopicState {
        available: false,
        value: Value::Null,
        reason: Some(AUTOMATION_UNKNOWN_STATE_REASON),
    }
}

fn unknown_automation_terminal_topics() -> HashMap<&'static str, AutomationTerminalTopicState> {
    AUTOMATION_TERMINAL_STATE_TOPICS
        .iter()
        .map(|&topic| (topic, unknown_automation_terminal_topic_state()))
        .collect()
}

fn available_automation_terminal_topic_state(
    payload: &MuxEventPayload,
) -> AutomationTerminalTopicState {
    AutomationTerminalTopicState {
        available: true,
        value: serde_json::to_value(payload).expect("serialize automation terminal event payload"),
        reason: None,
    }
}

fn automation_terminal_state_key(
    target: &MuxEventTarget,
    generation: u64,
) -> Option<AutomationTerminalStateKey> {
    Some(AutomationTerminalStateKey {
        session_id: target.session_id.clone()?,
        window_id: target.window_id.clone()?,
        pane_id: target.pane_id.clone()?,
        terminal_id: target.terminal_id.clone(),
        generation,
    })
}

fn automation_terminal_target_from_anchor(
    binding: &BindingRuntime,
    session_id: &str,
    window_id: &str,
    anchor: &MuxPaneAnchor,
) -> Option<(AutomationTerminalStateKey, MuxEventTarget, u64)> {
    let pane_id = anchor.pane_id.clone()?;
    let (terminal_id, generation) = match anchor.terminal_id.as_deref() {
        Some(terminal_id) => (
            Some(terminal_id.to_owned()),
            binding
                .mux
                .terminal_generation(session_id, window_id, terminal_id)?,
        ),
        None => (
            None,
            binding
                .mux
                .pane_generation(session_id, window_id, &pane_id)?,
        ),
    };
    let occupant = anchor
        .occupant_id
        .clone()
        .map(|backend_identity| MuxOccupantIdentity {
            backend_identity,
            pid: anchor.pane_pid,
            process: anchor.process.clone(),
        });
    let target = MuxEventTarget {
        session_id: Some(session_id.to_owned()),
        window_id: Some(window_id.to_owned()),
        pane_id: Some(pane_id),
        terminal_id,
        occupant,
    };
    let key = automation_terminal_state_key(&target, generation)?;
    Some((key, target, generation))
}

fn seeded_automation_terminal_topics(
    anchor: &MuxPaneAnchor,
) -> HashMap<&'static str, AutomationTerminalTopicState> {
    let mut topics = unknown_automation_terminal_topics();
    let foreground = MuxForegroundState {
        pid: anchor.pane_pid,
        command: anchor.process.clone(),
        cwd: anchor.cwd.clone(),
        executable: None,
    };
    let foreground_available = foreground.pid.is_some()
        || foreground.command.is_some()
        || foreground.cwd.is_some()
        || foreground.executable.is_some();
    if foreground_available {
        topics.insert(
            "terminal.foreground_changed",
            available_automation_terminal_topic_state(&MuxEventPayload::Foreground {
                old_state: None,
                new_state: Some(foreground.clone()),
            }),
        );
    }
    if anchor.cwd.is_some() {
        topics.insert(
            "terminal.cwd_changed",
            available_automation_terminal_topic_state(&MuxEventPayload::Cwd {
                old_cwd: None,
                new_cwd: anchor.cwd.clone(),
            }),
        );
    }
    if anchor.process.is_some() || anchor.pane_pid.is_some() {
        topics.insert(
            "terminal.process_changed",
            available_automation_terminal_topic_state(&MuxEventPayload::PaneState {
                state: MuxPaneState {
                    title: None,
                    options: Vec::new(),
                    foreground: foreground_available.then_some(foreground),
                },
            }),
        );
    }
    topics
}

fn merge_rebased_automation_terminal_state(
    mut fresh: AutomationTerminalState,
    previous: AutomationTerminalState,
) -> AutomationTerminalState {
    for topic in AUTOMATION_TERMINAL_STATE_TOPICS {
        let fresh_state = fresh.topics.get(topic).is_some_and(|state| state.available);
        if !fresh_state
            && let Some(previous_state) = previous.topics.get(topic)
            && previous_state.available
        {
            fresh.topics.insert(topic, previous_state.clone());
        }
    }
    if !fresh.options_known && previous.options_known {
        let mut pane_state = previous.latest_pane_state.unwrap_or_default();
        if let Some(fresh_foreground) = fresh
            .latest_pane_state
            .as_ref()
            .and_then(|pane_state| pane_state.foreground.clone())
        {
            pane_state.foreground = Some(fresh_foreground);
        }
        fresh.latest_pane_state = Some(pane_state);
        fresh.options = previous.options;
        fresh.options_known = true;
    }
    fresh
}

fn rebase_automation_terminal_states(binding: &mut BindingRuntime) {
    let previous = std::mem::take(&mut binding.automation_terminal_states);
    let mut states = BTreeMap::new();
    for session in binding.mux.sessions() {
        for window in &session.windows {
            for anchor in std::iter::once(&window.anchor).chain(&window.panes) {
                let Some((key, target, generation)) = automation_terminal_target_from_anchor(
                    binding,
                    &session.id,
                    &window.id,
                    anchor,
                ) else {
                    continue;
                };
                let foreground = MuxForegroundState {
                    pid: anchor.pane_pid,
                    command: anchor.process.clone(),
                    cwd: anchor.cwd.clone(),
                    executable: None,
                };
                let foreground_available = foreground.pid.is_some()
                    || foreground.command.is_some()
                    || foreground.cwd.is_some()
                    || foreground.executable.is_some();
                let fresh_pane_state = foreground_available.then(|| MuxPaneState {
                    title: None,
                    options: Vec::new(),
                    foreground: Some(foreground),
                });
                let fresh = AutomationTerminalState {
                    target,
                    generation,
                    topics: seeded_automation_terminal_topics(anchor),
                    latest_pane_state: fresh_pane_state,
                    options: BTreeMap::new(),
                    options_known: false,
                };
                let state = previous
                    .get(&key)
                    .cloned()
                    .map_or(fresh.clone(), |previous| {
                        merge_rebased_automation_terminal_state(fresh, previous)
                    });
                states.entry(key).or_insert(state);
            }
        }
    }
    binding.automation_terminal_states = states;
}

fn ensure_automation_terminal_state<'a>(
    binding: &'a mut BindingRuntime,
    target: &MuxEventTarget,
    generation: u64,
) -> Option<&'a mut AutomationTerminalState> {
    let key = automation_terminal_state_key(target, generation)?;
    Some(
        binding
            .automation_terminal_states
            .entry(key)
            .or_insert_with(|| AutomationTerminalState {
                target: target.clone(),
                generation,
                topics: unknown_automation_terminal_topics(),
                latest_pane_state: None,
                options: BTreeMap::new(),
                options_known: false,
            }),
    )
}

fn retire_automation_terminal_state(
    binding: &mut BindingRuntime,
    target: &MuxEventTarget,
    generation: Option<u64>,
) {
    let Some(generation) = generation else {
        return;
    };
    let Some(key) = automation_terminal_state_key(target, generation) else {
        return;
    };
    if binding.automation_terminal_states.remove(&key).is_some() {
        return;
    }
    // Replacement events carry the new terminal identity while their
    // retired generation belongs to the old identity. The pane IDs and exact
    // generation still identify one cache entry, so retire that old identity
    // without broadening the generation match.
    binding.automation_terminal_states.retain(|candidate, _| {
        !(candidate.session_id == key.session_id
            && candidate.window_id == key.window_id
            && candidate.pane_id == key.pane_id
            && candidate.generation == key.generation)
    });
}

fn automation_pane_state_payload(state: &MuxPaneState) -> MuxEventPayload {
    MuxEventPayload::PaneState {
        state: state.clone(),
    }
}

fn refresh_automation_process_state(state: &mut AutomationTerminalState) {
    let Some(pane_state) = state.latest_pane_state.as_ref() else {
        state.topics.insert(
            "terminal.process_changed",
            unknown_automation_terminal_topic_state(),
        );
        return;
    };
    if pane_state.foreground.as_ref().is_some_and(|foreground| {
        foreground.pid.is_some() || foreground.command.is_some() || foreground.executable.is_some()
    }) {
        state.topics.insert(
            "terminal.process_changed",
            available_automation_terminal_topic_state(&automation_pane_state_payload(pane_state)),
        );
    } else {
        state.topics.insert(
            "terminal.process_changed",
            unknown_automation_terminal_topic_state(),
        );
    }
}

/// Apply the backend delta to the source cache before the corresponding event
/// reaches the automation hub. Replacements and closes only retire the exact
/// target generation they name.
fn apply_automation_terminal_event_state(
    binding: &mut BindingRuntime,
    observation: &MuxEventObservation,
) {
    if observation.binding_generation != binding.mux.binding_generation() {
        return;
    }
    let event = &observation.event;
    if matches!(
        event.topic,
        MuxEventTopic::PaneOccupantReplaced | MuxEventTopic::PaneClosed
    ) {
        if let Some(target) = event.target.as_ref() {
            retire_automation_terminal_state(
                binding,
                target,
                match event.topic {
                    MuxEventTopic::PaneClosed => observation
                        .retired_target_generation
                        .or(observation.target_generation),
                    MuxEventTopic::PaneOccupantReplaced => observation.retired_target_generation,
                    _ => None,
                },
            );
        }
        if event.topic == MuxEventTopic::PaneClosed {
            return;
        }
    }
    let (Some(target), Some(generation)) = (event.target.as_ref(), observation.target_generation)
    else {
        return;
    };
    let Some(state) = ensure_automation_terminal_state(binding, target, generation) else {
        return;
    };
    state.target = target.clone();
    state.generation = generation;
    if let MuxEventPayload::PaneState { state: pane_state } = &event.payload {
        state.latest_pane_state = Some(pane_state.clone());
        state.options = pane_state
            .options
            .iter()
            .map(|option| (option.name.clone(), option.value.clone()))
            .collect();
        state.options_known = true;
        state.topics.insert(
            "terminal.options_changed",
            available_automation_terminal_topic_state(&event.payload),
        );
        if pane_state.title.is_some() {
            state.topics.insert(
                "terminal.title_changed",
                available_automation_terminal_topic_state(&MuxEventPayload::Title {
                    old_title: None,
                    new_title: pane_state.title.clone(),
                }),
            );
        } else {
            state.topics.insert(
                "terminal.title_changed",
                unknown_automation_terminal_topic_state(),
            );
        }
        if pane_state.foreground.is_some() {
            state.topics.insert(
                "terminal.foreground_changed",
                available_automation_terminal_topic_state(&MuxEventPayload::Foreground {
                    old_state: None,
                    new_state: pane_state.foreground.clone(),
                }),
            );
            if pane_state
                .foreground
                .as_ref()
                .and_then(|foreground| foreground.cwd.as_ref())
                .is_some()
            {
                state.topics.insert(
                    "terminal.cwd_changed",
                    available_automation_terminal_topic_state(&MuxEventPayload::Cwd {
                        old_cwd: None,
                        new_cwd: pane_state
                            .foreground
                            .as_ref()
                            .and_then(|foreground| foreground.cwd.clone()),
                    }),
                );
            } else {
                state.topics.insert(
                    "terminal.cwd_changed",
                    unknown_automation_terminal_topic_state(),
                );
            }
            if pane_state.foreground.as_ref().is_some_and(|foreground| {
                foreground.pid.is_some()
                    || foreground.command.is_some()
                    || foreground.executable.is_some()
            }) {
                state.topics.insert(
                    "terminal.process_changed",
                    available_automation_terminal_topic_state(&event.payload),
                );
            } else {
                state.topics.insert(
                    "terminal.process_changed",
                    unknown_automation_terminal_topic_state(),
                );
            }
        } else {
            state.topics.insert(
                "terminal.foreground_changed",
                unknown_automation_terminal_topic_state(),
            );
            state.topics.insert(
                "terminal.cwd_changed",
                unknown_automation_terminal_topic_state(),
            );
            state.topics.insert(
                "terminal.process_changed",
                unknown_automation_terminal_topic_state(),
            );
        }
        return;
    }
    let Some(topic) = automation_terminal_state_topic(event.topic) else {
        if event.topic == MuxEventTopic::PaneOccupantReplaced {
            let _ = ensure_automation_terminal_state(binding, target, generation);
        }
        return;
    };
    match &event.payload {
        MuxEventPayload::Option {
            name, new_value, ..
        } => {
            if let Some(value) = new_value {
                state.options.insert(name.clone(), value.clone());
            } else {
                state.options.remove(name);
            }
            if state.options_known {
                let mut pane_state = state.latest_pane_state.clone().unwrap_or_default();
                pane_state.options = state
                    .options
                    .iter()
                    .map(|(name, value)| MuxPaneOption {
                        name: name.clone(),
                        value: value.clone(),
                    })
                    .collect();
                state.latest_pane_state = Some(pane_state.clone());
                refresh_automation_process_state(state);
                state.topics.insert(
                    "terminal.options_changed",
                    available_automation_terminal_topic_state(&automation_pane_state_payload(
                        &pane_state,
                    )),
                );
            } else {
                state.topics.insert(
                    "terminal.options_changed",
                    unknown_automation_terminal_topic_state(),
                );
            }
        }
        MuxEventPayload::Title { new_title, .. } => {
            let mut pane_state = state.latest_pane_state.take().unwrap_or_default();
            pane_state.title = new_title.clone();
            state.latest_pane_state = Some(pane_state);
            refresh_automation_process_state(state);
            state.topics.insert(
                topic,
                available_automation_terminal_topic_state(&event.payload),
            );
        }
        MuxEventPayload::Cwd { new_cwd, .. } => {
            let mut pane_state = state.latest_pane_state.take().unwrap_or_default();
            let had_foreground = pane_state.foreground.is_some();
            let mut foreground = pane_state.foreground.take().unwrap_or_default();
            foreground.cwd = new_cwd.clone();
            let foreground_state = foreground.clone();
            pane_state.foreground = Some(foreground);
            state.latest_pane_state = Some(pane_state);
            refresh_automation_process_state(state);
            state.topics.insert(
                topic,
                available_automation_terminal_topic_state(&event.payload),
            );
            if had_foreground {
                state.topics.insert(
                    "terminal.foreground_changed",
                    available_automation_terminal_topic_state(&MuxEventPayload::Foreground {
                        old_state: None,
                        new_state: Some(foreground_state),
                    }),
                );
            }
        }
        MuxEventPayload::Foreground { new_state, .. } => {
            let mut pane_state = state.latest_pane_state.take().unwrap_or_default();
            pane_state.foreground = new_state.clone();
            state.latest_pane_state = Some(pane_state);
            refresh_automation_process_state(state);
            state.topics.insert(
                topic,
                available_automation_terminal_topic_state(&event.payload),
            );
            if let Some(foreground) = new_state {
                if foreground.cwd.is_some() {
                    state.topics.insert(
                        "terminal.cwd_changed",
                        available_automation_terminal_topic_state(&MuxEventPayload::Cwd {
                            old_cwd: None,
                            new_cwd: foreground.cwd.clone(),
                        }),
                    );
                } else {
                    state.topics.insert(
                        "terminal.cwd_changed",
                        unknown_automation_terminal_topic_state(),
                    );
                }
            } else {
                state.topics.insert(
                    "terminal.cwd_changed",
                    unknown_automation_terminal_topic_state(),
                );
            }
        }
        _ => {
            state.topics.insert(
                topic,
                available_automation_terminal_topic_state(&event.payload),
            );
        }
    }
}

fn backend_topic_state(value: Value) -> AutomationTerminalTopicState {
    AutomationTerminalTopicState {
        available: true,
        value,
        reason: None,
    }
}

fn apply_automation_backend_event_state(
    binding: &mut BindingRuntime,
    observation: &MuxEventObservation,
) {
    if observation.binding_generation != binding.mux.binding_generation() {
        return;
    }
    let event = &observation.event;
    match event.topic {
        MuxEventTopic::BackendDisconnected => {
            binding.automation_backend_states.insert(
                "backend.connection_changed",
                backend_topic_state(
                    serde_json::to_value(&event.payload)
                        .expect("serialize backend connection payload"),
                ),
            );
        }
        MuxEventTopic::BackendLagged => {
            binding.automation_backend_states.insert(
                "backend.lagged",
                backend_topic_state(json!({
                    "lagged": true,
                    "last_event": serde_json::to_value(&event.payload)
                        .expect("serialize backend lag payload"),
                })),
            );
        }
        MuxEventTopic::SnapshotRebased => {
            binding.automation_backend_states.insert(
                "backend.rebased",
                backend_topic_state(
                    serde_json::to_value(&event.payload).expect("serialize backend rebase payload"),
                ),
            );
            binding.automation_backend_states.insert(
                "backend.lagged",
                backend_topic_state(json!({"lagged": false, "last_event": null})),
            );
            binding.automation_backend_states.insert(
                "backend.connection_changed",
                unknown_automation_terminal_topic_state(),
            );
        }
        _ => {}
    }
}

fn rebase_automation_backend_states(binding: &mut BindingRuntime) {
    binding.automation_backend_states.insert(
        "backend.rebased",
        backend_topic_state(
            serde_json::to_value(MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Bootstrap,
            })
            .expect("serialize bootstrap rebase payload"),
        ),
    );
    binding.automation_backend_states.insert(
        "backend.lagged",
        backend_topic_state(json!({"lagged": false, "last_event": null})),
    );
    binding.automation_backend_states.insert(
        "backend.connection_changed",
        unknown_automation_terminal_topic_state(),
    );
}

fn automation_binding_backend_snapshot(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    topic: &str,
) -> Value {
    let topic_state = binding
        .automation_backend_states
        .get(topic)
        .cloned()
        .unwrap_or_else(unknown_automation_terminal_topic_state);
    let mut target = serde_json::Map::new();
    target.insert(
        "target".to_owned(),
        serde_json::to_value(MuxEventTarget::default()).expect("serialize binding status target"),
    );
    target.insert(
        "generation".to_owned(),
        json!(binding.mux.binding_generation()),
    );
    target.insert(
        "availability".to_owned(),
        Value::String(if topic_state.available {
            "available".to_owned()
        } else {
            "unknown".to_owned()
        }),
    );
    target.insert("value".to_owned(), topic_state.value);
    if let Some(reason) = topic_state.reason {
        target.insert("reason".to_owned(), Value::String(reason.to_owned()));
    }
    json!({
        "topic": topic,
        "scope": automation_event_scope(binding.scope),
        "binding": automation_binding_target(binding, context),
        "targets": [Value::Object(target)],
    })
}

fn automation_binding_state_snapshot(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    topic: &str,
) -> Value {
    let targets = binding
        .automation_terminal_states
        .values()
        .map(|state| {
            let topic_state = state
                .topics
                .get(topic)
                .cloned()
                .unwrap_or_else(unknown_automation_terminal_topic_state);
            let mut target = serde_json::Map::new();
            target.insert(
                "target".to_owned(),
                serde_json::to_value(&state.target).expect("serialize automation target"),
            );
            target.insert("generation".to_owned(), json!(state.generation));
            target.insert(
                "availability".to_owned(),
                Value::String(if topic_state.available {
                    "available".to_owned()
                } else {
                    "unknown".to_owned()
                }),
            );
            target.insert("value".to_owned(), topic_state.value);
            if let Some(reason) = topic_state.reason {
                target.insert("reason".to_owned(), Value::String(reason.to_owned()));
            }
            Value::Object(target)
        })
        .collect::<Vec<_>>();
    json!({
        "topic": topic,
        "scope": automation_event_scope(binding.scope),
        "binding": automation_binding_target(binding, context),
        "targets": targets,
    })
}

fn event_cwd(event: &MuxEvent) -> Option<&str> {
    match &event.payload {
        MuxEventPayload::Cwd {
            new_cwd: Some(cwd), ..
        } => Some(cwd),
        MuxEventPayload::Foreground {
            new_state: Some(state),
            ..
        } => state.cwd.as_deref(),
        MuxEventPayload::PaneState { state } => state
            .foreground
            .as_ref()
            .and_then(|foreground| foreground.cwd.as_deref()),
        _ => None,
    }
}

fn directory_claimant_from_observation(
    context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
    observation: &MuxEventObservation,
    occupant_generation: u64,
) -> Option<ClaimantRef> {
    let target = observation.event.target.as_ref()?;
    let (Some(session_id), Some(_window_id), Some(pane_id), Some(terminal_id)) = (
        target.session_id.as_deref(),
        target.window_id.as_deref(),
        target.pane_id.as_deref(),
        target.terminal_id.as_deref(),
    ) else {
        return None;
    };
    Some(directory_claimant_for_pane_at_generation(
        context,
        binding,
        session_id,
        pane_id,
        terminal_id,
        observation.binding_generation,
        occupant_generation,
    ))
}

fn directory_claim_observation_is_current(
    context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
    observation: &MuxEventObservation,
    generation: u64,
) -> bool {
    let observed_binding =
        directory_binding_ref_for_generation(context, binding, observation.binding_generation);
    if observed_binding != directory_binding_ref(context, binding) {
        return false;
    }

    let Some(target) = observation.event.target.as_ref() else {
        return false;
    };
    let (Some(session_id), Some(window_id), Some(pane_id), Some(terminal_id)) = (
        target.session_id.as_deref(),
        target.window_id.as_deref(),
        target.pane_id.as_deref(),
        target.terminal_id.as_deref(),
    ) else {
        return false;
    };

    binding
        .mux
        .terminal_id_for_pane(session_id, window_id, pane_id)
        == Some(terminal_id)
        && binding
            .mux
            .terminal_generation(session_id, window_id, terminal_id)
            == Some(generation)
}

fn consume_directory_claim_event(
    claims: &DirectoryClaims,
    claims_context: &DirectoryClaimsContext,
    automation: &AutomationHub,
    binding: &BindingRuntime,
    target_context: &AutomationTargetContext,
    observation: &MuxEventObservation,
) -> Result<(), AutomationError> {
    let event = &observation.event;
    if event.scope != binding.scope || binding.multiplexer.remote.is_some() {
        return Ok(());
    }
    let Some(target) = event.target.as_ref() else {
        return Ok(());
    };
    let terminal_id = target.terminal_id.as_deref();

    match event.topic {
        MuxEventTopic::PaneOccupantReplaced => {
            let (Some(terminal_id), Some(retired_generation)) =
                (terminal_id, observation.retired_target_generation)
            else {
                return Ok(());
            };
            let Some(claimant) = directory_claimant_from_observation(
                claims_context,
                binding,
                observation,
                retired_generation,
            ) else {
                return Ok(());
            };
            if let Some(revision) = claims
                .release_observed_claimant(&claimant)
                .map_err(directory_claims_automation_error)?
            {
                publish_directory_usage_changed(
                    automation,
                    claims,
                    binding,
                    automation_target_from_mux_event(binding, target_context, observation),
                    json!({
                        "reason": "occupant_replaced",
                        "terminal_id": terminal_id,
                        "revision": revision,
                    }),
                )?;
            }
        }
        MuxEventTopic::PaneClosed => {
            let (Some(terminal_id), Some(retired_generation)) =
                (terminal_id, observation.retired_target_generation)
            else {
                return Ok(());
            };
            let Some(claimant) = directory_claimant_from_observation(
                claims_context,
                binding,
                observation,
                retired_generation,
            ) else {
                return Ok(());
            };
            if let Some(revision) = claims
                .release_claimant(&claimant)
                .map_err(directory_claims_automation_error)?
            {
                publish_directory_usage_changed(
                    automation,
                    claims,
                    binding,
                    automation_target_from_mux_event(binding, target_context, observation),
                    json!({
                        "reason": "terminal_closed",
                        "terminal_id": terminal_id,
                        "revision": revision,
                    }),
                )?;
            }
        }
        MuxEventTopic::PaneCwdChanged
        | MuxEventTopic::PaneForegroundChanged
        | MuxEventTopic::PaneStateChanged => {
            let Some(generation) = observation.target_generation else {
                return Ok(());
            };
            if !directory_claim_observation_is_current(
                claims_context,
                binding,
                observation,
                generation,
            ) {
                return Ok(());
            }
            let Some(claimant) = directory_claimant_from_observation(
                claims_context,
                binding,
                observation,
                generation,
            ) else {
                return Ok(());
            };
            let target = automation_terminal_target_from_observation(
                binding,
                target_context,
                observation,
                generation,
            );
            let Some(cwd) = event_cwd(event) else {
                if let Some(revision) = claims
                    .release_observed_claimant(&claimant)
                    .map_err(directory_claims_automation_error)?
                {
                    publish_directory_usage_changed(
                        automation,
                        claims,
                        binding,
                        target,
                        json!({
                            "reason": "cwd_cleared",
                            "revision": revision,
                        }),
                    )?;
                }
                return Ok(());
            };
            let directory =
                DirectoryRef::resolve(cwd).map_err(directory_claims_automation_error)?;
            let update = claims
                .observe_cwd(claimant, directory)
                .map_err(directory_claims_automation_error)?;
            publish_directory_usage_changed(
                automation,
                claims,
                binding,
                target,
                json!({
                    "reason": "cwd_changed",
                    "update": update,
                }),
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn collect_launch_cwds<'a>(layout: &'a MuxPaneLaunchPlan, cwds: &mut Vec<&'a str>) {
    match layout {
        MuxPaneLaunchPlan::Pane(pane) => cwds.push(&pane.cwd),
        MuxPaneLaunchPlan::Split(split) => {
            collect_launch_cwds(&split.first, cwds);
            collect_launch_cwds(&split.second, cwds);
        }
    }
}

fn directory_claim_warning_code(severity: DirectoryClaimSeverity) -> &'static str {
    match severity {
        DirectoryClaimSeverity::Informational => "directory_shared_same_session",
        DirectoryClaimSeverity::Warning => "directory_shared_cross_session",
        DirectoryClaimSeverity::StrongWarning => "worktree_shared_cross_session",
    }
}

fn directory_claim_warning_message(severity: DirectoryClaimSeverity) -> &'static str {
    match severity {
        DirectoryClaimSeverity::Informational => {
            "the launch directory is already used by another terminal in this session"
        }
        DirectoryClaimSeverity::Warning => {
            "the launch directory is already used by another session"
        }
        DirectoryClaimSeverity::StrongWarning => {
            "the linked worktree is already used by another session"
        }
    }
}

fn append_directory_claim_outcome(
    outcome: &mut CommandOutcome,
    outcome_warnings: Vec<CommandWarning>,
    structured_warnings: Vec<Value>,
) {
    let CommandOutcome::Success { value, warnings } = outcome else {
        return;
    };
    if !structured_warnings.is_empty() {
        match value {
            Value::Object(values) => {
                values.insert(
                    "directory_warnings".to_owned(),
                    Value::Array(structured_warnings),
                );
            }
            _ => {
                let prior = std::mem::replace(value, Value::Null);
                let mut values = serde_json::Map::new();
                values.insert("result".to_owned(), prior);
                values.insert(
                    "directory_warnings".to_owned(),
                    Value::Array(structured_warnings),
                );
                *value = Value::Object(values);
            }
        }
    }
    warnings.extend(outcome_warnings);
}

const AUTOMATION_PENDING_EVENT_LIMIT: usize = 256;

fn automation_event_is_replaceable(observation: &MuxEventObservation) -> bool {
    matches!(
        observation.event.topic,
        MuxEventTopic::PaneStateChanged
            | MuxEventTopic::PaneTitleChanged
            | MuxEventTopic::PaneOptionsChanged
            | MuxEventTopic::PaneForegroundChanged
            | MuxEventTopic::PaneCwdChanged
    )
}

fn enqueue_automation_event(binding: &mut BindingRuntime, observation: MuxEventObservation) {
    let replaceable = automation_event_is_replaceable(&observation);
    if replaceable
        && let Some(existing) = binding
            .pending_automation_events
            .iter_mut()
            .find(|existing| {
                existing.event.topic == observation.event.topic
                    && existing.event.target == observation.event.target
                    && existing.target_generation == observation.target_generation
                    && existing.binding_generation == observation.binding_generation
            })
    {
        *existing = observation;
        return;
    }
    if binding.pending_automation_events.len() < AUTOMATION_PENDING_EVENT_LIMIT {
        binding.pending_automation_events.push_back(observation);
        return;
    }
    if let Some(index) = binding
        .pending_automation_events
        .iter()
        .position(automation_event_is_replaceable)
    {
        binding.pending_automation_events.remove(index);
        binding.pending_automation_events.push_back(observation);
        binding.automation_event_refresh_pending = true;
    } else {
        // Rebase observations make an older refresh marker redundant. Keep
        // every newer lifecycle observation within the bound by evicting an
        // older refresh or non-close event; never evict an authoritative
        // close in favor of another status event.
        if let Some(index) = binding
            .pending_automation_events
            .iter()
            .position(automation_event_requires_refresh)
            .or_else(|| {
                binding
                    .pending_automation_events
                    .iter()
                    .position(|event| event.event.topic != MuxEventTopic::PaneClosed)
            })
        {
            binding.pending_automation_events.remove(index);
            binding.pending_automation_events.push_back(observation);
            binding.automation_event_refresh_pending = true;
        } else {
            binding.automation_event_refresh_pending = true;
        }
    }
}

fn automation_event_requires_refresh(observation: &MuxEventObservation) -> bool {
    observation.event.topic == MuxEventTopic::TopologyChanged || observation.event.requires_rebase()
}

fn collect_binding_automation_events(binding: &mut BindingRuntime) -> usize {
    let observations = binding
        .mux
        .drain_events(&binding.multiplexer, AUTOMATION_BACKEND_EVENT_DRAIN_LIMIT);
    let drained = observations.len();
    for observation in observations {
        binding.automation_event_refresh_pending |= automation_event_requires_refresh(&observation);
        enqueue_automation_event(binding, observation);
    }
    drained
}

fn purge_retired_terminal_output(
    automation: &AutomationHub,
    binding: &BindingRuntime,
    target_context: &AutomationTargetContext,
    observation: &MuxEventObservation,
) -> Result<(), AutomationError> {
    let Some(generation) = observation.retired_target_generation else {
        return Ok(());
    };
    let Some(target) = automation_terminal_target_from_observation(
        binding,
        target_context,
        observation,
        generation,
    ) else {
        return Ok(());
    };
    let scope = automation_event_scope(binding.scope);
    automation
        .events()
        .purge_terminal_output(&scope, |candidate| candidate == &target)?;
    Ok(())
}

fn consume_pending_automation_events(
    pending: &mut VecDeque<MuxEventObservation>,
    mut ready: impl FnMut(&MuxEventObservation) -> bool,
    mut publish: impl FnMut(&MuxEventObservation) -> Result<(), AutomationError>,
) -> Result<usize, AutomationError> {
    let mut published = 0;
    while let Some(observation) = pending.front() {
        if !ready(observation) {
            break;
        }
        publish(observation)?;
        pending.pop_front();
        published += 1;
    }
    Ok(published)
}

fn publish_pending_binding_automation_events(
    binding: &mut BindingRuntime,
    automation: &AutomationHub,
    target_context: &AutomationTargetContext,
    claims: &DirectoryClaims,
    claims_context: &DirectoryClaimsContext,
) -> Result<usize, AutomationError> {
    let refresh_pending = binding.automation_event_refresh_pending;
    let mut pending = std::mem::take(&mut binding.pending_automation_events);
    let result = consume_pending_automation_events(
        &mut pending,
        |_| !refresh_pending,
        |observation| {
            purge_retired_terminal_output(automation, binding, target_context, observation)?;
            consume_directory_claim_event(
                claims,
                claims_context,
                automation,
                binding,
                target_context,
                observation,
            )?;
            publish_mux_event(automation, binding, target_context, observation)
        },
    );
    binding.pending_automation_events = pending;
    result
}

fn automation_event_mutates_terminal_cache(observation: &MuxEventObservation) -> bool {
    matches!(
        observation.event.topic,
        MuxEventTopic::PaneStateChanged
            | MuxEventTopic::PaneTitleChanged
            | MuxEventTopic::PaneOptionsChanged
            | MuxEventTopic::PaneForegroundChanged
            | MuxEventTopic::PaneCwdChanged
            | MuxEventTopic::PaneOccupantReplaced
            | MuxEventTopic::PaneClosed
    )
}

fn automation_observation_target_is_live(
    binding: &BindingRuntime,
    observation: &MuxEventObservation,
) -> bool {
    let Some(target) = observation.event.target.as_ref() else {
        return false;
    };
    let generation = if observation.event.topic == MuxEventTopic::PaneClosed {
        observation
            .retired_target_generation
            .or(observation.target_generation)
    } else {
        observation.target_generation
    };
    let Some(generation) = generation else {
        return false;
    };
    if let Some(key) = automation_terminal_state_key(target, generation)
        && binding.automation_terminal_states.contains_key(&key)
    {
        return true;
    }
    // Unit callers can apply events before any authoritative inventory exists.
    // Once the controller has a live inventory, require an exact generation
    // match so historical events cannot repopulate a rebased cache.
    if binding.mux.sessions().is_empty() {
        return true;
    }
    let Some(session_id) = target.session_id.as_deref() else {
        return false;
    };
    let Some(window_id) = target.window_id.as_deref() else {
        return false;
    };
    match (target.terminal_id.as_deref(), target.pane_id.as_deref()) {
        (Some(terminal_id), Some(pane_id)) => {
            binding
                .mux
                .terminal_id_for_pane(session_id, window_id, pane_id)
                == Some(terminal_id)
                && binding
                    .mux
                    .terminal_generation(session_id, window_id, terminal_id)
                    == Some(generation)
        }
        (Some(terminal_id), None) => {
            binding
                .mux
                .terminal_generation(session_id, window_id, terminal_id)
                == Some(generation)
        }
        (None, Some(pane_id)) => {
            binding.mux.pane_generation(session_id, window_id, pane_id) == Some(generation)
        }
        (None, None) => false,
    }
}

fn publish_mux_event(
    automation: &AutomationHub,
    binding: &mut BindingRuntime,
    target_context: &AutomationTargetContext,
    observation: &MuxEventObservation,
) -> Result<(), AutomationError> {
    let event = &observation.event;
    apply_automation_backend_event_state(binding, observation);
    if !automation_event_mutates_terminal_cache(observation)
        || automation_observation_target_is_live(binding, observation)
    {
        apply_automation_terminal_event_state(binding, observation);
    }
    let target = automation_target_from_mux_event(binding, target_context, observation);
    let scope = automation_event_scope(event.scope);
    let provenance = json!({
        "backend": &event.provenance,
        "backend_identity": &event.backend_identity,
        "cursor": &event.cursor,
    });
    let payload = json!({
        "backend_revision": event.revision,
        "backend_cursor": &event.cursor,
        "backend_target": &event.target,
        "data": &event.payload,
    });
    if event.topic == MuxEventTopic::TerminalOutput
        && let Some(target) = target
            .as_ref()
            .filter(|target| target.kind == ResourceKind::Terminal)
    {
        automation.publish_terminal_output(scope, provenance, target.clone(), payload)?;
        return Ok(());
    }
    let publication = EventPublication::new(
        scope,
        automation_event_topic(event.topic),
        provenance,
        target,
        payload,
    );
    automation.events().publish_with_snapshots(
        publication,
        AUTOMATION_BINDING_SNAPSHOT_TOPICS.iter().map(|topic| {
            let snapshot = if AUTOMATION_TERMINAL_STATE_TOPICS.contains(topic) {
                automation_binding_state_snapshot(binding, target_context, topic)
            } else if AUTOMATION_BACKEND_STATE_TOPICS.contains(topic) {
                automation_binding_backend_snapshot(binding, target_context, topic)
            } else {
                automation_binding_snapshot(binding, target_context)
            };
            ((*topic).to_owned(), snapshot)
        }),
    )?;
    Ok(())
}

fn automation_target_from_mux_event(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    observation: &MuxEventObservation,
) -> Option<CommandTarget> {
    let Some(target) = observation.event.target.as_ref() else {
        return Some(automation_binding_target_for_generation(
            binding,
            context,
            observation.binding_generation,
        ));
    };
    let session = target.session_id.as_deref()?;
    let generation = if observation.event.topic == MuxEventTopic::PaneClosed {
        observation
            .retired_target_generation
            .or(observation.target_generation)?
    } else {
        observation.target_generation?
    };
    let binding_handle =
        automation_binding_handle_for_generation(binding, context, observation.binding_generation);
    match (
        target.window_id.as_deref(),
        target.pane_id.as_deref(),
        target.terminal_id.as_deref(),
    ) {
        (Some(window), Some(pane), Some(terminal)) => Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::to_string(&[&binding_handle, session, window, pane, terminal])
                .expect("serialize terminal target"),
            generation,
        }),
        (Some(window), Some(pane), None) => Some(CommandTarget {
            kind: ResourceKind::Pane,
            handle: serde_json::to_string(&[&binding_handle, session, window, pane])
                .expect("serialize pane target"),
            generation,
        }),
        (Some(window), None, _) => Some(CommandTarget {
            kind: ResourceKind::MuxWindow,
            handle: serde_json::to_string(&[&binding_handle, session, window])
                .expect("serialize mux window target"),
            generation,
        }),
        (None, None, None) => Some(CommandTarget {
            kind: ResourceKind::Session,
            handle: serde_json::to_string(&[&binding_handle, session])
                .expect("serialize session target"),
            generation,
        }),
        _ => None,
    }
}

fn automation_binding_handle(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
) -> String {
    automation_binding_handle_for_generation(binding, context, binding.mux.binding_generation())
}

fn automation_binding_handle_for_generation(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    generation: u64,
) -> String {
    let scope = binding.scope;
    let space = scope.space_id().persistence_value().to_string();
    let binding_id = scope.binding_id().persistence_value().to_string();
    serde_json::to_string(&(
        &context.process,
        &context.window_state_key,
        context.window_generation,
        &space,
        &binding_id,
        generation,
    ))
    .expect("serialize binding target")
}

fn automation_binding_target(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
) -> CommandTarget {
    automation_binding_target_for_generation(binding, context, binding.mux.binding_generation())
}

fn automation_binding_target_for_generation(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
    generation: u64,
) -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Binding,
        handle: automation_binding_handle_for_generation(binding, context, generation),
        generation,
    }
}

fn live_directory_claimants(
    claims_context: &DirectoryClaimsContext,
    binding: &BindingRuntime,
) -> Vec<ClaimantRef> {
    let mut claimants = Vec::new();
    let mut seen = HashSet::new();
    for session in binding.mux.all_sessions() {
        for window in &session.windows {
            for pane in std::iter::once(&window.anchor).chain(&window.panes) {
                let (Some(pane_id), Some(terminal_id)) =
                    (pane.pane_id.as_deref(), pane.terminal_id.as_deref())
                else {
                    continue;
                };
                if !seen.insert((
                    session.id.clone(),
                    window.id.clone(),
                    pane_id.to_owned(),
                    terminal_id.to_owned(),
                )) {
                    continue;
                }
                if let Some(claimant) = directory_claimant_for_pane(
                    claims_context,
                    binding,
                    &session.id,
                    &window.id,
                    pane_id,
                    terminal_id,
                ) {
                    claimants.push(claimant);
                }
            }
        }
    }
    claimants
}

fn reconcile_directory_claims_after_authoritative_refresh(
    claims: &DirectoryClaims,
    claims_context: &DirectoryClaimsContext,
    automation: &AutomationHub,
    binding: &BindingRuntime,
    target_context: &AutomationTargetContext,
) -> Result<Option<u64>, AutomationError> {
    if binding.multiplexer.remote.is_some() {
        return Ok(None);
    }
    let binding_ref = directory_binding_ref(claims_context, binding);
    let revision = claims
        .reconcile_live_claimants(
            &binding_ref,
            live_directory_claimants(claims_context, binding),
        )
        .map_err(directory_claims_automation_error)?;
    if let Some(revision) = revision {
        publish_directory_usage_changed(
            automation,
            claims,
            binding,
            Some(automation_binding_target(binding, target_context)),
            json!({
                "reason": "topology_reconciled",
                "revision": revision,
            }),
        )?;
    }
    Ok(revision)
}

fn terminal_target_is_retired_binding(
    target: &CommandTarget,
    current_binding_handle: &str,
) -> bool {
    if target.kind != ResourceKind::Terminal {
        return false;
    }
    let Ok([binding_handle, _, _, _, _]) = serde_json::from_str::<[String; 5]>(&target.handle)
    else {
        return false;
    };
    let Ok((process, window, window_generation, space, binding, generation)) =
        serde_json::from_str::<(String, String, u64, String, String, u64)>(&binding_handle)
    else {
        return false;
    };
    let Ok((
        current_process,
        current_window,
        current_window_generation,
        current_space,
        current_binding,
        current_generation,
    )) = serde_json::from_str::<(String, String, u64, String, String, u64)>(current_binding_handle)
    else {
        return false;
    };
    process == current_process
        && window == current_window
        && window_generation == current_window_generation
        && space == current_space
        && binding == current_binding
        && generation != current_generation
}

struct BindingGenerationRetirement {
    scope: String,
    target: CommandTarget,
    retired_generation: u64,
    claims_revision: Option<u64>,
}

fn reconcile_binding_automation_generation(
    automation: &AutomationHub,
    claims: &DirectoryClaims,
    claims_context: &DirectoryClaimsContext,
    binding: &mut BindingRuntime,
    context: &AutomationTargetContext,
) -> Result<Option<BindingGenerationRetirement>, AutomationError> {
    let current_generation = binding.mux.binding_generation();
    let previous_generation = binding.automation_generation;
    if previous_generation == Some(current_generation) {
        return Ok(None);
    }

    let scope = automation_event_scope(binding.scope);
    let current_binding_handle = automation_binding_handle(binding, context);
    automation
        .events()
        .purge_terminal_output(&scope, |target| {
            terminal_target_is_retired_binding(target, &current_binding_handle)
        })?;
    let current_binding = directory_binding_ref(claims_context, binding);
    let claims_revision = claims
        .rebase_retired_binding_claims(
            &current_binding,
            live_directory_claimants(claims_context, binding),
        )
        .map_err(directory_claims_automation_error)?;
    binding.automation_generation = Some(current_generation);

    Ok(
        previous_generation.map(|retired_generation| BindingGenerationRetirement {
            scope,
            target: automation_binding_target(binding, context),
            retired_generation,
            claims_revision,
        }),
    )
}

/// The controller's cached topology is the UI's authoritative binding state.
/// Backend events may be deltas, so topology snapshots deliberately carry this
/// complete source view rather than re-labeling a delta as bootstrap state.
fn automation_binding_snapshot(
    binding: &BindingRuntime,
    context: &AutomationTargetContext,
) -> Value {
    json!({
        "scope": automation_event_scope(binding.scope),
        "binding": automation_binding_target(binding, context),
        "sessions": binding.mux.sessions(),
        "selected_session": binding.mux.selected_session(),
        "selected_window": binding.mux.selected_window(),
    })
}

fn install_binding_automation_sources(
    automation: &AutomationHub,
    claims: &DirectoryClaims,
    binding: &mut BindingRuntime,
    context: &AutomationTargetContext,
    force: bool,
) -> Result<(), AutomationError> {
    let first_install = !binding.automation_sources_installed;
    if !first_install && !force {
        return Ok(());
    }
    rebase_automation_terminal_states(binding);
    rebase_automation_backend_states(binding);
    let scope = automation_event_scope(binding.scope);
    let topology = automation_binding_snapshot(binding, context);
    let terminal_snapshots = AUTOMATION_BINDING_SNAPSHOT_TOPICS
        .iter()
        .map(|topic| {
            let snapshot = if AUTOMATION_TERMINAL_STATE_TOPICS.contains(topic) {
                automation_binding_state_snapshot(binding, context, topic)
            } else if AUTOMATION_BACKEND_STATE_TOPICS.contains(topic) {
                automation_binding_backend_snapshot(binding, context, topic)
            } else {
                topology.clone()
            };
            ((*topic).to_owned(), snapshot)
        })
        .collect::<Vec<_>>();
    let result = claims
        .with_live_snapshots(|directory_snapshots| {
            let directory_snapshot = serde_json::to_value(directory_snapshots)
                .map_err(directory_claims_automation_error)?;
            let mut snapshots = terminal_snapshots.clone();
            snapshots.push(("directory.usage_changed".to_owned(), directory_snapshot));
            if first_install {
                // A binding has no repository-wide worktree inventory until a
                // worktree mutation identifies its repository. Do not overwrite that
                // authoritative repository-specific snapshot on later refreshes.
                snapshots.push(("worktree.changed".to_owned(), json!([])));
            }
            automation
                .events()
                .replace_snapshots_with_terminal_output(scope.clone(), snapshots)
        })
        .map_err(directory_claims_automation_error)?;
    result?;
    automation.metadata().install_snapshot(&scope)?;
    binding.automation_sources_installed = true;
    Ok(())
}

fn automation_event_scope(scope: MuxScope) -> String {
    format!(
        "binding:{}:{}",
        scope.space_id().persistence_value(),
        scope.binding_id().persistence_value()
    )
}

fn automation_event_topic(topic: MuxEventTopic) -> &'static str {
    match topic {
        MuxEventTopic::TopologyChanged => "topology.changed",
        MuxEventTopic::TerminalOutput => "terminal.output",
        MuxEventTopic::PaneStateChanged => "terminal.process_changed",
        MuxEventTopic::PaneTitleChanged => "terminal.title_changed",
        MuxEventTopic::PaneOptionsChanged => "terminal.options_changed",
        MuxEventTopic::PaneForegroundChanged => "terminal.foreground_changed",
        MuxEventTopic::PaneCwdChanged => "terminal.cwd_changed",
        MuxEventTopic::PaneOccupantReplaced => "terminal.occupant_replaced",
        MuxEventTopic::PaneClosed => "terminal.closed",
        MuxEventTopic::BackendDisconnected => "backend.connection_changed",
        MuxEventTopic::BackendLagged => "backend.lagged",
        MuxEventTopic::SnapshotRebased => "backend.rebased",
    }
}
struct BindingRuntimeSpec<'a> {
    config: &'a BoottyConfig,
    scope: MuxScope,
    label: String,
    backend_override: Option<MultiplexerBackendConfig>,
    remote_override: SpaceRemoteOverride,
    variant: AppearanceVariant,
    repaint: RepaintHandle,
    register_namespace: bool,
    restore_sessions: bool,
}

fn binding_runtime_for_multiplexer(spec: BindingRuntimeSpec<'_>) -> Result<BindingRuntime> {
    let BindingRuntimeSpec {
        config,
        scope,
        label,
        backend_override,
        remote_override,
        variant,
        repaint,
        register_namespace,
        restore_sessions,
    } = spec;
    let mut binding = BindingRuntime::new_with_backend_override(
        scope,
        config,
        backend_override,
        remote_override,
        variant,
        repaint.clone(),
        register_namespace,
    )?;
    binding.label = label;
    if restore_sessions {
        binding.restore_persisted_sessions(false, &repaint)?;
    }
    Ok(binding)
}

struct SpaceRuntime {
    id: SpaceId,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    position: i64,
    binding: BindingRuntime,
    inactive_bindings: Vec<BindingRuntime>,
}

impl SpaceRuntime {
    fn from_workspace(
        space: &WorkspaceSpace,
        config: &BoottyConfig,
        variant: AppearanceVariant,
        repaint: RepaintHandle,
    ) -> Result<Option<Self>> {
        let mut bindings = space
            .bindings()
            .iter()
            .map(|workspace_binding| {
                let mut runtime = binding_runtime_for_multiplexer(BindingRuntimeSpec {
                    config,
                    scope: workspace_binding.mux_scope(),
                    label: workspace_binding.name().to_owned(),
                    backend_override: workspace_binding.backend_override(),
                    remote_override: workspace_binding.remote_override().clone(),
                    variant,
                    repaint: repaint.clone(),
                    register_namespace: true,
                    restore_sessions: true,
                })?;
                if workspace_binding.unavailable() {
                    runtime.mux.set_availability_error(Some(
                        "binding unavailable; reconnect to restore it".to_owned(),
                    ));
                }
                if let Some(selection) = workspace_binding.selection() {
                    runtime.mux.restore_selection(
                        selection.session_id().to_owned(),
                        selection.window_id().map(str::to_owned),
                    );
                }
                Ok(runtime)
            })
            .collect::<Result<Vec<_>>>()?;
        if bindings.is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            id: space.id(),
            name: space.name().to_owned(),
            icon: space.icon().to_owned(),
            color: space.color(),
            tint_sidebar: space.tint_sidebar(),
            position: space.position(),
            binding: bindings.remove(0),
            inactive_bindings: bindings,
        }))
    }

    fn bindings(&self) -> impl Iterator<Item = &BindingRuntime> {
        std::iter::once(&self.binding).chain(self.inactive_bindings.iter())
    }

    fn bindings_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.binding).chain(self.inactive_bindings.iter_mut())
    }
}

/// A remote binding's attach client is gone and bootty is waiting to start the next one.
///
/// The sessions themselves live on the other host and outlive the connection, so a lost link is
/// reconnected to rather than treated as the pane ending. Attempts back off, because the same loss
/// that ends one client usually ends the next few too, and each attempt is a fresh SSH handshake.
#[derive(Clone, Copy, Debug)]
struct RemoteReattach {
    retry_at: Instant,
    attempts: u32,
    /// Set once the waiting is over and a new attach client has been asked for.
    started: bool,
}

impl RemoteReattach {
    const FIRST_DELAY: Duration = Duration::from_millis(500);
    const MAX_DELAY: Duration = Duration::from_secs(30);
    /// How long an attach client has to survive before its connection counts as established. A
    /// client that dies sooner is the same outage continuing, so the backoff keeps growing.
    const STABLE_AFTER: Duration = Duration::from_secs(5);

    fn after_failure(previous: Option<Self>, attached_for: Option<Duration>, now: Instant) -> Self {
        let established = attached_for.is_some_and(|elapsed| elapsed >= Self::STABLE_AFTER);
        let attempts = match previous {
            Some(previous) if !established => previous.attempts.saturating_add(1),
            _ => 1,
        };
        Self {
            retry_at: now + Self::delay(attempts),
            attempts,
            started: false,
        }
    }

    fn due(self, now: Instant) -> bool {
        !self.started && now >= self.retry_at
    }

    fn delay(attempts: u32) -> Duration {
        Self::FIRST_DELAY
            .saturating_mul(1u32 << attempts.saturating_sub(1).min(8))
            .min(Self::MAX_DELAY)
    }
}

#[derive(Clone, Copy, Debug)]
struct SpaceTransition {
    from: SpaceId,
    to: SpaceId,
    started: Instant,
}

impl SpaceTransition {
    const DURATION: Duration = Duration::from_millis(180);

    fn progress_at(self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / Self::DURATION.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

fn binding_label(scope: MuxScope, multiplexer: &crate::config::MultiplexerConfig) -> String {
    let backend = match multiplexer.backend {
        crate::config::MultiplexerBackendConfig::Rmux => "Rmux",
        crate::config::MultiplexerBackendConfig::Native => "Native",
        crate::config::MultiplexerBackendConfig::Tmux => "Tmux",
        crate::config::MultiplexerBackendConfig::Zellij => "Zellij",
    };
    format!(
        "{backend} / Binding {}",
        scope.binding_id().persistence_value()
    )
}

struct NetworkChangeDetector {
    next_check: Instant,
    signature: Option<IpAddr>,
}

impl NetworkChangeDetector {
    const INTERVAL: Duration = Duration::from_secs(2);

    fn new(now: Instant) -> Self {
        Self {
            next_check: now + Self::INTERVAL,
            signature: network_signature(),
        }
    }

    fn changed(&mut self, now: Instant) -> bool {
        self.changed_to(now, network_signature())
    }

    fn changed_to(&mut self, now: Instant, signature: Option<IpAddr>) -> bool {
        if now < self.next_check {
            return false;
        }
        self.next_check = now + Self::INTERVAL;
        let changed = signature != self.signature;
        self.signature = signature;
        changed
    }
}

fn network_signature() -> Option<IpAddr> {
    let socket = UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("1.1.1.1", 80)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

pub struct AppState {
    window_state_key: String,
    automation: AutomationHub,
    /// Instance-scoped generic Luau runtime. Its command overlay is shared by
    /// CLI/socket/palette/keybinding/Luau callers without process-global state.
    extension_runtime: crate::extensions::ExtensionRuntime,
    directory_claims: DirectoryClaims,
    command_instance_handle: String,
    command_instance_generation: u64,
    command_window_generation: u64,
    binding: BindingRuntime,
    inactive_bindings: Vec<BindingRuntime>,
    active_space_id: SpaceId,
    active_space_name: String,
    active_space_icon: String,
    active_space_color: [u8; 3],
    active_space_tint_sidebar: bool,
    active_space_position: i64,
    inactive_spaces: Vec<SpaceRuntime>,
    space_transition: Option<SpaceTransition>,
    /// Keeps the one live native terminal while a non-native binding is active.
    parked_native_terminal: Option<NativeTerminalOwner>,
    repaint_scheduler: RepaintScheduler,
    network_change_detector: NetworkChangeDetector,
    last_error: Option<String>,
    last_drain: DrainStats,
    last_frame_dt_ms: f32,
    status_metrics: StatusMetrics,
    last_status_metrics_sample: Instant,
    terminal_surface: Option<TerminalSurface>,
    /// The full terminal area the panes were last laid out within, for geometric neighbor lookup.
    last_pane_area: Option<Rect>,
    terminal_view_transform: ViewTransform,
    config_state: ConfigState,
    active_appearance_variant: AppearanceVariant,
    input_focus: InputFocus,
    app_key_bindings: AppKeyBindings,
    sidebar_key_bindings: SidebarKeyBindings,
    has_new_session_config_changes: bool,
    repaint: RepaintHandle,
    direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
    modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    modifier_sides: ModifierSideState,
    pending_direct_input: Vec<DirectKeyInput>,
    suppress_next_egui_paste: bool,
    /// While the settings overlay is open the terminal behind it must receive no input, so the
    /// direct (winit) input path is gated on this just like it is on the modal mux dialogs.
    settings_open: bool,
    /// Mirrors whether a Luau-opened floating window is showing. That window lives on `BoottyApp`
    /// rather than here, so input gating reads this mirror to stop feeding the terminal behind it.
    lua_window_open: bool,
    terminal_selection: TerminalSelectionRouter,
    /// Screen rects of chrome resize handles (sidebar edge, pane dividers) registered during the
    /// previous frame's UI build. A primary press inside one of these must not begin a terminal
    /// text selection — the handle owns that drag. Populated each frame in `show_fixed_layout`.
    chrome_handle_rects: Vec<egui::Rect>,
    wheel_scroll_state: WheelScrollState,
    modifier_remaps: ModifierRemapSet,
    terminal_cursor_icon: egui::CursorIcon,
    mouse_pointer_hidden_while_typing: bool,
    last_mouse_hover_pos: Option<Pos2>,
    macos_option_as_alt: crate::terminal::MacosOptionAsAlt,
    stability_trace: Option<StabilityTrace>,
    config_hot_reload: ConfigHotReload,
    new_mux_session_dialog: Option<NewMuxSessionDialog>,
    sidebar_hovered_session: Option<ScopedSessionTarget>,
    session_picker_dialog: Option<SessionPickerDialog>,
    rename_session_dialog: Option<RenameSessionDialog>,
    rename_tab_dialog: Option<RenameTabDialog>,
    ditch_session_dialog: Option<DitchSessionDialog>,
    keybind_help_dialog: Option<KeybindHelpDialog>,
    command_palette_dialog: Option<CommandPaletteDialog>,
    theme_picker_dialog: Option<ThemePickerDialog>,
    space_editor_dialog: Option<SpaceEditorDialog>,
    terminal_find_dialog: Option<TerminalFindDialog>,
    terminal_find_return_focus_after_search: bool,
    last_terminal_search: String,
    last_terminal_search_direction: TerminalSearchDirection,
    theme_picker_restore_config: Option<BoottyConfig>,
    pending_command: Option<CommandInvocation>,
    pending_app_commands: Vec<PendingAppCommand>,
    pending_extension_commands: Vec<PendingExtensionCommand>,
    pending_completion_publications: VecDeque<PendingCompletionPublication>,
    app_command_tx: AppCommandSender,
    app_command_rx: AppCommandReceiver,
    reconciliation_tx: mpsc::Sender<ShutdownReconciliationCompletion>,
    reconciliation_rx: mpsc::Receiver<ShutdownReconciliationCompletion>,
    macos_non_native_fullscreen_active: bool,
    #[cfg(debug_assertions)]
    diagnostic_action_driver: Option<DiagnosticActionDriver>,
    macos_non_native_fullscreen_pending_apply: bool,
}
impl Drop for AppState {
    fn drop(&mut self) {
        self.app_command_rx.close();
        self.pending_command = None;

        for mut pending in std::mem::take(&mut self.pending_app_commands) {
            if pending.cancellation.is_started() {
                pending.cancellation.cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::completion_indeterminate());
                }
                enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Mux(
                    ShutdownMuxReconciliation {
                        request_id: pending.request_id,
                        command_id: pending.command_id,
                        command: pending.command,
                        origin: pending.origin,
                        binding_identity: pending.binding_identity,
                        binding_generation: pending.binding_generation,
                        namespace: pending.namespace,
                        result: pending.result,
                        deadline: pending
                            .deadline
                            .checked_add(SHUTDOWN_RECONCILIATION_GRACE)
                            .unwrap_or_else(Instant::now),
                        cancellation: pending.cancellation,
                        target: pending.target,
                        completion: pending.completion,
                        reconciliation: self.reconciliation_tx.clone(),
                        automation: self.automation.clone(),
                        scope: automation_event_scope(pending.origin),
                        fallback_scope: format!("instance:{}", self.command_instance_handle),
                    },
                ));
            } else {
                pending.cancellation.cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message: "application command service is shutting down".to_owned(),
                    });
                }
            }
        }

        for mut pending in std::mem::take(&mut self.pending_extension_commands) {
            if pending.cancellation.is_started() {
                pending.cancellation.cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::completion_indeterminate());
                }
                enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Extension(
                    ShutdownExtensionReconciliation {
                        request_id: pending.request_id,
                        command_id: pending.invocation.command.clone(),
                        invocation: pending.invocation,
                        extension_id: pending.extension_id,
                        generation: pending.generation,
                        result: pending.result,
                        deadline: pending
                            .deadline
                            .checked_add(SHUTDOWN_RECONCILIATION_GRACE)
                            .unwrap_or_else(Instant::now),
                        cancellation: pending.cancellation,
                        target: pending.target,
                        completion: pending.completion,
                        reconciliation: self.reconciliation_tx.clone(),
                        automation: self.automation.clone(),
                        scope: format!("instance:{}", self.command_instance_handle),
                        fallback_scope: format!("instance:{}", self.command_instance_handle),
                    },
                ));
            } else {
                pending.cancellation.cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message: "application command service is shutting down".to_owned(),
                    });
                }
            }
        }

        for pending in std::mem::take(&mut self.pending_completion_publications) {
            enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Publication(pending));
        }
        self.automation.cancel_all_tasks();

        let window = WindowRef {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let scopes = self
            .binding_runtimes()
            .map(|binding| automation_event_scope(binding.scope))
            .collect();
        enqueue_window_claim_release(
            self.directory_claims.clone(),
            window,
            self.automation.clone(),
            scopes,
        );
    }
}

fn terminal_session_config_with_side_effects(
    config: &BoottyConfig,
    variant: AppearanceVariant,
    side_effect_tx: &mpsc::Sender<TerminalSideEffectEvent>,
) -> TerminalSessionConfig {
    let mut session_config = config.terminal_session_config();
    session_config.colors = config
        .colors_for_appearance(variant)
        .terminal_color_config();
    session_config.side_effect_tx = Some(side_effect_tx.clone());
    session_config
}

fn remove_first_paste_event(events: &mut Vec<egui::Event>) -> bool {
    if let Some(index) = events
        .iter()
        .position(|event| matches!(event, egui::Event::Paste(_)))
    {
        events.remove(index);
        true
    } else {
        false
    }
}

fn route_find_modeless_events(
    focus: InputFocus,
    events: Vec<egui::Event>,
    find_rect: Option<egui::Rect>,
    hover_pos: Option<Pos2>,
) -> RoutedInput {
    let Some(find_rect) = find_rect else {
        return route_events(focus, events);
    };

    let mut routed = RoutedInput::default();
    for event in events {
        let inside_find = event_pointer_pos(&event)
            .or(hover_pos.filter(|_| matches!(event, egui::Event::MouseWheel { .. })))
            .is_some_and(|pos| find_rect.contains(pos));
        if inside_find {
            routed.ui_events.push(event);
        } else if focus.terminal_owns_input() || event_is_terminal_pointer(&event) {
            routed.terminal_events.push(event);
        } else {
            routed.ui_events.push(event);
        }
    }
    routed
}

fn event_pointer_pos(event: &egui::Event) -> Option<Pos2> {
    match event {
        egui::Event::PointerMoved(pos) => Some(*pos),
        egui::Event::PointerButton { pos, .. } => Some(*pos),
        _ => None,
    }
}

fn event_is_terminal_pointer(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::PointerMoved(_)
            | egui::Event::PointerButton { .. }
            | egui::Event::MouseWheel { .. }
    )
}

fn layout_direction(direction: crate::mux::command::MuxDirection) -> Direction {
    use crate::mux::command::MuxDirection;
    match direction {
        MuxDirection::Left => Direction::Left,
        MuxDirection::Right => Direction::Right,
        MuxDirection::Up => Direction::Up,
        MuxDirection::Down => Direction::Down,
    }
}

fn scoped_terminal_transition_key(
    scope: MuxScope,
    backend: MultiplexerBackendConfig,
    session_id: &str,
    pane_id: Option<&str>,
) -> String {
    format!(
        "{}:{}:{backend:?}:{session_id}:{}",
        scope.space_id().persistence_value(),
        scope.binding_id().persistence_value(),
        pane_id.unwrap_or_default(),
    )
}

fn mux_split_direction(direction: SplitDirection) -> MuxSplitDirection {
    match direction {
        SplitDirection::Right => MuxSplitDirection::Right,
        SplitDirection::Down => MuxSplitDirection::Down,
    }
}

fn pane_sets_match(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|pane| b.contains(pane))
}

fn focus_after_native_layout_reconcile(
    restored_from_server: bool,
    new_panes: &[String],
    selected_pane: Option<&str>,
) -> Option<String> {
    if restored_from_server {
        return selected_pane.map(str::to_owned);
    }
    if let Some(selected_pane) = selected_pane
        && new_panes.iter().any(|pane| pane == selected_pane)
    {
        return Some(selected_pane.to_owned());
    }
    new_panes.first().cloned()
}

fn terminal_cursor_icon_for_mouse_shape(shape: &str) -> Option<egui::CursorIcon> {
    let normalized = shape.to_ascii_lowercase().replace('_', "-");
    for token in normalized
        .split([';', ',', ':', '=', ' '])
        .filter(|token| !token.is_empty())
    {
        let icon = match token {
            "default" | "reset" | "arrow" => egui::CursorIcon::Default,
            "none" | "hidden" => egui::CursorIcon::None,
            "pointer" | "hand" | "pointing-hand" => egui::CursorIcon::PointingHand,
            "text" | "ibeam" | "i-beam" => egui::CursorIcon::Text,
            "vertical-text" => egui::CursorIcon::VerticalText,
            "crosshair" => egui::CursorIcon::Crosshair,
            "help" => egui::CursorIcon::Help,
            "wait" => egui::CursorIcon::Wait,
            "progress" => egui::CursorIcon::Progress,
            "cell" => egui::CursorIcon::Cell,
            "copy" => egui::CursorIcon::Copy,
            "alias" => egui::CursorIcon::Alias,
            "move" => egui::CursorIcon::Move,
            "no-drop" => egui::CursorIcon::NoDrop,
            "not-allowed" | "forbidden" => egui::CursorIcon::NotAllowed,
            "grab" => egui::CursorIcon::Grab,
            "grabbing" => egui::CursorIcon::Grabbing,
            "all-scroll" => egui::CursorIcon::AllScroll,
            "ew-resize" | "col-resize" | "resize-horizontal" => egui::CursorIcon::ResizeHorizontal,
            "ns-resize" | "row-resize" | "resize-vertical" => egui::CursorIcon::ResizeVertical,
            "nesw-resize" | "resize-nesw" => egui::CursorIcon::ResizeNeSw,
            "nwse-resize" | "resize-nwse" => egui::CursorIcon::ResizeNwSe,
            "e-resize" | "resize-east" => egui::CursorIcon::ResizeEast,
            "s-resize" | "resize-south" => egui::CursorIcon::ResizeSouth,
            "w-resize" | "resize-west" => egui::CursorIcon::ResizeWest,
            "n-resize" | "resize-north" => egui::CursorIcon::ResizeNorth,
            "ne-resize" | "resize-north-east" => egui::CursorIcon::ResizeNorthEast,
            "nw-resize" | "resize-north-west" => egui::CursorIcon::ResizeNorthWest,
            "se-resize" | "resize-south-east" => egui::CursorIcon::ResizeSouthEast,
            "sw-resize" | "resize-south-west" => egui::CursorIcon::ResizeSouthWest,
            "zoom-in" => egui::CursorIcon::ZoomIn,
            "zoom-out" => egui::CursorIcon::ZoomOut,
            _ => continue,
        };
        return Some(icon);
    }
    None
}
fn terminal_report_variable_response(name: &str, session_name: Option<&str>) -> Option<Vec<u8>> {
    match name {
        "session.name" => session_name.map(encode_iterm2_report_variable),
        _ => None,
    }
}

fn new_mux_session_request_with_name(
    config: &BoottyConfig,
    name: impl Into<String>,
) -> crate::ui::new_session_picker::NewMuxSessionRequest {
    let cwd = config
        .session
        .working_directory
        .clone()
        .or_else(crate::config::default_working_directory)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| {
            config
                .config_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_owned()
        });
    crate::ui::new_session_picker::NewMuxSessionRequest {
        session_id: name.into(),
        cwd: cwd.to_string_lossy().into_owned(),
    }
}

fn terminal_cwd_for_mux_command(
    live_terminal_cwd: Option<String>,
    anchor_cwd: Option<String>,
) -> Option<String> {
    live_terminal_cwd
        .and_then(|cwd| normalize_terminal_cwd(&cwd))
        .or(anchor_cwd)
}

fn normalize_terminal_cwd(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    if let Some(path) = cwd.strip_prefix("file://") {
        let path_start = path.find('/')?;
        let path = &path[path_start..];
        return percent_decode(path);
    }
    Some(cwd.to_owned())
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = hex_value(*bytes.get(index + 1)?)?;
            let lo = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

impl AppState {
    pub fn new(
        config: BoottyConfig,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window(
            config,
            PRIMARY_WINDOW_STATE_KEY.to_owned(),
            repaint,
            direct_input_rx,
            modifier_side_rx,
        )
    }

    pub fn new_for_window(
        config: BoottyConfig,
        window_state_key: String,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
    ) -> Result<Self> {
        Self::new_for_window_with_automation(
            config,
            window_state_key,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            AutomationHub::new(),
        )
    }

    pub fn new_for_window_with_automation(
        config: BoottyConfig,
        window_state_key: String,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
        automation: AutomationHub,
    ) -> Result<Self> {
        Self::new_for_window_with_automation_and_instance(
            config,
            window_state_key,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            automation,
            InstanceRef {
                instance_id: process_command_handle(),
                generation: 1,
            },
        )
    }

    pub(crate) fn new_for_window_with_automation_and_instance(
        config: BoottyConfig,
        window_state_key: String,
        repaint: RepaintHandle,
        direct_input_rx: Option<mpsc::Receiver<DirectKeyInput>>,
        modifier_side_rx: Option<mpsc::Receiver<ModifierSideState>>,
        automation: AutomationHub,
        command_instance: InstanceRef,
    ) -> Result<Self> {
        let workspace = WorkspaceStore::try_for_config_path(&config.config_path)?;
        let selected_space_id = workspace.selected_space(&window_state_key).ok().flatten();
        let modifier_remaps = config.input.modifier_remaps()?;
        let macos_option_as_alt = config.input.macos_option_as_alt.into();
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let stability_trace = StabilityTrace::from_config(&config);
        let active_appearance_variant = config.appearance.mode.variant(AppearanceVariant::Dark);
        let mut spaces = workspace
            .spaces()
            .iter()
            .map(|space| {
                SpaceRuntime::from_workspace(
                    space,
                    &config,
                    active_appearance_variant,
                    repaint.clone(),
                )
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if spaces.is_empty() {
            spaces.push(SpaceRuntime {
                id: SpaceId::from_persistence(0),
                name: "Default Space".to_owned(),
                icon: crate::workspace::DEFAULT_SPACE_ICON.to_owned(),
                color: crate::workspace::DEFAULT_SPACE_COLOR,
                tint_sidebar: false,
                position: 0,
                binding: BindingRuntime::new(
                    MuxScope::new(SpaceId::from_persistence(0), BindingId::from_persistence(0)),
                    &config,
                    active_appearance_variant,
                    repaint.clone(),
                )?,
                inactive_bindings: Vec::new(),
            });
        }
        let active_index = selected_space_id
            .and_then(|id| spaces.iter().position(|space| space.id == id))
            .unwrap_or(0);
        let active_space = spaces.remove(active_index);
        let SpaceRuntime {
            id: active_space_id,
            name: active_space_name,
            icon: active_space_icon,
            color: active_space_color,
            tint_sidebar: active_space_tint_sidebar,
            position: active_space_position,
            binding,
            inactive_bindings,
        } = active_space;
        workspace.set_selected_space(&window_state_key, active_space_id)?;
        let inactive_spaces = spaces;
        let keybinds = config
            .input
            .keybinds_for_backend(binding.multiplexer.backend);
        let app_key_bindings = AppKeyBindings::from_keybinds(&keybinds)?;
        let config_hot_reload = ConfigHotReload::new(&config.config_path);
        let macos_non_native_fullscreen_active = config.window.non_native_fullscreen_enabled();
        let macos_non_native_fullscreen_applied =
            apply_macos_non_native_fullscreen_presentation(&config.window);
        let macos_non_native_fullscreen_pending_apply =
            macos_non_native_fullscreen_active && !macos_non_native_fullscreen_applied;
        #[cfg(debug_assertions)]
        let diagnostic_action_driver = DiagnosticActionDriver::from_env();
        let (reconciliation_tx, reconciliation_rx) =
            mpsc::channel::<ShutdownReconciliationCompletion>();
        let (app_command_tx, app_command_rx) =
            app_command_channel_with_repaint(64, repaint.clone());
        let command_instance_handle = command_instance.instance_id;
        let command_instance_generation = command_instance.generation;
        let command_window_generation = next_window_command_generation();
        let directory_claims = process_directory_claims(&command_instance_handle)?;
        let extension_runtime = crate::extensions::ExtensionRuntime::new(automation.clone());
        if let Some(config_dir) = config.config_path.parent() {
            let _ = extension_runtime.set_storage_root(config_dir.join("extensions"));
        }

        let mut state = Self {
            window_state_key,
            automation,
            extension_runtime,
            directory_claims,
            command_instance_handle,
            command_instance_generation,
            command_window_generation,
            binding,
            inactive_bindings,
            active_space_id,
            active_space_name,
            active_space_icon,
            active_space_color,
            active_space_tint_sidebar,
            active_space_position,
            inactive_spaces,
            space_transition: None,
            parked_native_terminal: None,
            repaint_scheduler: RepaintScheduler::default(),
            network_change_detector: NetworkChangeDetector::new(Instant::now()),
            last_error: None,
            last_drain: DrainStats::default(),
            last_frame_dt_ms: 0.0,
            status_metrics: StatusMetrics::default(),
            last_status_metrics_sample: Instant::now() - STATUS_METRICS_SAMPLE_INTERVAL,
            terminal_surface: None,
            last_pane_area: None,
            chrome_handle_rects: Vec::new(),
            terminal_view_transform: ViewTransform::IDENTITY,
            config_state: ConfigState::new(config),
            active_appearance_variant,
            input_focus: InputFocus::Terminal,
            app_key_bindings,
            sidebar_key_bindings,
            has_new_session_config_changes: false,
            repaint,
            direct_input_rx,
            modifier_side_rx,
            modifier_sides: ModifierSideState::default(),
            pending_direct_input: Vec::new(),
            suppress_next_egui_paste: false,
            settings_open: false,
            lua_window_open: false,
            terminal_selection: TerminalSelectionRouter::default(),
            wheel_scroll_state: WheelScrollState::default(),
            modifier_remaps,
            terminal_cursor_icon: egui::CursorIcon::Text,
            mouse_pointer_hidden_while_typing: false,
            last_mouse_hover_pos: None,
            macos_option_as_alt,
            macos_non_native_fullscreen_active,
            stability_trace,
            config_hot_reload,
            new_mux_session_dialog: None,
            sidebar_hovered_session: None,
            session_picker_dialog: None,
            rename_session_dialog: None,
            rename_tab_dialog: None,
            command_palette_dialog: None,
            theme_picker_dialog: None,
            space_editor_dialog: None,
            terminal_find_dialog: None,
            terminal_find_return_focus_after_search: false,
            last_terminal_search: String::new(),
            last_terminal_search_direction: TerminalSearchDirection::Next,
            theme_picker_restore_config: None,
            pending_command: None,
            pending_app_commands: Vec::new(),
            pending_extension_commands: Vec::new(),
            pending_completion_publications: VecDeque::new(),
            app_command_tx,
            app_command_rx,
            reconciliation_tx,
            reconciliation_rx,
            ditch_session_dialog: None,
            keybind_help_dialog: None,
            #[cfg(debug_assertions)]
            diagnostic_action_driver,
            macos_non_native_fullscreen_pending_apply,
        };
        let bundled_extensions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("extensions");
        if let Err(error) = state
            .extension_runtime
            .discover_and_load(&bundled_extensions)
        {
            state.last_error = Some(format!("bundled extension startup failed: {error}"));
        }
        if let Some(config_dir) = state.config().config_path.parent()
            && let Err(error) = state
                .extension_runtime
                .discover_and_load(&config_dir.join("extensions"))
        {
            state.last_error = Some(format!("extension startup failed: {error}"));
        }
        state.initialize_automation_event_sources()?;
        state.record_restored_persisted_launch_claims();
        Ok(state)
    }

    /// Installs authoritative bootstrap state for every live binding before
    /// the owner-local control server can authorize subscriptions.
    pub fn initialize_automation_event_sources(&mut self) -> Result<(), AutomationError> {
        self.refresh_automation_event_sources(true)
    }

    fn synchronize_live_binding_event_scopes(&self) {
        self.automation.events().replace_live_binding_scopes(
            self.binding_runtimes()
                .map(|binding| automation_event_scope(binding.scope)),
        );
    }

    /// Replaces binding source snapshots only when a source is new, an
    /// authoritative refresh occurred, or reconnect advanced its generation.
    fn refresh_automation_event_sources(&mut self, force: bool) -> Result<(), AutomationError> {
        self.synchronize_live_binding_event_scopes();
        let automation = self.automation.clone();
        let target_context = AutomationTargetContext {
            process: self.command_instance_handle.clone(),
            window_state_key: self.window_state_key.clone(),
            window_generation: self.command_window_generation,
        };
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let claims = self.directory_claims.clone();
        let mut retirements = Vec::new();

        for binding in self.binding_runtimes_mut() {
            if let Some(retirement) = reconcile_binding_automation_generation(
                &automation,
                &claims,
                &claims_context,
                binding,
                &target_context,
            )? {
                retirements.push(retirement);
            }
        }

        let needs_source_refresh = force
            || !retirements.is_empty()
            || self
                .binding_runtimes()
                .any(|binding| !binding.automation_sources_installed);
        if !needs_source_refresh {
            return Ok(());
        }

        let install_force = force || !retirements.is_empty();
        for binding in self.binding_runtimes_mut() {
            install_binding_automation_sources(
                &automation,
                &claims,
                binding,
                &target_context,
                install_force,
            )?;
        }
        for retirement in retirements {
            publish_directory_usage_changed_for_scope(
                &automation,
                &claims,
                retirement.scope,
                Some(retirement.target.clone()),
                json!({
                    "reason": "binding_generation_retired",
                    "retired_binding_generation": retirement.retired_generation,
                    "binding_generation": retirement.target.generation,
                    "revision": retirement.claims_revision,
                }),
            )?;
        }
        Ok(())
    }

    pub fn config(&self) -> &BoottyConfig {
        self.config_state.current()
    }

    pub fn automation_hub(&self) -> AutomationHub {
        self.automation.clone()
    }
    pub fn extension_command_registry(&self) -> crate::commands::CommandRegistry {
        self.extension_runtime.command_registry()
    }

    fn publish_reconciled_command_completion(
        &mut self,
        request_id: u64,
        command_id: &str,
        target: Option<&CommandTarget>,
        completion: Option<&CommandCompletionContext>,
        outcome: &CommandOutcome,
    ) {
        self.publish_command_completion_event(
            request_id, command_id, target, completion, outcome, true,
        );
    }

    fn queue_completion_publication(&mut self, request_id: u64, publication: EventPublication) {
        if let Some(existing) = self
            .pending_completion_publications
            .iter_mut()
            .find(|pending| pending.request_id == request_id)
        {
            existing.publication = publication;
            existing.fallback_scope = format!("instance:{}", self.command_instance_handle);
            existing.attempts = 0;
            existing.next_attempt_at = Instant::now();
            return;
        }
        if self.pending_completion_publications.len() >= COMPLETION_PUBLICATION_QUEUE_LIMIT {
            eprintln!(
                "completion publication queue exhausted for request {request_id}; \
                 handing publication to the reconciliation worker"
            );
            enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Publication(
                PendingCompletionPublication {
                    request_id,
                    publication,
                    automation: self.automation.clone(),
                    fallback_scope: format!("instance:{}", self.command_instance_handle),
                    attempts: COMPLETION_PUBLICATION_RETRY_LIMIT as u32,
                    next_attempt_at: Instant::now(),
                },
            ));
            return;
        }
        self.pending_completion_publications
            .push_back(PendingCompletionPublication {
                request_id,
                publication,
                automation: self.automation.clone(),
                fallback_scope: format!("instance:{}", self.command_instance_handle),
                attempts: 0,
                next_attempt_at: Instant::now(),
            });
    }

    fn retry_pending_completion_publications(&mut self) {
        let attempts = self.pending_completion_publications.len();
        for _ in 0..attempts {
            let Some(mut pending) = self.pending_completion_publications.pop_front() else {
                break;
            };
            if Instant::now() < pending.next_attempt_at {
                self.pending_completion_publications.push_back(pending);
                continue;
            }
            match pending
                .automation
                .publish_event(pending.publication.clone())
            {
                Ok(_) => {}
                Err(error) if publication_error_is_oversized(&error) => {
                    pending.publication = bounded_completion_publication(&pending, &error);
                    pending.attempts = 0;
                    pending.next_attempt_at = Instant::now();
                    self.pending_completion_publications.push_back(pending);
                }
                Err(error)
                    if publication_error_is_retired_scope(&error)
                        && pending.publication.scope != pending.fallback_scope =>
                {
                    pending.publication.scope = pending.fallback_scope.clone();
                    pending.attempts = 0;
                    pending.next_attempt_at = Instant::now();
                    self.pending_completion_publications.push_back(pending);
                }
                Err(error) => {
                    pending.attempts = pending.attempts.saturating_add(1);
                    pending.next_attempt_at =
                        Instant::now() + completion_publication_retry_delay(pending.attempts);
                    eprintln!(
                        "completion publication retry {} failed for request {}: {error}",
                        pending.attempts, pending.request_id
                    );
                    if pending.attempts >= COMPLETION_PUBLICATION_RETRY_LIMIT as u32 {
                        eprintln!(
                            "completion publication retry limit reached for request {}; \
                             handing publication to the reconciliation worker",
                            pending.request_id
                        );
                        enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Publication(
                            pending,
                        ));
                    } else {
                        self.pending_completion_publications.push_back(pending);
                    }
                }
            }
        }
    }

    fn publish_command_completion_event(
        &mut self,
        request_id: u64,
        command_id: &str,
        target: Option<&CommandTarget>,
        completion: Option<&CommandCompletionContext>,
        outcome: &CommandOutcome,
        reconciled: bool,
    ) {
        let target = completion
            .and_then(|completion| completion.target.as_ref())
            .or(target);
        let outcome = bounded_command_outcome(outcome.clone());
        let outcome = serde_json::to_value(&outcome).unwrap_or_else(|error| {
            json!({
                "status": "failed",
                "code": "result_too_large",
                "message": error.to_string(),
            })
        });
        let publication = EventPublication::new(
            format!("instance:{}", self.command_instance_handle),
            "command.completed",
            json!({
                "source": "app_state",
                "reconciled": reconciled,
                "request_id": request_id,
                "caller": completion.map(|completion| completion.caller),
                "owner_pid": completion.map(|completion| completion.owner_pid),
                "owner_generation": completion.map(|completion| completion.owner_generation),
            }),
            target.cloned(),
            json!({
                "command": command_id,
                "request_id": request_id,
                "reconciled": reconciled,
                "target": target,
                "outcome": outcome,
            }),
        );
        if let Err(error) = self.automation.publish_event(publication.clone()) {
            eprintln!("command completion publication failed for request {request_id}: {error}");
            self.queue_completion_publication(request_id, publication);
        }
    }
    fn completion_target_for_invocation(
        &self,
        invocation: &CommandInvocation,
    ) -> Option<CommandTarget> {
        let resolved = self
            .extension_runtime
            .command_registry()
            .resolve(invocation.clone())
            .ok()?;
        self.resolve_command_target(
            resolved.descriptor.id.as_str(),
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        )
        .ok()
        .flatten()
        .map(|target| target.target)
    }

    pub fn publish_automation_event(
        &self,
        publication: EventPublication,
    ) -> Result<u64, AutomationError> {
        self.automation.publish_event(publication)
    }

    pub fn publish_automation_event_with_snapshot(
        &self,
        publication: EventPublication,
        snapshot: Value,
    ) -> Result<u64, AutomationError> {
        self.automation
            .publish_event_with_snapshot(publication, snapshot)
    }

    pub fn drain_automation_events(
        &self,
        events: impl IntoIterator<Item = EventPublication>,
    ) -> Result<usize, AutomationError> {
        let mut drained = 0;
        for event in events {
            self.publish_automation_event(event)?;
            drained += 1;
        }
        Ok(drained)
    }

    pub fn publish_terminal_output(
        &self,
        scope: impl Into<String>,
        provenance: Value,
        target: CommandTarget,
        payload: Value,
    ) -> Result<u64, AutomationError> {
        self.automation
            .publish_terminal_output(scope, provenance, target, payload)
    }

    pub fn terminal_output_after(
        &self,
        scope: &str,
        target: &CommandTarget,
        cursor: u64,
    ) -> Result<TerminalOutputRead, AutomationError> {
        self.automation.terminal_output_after(scope, target, cursor)
    }

    /// Return the independent worktree executor sharing this AppState's
    /// authoritative directory-claims store. The executor never creates or
    /// closes a session.
    pub(crate) fn worktree_service(&self) -> crate::git::WorktreeService {
        crate::git::WorktreeService::new(
            self.directory_claims.clone(),
            InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
        )
    }

    pub(crate) fn directory_session_ref(&self, session_id: &str) -> SessionRef {
        let context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        SessionRef {
            binding: directory_binding_ref(&context, &self.binding),
            session_id: session_id.to_owned(),
        }
    }

    /// Record immutable local launch-directory claims only after the backend
    /// has supplied exact allocated resources and the controller has recorded
    /// their current generations. This is invoked by the authoritative command
    /// completion path, never by a speculative request.
    pub(crate) fn record_authoritative_directory_claims(
        &mut self,
        origin: MuxScope,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
        outcome: &mut CommandOutcome,
    ) {
        let requested_cwds_by_window = match command {
            MuxCommand::CreateSession { plan } => plan
                .windows
                .iter()
                .map(|window| {
                    let mut cwds = Vec::new();
                    collect_launch_cwds(&window.layout, &mut cwds);
                    cwds
                })
                .collect::<Vec<_>>(),
            MuxCommand::CreateProjectSession { cwd, .. }
            | MuxCommand::CreateWorktreeSession { cwd, .. } => vec![vec![cwd.as_str()]],
            _ => return,
        };
        let Some(binding) = self.binding_runtime(origin) else {
            append_directory_claim_outcome(
                outcome,
                vec![CommandWarning {
                    code: "directory_claims_unavailable".to_owned(),
                    message: "the completion binding is unavailable".to_owned(),
                }],
                Vec::new(),
            );
            return;
        };
        if binding.multiplexer.remote.is_some() {
            return;
        }
        let Some(allocated) = completion.allocated() else {
            return;
        };

        let mut outcome_warnings = Vec::new();
        let mut structured_warnings = Vec::new();
        let counts_match = requested_cwds_by_window.len() == allocated.windows.len()
            && requested_cwds_by_window
                .iter()
                .zip(&allocated.windows)
                .all(|(cwds, ids)| cwds.len() == ids.pane_ids.len());
        if !counts_match {
            outcome_warnings.push(CommandWarning {
                code: "directory_claims_unavailable".to_owned(),
                message: "backend allocation did not preserve the launch pane topology".to_owned(),
            });
            append_directory_claim_outcome(outcome, outcome_warnings, structured_warnings);
            return;
        }

        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let mut updates = Vec::new();
        for (cwds, allocated_window) in requested_cwds_by_window.iter().zip(&allocated.windows) {
            for (pane_id, &cwd) in allocated_window.pane_ids.iter().zip(cwds.iter()) {
                let Some(terminal_id) = binding.mux.terminal_id_for_pane(
                    &allocated.session_id,
                    &allocated_window.window_id,
                    pane_id,
                ) else {
                    outcome_warnings.push(CommandWarning {
                        code: "directory_claims_unavailable".to_owned(),
                        message: format!(
                            "backend did not expose a terminal identity for pane {pane_id:?}"
                        ),
                    });
                    continue;
                };
                let Some(claimant) = directory_claimant_for_pane(
                    &claims_context,
                    binding,
                    &allocated.session_id,
                    &allocated_window.window_id,
                    pane_id,
                    terminal_id,
                ) else {
                    outcome_warnings.push(CommandWarning {
                        code: "directory_claims_unavailable".to_owned(),
                        message: format!(
                            "backend did not expose an occupant generation for pane {pane_id:?}"
                        ),
                    });
                    continue;
                };
                let directory = match DirectoryRef::resolve(cwd) {
                    Ok(directory) => directory,
                    Err(error) => {
                        outcome_warnings.push(CommandWarning {
                            code: "directory_unresolved".to_owned(),
                            message: format!("could not resolve launch directory {cwd:?}: {error}"),
                        });
                        continue;
                    }
                };
                match self.directory_claims.record_launch(claimant, directory) {
                    Ok(update) => {
                        if let Some(warning) = &update.warning {
                            structured_warnings.push(
                                serde_json::to_value(warning)
                                    .expect("serialize directory claim warning"),
                            );
                            outcome_warnings.push(CommandWarning {
                                code: directory_claim_warning_code(warning.severity).to_owned(),
                                message: directory_claim_warning_message(warning.severity)
                                    .to_owned(),
                            });
                        }
                        updates.push(update);
                    }
                    Err(error) => outcome_warnings.push(CommandWarning {
                        code: "directory_claims_unavailable".to_owned(),
                        message: error.to_string(),
                    }),
                }
            }
        }

        if !updates.is_empty()
            && let Err(error) = publish_directory_usage_changed(
                &self.automation,
                &self.directory_claims,
                binding,
                None,
                json!({
                    "reason": "launch",
                    "updates": updates,
                }),
            )
        {
            outcome_warnings.push(CommandWarning {
                code: "directory_event_failed".to_owned(),
                message: error.to_string(),
            });
        }
        append_directory_claim_outcome(outcome, outcome_warnings, structured_warnings);
    }

    /// Persisted sessions use an idempotent restore path rather than an
    /// ordinary create command. Once that path exposes a concrete local pane
    /// identity, record the same immutable launch claim that an authoritative
    /// command completion would record.
    fn record_restored_persisted_launch_claims(&mut self) {
        let context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let mut resolved = Vec::new();
        for binding in self.binding_runtimes_mut() {
            let scope = binding.scope;
            resolved.extend(
                binding
                    .take_resolved_persisted_session_launch_claims(&context)
                    .into_iter()
                    .map(|claim| (scope, claim)),
            );
        }

        let mut updates_by_scope = Vec::<(MuxScope, Vec<Value>)>::new();
        for (scope, claim) in resolved {
            match self
                .directory_claims
                .record_launch(claim.claimant, claim.directory)
            {
                Ok(update) => {
                    if let Some((_, updates)) = updates_by_scope
                        .iter_mut()
                        .find(|(candidate, _)| *candidate == scope)
                    {
                        updates.push(
                            serde_json::to_value(update)
                                .expect("serialize restored launch claim update"),
                        );
                    } else {
                        updates_by_scope.push((
                            scope,
                            vec![
                                serde_json::to_value(update)
                                    .expect("serialize restored launch claim update"),
                            ],
                        ));
                    }
                }
                Err(error) => {
                    if let Some(binding) = self.binding_runtime_mut(scope) {
                        binding
                            .pending_persisted_session_launches
                            .push(claim.launch);
                    }
                    self.last_error = Some(format!("restored session directory claim: {error}"));
                }
            }
        }

        for (scope, updates) in updates_by_scope {
            let event_result = self.binding_runtime(scope).map_or_else(
                || {
                    Err(AutomationError::new(
                        -32602,
                        "restored session binding is unavailable",
                    ))
                },
                |binding| {
                    publish_directory_usage_changed(
                        &self.automation,
                        &self.directory_claims,
                        binding,
                        None,
                        json!({
                            "reason": "persisted_launch",
                            "updates": updates,
                        }),
                    )
                },
            );
            if let Err(error) = event_result {
                self.last_error = Some(format!("restored session directory event: {error}"));
            }
        }
    }

    /// Capture backend observations before polling. Every binding visited here
    /// is also eligible for an authoritative refresh later in this frame.
    fn collect_backend_automation_events(&mut self) -> usize {
        self.binding_runtimes_mut()
            .map(collect_binding_automation_events)
            .sum()
    }

    fn publish_backend_automation_events(&mut self) -> Result<usize, AutomationError> {
        let automation = self.automation.clone();
        let claims = self.directory_claims.clone();
        let target_context = AutomationTargetContext {
            process: self.command_instance_handle.clone(),
            window_state_key: self.window_state_key.clone(),
            window_generation: self.command_window_generation,
        };
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let mut published = automation.reap_expired_metadata()?;
        for binding in self.binding_runtimes_mut() {
            published += publish_pending_binding_automation_events(
                binding,
                &automation,
                &target_context,
                &claims,
                &claims_context,
            )?;
        }
        Ok(published)
    }

    fn refresh_inactive_bindings_for_frame(&mut self) -> (bool, bool, Vec<MuxScope>) {
        let active_scope = self.binding.scope;
        let repaint = self.repaint.clone();
        let mut any_refresh_completed = false;
        let mut persisted_sessions_restored = false;
        let mut refreshed_event_scopes = Vec::new();

        for binding in self
            .binding_runtimes_mut()
            .filter(|binding| binding.scope != active_scope)
        {
            if binding.automation_event_refresh_pending {
                let config = binding.multiplexer.clone();
                if binding.mux.refresh_sessions(&repaint, &config).is_some() {
                    binding.mux.refresh_on_next_frame();
                }
            }
            let refresh_completed = binding.mux.take_refresh_completed();
            any_refresh_completed |= refresh_completed;
            if refresh_completed && binding.automation_event_refresh_pending {
                refreshed_event_scopes.push(binding.scope);
            }
            let restored = match binding.restore_persisted_sessions(refresh_completed, &repaint) {
                Ok(restored) => restored,
                Err(error) => {
                    binding.mux.set_error(Some(error.to_string()));
                    false
                }
            };
            persisted_sessions_restored |= restored;
        }

        (
            any_refresh_completed,
            persisted_sessions_restored,
            refreshed_event_scopes,
        )
    }

    fn reconcile_refreshed_binding_automation_events(
        &mut self,
        refreshed_scopes: &[MuxScope],
    ) -> Result<(), AutomationError> {
        let automation = self.automation.clone();
        let claims = self.directory_claims.clone();
        let target_context = AutomationTargetContext {
            process: self.command_instance_handle.clone(),
            window_state_key: self.window_state_key.clone(),
            window_generation: self.command_window_generation,
        };
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: self.command_instance_handle.clone(),
                generation: self.command_instance_generation,
            },
            window_id: self.window_state_key.clone(),
        };
        let mut first_error = None;

        for &scope in refreshed_scopes {
            let Some(result) = self
                .binding_runtime(scope)
                .filter(|binding| binding.automation_event_refresh_pending)
                .map(|binding| {
                    reconcile_directory_claims_after_authoritative_refresh(
                        &claims,
                        &claims_context,
                        &automation,
                        binding,
                        &target_context,
                    )
                })
            else {
                continue;
            };
            match result {
                Ok(_) => {
                    if let Some(binding) = self.binding_runtime_mut(scope) {
                        binding.automation_event_refresh_pending = false;
                    }
                }
                Err(error) => {
                    self.retry_binding_automation_event_refresh(scope);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }

        first_error.map_or(Ok(()), Err)
    }

    fn retry_binding_automation_event_refresh(&mut self, scope: MuxScope) {
        if let Some(binding) = self.binding_runtime_mut(scope)
            && binding.automation_event_refresh_pending
        {
            binding.mux.refresh_on_next_frame();
        }
    }

    fn prepare_native_terminal_transition(&mut self, target: &mut BindingRuntime) {
        let active_is_native =
            selected_backend(&self.binding.multiplexer) == MultiplexerBackendConfig::Native;
        let target_is_native =
            selected_backend(&target.multiplexer) == MultiplexerBackendConfig::Native;

        match (active_is_native, target_is_native) {
            (true, true) => {
                std::mem::swap(&mut self.binding.terminal, &mut target.terminal);
                std::mem::swap(
                    &mut self.binding.terminal_side_effect_tx,
                    &mut target.terminal_side_effect_tx,
                );
                std::mem::swap(
                    &mut self.binding.terminal_side_effect_rx,
                    &mut target.terminal_side_effect_rx,
                );
            }
            (true, false) => {
                let mut binding_config = self.config().clone();
                binding_config.multiplexer = self.binding.multiplexer.clone();
                let mut replacement = NativeTerminalOwner::new(
                    &binding_config,
                    self.active_appearance_variant,
                    self.repaint.clone(),
                );
                replacement
                    .terminal
                    .set_native_event_backend(NativeBackend::for_workspace(
                        &binding_config.config_path,
                    ));
                let native_terminal =
                    NativeTerminalOwner::replace_binding(&mut self.binding, replacement);
                debug_assert!(self.parked_native_terminal.is_none());
                self.parked_native_terminal = Some(native_terminal);
            }
            (false, true) => {
                if let Some(mut native_terminal) = self.parked_native_terminal.take() {
                    native_terminal.swap_with_binding(target);
                }
            }
            (false, false) => {}
        }
    }

    /// Apply a dragged sidebar width to the live config without touching disk, so the layout
    /// tracks the pointer each frame. [`Self::persist_sidebar_width`] writes the final value.
    pub fn set_sidebar_width_live(&mut self, width: f32) {
        self.config_state.current_mut().chrome.sidebar_width = width;
    }

    /// Persist the sidebar width to `config.toml` on drag release. The live value already matches,
    /// so the hot-reload baseline is refreshed to skip the redundant reload the write would trigger.
    pub fn persist_sidebar_width(&mut self, width: f32) {
        let path = self.config().config_path.clone();
        let previous_width = load_config_from_path(&path)
            .ok()
            .map(|config| config.chrome.sidebar_width);
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["chrome", "sidebar-width"],
                bootty_config::toml_edit::value(f64::from(width)),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => self.config_hot_reload.refresh_after_reload(&path),
            Err(error) => {
                if let Some(previous_width) = previous_width {
                    self.config_state.current_mut().chrome.sidebar_width = previous_width;
                }
                self.last_error = Some(error.to_string());
            }
        }
    }

    fn persist_appearance_mode(&mut self, mode: AppearanceMode, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let token = match mode {
            AppearanceMode::System => "system",
            AppearanceMode::Light => "light",
            AppearanceMode::Dark => "dark",
        };
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["appearance", "mode"],
                bootty_config::toml_edit::value(token),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => {
                if self.reload_config(effects) {
                    self.config_hot_reload.refresh_after_reload(&path);
                }
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn persist_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "light",
            AppearanceVariant::Dark => "dark",
        };
        let result = (|| {
            let mut document = load_or_create_config_document(&path)?;
            document.set_item(
                &["appearance", branch, "theme"],
                bootty_config::toml_edit::value(theme),
            )?;
            document.write_to_disk()
        })();
        match result {
            Ok(()) => {
                if self.reload_config(effects) {
                    self.config_hot_reload.refresh_after_reload(&path);
                }
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn preview_active_theme(&mut self, theme: &str, effects: &mut Vec<AppEffect>) {
        let path = self.config().config_path.clone();
        let Some(config_dir) = path.parent() else {
            return;
        };
        let resolved = match bootty_config::config::resolve_theme(theme, config_dir) {
            Ok(theme) => theme,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };
        let variant = self.active_appearance_variant;
        let config = self.config_state.current_mut();
        let branch = match variant {
            AppearanceVariant::Light => &mut config.appearance.light,
            AppearanceVariant::Dark => &mut config.appearance.dark,
        };
        branch.theme = Some(theme.to_owned());
        branch.colors = resolved.colors;
        let colors = self
            .config()
            .colors_for_appearance(variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => effects.push(AppEffect::RequestRepaint),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn restore_theme_picker_preview(&mut self) -> bool {
        let Some(config) = self.theme_picker_restore_config.clone() else {
            return false;
        };
        self.config_state.accept(config);
        let colors = self
            .config()
            .colors_for_appearance(self.active_appearance_variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn theme_picker_preview_active(&self) -> bool {
        self.theme_picker_restore_config.is_some() && self.theme_picker_dialog.is_some()
    }

    pub fn set_appearance_variant(&mut self, variant: AppearanceVariant) {
        if self.active_appearance_variant == variant {
            return;
        }
        let colors = self
            .config()
            .colors_for_appearance(variant)
            .terminal_color_config();
        match self.set_binding_terminal_colors(colors) {
            Ok(()) => {
                self.active_appearance_variant = variant;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
    }

    pub fn active_appearance_variant(&self) -> AppearanceVariant {
        self.active_appearance_variant
    }

    pub fn ui_theme(&self) -> bootty_ui::Theme {
        theme_from_config(self.config(), self.active_appearance_variant)
    }

    pub fn mux(&self) -> &MuxController {
        &self.binding.mux
    }

    pub fn mux_scope(&self) -> MuxScope {
        self.binding.scope
    }

    pub fn binding_count(&self) -> usize {
        self.inactive_bindings.len() + 1
    }

    pub fn active_space_id(&self) -> SpaceId {
        self.active_space_id
    }

    pub fn space_summaries(&self) -> Vec<SpaceSummary> {
        let mut spaces = vec![(
            self.active_space_position,
            SpaceSummary {
                id: self.active_space_id,
                name: self.active_space_name.clone(),
                icon: self.active_space_icon.clone(),
                color: self.active_space_color,
                tint_sidebar: self.active_space_tint_sidebar,
                active: true,
                error: self.binding.degraded_error(),
            },
        )];
        spaces.extend(self.inactive_spaces.iter().map(|space| {
            (
                space.position,
                SpaceSummary {
                    id: space.id,
                    name: space.name.clone(),
                    icon: space.icon.clone(),
                    color: space.color,
                    tint_sidebar: space.tint_sidebar,
                    active: false,
                    error: space.binding.degraded_error(),
                },
            )
        }));
        spaces.sort_by_key(|(position, _)| *position);
        spaces.into_iter().map(|(_, summary)| summary).collect()
    }

    fn space_backend_override(
        &self,
        space_id: SpaceId,
    ) -> Option<Option<MultiplexerBackendConfig>> {
        if space_id == self.active_space_id {
            return Some(self.binding.backend_override);
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.binding.backend_override)
    }

    fn space_remote_override(&self, space_id: SpaceId) -> Option<SpaceRemoteOverride> {
        if space_id == self.active_space_id {
            return Some(self.binding.remote_override.clone());
        }
        self.inactive_spaces
            .iter()
            .find(|space| space.id == space_id)
            .map(|space| space.binding.remote_override.clone())
    }

    pub fn space_transition(&self, now: Instant) -> Option<(SpaceId, SpaceId, f32)> {
        let transition = self.space_transition?;
        let progress = transition.progress_at(now);
        (progress < 1.0).then_some((transition.from, transition.to, progress))
    }

    fn select_space(&mut self, index: u32) -> bool {
        let Some(index) = usize::try_from(index)
            .ok()
            .and_then(|index| index.checked_sub(1))
        else {
            return false;
        };
        self.space_summaries()
            .get(index)
            .is_some_and(|space| self.activate_space_from_ui(space.id))
    }
    pub fn create_space_from_ui(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
    ) -> bool {
        self.create_space_with_backend_from_ui(
            name,
            icon,
            color,
            tint_sidebar,
            SpaceMuxOverride::default(),
        )
    }

    fn create_space_with_backend_from_ui(
        &mut self,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let config_path = self.config().config_path.clone();
        let mut workspace = WorkspaceStore::for_config_path(&config_path);
        let space = match workspace.create_space(
            name,
            icon,
            color,
            tint_sidebar,
            mux,
            &self.config().multiplexer,
        ) {
            Ok(Some(space)) => space,
            Ok(None) => return false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        let runtime = match SpaceRuntime::from_workspace(
            &space,
            self.config(),
            self.active_appearance_variant,
            self.repaint.clone(),
        ) {
            Ok(Some(runtime)) => runtime,
            Ok(None) => {
                self.last_error = Some("newly created space has no binding".to_owned());
                let _ = workspace.delete_space(space.id());
                return false;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                let _ = workspace.delete_space(space.id());
                return false;
            }
        };
        let id = runtime.id;
        self.inactive_spaces.push(runtime);
        self.inactive_spaces.sort_by_key(|space| space.position);
        self.synchronize_live_binding_event_scopes();
        self.activate_space_from_ui(id)
    }

    fn reject_space_transition_with_pending_commands(&mut self, space_id: SpaceId) -> bool {
        let mut commands_in_flight = false;
        for pending in &self.pending_app_commands {
            if pending.origin.space_id() == space_id {
                pending.cancellation.request_cancel();
                commands_in_flight = true;
            }
        }
        if commands_in_flight {
            self.last_error = Some(
                "commands_in_flight: wait for command reconciliation before changing this Space"
                    .to_owned(),
            );
        }
        commands_in_flight
    }

    pub fn close_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let spaces = self.space_summaries();
        if spaces.len() <= 1 {
            return false;
        }
        let Some(index) = spaces.iter().position(|space| space.id == space_id) else {
            return false;
        };
        if self.reject_space_transition_with_pending_commands(space_id) {
            return false;
        }
        if space_id == self.active_space_id {
            let neighbor = spaces
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|index| spaces.get(index)));
            if !neighbor.is_some_and(|space| self.activate_space_from_ui(space.id)) {
                return false;
            }
        }
        let config_path = self.config().config_path.clone();
        let mut workspace = WorkspaceStore::for_config_path(&config_path);
        match workspace.delete_space(space_id) {
            Ok(true) => {
                let claims_context = DirectoryClaimsContext {
                    instance: InstanceRef {
                        instance_id: self.command_instance_handle.clone(),
                        generation: self.command_instance_generation,
                    },
                    window_id: self.window_state_key.clone(),
                };
                let mut release_error = None;
                if let Some(space) = self
                    .inactive_spaces
                    .iter()
                    .find(|space| space.id == space_id)
                {
                    for binding in space.bindings() {
                        let binding_ref = directory_binding_ref(&claims_context, binding);
                        match self
                            .directory_claims
                            .reconcile_live_claimants(&binding_ref, Vec::new())
                        {
                            Ok(Some(revision)) => {
                                if let Err(error) = publish_directory_usage_changed(
                                    &self.automation,
                                    &self.directory_claims,
                                    binding,
                                    None,
                                    json!({
                                        "reason": "space_closed",
                                        "space_id": space_id.persistence_value(),
                                        "revision": revision,
                                    }),
                                ) {
                                    release_error = Some(error.to_string());
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                enqueue_binding_claim_release(
                                    self.directory_claims.clone(),
                                    binding_ref.clone(),
                                    self.automation.clone(),
                                    vec![automation_event_scope(binding.scope)],
                                );
                                release_error = Some(format!(
                                    "{error}; durable space claim cleanup queued for retry"
                                ));
                            }
                        }
                    }
                }
                self.inactive_spaces.retain(|space| space.id != space_id);
                self.synchronize_live_binding_event_scopes();
                if let Some(error) = release_error {
                    self.last_error = Some(error);
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    pub fn update_space_from_ui(
        &mut self,
        space_id: SpaceId,
        name: &str,
        icon: &str,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> bool {
        let SpaceMuxOverride {
            backend: backend_override,
            remote: remote_override,
        } = mux.clone();
        let Some(previous_override) = self.space_backend_override(space_id) else {
            return false;
        };
        let previous_remote = self.space_remote_override(space_id);
        let resolved_backend = backend_override.unwrap_or(self.config().multiplexer.backend);
        // Name/icon/color edits retain the binding runtime and are safe while a command is
        // completing. Backend, remote, or binding-identity changes must wait for reconciliation.
        let backend_changed = previous_override != backend_override
            || previous_remote.as_ref() != Some(&remote_override);
        if backend_changed && self.reject_space_transition_with_pending_commands(space_id) {
            return false;
        }
        let app_key_bindings = if space_id == self.active_space_id {
            let keybinds = self.config().input.keybinds_for_backend(resolved_backend);
            match AppKeyBindings::from_keybinds(&keybinds) {
                Ok(bindings) => Some(bindings),
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            }
        } else {
            None
        };
        // The remote decides which machine the binding's sessions live on, so a change to it needs
        // the same rebuild a backend change does.
        let config_path = self.config().config_path.clone();
        let mut workspace = WorkspaceStore::for_config_path(&config_path);
        let runtime_config = self.config().clone();
        let active_appearance_variant = self.active_appearance_variant;
        let repaint = self.repaint.clone();
        let mut replacement_namespace = None;
        let mut replacement_binding_id = None;
        let mut replacement = if backend_changed {
            let binding = if space_id == self.active_space_id {
                Some(&self.binding)
            } else {
                self.inactive_spaces
                    .iter()
                    .find(|space| space.id == space_id)
                    .map(|space| &space.binding)
            };
            let Some(binding) = binding else {
                return false;
            };
            match binding_runtime_for_multiplexer(BindingRuntimeSpec {
                config: &runtime_config,
                scope: binding.scope,
                label: binding.label.clone(),
                backend_override,
                remote_override: remote_override.clone(),
                variant: active_appearance_variant,
                repaint: repaint.clone(),
                register_namespace: false,
                restore_sessions: false,
            }) {
                Ok(binding) => {
                    replacement_binding_id = Some(binding.scope.binding_id().persistence_value());
                    replacement_namespace = Some(
                        namespace_for_binding(binding.scope, &binding.multiplexer)
                            .persistence_key(),
                    );
                    Some(binding)
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            }
        } else {
            None
        };
        let update = if backend_changed {
            workspace.update_space_with_namespace(
                WorkspaceSpaceUpdate {
                    id: space_id,
                    name,
                    icon,
                    color,
                    tint_sidebar,
                    mux,
                },
                WorkspaceNamespaceUpdateContext {
                    binding_id: replacement_binding_id.expect("changed backend has a binding"),
                    namespace: replacement_namespace
                        .as_deref()
                        .expect("changed backend has a namespace"),
                },
            )
        } else {
            workspace.update_space(space_id, name, icon, color, tint_sidebar, mux)
        };
        match update {
            Ok(true) => {
                if space_id == self.active_space_id {
                    self.active_space_name = name.trim().to_owned();
                    self.active_space_icon = icon.trim().to_owned();
                    self.active_space_color = color;
                    self.active_space_tint_sidebar = tint_sidebar;
                    if backend_changed {
                        self.binding = replacement
                            .take()
                            .expect("changed backend has a prepared binding");
                        if let Err(error) = self.binding.restore_persisted_sessions(false, &repaint)
                        {
                            self.last_error = Some(error.to_string());
                        }
                        self.app_key_bindings =
                            app_key_bindings.expect("active backend bindings were validated");
                        self.terminal_surface = None;
                        self.last_pane_area = None;
                        if let Err(error) = self.sync_terminal_panes() {
                            self.last_error = Some(error.to_string());
                        }
                    }
                } else if let Some(space) = self
                    .inactive_spaces
                    .iter_mut()
                    .find(|space| space.id == space_id)
                {
                    space.name = name.trim().to_owned();
                    space.icon = icon.trim().to_owned();
                    space.color = color;
                    space.tint_sidebar = tint_sidebar;
                    if backend_changed {
                        space.binding = replacement
                            .take()
                            .expect("changed backend has a prepared binding");
                        if let Err(error) =
                            space.binding.restore_persisted_sessions(false, &repaint)
                        {
                            self.last_error = Some(error.to_string());
                        }
                    }
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn activate_relative_space(&mut self, delta: isize) -> bool {
        let spaces = self.space_summaries();
        let Some(active) = spaces.iter().position(|space| space.active) else {
            return false;
        };
        let Some(target) = active
            .checked_add_signed(delta)
            .and_then(|index| spaces.get(index))
        else {
            return false;
        };
        self.activate_space_from_ui(target.id)
    }
    fn activate_space_target(&mut self, space_id: SpaceId) -> bool {
        space_id == self.active_space_id || self.activate_space_from_ui(space_id)
    }

    fn activate_relative_space_from(&mut self, space_id: SpaceId, delta: isize) -> bool {
        let spaces = self.space_summaries();
        let Some(index) = spaces.iter().position(|space| space.id == space_id) else {
            return false;
        };
        let Some(target) = index
            .checked_add_signed(delta)
            .and_then(|index| spaces.get(index))
        else {
            return false;
        };
        self.activate_space_target(target.id)
    }

    fn persist_active_binding_restore_state(&mut self) {
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let mut workspace = WorkspaceStore::for_config_path(&self.config().config_path);
        if let Err(error) = workspace.set_binding_restore_state(
            self.binding.scope,
            self.binding.mux.last_error().is_some(),
            selected_session.as_deref(),
            selected_window.as_deref(),
        ) {
            self.last_error = Some(error.to_string());
        }
    }
    fn persist_rmux_restore_state(&mut self) {
        if selected_backend(&self.binding.multiplexer) == MultiplexerBackendConfig::Rmux {
            self.persist_active_binding_restore_state();
        }
    }

    pub fn activate_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        if space_id == self.active_space_id {
            return false;
        }
        let Some(index) = self
            .inactive_spaces
            .iter()
            .position(|space| space.id == space_id)
        else {
            return false;
        };
        let backend = self.inactive_spaces[index].binding.multiplexer.backend;
        let keybinds = self.config().input.keybinds_for_backend(backend);
        let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        let switch_started = crate::diagnostics::latency_start();
        let phase = crate::diagnostics::latency_start();
        let config_path = self.config().config_path.clone();
        let workspace = match WorkspaceStore::try_for_config_path(&config_path) {
            Ok(workspace) => workspace,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };
        if let Err(error) = workspace.set_selected_space(&self.window_state_key, space_id) {
            self.last_error = Some(error.to_string());
            return false;
        }
        crate::diagnostics::trace_phase("space.persist_selected_space", phase);
        self.persist_active_binding_restore_state();
        crate::diagnostics::trace_phase("space.persist_restore_state", switch_started);
        // Leave the outgoing space's tmux overrides in place. It keeps a live runtime, so its
        // status bar should stay hidden, and its terminal carries the bookkeeping to restore on
        // drop. Restoring here cost a tmux fork per pane and session, then the incoming binding
        // immediately paid to set them again.
        let phase = crate::diagnostics::latency_start();
        let mut target = self.inactive_spaces.remove(index);
        self.binding.discard_terminal_side_effects();
        for binding in &mut self.inactive_bindings {
            binding.discard_terminal_side_effects();
        }
        for binding in target.bindings_mut() {
            binding.discard_terminal_side_effects();
        }
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.discard_side_effects();
        }
        self.prepare_native_terminal_transition(&mut target.binding);
        crate::diagnostics::trace_phase("space.prepare_transition", phase);
        let phase = crate::diagnostics::latency_start();
        let current = SpaceRuntime {
            id: std::mem::replace(&mut self.active_space_id, target.id),
            name: std::mem::replace(&mut self.active_space_name, target.name),
            icon: std::mem::replace(&mut self.active_space_icon, target.icon),
            color: std::mem::replace(&mut self.active_space_color, target.color),
            tint_sidebar: std::mem::replace(
                &mut self.active_space_tint_sidebar,
                target.tint_sidebar,
            ),
            position: std::mem::replace(&mut self.active_space_position, target.position),
            binding: std::mem::replace(&mut self.binding, target.binding),
            inactive_bindings: std::mem::replace(
                &mut self.inactive_bindings,
                target.inactive_bindings,
            ),
        };
        if !self.binding.session_order.session_names().is_empty() {
            self.binding.mux.refresh_on_next_frame();
            let active_config = self.binding.multiplexer.clone();
            let _ = self
                .binding
                .mux
                .refresh_sessions(&self.repaint, &active_config);
            crate::diagnostics::trace_phase("space.refresh_sessions", phase);
            let phase = crate::diagnostics::latency_start();
            self.sync_session_order();
            crate::diagnostics::trace_phase("space.sync_session_order", phase);
            let refresh_completed = self.binding.mux.take_refresh_completed();
            let restored_persisted_sessions =
                if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                    self.binding.persisted_sessions_restored = false;
                    match self
                        .binding
                        .restore_persisted_sessions(refresh_completed, &self.repaint)
                    {
                        Ok(restored) => restored,
                        Err(error) => {
                            self.last_error = Some(error.to_string());
                            false
                        }
                    }
                } else {
                    false
                };
            if refresh_completed {
                self.prune_native_terminal_snapshot();
            }
            let sources_refreshed = if restored_persisted_sessions || refresh_completed {
                match self.refresh_automation_event_sources(true) {
                    Ok(()) => true,
                    Err(error) => {
                        self.last_error = Some(error.to_string());
                        false
                    }
                }
            } else {
                false
            };
            if refresh_completed {
                let scope = self.binding.scope;
                if sources_refreshed {
                    if let Err(error) = self.reconcile_refreshed_binding_automation_events(&[scope])
                    {
                        self.last_error = Some(error.to_string());
                    }
                } else {
                    self.retry_binding_automation_event_refresh(scope);
                }
            }
        }
        self.record_restored_persisted_launch_claims();
        let previous_space_id = current.id;
        self.inactive_spaces.push(current);
        self.inactive_spaces.sort_by_key(|space| space.position);
        self.space_transition = Some(SpaceTransition {
            from: previous_space_id,
            to: self.active_space_id,
            started: Instant::now(),
        });
        self.app_key_bindings = app_key_bindings;
        self.terminal_surface = None;
        self.last_pane_area = None;
        self.clear_space_context_dialogs();
        self.input_focus = InputFocus::Terminal;
        let phase = crate::diagnostics::latency_start();
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
        crate::diagnostics::trace_phase("space.sync_terminal_panes", phase);
        crate::diagnostics::trace_phase("space.TOTAL", switch_started);
        (self.repaint)();
        true
    }

    fn clear_space_context_dialogs(&mut self) {
        self.new_mux_session_dialog = None;
        self.sidebar_hovered_session = None;
        self.session_picker_dialog = None;
        self.rename_session_dialog = None;
        self.rename_tab_dialog = None;
        self.ditch_session_dialog = None;
        self.space_editor_dialog = None;
    }

    pub fn binding_session_groups(&self) -> Vec<BindingSessionGroup> {
        let mut bindings = std::iter::once(&self.binding)
            .chain(self.inactive_bindings.iter())
            .collect::<Vec<_>>();
        bindings.sort_by_key(|binding| binding.scope.binding_id().persistence_value());
        bindings
            .iter()
            .map(|binding| {
                let duplicate_label = bindings
                    .iter()
                    .filter(|candidate| candidate.label == binding.label)
                    .count()
                    > 1;
                let label = if duplicate_label {
                    format!(
                        "{} / Binding {}",
                        binding.label,
                        binding.scope.binding_id().persistence_value()
                    )
                } else {
                    binding.label.clone()
                };
                let sessions = binding.mux.sessions().to_vec();
                BindingSessionGroup {
                    scope: binding.scope,
                    label,
                    display_names: binding.session_display_name_map(&sessions),
                    sessions,
                    selected_session: binding.mux.selected_session().map(str::to_owned),
                    active: binding.scope == self.binding.scope,
                    can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
                }
            })
            .collect()
    }

    /// Every session the workspace can reach, grouped by the Space that owns it, with a trailing
    /// group for the sessions no Space claims. The finder needs the owner to know whether selecting a
    /// session means switching Spaces or adopting the session into the current one; the sidebar stays
    /// on `binding_session_groups`, which is this Space only.
    pub fn session_finder_groups(&self) -> Vec<BindingSessionGroup> {
        let mut spaces = vec![(
            self.active_space_position,
            self.active_space_name.as_str(),
            std::iter::once(&self.binding)
                .chain(self.inactive_bindings.iter())
                .collect::<Vec<_>>(),
        )];
        spaces.extend(self.inactive_spaces.iter().map(|space| {
            (
                space.position,
                space.name.as_str(),
                space.bindings().collect::<Vec<_>>(),
            )
        }));
        spaces.sort_by_key(|(position, ..)| *position);

        // One entry per session name: only the active binding refreshes, so a Space that has not been
        // visited this run has no snapshot of its own and has to borrow the shared backend's view of
        // its members. Names are what membership is keyed by, so names are the identity here.
        let mut sessions_across_spaces = Vec::<&MuxSession>::new();
        for binding in spaces.iter().flat_map(|(_, _, bindings)| bindings) {
            for session in binding.mux.all_sessions() {
                if !sessions_across_spaces
                    .iter()
                    .any(|known| known.name == session.name)
                {
                    sessions_across_spaces.push(session);
                }
            }
        }

        let mut claimed = HashSet::new();
        let mut groups = Vec::new();
        for (_, space_name, bindings) in &spaces {
            for binding in bindings {
                let members = binding.session_order.session_names();
                let sessions = members
                    .iter()
                    .filter_map(|name| {
                        // The owner's own snapshot first: session ids are per backend, and the id is
                        // what activation targets.
                        binding
                            .mux
                            .all_sessions()
                            .iter()
                            .chain(sessions_across_spaces.iter().copied())
                            .find(|session| session.name == *name)
                            .cloned()
                    })
                    .collect::<Vec<_>>();
                claimed.extend(members);
                if sessions.is_empty() {
                    continue;
                }
                groups.push(BindingSessionGroup {
                    scope: binding.scope,
                    label: if bindings.len() > 1 {
                        format!("{space_name} / {}", binding.label)
                    } else {
                        (*space_name).to_owned()
                    },
                    display_names: binding.session_display_name_map(&sessions),
                    sessions,
                    selected_session: binding.mux.selected_session().map(str::to_owned),
                    active: binding.scope == self.binding.scope,
                    can_return_to_last_session: binding.mux.previous_selected_session().is_some(),
                });
            }
        }

        let unclaimed = sessions_across_spaces
            .into_iter()
            .filter(|session| !claimed.contains(&session.name))
            .cloned()
            .collect::<Vec<_>>();
        if !unclaimed.is_empty() {
            groups.push(BindingSessionGroup {
                // Activating one of these adopts it into the current Space.
                scope: self.binding.scope,
                label: UNCLAIMED_SESSIONS_LABEL.to_owned(),
                sessions: unclaimed,
                selected_session: None,
                active: false,
                can_return_to_last_session: false,
                // No Space owns these, so bootty has no name of its own for them.
                display_names: HashMap::new(),
            });
        }
        groups
    }

    fn binding_runtimes(&self) -> impl Iterator<Item = &BindingRuntime> {
        std::iter::once(&self.binding)
            .chain(self.inactive_bindings.iter())
            .chain(self.inactive_spaces.iter().flat_map(SpaceRuntime::bindings))
    }

    fn binding_runtime(&self, scope: MuxScope) -> Option<&BindingRuntime> {
        self.binding_runtimes()
            .find(|binding| binding.scope == scope)
    }

    fn binding_runtime_mut(&mut self, scope: MuxScope) -> Option<&mut BindingRuntime> {
        self.binding_runtimes_mut()
            .find(|binding| binding.scope == scope)
    }
    fn binding_identity(&self, binding: &BindingRuntime) -> BindingRef {
        BindingRef {
            window: WindowRef {
                instance: InstanceRef {
                    instance_id: self.command_instance_handle.clone(),
                    generation: self.command_instance_generation,
                },
                window_id: self.window_state_key.clone(),
            },
            space_id: binding.scope.space_id().persistence_value().to_string(),
            binding_id: binding.scope.binding_id().persistence_value().to_string(),
            generation: 0,
        }
    }

    fn binding_runtimes_mut(&mut self) -> impl Iterator<Item = &mut BindingRuntime> {
        std::iter::once(&mut self.binding)
            .chain(self.inactive_bindings.iter_mut())
            .chain(
                self.inactive_spaces
                    .iter_mut()
                    .flat_map(SpaceRuntime::bindings_mut),
            )
    }

    fn set_binding_terminal_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.terminal.set_colors(colors.clone())?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_colors(colors.clone())?;
        }
        Ok(())
    }

    fn set_binding_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.terminal.set_cursor_config(cursor)?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_cursor_config(cursor)?;
        }
        Ok(())
    }

    fn set_binding_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.terminal.set_feature_config(features)?;
        }
        for binding in self.binding_runtimes_mut() {
            binding.terminal.set_feature_config(features)?;
        }
        Ok(())
    }

    fn apply_terminal_reload(
        &mut self,
        previous: &BoottyConfig,
        next: &BoottyConfig,
    ) -> Result<()> {
        let colors_changed = previous.colors_for_appearance(self.active_appearance_variant)
            != next.colors_for_appearance(self.active_appearance_variant);
        let cursor_changed = previous.cursor != next.cursor;
        let features_changed = previous.session.glyph_protocol != next.session.glyph_protocol;
        let result = (|| {
            if colors_changed {
                self.set_binding_terminal_colors(
                    next.colors_for_appearance(self.active_appearance_variant)
                        .terminal_color_config(),
                )?;
            }
            if cursor_changed {
                self.set_binding_cursor_config(next.cursor.terminal_cursor_config())?;
            }
            if features_changed {
                self.set_binding_feature_config(next.session.terminal_feature_config())?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            if features_changed {
                let _ = self.set_binding_feature_config(previous.session.terminal_feature_config());
            }
            if cursor_changed {
                let _ = self.set_binding_cursor_config(previous.cursor.terminal_cursor_config());
            }
            if colors_changed {
                let _ = self.set_binding_terminal_colors(
                    previous
                        .colors_for_appearance(self.active_appearance_variant)
                        .terminal_color_config(),
                );
            }
            return Err(error);
        }
        Ok(())
    }

    fn active_multiplexer(&self) -> &crate::config::MultiplexerConfig {
        &self.binding.multiplexer
    }

    pub fn multiplexer_backend(&self) -> crate::config::MultiplexerBackendConfig {
        self.binding.multiplexer.backend
    }

    pub fn terminal_transition_key(&self) -> Option<String> {
        self.binding.mux.selected_session_anchor().map(|anchor| {
            scoped_terminal_transition_key(
                self.binding.scope,
                selected_backend(self.active_multiplexer()),
                &anchor.session_id,
                anchor.pane_id.as_deref(),
            )
        })
    }

    pub fn status_metrics(&self) -> StatusMetrics {
        self.status_metrics
    }

    pub fn last_error(&self) -> Option<&str> {
        self.binding.mux.last_error().or(self.last_error.as_deref())
    }

    pub fn clear_last_error(&mut self) {
        self.binding.mux.set_error(None);
        self.last_error = None;
    }

    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }

    pub fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn sidebar_hovered_session(&self) -> Option<&ScopedSessionTarget> {
        self.sidebar_hovered_session.as_ref()
    }
    pub fn direct_input_suppresses_egui_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    /// Mirror the settings overlay's open/closed state so the direct input path stops feeding the
    /// terminal behind it (otherwise shortcuts like ⌘V paste into the hidden terminal).
    pub fn set_settings_open(&mut self, open: bool) {
        self.settings_open = open;
    }

    /// Mirror whether a Luau floating window is showing so the direct input path stops feeding the
    /// terminal behind it, matching how the native overlays gate input.
    pub fn set_lua_window_open(&mut self, open: bool) {
        self.lua_window_open = open;
    }

    pub fn macos_non_native_fullscreen_active(&self) -> bool {
        self.macos_non_native_fullscreen_active
    }

    fn sync_macos_non_native_fullscreen_presentation(&mut self) {
        if !self.macos_non_native_fullscreen_pending_apply {
            return;
        }
        if apply_macos_non_native_fullscreen_presentation(&self.config().window) {
            self.macos_non_native_fullscreen_pending_apply = false;
        }
    }

    pub fn terminal_mut(&mut self) -> &mut ActiveTerminal {
        &mut self.binding.terminal
    }

    pub fn record_surface(&mut self, surface: TerminalSurface) {
        self.terminal_surface = Some(surface);
    }

    pub fn record_render_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    /// Reset the registered chrome-handle rects at the start of a UI build; handles re-register
    /// themselves via `register_chrome_handle` as they are drawn.
    pub fn reset_chrome_handles(&mut self) {
        self.chrome_handle_rects.clear();
    }

    pub fn register_chrome_handle(&mut self, rect: egui::Rect) {
        self.chrome_handle_rects.push(rect);
    }

    fn is_native(&self) -> bool {
        matches!(
            self.active_multiplexer().backend,
            crate::config::MultiplexerBackendConfig::Native
        )
    }

    fn uses_native_terminal_layout(&self) -> bool {
        matches!(
            self.active_multiplexer().backend,
            crate::config::MultiplexerBackendConfig::Native
                | crate::config::MultiplexerBackendConfig::Rmux
        )
    }

    fn current_window_key(&self) -> ScopedWindowId {
        let session = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let window = self
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session || candidate.name == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            })
            .unwrap_or_default();
        self.binding.window_id(session, window)
    }
    pub fn pane_widget_key(&self, pane_id: &str) -> String {
        let window = self.current_window_key();
        let backend = selected_backend(self.active_multiplexer());
        format!(
            "{}:{}:{backend:?}:{}:{}:{pane_id}",
            window.scope.space_id().persistence_value(),
            window.scope.binding_id().persistence_value(),
            window.session_id,
            window.window_id,
        )
    }

    fn take_pending_pane_split_direction(
        &mut self,
        key: &ScopedWindowId,
    ) -> Option<SplitDirection> {
        self.binding
            .pending_pane_split_directions
            .remove(key)
            .or_else(|| {
                if key.window_id.is_empty() {
                    None
                } else {
                    self.binding.pending_pane_split_directions.remove(
                        &self
                            .binding
                            .window_id(key.session_id.clone(), String::new()),
                    )
                }
            })
    }

    fn current_pane_layout(&self) -> Option<&PaneLayout> {
        if !self.uses_native_terminal_layout() {
            return None;
        }
        self.binding.pane_layouts.get(&self.current_window_key())
    }

    /// Drop split layouts whose `(session, window)` no longer exists, so the map doesn't grow
    /// unbounded as the user creates and destroys native sessions and tabs. Keys are stored by
    /// whatever `current_window_key` recorded (session id, occasionally name), so accept either.
    fn prune_pane_layouts(&mut self) {
        if self.binding.pane_layouts.is_empty() {
            return;
        }
        let mut live = Vec::new();
        for session in self.binding.mux.sessions() {
            for window in &session.windows {
                live.push(
                    self.binding
                        .window_id(session.id.clone(), window.id.clone()),
                );
                live.push(
                    self.binding
                        .window_id(session.name.clone(), window.id.clone()),
                );
            }
        }

        live.push(self.current_window_key());
        self.binding
            .pane_layouts
            .retain(|key, _| live.contains(key));
    }

    /// An authoritative native snapshot replaces prior topology, so any local runtime it omits has
    /// no pane left to own it and must be dropped before the next layout reconciliation.
    fn prune_native_terminal_snapshot(&mut self) {
        if !self.uses_native_terminal_layout() {
            return;
        }
        let scope = self.binding.scope;
        let sessions = self.binding.mux.all_sessions();
        self.binding.terminal.prune_scoped_native_panes(
            scope,
            sessions
                .iter()
                .flat_map(|session| session.windows.iter())
                .flat_map(|window| window.panes.iter()),
        );
    }

    /// Reconcile the active native window's split layout against the backend's pane list, then make
    /// the layout's focused pane the input runtime and keep its siblings live. Non-native backends
    /// fall back to attaching the single selected anchor.
    fn sync_terminal_panes(&mut self) -> Result<()> {
        let phase = crate::diagnostics::latency_start();
        self.prune_pane_layouts();
        crate::diagnostics::trace_slow("panes.prune_pane_layouts", phase, 2.0);
        let phase = crate::diagnostics::latency_start();
        let config = self.active_multiplexer().clone();
        crate::diagnostics::trace_slow("panes.clone_config", phase, 2.0);
        if !self.uses_native_terminal_layout() {
            let phase = crate::diagnostics::latency_start();
            let result = self.binding.terminal.sync_scoped_mux_anchor(
                self.binding.scope,
                &config,
                self.binding.mux.selected_session_anchor(),
            );
            crate::diagnostics::trace_slow("panes.sync_scoped_mux_anchor", phase, 2.0);
            return result;
        }
        let panes: Vec<MuxPaneAnchor> = self.binding.mux.selected_window_panes().to_vec();
        let pane_ids: Vec<String> = panes
            .iter()
            .filter_map(|pane| pane.pane_id.clone())
            .collect();
        if pane_ids.is_empty() {
            // Idle native session (all tabs closed): nothing to render.
            return self.binding.terminal.sync_scoped_mux_anchor(
                self.binding.scope,
                &config,
                self.binding.mux.selected_session_anchor(),
            );
        }
        let key = self.current_window_key();
        let window_id = (!key.window_id.is_empty()).then(|| key.window_id.clone());
        let selected_pane = self
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        let server_layout = self
            .binding
            .mux
            .selected_window_layout()
            .and_then(PaneLayout::from_mux_layout)
            .filter(|layout| pane_sets_match(&layout.panes(), &pane_ids));
        let layout_missing = !self.binding.pane_layouts.contains_key(&key);
        let stale_layout = self
            .binding
            .pane_layouts
            .get(&key)
            .is_some_and(|layout| layout.panes().iter().all(|pane| !pane_ids.contains(pane)));
        let mut restored_from_server = false;
        if (layout_missing || stale_layout)
            && let Some(layout) = server_layout.clone()
        {
            self.binding.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        }

        let previous_panes = self
            .binding
            .pane_layouts
            .get(&key)
            .map(PaneLayout::panes)
            .unwrap_or_default();
        let new_panes = pane_ids
            .iter()
            .filter(|pane| !previous_panes.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let has_new_pane = !new_panes.is_empty();
        {
            let layout = self
                .binding
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(pane_ids[0].clone()));
            // A window id can be reused after its window is closed (native names tabs `tab-N`). If none
            // of the cached layout's panes still exist, it belongs to the old window -- start fresh.
            if layout.panes().iter().all(|pane| !pane_ids.contains(pane)) {
                *layout = PaneLayout::single(pane_ids[0].clone());
            }
        }
        let removed_panes = previous_panes
            .iter()
            .filter(|pane| !pane_ids.contains(pane))
            .cloned()
            .collect::<Vec<_>>();
        let pane_set_changed = has_new_pane || !removed_panes.is_empty();
        if pane_set_changed && let Some(layout) = server_layout {
            self.binding.pane_layouts.insert(key.clone(), layout);
            restored_from_server = true;
        } else if pane_set_changed {
            let new_pane_direction = self
                .take_pending_pane_split_direction(&key)
                .unwrap_or(SplitDirection::Right);
            let layout = self
                .binding
                .pane_layouts
                .get_mut(&key)
                .expect("native layout should be initialized");
            layout.reconcile_with_new_pane_direction(&pane_ids, new_pane_direction);
        }
        let layout = self
            .binding
            .pane_layouts
            .get_mut(&key)
            .expect("native layout should be initialized");
        if let Some(focus) = focus_after_native_layout_reconcile(
            restored_from_server,
            &new_panes,
            selected_pane.as_deref(),
        ) {
            layout.set_focus(&focus);
        }
        let focused_id = layout.focused().to_owned();
        let focused_anchor = panes
            .iter()
            .find(|pane| pane.pane_id.as_deref() == Some(focused_id.as_str()))
            .cloned();
        self.binding.terminal.sync_scoped_native_window(
            self.binding.scope,
            &panes,
            focused_anchor.as_ref(),
            window_id.as_deref(),
            selected_backend(&config),
            config.hide_tmux_status,
        )
    }

    /// True when the active native window holds more than one pane and should render as a split.
    pub fn native_multi_pane(&self) -> bool {
        self.current_pane_layout()
            .is_some_and(|layout| !layout.is_single())
    }

    pub fn focused_pane(&self) -> Option<String> {
        self.current_pane_layout()
            .map(|layout| layout.focused().to_owned())
    }

    fn pane_cache_key(&self, pane_id: &str) -> ScopedPaneId {
        let window = self
            .window_key_for_pane(pane_id)
            .unwrap_or_else(|| self.current_window_key());
        self.binding.pane_id(window, pane_id)
    }

    pub(crate) fn current_terminal_progress(&self) -> Option<TerminalProgress> {
        self.selected_window_backend_progress()
            .or_else(|| self.current_terminal_progress_from_panes())
    }

    fn selected_window_backend_progress(&self) -> Option<TerminalProgress> {
        let selected = self.mux().selected_window();
        self.mux()
            .selected_session_windows()
            .iter()
            .find(|window| match selected {
                Some(selected) => window.id == selected,
                None => window.active,
            })
            .and_then(|window| self.backend_window_progress(window))
    }

    fn current_terminal_progress_from_panes(&self) -> Option<TerminalProgress> {
        self.focused_pane()
            .as_deref()
            .and_then(|pane_id| self.pane_progress(pane_id))
            .or_else(|| {
                self.binding
                    .mux
                    .selected_session_anchor()
                    .and_then(|anchor| anchor.pane_id.as_deref())
                    .and_then(|pane_id| self.pane_progress(pane_id))
            })
            .or(self.binding.unscoped_terminal_progress)
    }

    pub(crate) fn pane_progress(&self, pane_id: &str) -> Option<TerminalProgress> {
        self.binding
            .terminal_progress
            .get(&self.pane_cache_key(pane_id))
            .copied()
    }

    pub(crate) fn pane_ports(&self, pane_id: &str) -> Option<&[u16]> {
        self.binding
            .terminal_ports
            .get(&self.pane_cache_key(pane_id))
            .map(Vec::as_slice)
    }

    pub(crate) fn session_ports(&self, session: &MuxSession) -> Vec<u16> {
        let selected = self.binding.mux.selected_session();
        let mut ports =
            if selected == Some(session.id.as_str()) || selected == Some(session.name.as_str()) {
                self.binding.unscoped_terminal_ports.clone()
            } else {
                Vec::new()
            };
        for pane in session
            .windows
            .iter()
            .flat_map(|window| window.panes.iter().chain(std::iter::once(&window.anchor)))
            .filter_map(|pane| pane.pane_id.as_deref())
        {
            if let Some(reported) = self.pane_ports(pane) {
                for port in reported {
                    if !ports.contains(port) {
                        ports.push(*port);
                    }
                }
            }
        }
        ports
    }

    pub(crate) fn has_indeterminate_terminal_progress(&self) -> bool {
        self.binding
            .terminal_progress
            .values()
            .chain(self.binding.unscoped_terminal_progress.iter())
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
            || self.binding.mux.sessions().iter().any(|session| {
                session
                    .windows
                    .iter()
                    .any(|window| self.window_has_indeterminate_progress(window))
            })
    }

    /// The names the active binding shows for `sessions`, in the same order.
    pub(crate) fn session_display_names(&self, sessions: &[MuxSession]) -> Vec<String> {
        self.binding.session_display_names(sessions)
    }

    pub(crate) fn window_has_indeterminate_progress(&self, window: &MuxWindow) -> bool {
        if let Some(progress) = self.backend_window_progress(window) {
            return progress.state == TerminalProgressState::Indeterminate;
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .any(|progress| progress.state == TerminalProgressState::Indeterminate)
    }

    pub(crate) fn window_progress(&self, window: &MuxWindow) -> Option<u8> {
        if let Some(progress) = self.backend_window_progress(window) {
            return progress.percent();
        }
        window
            .panes
            .iter()
            .chain(std::iter::once(&window.anchor))
            .filter_map(|pane| pane.pane_id.as_deref())
            .filter_map(|pane_id| self.pane_progress(pane_id))
            .filter_map(TerminalProgress::percent)
            .max()
    }

    /// An attached client forwards OSC 9;4 only for the pane it is currently showing, so its own
    /// per-window bookkeeping is the only source that can speak for a background window.
    fn backend_window_progress(&self, window: &MuxWindow) -> Option<TerminalProgress> {
        window
            .progress
            .as_ref()
            .and_then(TerminalProgress::from_mux)
    }

    pub fn pane_rects(&self, area: Rect, gap: f32) -> Vec<(String, Rect)> {
        self.current_pane_layout()
            .map(|layout| layout.rects(area, gap))
            .unwrap_or_default()
    }

    pub fn pane_dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        self.current_pane_layout()
            .map(|layout| layout.dividers(area, gap))
            .unwrap_or_default()
    }

    pub fn focus_pane(&mut self, pane_id: &str) {
        let key = self.current_window_key();
        let moved = match self.binding.pane_layouts.get_mut(&key) {
            Some(layout) if layout.focused() != pane_id => layout.set_focus(pane_id),
            _ => false,
        };
        // Make the new pane the input runtime this frame so its rect doesn't briefly render the
        // previously focused pane (the deref runtime would otherwise lag until the next frame's sync).
        if moved {
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn set_pane_ratio(&mut self, path: &[u8], ratio: f32, min_fraction: f32) {
        let key = self.current_window_key();
        if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
            layout.set_ratio_at(path, ratio, min_fraction, min_fraction);
        }
    }

    pub fn render_source_for_pane(
        &mut self,
        pane_id: &str,
    ) -> Option<&mut (dyn TerminalRuntime + '_)> {
        self.binding.terminal.render_source_for_pane(pane_id)
    }

    pub fn pane_terminal_window_size<F>(&self, leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        self.current_pane_layout()?.terminal_window_size(leaf_size)
    }

    pub fn resize_native_layout_window(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.binding
            .terminal
            .resize_native_layout_window(cols, rows)
    }

    fn sync_native_layout_terminal_now(&mut self) {
        if !self.uses_native_terminal_layout() {
            return;
        }
        if let Err(error) = self.sync_terminal_panes() {
            self.last_error = Some(error.to_string());
        }
    }

    fn split_focused_pane(&mut self, direction: SplitDirection, target_pane_id: Option<&str>) {
        let session = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        if !self.uses_native_terminal_layout() {
            self.binding.mux.execute_command(
                &self.repaint,
                &mux_config,
                MuxCommand::SplitPane {
                    session_id: session,
                    pane_id: target_pane_id.map(str::to_owned),
                    direction: mux_split_direction(direction),
                },
            );
            return;
        }
        let backend = selected_backend(&mux_config);
        let key = self.current_window_key();
        let focused = target_pane_id.map(str::to_owned).or_else(|| {
            self.binding
                .pane_layouts
                .get(&key)
                .map(|layout| layout.focused().to_owned())
                .or_else(|| {
                    self.binding
                        .mux
                        .selected_session_anchor()
                        .and_then(|anchor| anchor.pane_id.clone())
                })
        });
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::SplitPane {
                session_id: session,
                pane_id: focused.clone(),
                direction: mux_split_direction(direction),
            },
        );
        self.apply_split_layout_after_command(key, focused, direction, backend);
    }

    fn apply_split_layout_after_command(
        &mut self,
        key: ScopedWindowId,
        focused: Option<String>,
        direction: SplitDirection,
        backend: MultiplexerBackendConfig,
    ) {
        if backend == MultiplexerBackendConfig::Rmux {
            self.binding
                .pending_pane_split_directions
                .insert(key, direction);
            return;
        }

        // The native split synchronously sets the new pane active, so the refreshed anchor names it.
        let new_pane = self
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.clone());
        if let Some(new_pane) = new_pane {
            let layout = self
                .binding
                .pane_layouts
                .entry(key.clone())
                .or_insert_with(|| PaneLayout::single(new_pane.clone()));
            if let Some(focused) = &focused {
                layout.set_focus(focused);
            }
            if !layout.contains(&new_pane) {
                layout.split_focused(new_pane, direction);
            }
            self.binding.pending_pane_split_directions.remove(&key);
            let _ = self.sync_terminal_panes();
        }
    }

    pub fn record_pane_area(&mut self, area: Rect) {
        self.last_pane_area = Some(area);
    }

    fn focus_pane_neighbor(&mut self, direction: Direction) {
        let key = self.current_window_key();
        let Some(area) = self.last_pane_area else {
            return;
        };
        let gap = self.config().chrome.pane_divider_width;
        let neighbor = self
            .binding
            .pane_layouts
            .get(&key)
            .and_then(|layout| layout.neighbor(layout.focused(), direction, area, gap));
        if let Some(neighbor) = neighbor {
            self.focus_pane(&neighbor);
        }
    }

    fn focus_pane_relative(&mut self, delta: isize) {
        let key = self.current_window_key();
        let Some(layout) = self.binding.pane_layouts.get(&key) else {
            return;
        };
        let panes = layout.panes();
        if panes.len() < 2 {
            return;
        }
        let Some(index) = panes.iter().position(|pane| pane == layout.focused()) else {
            return;
        };
        let next = (index as isize + delta).rem_euclid(panes.len() as isize) as usize;
        let pane = panes[next].clone();
        self.focus_pane(&pane);
    }

    pub fn activate_scoped_session_from_ui(&mut self, target: &ScopedSessionTarget) -> bool {
        // A session that belongs to another Space is switched to there, not dragged over here: its
        // binding, terminal, and pane layout all live in that Space.
        if target.scope.space_id() != self.active_space_id
            && !self.activate_space_from_ui(target.scope.space_id())
        {
            return false;
        }
        if target.scope != self.binding.scope {
            let Some(index) = self
                .inactive_bindings
                .iter()
                .position(|binding| binding.scope == target.scope)
            else {
                return false;
            };
            let backend = self.inactive_bindings[index].multiplexer.backend;
            let keybinds = self.config().input.keybinds_for_backend(backend);
            let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return false;
                }
            };
            // Same as the space switch: the outgoing binding stays live and restores its own tmux
            // overrides on drop, so skip the fork-per-option restore the next attach would undo.
            let mut target_binding = self.inactive_bindings.remove(index);
            self.binding.discard_terminal_side_effects();
            target_binding.discard_terminal_side_effects();
            if let Some(owner) = &mut self.parked_native_terminal {
                owner.discard_side_effects();
            }
            self.prepare_native_terminal_transition(&mut target_binding);
            let current_binding = std::mem::replace(&mut self.binding, target_binding);
            self.inactive_bindings.insert(index, current_binding);
            if !self.binding.session_order.session_names().is_empty() {
                self.binding.mux.refresh_on_next_frame();
                let active_config = self.binding.multiplexer.clone();
                let _ = self
                    .binding
                    .mux
                    .refresh_sessions(&self.repaint, &active_config);
                self.sync_session_order();
                let refresh_completed = self.binding.mux.take_refresh_completed();
                let restored_persisted_sessions =
                    if selected_backend(&active_config) == MultiplexerBackendConfig::Native {
                        self.binding.persisted_sessions_restored = false;
                        match self
                            .binding
                            .restore_persisted_sessions(refresh_completed, &self.repaint)
                        {
                            Ok(restored) => restored,
                            Err(error) => {
                                self.last_error = Some(error.to_string());
                                false
                            }
                        }
                    } else {
                        false
                    };
                if refresh_completed {
                    self.prune_native_terminal_snapshot();
                }
                let sources_refreshed = if restored_persisted_sessions || refresh_completed {
                    match self.refresh_automation_event_sources(true) {
                        Ok(()) => true,
                        Err(error) => {
                            self.last_error = Some(error.to_string());
                            false
                        }
                    }
                } else {
                    false
                };
                if refresh_completed {
                    let scope = self.binding.scope;
                    if sources_refreshed {
                        if let Err(error) =
                            self.reconcile_refreshed_binding_automation_events(&[scope])
                        {
                            self.last_error = Some(error.to_string());
                        }
                    } else {
                        self.retry_binding_automation_event_refresh(scope);
                    }
                }
            }
            self.record_restored_persisted_launch_claims();
            self.app_key_bindings = app_key_bindings;
            self.terminal_surface = None;
            self.last_pane_area = None;
        }
        self.binding.mux.activate_session(&target.session_id);
        self.persist_rmux_restore_state();
        self.sync_native_layout_terminal_now();
        self.sidebar_hovered_session = Some(target.clone());
        (self.repaint)();
        true
    }

    pub fn activate_session_from_ui(&mut self, session_id: &str) {
        let target = ScopedSessionTarget::new(self.binding.scope, session_id);
        self.activate_scoped_session_from_ui(&target);
    }

    pub fn activate_relative_session_from_ui(&mut self, session_id: &str, delta: isize) -> bool {
        let sessions = self.binding.mux.sessions();
        let Some(current) = sessions
            .iter()
            .position(|session| session.id == session_id || session.name == session_id)
        else {
            return false;
        };
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        let session_id = sessions[next].id.clone();
        self.activate_session_from_ui(&session_id);
        true
    }

    pub fn activate_relative_scoped_session_from_ui(
        &mut self,
        target: &ScopedSessionTarget,
        delta: isize,
    ) -> bool {
        if !self.activate_scoped_session_from_ui(target) {
            return false;
        }
        self.activate_relative_session_from_ui(&target.session_id, delta)
    }

    pub fn activate_last_session_from_ui(&mut self) -> bool {
        let Some(session_id) = self
            .binding
            .mux
            .previous_selected_session()
            .map(str::to_owned)
        else {
            return false;
        };
        self.activate_session_from_ui(&session_id);
        true
    }

    pub fn activate_window_from_ui(&mut self, session_id: &str, window_id: &str) {
        let mux_config = self.active_multiplexer().clone();
        self.binding
            .mux
            .activate_window(session_id, window_id, &self.repaint, &mux_config);
        self.persist_rmux_restore_state();
        self.sync_native_layout_terminal_now();
    }

    pub fn activate_relative_window_from_ui(
        &mut self,
        session_id: &str,
        window_id: &str,
        delta: isize,
    ) -> bool {
        let Some((session_id, window_id)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let current = windows.iter().position(|window| window.id == window_id)?;
                let next = (current as isize + delta).rem_euclid(windows.len() as isize) as usize;
                Some((session.id.clone(), windows[next].id.clone()))
            })
        else {
            return false;
        };
        self.activate_window_from_ui(&session_id, &window_id);
        true
    }

    pub fn activate_last_window_from_ui(&mut self, session_id: &str) -> bool {
        let Some(session_id) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .filter(|session| session.windows.len() > 1)
            .map(|session| session.id.clone())
        else {
            return false;
        };
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ActivateLastWindow { session_id },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn new_tab_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let Some((resolved_session_id, anchor_cwd, target_is_current)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let window = session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)?;
                let session_is_current = selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name);
                let window_is_current = selected_window.as_deref().map_or_else(
                    || session.active_window_id.as_deref() == Some(window_id),
                    |selected| selected == window_id,
                );
                Some((
                    session.id.clone(),
                    window
                        .anchor
                        .cwd
                        .clone()
                        .or_else(|| session.anchor.cwd.clone()),
                    session_is_current && window_is_current,
                ))
            })
        else {
            return false;
        };
        let live_terminal_cwd = target_is_current
            .then(|| {
                self.binding
                    .terminal
                    .current_working_directory()
                    .ok()
                    .flatten()
            })
            .flatten();
        self.new_tab_from_ui(
            resolved_session_id,
            terminal_cwd_for_mux_command(live_terminal_cwd, anchor_cwd),
        )
    }

    fn new_tab_from_ui(&mut self, session_id: String, cwd: Option<String>) -> bool {
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::NewWindow { session_id, cwd },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn reorder_window_before_from_ui(&mut self, source: &str, before: Option<&str>) -> bool {
        let Some(session_id) = self.binding.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        if before == Some(source) {
            return false;
        }
        let windows = self.binding.mux.selected_session_windows();
        let Some(from) = windows.iter().position(|window| window.id == source) else {
            return false;
        };
        let mut target_ids = windows
            .iter()
            .map(|window| window.id.as_str())
            .filter(|id| *id != source)
            .collect::<Vec<_>>();
        let to = before
            .and_then(|before| target_ids.iter().position(|id| *id == before))
            .unwrap_or(target_ids.len());
        target_ids.insert(to, source);
        let Some(to) = target_ids.iter().position(|id| *id == source) else {
            return false;
        };
        let delta = to as i32 - from as i32;
        if delta == 0 {
            return false;
        }

        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::MoveWindow {
                session_id,
                window_id: Some(source.to_owned()),
                delta,
            },
        );
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn move_window_from_ui(&mut self, session_id: &str, window_id: &str, delta: i32) -> bool {
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let Some((session_id, position, window_count, active_window_id)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                let mut windows = session.windows.iter().collect::<Vec<_>>();
                windows.sort_by_key(|window| window.index);
                let active_window_id = (selected_session
                    .as_deref()
                    .is_some_and(|selected| selected == session.id || selected == session.name))
                .then_some(selected_window.as_deref())
                .flatten()
                .filter(|selected| windows.iter().any(|window| window.id == *selected))
                .map(str::to_owned)
                .or_else(|| session.active_window_id.clone());
                windows
                    .iter()
                    .position(|window| window.id == window_id)
                    .map(|position| {
                        (
                            session.id.clone(),
                            position,
                            windows.len(),
                            active_window_id,
                        )
                    })
            })
        else {
            return false;
        };
        let target = (position as i32 + delta).clamp(0, window_count as i32 - 1) as usize;
        if target == position {
            return false;
        }

        let mux_config = self.active_multiplexer().clone();
        let command = match active_window_id {
            Some(selected_window_id) if selected_window_id.as_str() != window_id => {
                MuxCommand::MoveWindowPreservingSelection {
                    session_id,
                    window_id: window_id.to_owned(),
                    delta,
                    selected_window_id,
                }
            }
            _ => MuxCommand::MoveWindow {
                session_id,
                window_id: Some(window_id.to_owned()),
                delta,
            },
        };
        self.binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
        true
    }

    pub fn close_pane_for_window_from_ui(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, pane_id)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .and_then(|window| {
                        window
                            .anchor
                            .pane_id
                            .clone()
                            .map(|pane_id| (session.id.clone(), window.id.clone(), pane_id))
                    })
            })
        else {
            return false;
        };
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let current_window = self.current_window_key();
        let target_is_current = current_window.window_id == window_id
            && self
                .binding
                .mux
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .is_some_and(|session| {
                    selected_session
                        .as_deref()
                        .is_some_and(|selected| selected == session.id || selected == session.name)
                });
        let mux_config = self.active_multiplexer().clone();
        self.binding
            .mux
            .close_pane(&session_id, Some(&pane_id), &self.repaint, &mux_config);
        self.binding
            .terminal
            .discard_scoped_pane(self.binding.scope, &pane_id);
        if self.uses_native_terminal_layout() {
            let key = self
                .binding
                .window_id(session_id.clone(), window_id.clone());
            if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
                layout.remove(&pane_id);
            }
            if target_is_current {
                let _ = self.sync_terminal_panes();
            }
        }
        true
    }

    fn record_session_order_error(&mut self, error: impl std::fmt::Display) {
        self.last_error = Some(format!("session membership persistence failed: {error}"));
    }

    fn sync_session_order(&mut self) {
        if let Err(error) = self.binding.sync_session_order() {
            self.record_session_order_error(error);
        }
    }
    /// Whether the generated-name reconciler needs to run, updating the stored fingerprint as a
    /// side effect. Reconciling forks up to four `git` subprocesses per session (a worktree lookup,
    /// then a suggested name), so this returns `false` while nothing relevant has changed, keeping
    /// that work off the steady-state frame path.
    ///
    /// Fingerprints the whole backend list, which changes only when the backend really did.
    /// `mux.sessions()` cannot be used: it is narrowed to this binding's membership, and it is
    /// unstable *within* a frame, because `apply_snapshot` resets it to the full backend list on
    /// every refresh and `sync_session_order` narrows it again later in the same frame. Hashing it
    /// reconciled several times a second forever, which is a `git` fork per session per refresh.
    ///
    /// Membership is left out on purpose. Including it would let a newly attached session take its
    /// generated name immediately, rather than waiting for the next backend change, but the extra
    /// reconciles it causes reach the cwd-keyed `SessionNameStore` collision between bindings often
    /// enough to fail Space membership tests. Include it once that store is keyed by session id.
    fn generated_names_need_sync(&mut self) -> bool {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for session in self.binding.mux.all_sessions() {
            hasher.write(session.id.as_bytes());
            hasher.write_u8(0);
            hasher.write(session.name.as_bytes());
            hasher.write_u8(0);
            if let Some(cwd) = session.anchor.cwd.as_deref() {
                hasher.write(cwd.as_bytes());
            }
            hasher.write_u8(1);
        }
        let signature = hasher.finish();
        if self.binding.generated_names_signature == Some(signature) {
            return false;
        }
        self.binding.generated_names_signature = Some(signature);
        true
    }

    fn sync_generated_session_names(&mut self) {
        let remote = self.active_multiplexer().remote.is_some();
        // Preserve membership before `observe_session` records the backend's new names below.
        if let Err(error) = self.binding.carry_renamed_members() {
            self.record_session_order_error(error);
            return;
        }
        if selected_backend(self.active_multiplexer()) == MultiplexerBackendConfig::Rmux {
            return;
        }
        if !self.generated_names_need_sync() {
            return;
        }
        // Reconcile only this binding's sessions. Generating names for the whole backend list
        // renames sessions that belong to other Spaces.
        let sessions = self.binding.mux.sessions().to_vec();
        let mut renames = Vec::new();
        self.binding
            .pending_generated_names
            .retain(|session_id, pending| {
                // A pending name the backend already reports has served its purpose: it exists to
                // keep the name alive for membership and uniqueness until the rename or create lands.
                // Renames record it under the new name rather than a session id, so the id lookup
                // below never prunes those and they would otherwise be held forever.
                if sessions.iter().any(|session| session.name == pending.name) {
                    return false;
                }
                sessions
                    .iter()
                    .find(|session| session.id == *session_id)
                    .is_none_or(|session| {
                        session
                            .anchor
                            .cwd
                            .as_deref()
                            .is_some_and(|cwd| Self::session_cwd(cwd, remote) == pending.cwd)
                    })
            });
        let mut planned_names = self
            .binding
            .pending_generated_names
            .values()
            .map(|pending| pending.name.clone())
            .collect::<HashSet<_>>();
        let rename_supported =
            selected_backend(self.active_multiplexer()) != MultiplexerBackendConfig::Rmux;
        // A generated name has to clear every session on the server, not just this binding's members:
        // asking for one another Space or a hand-made session already holds is a rename the backend
        // rejects, leaving bootty asking for it again on every change.
        let taken_names = self.taken_session_names(None);

        for session in &sessions {
            let Some(raw_cwd) = session.anchor.cwd.as_deref() else {
                continue;
            };
            let cwd = Self::session_cwd(raw_cwd, remote);
            let mut record = if let Some(record) =
                self.binding
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
            {
                record
            } else {
                let legacy_name = if remote {
                    crate::strings::session_name_for_remote_path(&cwd)
                } else {
                    crate::strings::session_name_for_path(&cwd)
                };
                if session.name == legacy_name {
                    self.binding.session_names.remember_generated(
                        &session.id,
                        &cwd,
                        &session.name,
                        &session.name,
                    );
                } else {
                    self.binding.session_names.mark_explicit(
                        &session.id,
                        &session.name,
                        &session.name,
                        &cwd,
                    );
                }
                self.binding
                    .session_names
                    .observe_session(&session.id, &session.name, &cwd)
                    .expect("session name metadata should be observable after recording")
            };

            // Records written before display names existed have none, and only those need one worked
            // out: from here on, creating and renaming both record what bootty means to show, so a
            // name someone typed is never something to second-guess.
            if record.display_name.is_empty() {
                let generated_suffix = session.name != record.generated_name
                    && crate::strings::is_uniquified_session_name(
                        &session.name,
                        &record.generated_name,
                    );
                if record.explicit && generated_suffix {
                    // Bootty generated `generated_name`, then asked the backend for that name plus a
                    // uniqueness suffix — which the old reconciler read back as somebody's rename.
                    self.binding
                        .session_names
                        .reclaim_generated(&session.id, &session.name);
                    record.generated_name = session.name.clone();
                    record.explicit = false;
                }
                let display_name = if record.explicit {
                    session.name.clone()
                } else {
                    // The name bootty means for this worktree, whenever the backend name is that name
                    // or that name plus the suffix it needed to clear the server.
                    let suggested = Self::suggested_session_name(&cwd, remote);
                    if crate::strings::is_uniquified_session_name(&session.name, &suggested) {
                        suggested
                    } else {
                        session.name.clone()
                    }
                };
                self.binding
                    .session_names
                    .set_display_name(&session.id, &display_name);
                record.display_name = display_name;
            }

            if let Some(pending) = self
                .binding
                .pending_generated_names
                .get(&session.id)
                .cloned()
            {
                if pending.cwd == cwd {
                    if session.name == pending.name {
                        planned_names.remove(&pending.name);
                        self.binding.session_names.remember_generated(
                            &session.id,
                            &cwd,
                            &pending.name,
                            &pending.display_name,
                        );
                        self.binding.pending_generated_names.remove(&session.id);
                    } else if session.name != record.generated_name {
                        planned_names.remove(&pending.name);
                        self.binding.pending_generated_names.remove(&session.id);
                        self.binding.session_names.mark_explicit(
                            &session.id,
                            &session.name,
                            &session.name,
                            &cwd,
                        );
                    }
                    continue;
                }
                self.binding.pending_generated_names.remove(&session.id);
            }
            if record.explicit {
                continue;
            }
            if session.name != record.generated_name {
                self.binding.session_names.mark_explicit(
                    &session.id,
                    &session.name,
                    &session.name,
                    &cwd,
                );
                continue;
            }

            let existing_names = taken_names
                .iter()
                .map(String::as_str)
                .filter(|name| *name != session.name)
                .chain(planned_names.iter().map(String::as_str));
            let display_name = Self::suggested_session_name(&cwd, remote);
            let desired = crate::strings::unique_session_name(&display_name, existing_names);
            if desired == session.name || !rename_supported {
                continue;
            }
            planned_names.insert(desired.clone());
            self.binding.pending_generated_names.insert(
                session.id.clone(),
                PendingGeneratedName {
                    cwd,
                    name: desired.clone(),
                    display_name,
                    previous_display_name: None,
                },
            );
            renames.push((session.id.clone(), desired));
        }

        if renames.is_empty() {
            return;
        }
        let mux_config = self.active_multiplexer().clone();
        for (session_id, name) in renames {
            self.binding
                .mux
                .rename_session(&session_id, name, &self.repaint, &mux_config);
        }
    }

    /// Every session name the backend already answers to, plus the names bootty has asked it for and
    /// is still waiting on. `keep` is the name of the session being renamed, which must not count as
    /// taken against itself.
    fn taken_session_names(&self, keep: Option<&str>) -> Vec<String> {
        std::iter::once(&self.binding)
            .chain(self.inactive_bindings.iter())
            .chain(self.inactive_spaces.iter().flat_map(SpaceRuntime::bindings))
            .flat_map(|binding| {
                binding.mux.backend_session_names().iter().cloned().chain(
                    binding
                        .pending_generated_names
                        .values()
                        .map(|pending| pending.name.clone()),
                )
            })
            .filter(|name| Some(name.as_str()) != keep)
            .collect()
    }

    fn create_project_session_for_cwd(&mut self, cwd: String) {
        let remote = self.active_multiplexer().remote.is_some();
        let cwd = Self::session_cwd(&cwd, remote);

        let existing_names = self.taken_session_names(None);
        // The backend name has to clear every session on the server, including sessions bootty does
        // not own; the display name is the one bootty meant, before that uniqueness pass.
        let display_name = Self::suggested_session_name(&cwd, remote);
        let session_id = crate::strings::unique_session_name(
            &display_name,
            existing_names.iter().map(String::as_str),
        );
        let descriptor = SessionLaunchDescriptor::simple(session_id.clone(), cwd.clone());
        let normalized = match Self::normalize_session_launch_descriptor(&descriptor, remote) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };
        let plan = normalized.mux_plan(session_id.clone());
        let mux_config = self.active_multiplexer().clone();
        if let Some(outcome) =
            Self::session_launch_preflight_outcome(&self.binding.mux, &mux_config, &plan)
        {
            self.last_error = command_outcome_message(&outcome);
            return;
        }
        if let Err(error) = persist_session_launch_plan(
            &self.config().config_path,
            self.binding.scope.binding_id().persistence_value(),
            &plan,
        ) {
            self.last_error = Some(format!("persisting session launch plan failed: {error}"));
            return;
        }
        let plan_id = plan.session_id.clone();
        let origin = self.binding.scope;
        let pending = match self.enqueue_authoritative_mux_command(
            "session.create",
            MuxCommand::CreateSession { plan },
            origin,
            None,
            None,
        ) {
            CommandDispatch::Pending {
                command,
                command_id,
                origin,
                binding_identity,
                binding_generation,
                namespace,
                target,
                deadline,
                cancellation,
                result,
            } => PendingAppCommand {
                request_id: next_app_command_reconciliation_id(),
                command,
                command_id,
                origin,
                binding_identity,
                binding_generation,
                namespace,
                target,
                deadline,
                cancellation,
                response: None,
                completion: None,
                rename: None,
                result,
            },
            CommandDispatch::Complete(outcome) => {
                let cleanup_error = delete_session_launch_plan(
                    &self.config().config_path,
                    self.binding.scope.binding_id().persistence_value(),
                    &plan_id,
                )
                .err()
                .map(|error| error.to_string());
                self.binding.session_order.forget_session_cache(&plan_id);
                self.last_error = cleanup_error
                    .map(|error| format!("session launch cleanup failed: {error}"))
                    .or_else(|| command_outcome_message(&outcome));
                return;
            }
            CommandDispatch::ExtensionPending { .. } => {
                let outcome = CommandOutcome::Failed {
                    code: "execution_failed".to_owned(),
                    message: "authoritative mux enqueue returned extension pending".to_owned(),
                };
                let cleanup_error = delete_session_launch_plan(
                    &self.config().config_path,
                    self.binding.scope.binding_id().persistence_value(),
                    &plan_id,
                )
                .err()
                .map(|error| error.to_string());
                self.binding.session_order.forget_session_cache(&plan_id);
                self.last_error = cleanup_error
                    .map(|error| format!("session launch cleanup failed: {error}"))
                    .or_else(|| command_outcome_message(&outcome));
                return;
            }
        };
        self.binding.pending_generated_names.insert(
            session_id.clone(),
            PendingGeneratedName {
                cwd: cwd.clone(),
                name: session_id.clone(),
                display_name: display_name.clone(),
                previous_display_name: None,
            },
        );
        self.binding.session_names.remember_generated(
            &session_id,
            &cwd,
            &session_id,
            &display_name,
        );
        self.pending_app_commands.push(pending);
        self.persist_rmux_restore_state();
    }

    fn normalize_session_launch_descriptor(
        descriptor: &SessionLaunchDescriptor,
        remote: bool,
    ) -> std::result::Result<NormalizedSessionLaunch, LaunchValidationError> {
        if remote {
            descriptor.normalize_for_remote()
        } else {
            descriptor.normalize()
        }
    }

    fn session_launch_preflight_outcome(
        mux: &BindingMuxController,
        mux_config: &MultiplexerConfig,
        plan: &MuxSessionLaunchPlan,
    ) -> Option<CommandOutcome> {
        match mux.preflight_session_launch(mux_config, plan) {
            Ok(outcome) => command_outcome_for_binding_operation(outcome),
            Err(error) => Some(CommandOutcome::Failed {
                code: "invalid_launch".to_owned(),
                message: error.to_string(),
            }),
        }
    }

    fn session_cwd(cwd: &str, remote: bool) -> String {
        if remote {
            cwd.to_owned()
        } else {
            Self::session_root(cwd)
        }
    }

    fn suggested_session_name(cwd: &str, remote: bool) -> String {
        if remote {
            crate::strings::session_name_for_remote_path(cwd)
        } else {
            crate::git::suggested_session_name(cwd)
        }
    }

    fn session_root(cwd: &str) -> String {
        let cwd = crate::git::worktree_root(cwd).unwrap_or_else(|| cwd.to_owned());
        std::fs::canonicalize(&cwd)
            .unwrap_or_else(|_| PathBuf::from(cwd))
            .to_string_lossy()
            .into_owned()
    }

    fn move_selected_session(&mut self, delta: i32) -> bool {
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return false;
        };
        self.move_session_from_ui(&selected, delta)
    }

    pub fn move_session_from_ui(&mut self, session_id: &str, delta: i32) -> bool {
        let Some(session_name) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| session.name.clone())
        else {
            return false;
        };
        let moved = match self.binding.session_order.move_session(
            &session_name,
            delta,
            self.binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            Ok(moved) => moved,
            Err(error) => {
                self.record_session_order_error(error);
                return false;
            }
        };
        if !moved {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn reorder_session_before(&mut self, source: &str, target: Option<&str>) -> bool {
        // Per-session anchors: a drag reorders within a group when source and target share one,
        // and moves the whole group across groups.
        let moved = match self.binding.session_order.move_session_before(
            source,
            target,
            self.binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str()),
        ) {
            Ok(moved) => moved,
            Err(error) => {
                self.record_session_order_error(error);
                return false;
            }
        };
        if !moved {
            return false;
        }
        self.sync_session_order();
        true
    }

    pub fn take_dialog(&mut self) -> Option<NewMuxSessionDialog> {
        self.new_mux_session_dialog.take()
    }
    pub fn take_space_editor_dialog(&mut self) -> Option<SpaceEditorDialog> {
        self.space_editor_dialog.take()
    }

    pub fn apply_space_editor_event(&mut self, dialog: SpaceEditorDialog, event: SpaceEditorEvent) {
        match event {
            SpaceEditorEvent::None => self.space_editor_dialog = Some(dialog),
            SpaceEditorEvent::Close => self.input_focus = InputFocus::Terminal,
            SpaceEditorEvent::Save {
                space_id,
                name,
                icon,
                color,
                tint_sidebar,
                mux,
            } => {
                let saved = match space_id {
                    Some(space_id) => self.update_space_from_ui(
                        space_id,
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux.clone(),
                    ),
                    None => self.create_space_with_backend_from_ui(
                        &name,
                        &icon,
                        color,
                        tint_sidebar,
                        mux,
                    ),
                };
                if !saved {
                    self.space_editor_dialog = Some(dialog);
                }
            }
        }
    }

    pub fn detach_scoped_session_from_space(&mut self, target: &ScopedSessionTarget) -> bool {
        let result = (|| -> Result<bool> {
            let Some(binding) = self
                .binding_runtimes_mut()
                .find(|binding| binding.scope == target.scope)
            else {
                return Ok(false);
            };
            let Some(name) = binding
                .mux
                .all_sessions()
                .iter()
                .find(|session| {
                    session.id == target.session_id || session.name == target.session_id
                })
                .map(|session| session.name.clone())
            else {
                return Ok(false);
            };
            let plan_ids = [target.session_id.as_str(), name.as_str()];
            remove_session_membership_and_launch_plan(
                &binding.workspace_config_path,
                target.scope.binding_id().persistence_value(),
                &name,
                &plan_ids,
            )?;
            binding.session_order.forget_session_cache(&name);
            binding.sync_session_order()?;
            Ok(true)
        })();
        match result {
            Ok(false) => false,
            Ok(true) => {
                (self.repaint)();
                true
            }
            Err(error) => {
                self.record_session_order_error(error);
                false
            }
        }
    }

    pub fn take_session_picker_dialog(&mut self) -> Option<SessionPickerDialog> {
        self.session_picker_dialog.take()
    }

    pub fn apply_session_picker_event(
        &mut self,
        dialog: SessionPickerDialog,
        event: SessionPickerEvent,
    ) {
        match event {
            SessionPickerEvent::None => {
                self.session_picker_dialog = Some(dialog);
            }
            SessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            SessionPickerEvent::ActivateSession(target) => {
                self.input_focus = InputFocus::Terminal;
                let result = (|| -> Result<()> {
                    if let Some(binding) = self
                        .binding_runtimes_mut()
                        .find(|binding| binding.scope == target.scope)
                        && let Some(name) = binding
                            .mux
                            .all_sessions()
                            .iter()
                            .find(|session| {
                                session.id == target.session_id || session.name == target.session_id
                            })
                            .map(|session| session.name.clone())
                    {
                        binding.session_order.add_session(&name)?;
                        binding.sync_session_order()?;
                    }
                    Ok(())
                })();
                let activation_succeeded = result.is_ok();
                if let Err(error) = result {
                    self.record_session_order_error(error);
                }
                if activation_succeeded {
                    self.activate_scoped_session_from_ui(&target);
                }
            }
        }
    }

    pub fn take_rename_session_dialog(&mut self) -> Option<RenameSessionDialog> {
        self.rename_session_dialog.take()
    }

    pub fn apply_rename_session_event(
        &mut self,
        dialog: RenameSessionDialog,
        event: RenameSessionEvent,
    ) {
        match event {
            RenameSessionEvent::None => {
                self.rename_session_dialog = Some(dialog);
            }
            RenameSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                let session = self
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|session| session.id == session_id || session.name == session_id)
                    .cloned();
                if let Some(session) = session {
                    let cwd = session
                        .anchor
                        .cwd
                        .as_deref()
                        .map(Self::session_root)
                        .unwrap_or_default();
                    let previous_display_name = self
                        .binding
                        .session_names
                        .display_name(&session.id)
                        .map(str::to_owned)
                        .or_else(|| Some(session.name.clone()));

                    // The typed name is what bootty shows. The backend still needs a name no other
                    // session on the server holds, so it may carry a suffix the sidebar never shows.
                    let taken = self.taken_session_names(Some(session.name.as_str()));
                    let backend_name = crate::strings::unique_session_name(
                        &name,
                        taken.iter().map(String::as_str),
                    );
                    let command_id =
                        format!("session.rename.{}", next_app_command_reconciliation_id());
                    let rename = PendingSessionRename {
                        session_id: session.id.clone(),
                        old_name: session.name.clone(),
                        new_name: backend_name.clone(),
                        display_name: name.clone(),
                        cwd: cwd.clone(),
                    };
                    let binding_id = self.binding.scope.binding_id().persistence_value();
                    if let Err(error) = persist_pending_session_rename(
                        &self.binding.workspace_config_path,
                        binding_id,
                        &command_id,
                        &rename,
                    ) {
                        self.record_session_order_error(error);
                    } else {
                        let command = MuxCommand::RenameSession {
                            session_id: session.id.clone(),
                            name: backend_name.clone(),
                        };
                        match self.enqueue_authoritative_mux_command(
                            command_id.clone(),
                            command,
                            self.binding.scope,
                            None,
                            None,
                        ) {
                            CommandDispatch::Pending {
                                command,
                                command_id,
                                origin,
                                binding_identity,
                                binding_generation,
                                namespace,
                                target,
                                deadline,
                                cancellation,
                                result,
                            } => {
                                self.binding.pending_generated_names.insert(
                                    backend_name.clone(),
                                    PendingGeneratedName {
                                        cwd: cwd.clone(),
                                        name: backend_name.clone(),
                                        display_name: name.clone(),
                                        previous_display_name,
                                    },
                                );
                                self.binding
                                    .session_names
                                    .set_display_name(&session.id, &name);
                                self.pending_app_commands.push(PendingAppCommand {
                                    request_id: next_app_command_reconciliation_id(),
                                    command,
                                    command_id,
                                    origin,
                                    binding_identity,
                                    binding_generation,
                                    namespace,
                                    target,
                                    deadline,
                                    cancellation,
                                    response: None,
                                    completion: None,
                                    rename: Some(rename),
                                    result,
                                });
                                self.persist_rmux_restore_state();
                            }
                            CommandDispatch::Complete(outcome) => {
                                let cleanup = clear_pending_session_rename(
                                    &self.binding.workspace_config_path,
                                    binding_id,
                                    &command_id,
                                );
                                self.last_error = cleanup
                                    .err()
                                    .map(|error| format!("session rename cleanup failed: {error}"))
                                    .or_else(|| command_outcome_message(&outcome));
                            }
                            CommandDispatch::ExtensionPending { .. } => {
                                let outcome = CommandOutcome::Failed {
                                    code: "execution_failed".to_owned(),
                                    message: "authoritative mux enqueue returned extension pending"
                                        .to_owned(),
                                };
                                let cleanup = clear_pending_session_rename(
                                    &self.binding.workspace_config_path,
                                    binding_id,
                                    &command_id,
                                );
                                self.last_error = cleanup
                                    .err()
                                    .map(|error| format!("session rename cleanup failed: {error}"))
                                    .or_else(|| command_outcome_message(&outcome));
                            }
                        }
                    }
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_rename_tab_dialog(&mut self) -> Option<RenameTabDialog> {
        self.rename_tab_dialog.take()
    }

    pub fn apply_rename_tab_event(&mut self, dialog: RenameTabDialog, event: RenameTabEvent) {
        match event {
            RenameTabEvent::None => {
                self.rename_tab_dialog = Some(dialog);
            }
            RenameTabEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            RenameTabEvent::Rename {
                session_id,
                window_id,
                name,
            } => {
                let name = name.trim();
                let key = self
                    .binding
                    .window_id(session_id.clone(), window_id.clone());
                if name.is_empty() {
                    self.binding.custom_tab_names.remove(&key);
                    if let Some(title) = self.binding.terminal_tab_titles.get(&key).cloned() {
                        self.rename_window_for_terminal_title(&session_id, &window_id, &title);
                    }
                } else {
                    self.binding.custom_tab_names.insert(key);
                    self.rename_window(&session_id, &window_id, name);
                }
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_terminal_find_dialog(&mut self) -> Option<TerminalFindDialog> {
        self.terminal_find_dialog.take()
    }

    pub fn apply_terminal_find_event(
        &mut self,
        mut dialog: TerminalFindDialog,
        event: TerminalFindEvent,
    ) {
        match event {
            TerminalFindEvent::None => {
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::Close => {
                self.input_focus = InputFocus::Terminal;
                self.terminal_find_return_focus_after_search = false;
            }
            TerminalFindEvent::FocusFind => {
                self.input_focus = InputFocus::Picker;
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::FocusTerminal => {
                self.input_focus = InputFocus::Terminal;
                self.terminal_find_dialog = Some(dialog);
            }
            TerminalFindEvent::Search { query, direction } => {
                let result = self.search_terminal(&query, direction);
                dialog.set_result(result);
                if direction != TerminalSearchDirection::Current
                    && self.terminal_find_return_focus_after_search
                {
                    self.input_focus = InputFocus::Terminal;
                }
                self.terminal_find_dialog = Some(dialog);
            }
        }
    }

    pub fn take_ditch_session_dialog(&mut self) -> Option<DitchSessionDialog> {
        self.ditch_session_dialog.take()
    }

    pub fn apply_ditch_session_event(
        &mut self,
        mut dialog: DitchSessionDialog,
        event: DitchSessionEvent,
    ) {
        match event {
            DitchSessionEvent::None => {
                self.ditch_session_dialog = Some(dialog);
            }
            DitchSessionEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            DitchSessionEvent::Ditch {
                session_id,
                cwd,
                action,
                confirmation,
            } => {
                let command_session_id = self
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|session| session.id == session_id || session.name == session_id)
                    .map(|session| session.name.clone())
                    .unwrap_or_else(|| session_id.clone());
                let action_json = match serde_json::to_string(&action) {
                    Ok(action_json) => action_json,
                    Err(error) => {
                        self.last_error =
                            Some(format!("serializing pending ditch intent failed: {error}"));
                        self.ditch_session_dialog = Some(dialog);
                        return;
                    }
                };
                if let Err(error) = persist_pending_ditch(
                    &self.binding.workspace_config_path,
                    self.binding.scope.binding_id().persistence_value(),
                    &command_session_id,
                    cwd.as_deref(),
                    &action_json,
                ) {
                    self.last_error =
                        Some(format!("persisting pending ditch intent failed: {error}"));
                    self.ditch_session_dialog = Some(dialog);
                    return;
                }
                match run_ditch_cleanup(
                    self,
                    &session_id,
                    cwd.as_deref(),
                    &action,
                    confirmation.as_deref(),
                ) {
                    Ok(()) => {
                        let pending_ditch_session_id = command_session_id.clone();
                        let command = MuxCommand::DitchSession {
                            session_id: command_session_id,
                        };
                        match self.enqueue_authoritative_mux_command(
                            "session.ditch",
                            command,
                            self.binding.scope,
                            None,
                            None,
                        ) {
                            CommandDispatch::Pending {
                                command,
                                command_id,
                                origin,
                                binding_identity,
                                binding_generation,
                                namespace,
                                target,
                                deadline,
                                cancellation,
                                result,
                            } => {
                                self.pending_app_commands.push(PendingAppCommand {
                                    request_id: next_app_command_reconciliation_id(),
                                    command,
                                    command_id,
                                    origin,
                                    binding_identity,
                                    binding_generation,
                                    namespace,
                                    target,
                                    deadline,
                                    cancellation,
                                    response: None,
                                    completion: None,
                                    rename: None,
                                    result,
                                });
                                let _ = self.drain_pending_app_commands(Instant::now());
                                self.input_focus = InputFocus::Terminal;
                            }
                            CommandDispatch::Complete(outcome) => {
                                let _ = clear_pending_ditch(
                                    &self.binding.workspace_config_path,
                                    self.binding.scope.binding_id().persistence_value(),
                                    &pending_ditch_session_id,
                                );
                                self.last_error = command_outcome_message(&outcome);
                                self.ditch_session_dialog = Some(dialog);
                            }
                            CommandDispatch::ExtensionPending { .. } => {
                                let _ = clear_pending_ditch(
                                    &self.binding.workspace_config_path,
                                    self.binding.scope.binding_id().persistence_value(),
                                    &pending_ditch_session_id,
                                );
                                let outcome = CommandOutcome::Failed {
                                    code: "execution_failed".to_owned(),
                                    message: "authoritative mux enqueue returned extension pending"
                                        .to_owned(),
                                };
                                self.last_error = command_outcome_message(&outcome);
                                self.ditch_session_dialog = Some(dialog);
                            }
                        }
                    }
                    Err(DitchCleanupError::ConfirmationRequired(confirmation)) => {
                        dialog.require_worktree_removal_confirmation(action, *confirmation);
                        self.ditch_session_dialog = Some(dialog);
                    }
                    Err(DitchCleanupError::StaleTarget(message)) => {
                        let _ = clear_pending_ditch(
                            &self.binding.workspace_config_path,
                            self.binding.scope.binding_id().persistence_value(),
                            &command_session_id,
                        );
                        self.last_error = Some(format!("ditch: {message}"));
                        self.ditch_session_dialog = Some(DitchSessionDialog::open(session_id, cwd));
                    }
                    Err(error) => {
                        // The git cleanup failed; clear the one-shot intent so a partial/CAS
                        // failure is retried only after the user confirms a fresh action.
                        let _ = clear_pending_ditch(
                            &self.binding.workspace_config_path,
                            self.binding.scope.binding_id().persistence_value(),
                            &command_session_id,
                        );
                        self.last_error = Some(format!("ditch: {error}"));
                        self.ditch_session_dialog = Some(dialog);
                    }
                }
            }
        }
    }

    pub fn take_keybind_help_dialog(&mut self) -> Option<KeybindHelpDialog> {
        self.keybind_help_dialog.take()
    }

    pub fn apply_keybind_help_event(&mut self, dialog: KeybindHelpDialog, event: KeybindHelpEvent) {
        match event {
            KeybindHelpEvent::None => {
                self.keybind_help_dialog = Some(dialog);
            }
            KeybindHelpEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn take_command_palette_dialog(&mut self) -> Option<CommandPaletteDialog> {
        self.command_palette_dialog.take()
    }

    pub fn apply_command_palette_event(
        &mut self,
        dialog: CommandPaletteDialog,
        event: CommandPaletteEvent,
    ) {
        match event {
            CommandPaletteEvent::None => {
                self.command_palette_dialog = Some(dialog);
            }
            CommandPaletteEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            CommandPaletteEvent::Run(command) => {
                // Resolve the user's current context before another queued caller can change it.
                self.input_focus = InputFocus::Terminal;
                if command == crate::action_catalog::Command::NewWindow {
                    // This is a local UI action, not an external top-level-window command.
                    self.open_new_mux_session_dialog();
                    return;
                }
                let Some(mut invocation) =
                    CommandInvocation::from_catalog(command, Caller::CommandPalette)
                else {
                    return;
                };
                if let Some(kind) = self
                    .extension_runtime
                    .command_registry()
                    .describe(&invocation.command)
                    .and_then(|descriptor| descriptor.target)
                {
                    let Some(target) = self.current_command_target_for(&invocation.command, kind)
                    else {
                        self.pending_command = None;
                        self.last_error = Some(format!("no current {kind:?} target is available"));
                        return;
                    };
                    invocation.target = Some(target);
                }
                self.pending_command = Some(invocation);
            }
        }
    }

    pub fn take_theme_picker_dialog(&mut self) -> Option<ThemePickerDialog> {
        self.theme_picker_dialog.take()
    }

    pub fn apply_theme_picker_event(
        &mut self,
        dialog: ThemePickerDialog,
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::None => {
                self.theme_picker_dialog = Some(dialog);
            }
            ThemePickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_restore_config = None;
            }
            ThemePickerEvent::RestorePreview => {
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_dialog = Some(dialog);
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
                self.theme_picker_dialog = Some(dialog);
            }
            ThemePickerEvent::Select(theme) => {
                self.input_focus = InputFocus::Terminal;
                self.theme_picker_restore_config = None;
                self.persist_active_theme(&theme, effects);
            }
        }
    }

    pub fn apply_picker_event(
        &mut self,
        dialog: NewMuxSessionDialog,
        event: NewSessionPickerEvent,
    ) {
        match event {
            NewSessionPickerEvent::None => {
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::Close => {
                self.input_focus = InputFocus::Terminal;
            }
            NewSessionPickerEvent::Error(error) => {
                self.last_error = Some(error);
                self.new_mux_session_dialog = Some(dialog);
            }
            NewSessionPickerEvent::CreateWorktree { repo, branch } => {
                let service = self.worktree_service();
                match service.create(crate::git::WorktreeCreateRequest {
                    repository_path: PathBuf::from(repo),
                    branch,
                    managed_by_bootty: true,
                    caller: "ui.new_session_picker".to_owned(),
                }) {
                    Ok(details) => {
                        let path = details.worktree.path.to_string_lossy().into_owned();
                        let event_result = publish_worktree_changed(
                            &self.automation,
                            &service,
                            &self.binding,
                            &details.worktree,
                            json!({
                                "change": "created",
                                "worktree": &details.worktree,
                            }),
                        );
                        self.create_project_session_for_cwd(path);
                        self.input_focus = InputFocus::Terminal;
                        if let Err(error) = event_result {
                            self.last_error = Some(format!("worktree event: {error}"));
                        }
                    }
                    Err(error) => {
                        self.last_error = Some(format!("worktree: {error}"));
                        self.new_mux_session_dialog = Some(dialog);
                    }
                }
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                self.create_project_session_for_cwd(cwd);
                self.input_focus = InputFocus::Terminal;
            }
        }
    }

    pub fn drain_direct_input(&mut self) {
        if let Some(rx) = &self.modifier_side_rx
            && let Some(latest) = rx.try_iter().last()
        {
            self.modifier_sides = latest;
        }
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }

    fn drain_terminal_side_effects(
        &mut self,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let side_effects = self
            .binding
            .terminal_side_effect_rx
            .try_iter()
            .collect::<Vec<_>>();
        for side_effect in side_effects {
            self.apply_terminal_side_effect_event(
                side_effect,
                effects,
                terminal_cell_width,
                terminal_cell_height,
                terminal_scale_factor,
            );
        }
    }

    fn apply_terminal_side_effect_event(
        &mut self,
        event: TerminalSideEffectEvent,
        effects: &mut Vec<AppEffect>,
        terminal_cell_width: f32,
        terminal_cell_height: f32,
        terminal_scale_factor: f32,
    ) {
        let TerminalSideEffectEvent {
            source_pane_id,
            effect,
        } = event;
        let source_pane_id = match source_pane_id {
            Some(source_pane_id) => {
                if let Some((scope, pane_id)) = decode_scoped_pane_id(&source_pane_id) {
                    if scope != self.binding.scope {
                        return;
                    }
                    Some(pane_id)
                } else {
                    Some(source_pane_id)
                }
            }
            None => None,
        };
        match effect {
            TerminalSideEffect::Bell => effects.push(AppEffect::Bell),
            TerminalSideEffect::ClipboardWrite(text) => {
                if let Err(error) = write_clipboard_text(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ClipboardQuery { selection } => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self
                        .binding
                        .terminal
                        .write_input(&encode_osc52_response(&selection, &text))
                    {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
            TerminalSideEffect::WindowTitle(title) => {
                self.apply_terminal_window_title(source_pane_id.as_deref(), title, effects);
            }
            TerminalSideEffect::WindowIcon(_) => {}
            TerminalSideEffect::DesktopNotification { title, body } => {
                if let Err(error) = show_desktop_notification(&title, &body) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::MouseShape(shape) => {
                if let Some(icon) = terminal_cursor_icon_for_mouse_shape(&shape) {
                    self.terminal_cursor_icon = icon;
                    effects.push(AppEffect::SetTerminalCursorIcon(
                        self.effective_terminal_cursor_icon(),
                    ));
                }
            }
            TerminalSideEffect::OpenUrl(url) => effects.push(AppEffect::OpenUrl(url)),
            TerminalSideEffect::FocusWindow => effects.push(AppEffect::SetWindowFocus),
            TerminalSideEffect::ReportCellSize => {
                let response = encode_iterm2_report_cell_size(
                    terminal_cell_width,
                    terminal_cell_height,
                    terminal_scale_factor,
                );
                if let Err(error) = self.binding.terminal.write_input(&response) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ReportVariable(name) => {
                if let Some(response) =
                    terminal_report_variable_response(&name, self.binding.mux.selected_session())
                    && let Err(error) = self.binding.terminal.write_input(&response)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalSideEffect::ConEmuProgress { state, value } => {
                self.apply_terminal_progress(source_pane_id.as_deref(), state, value);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::Iterm2UserVarPorts(ports) => {
                self.apply_terminal_ports(source_pane_id.as_deref(), ports);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalSideEffect::SemanticPrompt(_)
            | TerminalSideEffect::KittyTextSizing(_)
            | TerminalSideEffect::ConEmuControl(_)
            | TerminalSideEffect::Iterm2Control(_)
            | TerminalSideEffect::Iterm2File(_)
            | TerminalSideEffect::UnsupportedHostCommand { .. } => {}
        }
    }

    fn apply_terminal_progress(
        &mut self,
        source_pane_id: Option<&str>,
        state: String,
        value: Option<u8>,
    ) {
        if state == "unknown" {
            return;
        }
        // A tmux client reports progress for every window through its own bookkeeping, and
        // forwards OSC 9;4 only for the pane it currently shows. Recording the forwarded copy
        // would credit it to whichever pane the attach started on, painting a bar on the wrong
        // window and never clearing it.
        if selected_backend(&self.config().multiplexer) == MultiplexerBackendConfig::Tmux {
            return;
        }
        let progress = TerminalProgress::from_conemu(&state, value);
        match source_pane_id {
            Some(pane_id) => {
                let key = self.pane_cache_key(pane_id);
                match progress {
                    Some(progress) => {
                        self.binding.terminal_progress.insert(key, progress);
                    }
                    None => {
                        self.binding.terminal_progress.remove(&key);
                    }
                }
            }
            None => self.binding.unscoped_terminal_progress = progress,
        }
    }

    fn apply_terminal_ports(&mut self, source_pane_id: Option<&str>, ports: Vec<u16>) {
        match source_pane_id {
            Some(pane_id) => {
                let key = self.pane_cache_key(pane_id);
                self.binding.terminal_ports.insert(key, ports);
            }
            None => self.binding.unscoped_terminal_ports = ports,
        }
    }

    fn apply_terminal_window_title(
        &mut self,
        source_pane_id: Option<&str>,
        title: String,
        effects: &mut Vec<AppEffect>,
    ) {
        let window_key = source_pane_id
            .and_then(|pane_id| self.window_key_for_pane(pane_id))
            .or_else(|| source_pane_id.is_none().then(|| self.current_window_key()))
            .filter(|key| !key.window_id.is_empty());
        if let Some(key) = window_key {
            self.binding
                .terminal_tab_titles
                .insert(key.clone(), title.clone());
            if !self.binding.custom_tab_names.contains(&key) {
                self.rename_window_for_terminal_title(&key.session_id, &key.window_id, &title);
            }
        }
        if source_pane_id.is_none() || self.binding.terminal.focused_pane_id() == source_pane_id {
            effects.push(AppEffect::SetWindowTitle(title));
        }
    }

    fn window_key_for_pane(&self, pane_id: &str) -> Option<ScopedWindowId> {
        self.binding.mux.sessions().iter().find_map(|session| {
            session.windows.iter().find_map(|window| {
                let anchor_matches = window.anchor.pane_id.as_deref() == Some(pane_id);
                let pane_matches = window
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id.as_deref() == Some(pane_id));
                (anchor_matches || pane_matches).then(|| {
                    self.binding
                        .window_id(session.id.clone(), window.id.clone())
                })
            })
        })
    }

    fn rename_window_for_terminal_title(&mut self, session_id: &str, window_id: &str, title: &str) {
        if self.window_name_for_key(session_id, window_id) == Some(title) {
            return;
        }
        self.rename_window(session_id, window_id, title);
    }

    fn rename_window(&mut self, session_id: &str, window_id: &str, name: &str) {
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.rename_window(
            session_id,
            window_id,
            name.to_owned(),
            &self.repaint,
            &mux_config,
        );
    }

    fn window_name_for_key(&self, session_id: &str, window_id: &str) -> Option<&str> {
        self.binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)?
            .windows
            .iter()
            .find(|window| window.id == window_id)
            .map(|window| window.name.as_str())
    }

    fn effective_terminal_cursor_icon(&self) -> egui::CursorIcon {
        if self.mouse_pointer_hidden_while_typing {
            egui::CursorIcon::None
        } else {
            self.terminal_cursor_icon
        }
    }

    fn set_mouse_pointer_hidden_while_typing(
        &mut self,
        hidden: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let hidden = hidden && self.config().input.hide_mouse_pointer_while_typing;
        if self.mouse_pointer_hidden_while_typing == hidden {
            return;
        }
        self.mouse_pointer_hidden_while_typing = hidden;
        effects.push(AppEffect::SetTerminalCursorIcon(
            self.effective_terminal_cursor_icon(),
        ));
    }

    fn hide_mouse_pointer_for_terminal_typing(&mut self, effects: &mut Vec<AppEffect>) {
        self.set_mouse_pointer_hidden_while_typing(true, effects);
    }

    fn restore_mouse_pointer_after_pointer_moved(
        &mut self,
        events: &[egui::Event],
        hover_pos: Option<Pos2>,
        effects: &mut Vec<AppEffect>,
    ) {
        let moved_by_event = events
            .iter()
            .any(|event| matches!(event, egui::Event::PointerMoved(_)));
        let moved_by_hover_pos = hover_pos.is_some() && hover_pos != self.last_mouse_hover_pos;
        self.last_mouse_hover_pos = hover_pos;

        if moved_by_event || moved_by_hover_pos {
            self.set_mouse_pointer_hidden_while_typing(false, effects);
        }
    }

    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    /// The modifier keys held right now, with their left/right sides, as tracked by the direct
    /// winit input path. The settings recorder needs this for wheel steps, which arrive as egui
    /// events with side-less modifiers.
    pub fn modifier_sides(&self) -> ModifierSideState {
        self.modifier_sides
    }

    /// Drain the pending direct-input chords as binding-trigger strings for the settings keybind
    /// recorder. This is how the recorder captures cmd-modified chords like ⌘V and ⌘⌥X: egui
    /// collapses those into copy/cut/paste events with no key event, but bootty's direct winit path
    /// keeps the full key + modifiers. Only meaningful while settings is open (the terminal is not
    /// consuming this input).
    pub fn take_settings_capture_chords(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_direct_input)
            .into_iter()
            .map(|direct| {
                let chord =
                    crate::input_binding::BindingTrigger::from_key_input_with_modifier_sides(
                        direct.input(),
                    )
                    .format_entry();
                normalize_recorded_chord(chord)
            })
            .collect()
    }

    #[cfg(debug_assertions)]
    fn drive_diagnostic_actions(&mut self, now: Instant, effects: &mut Vec<AppEffect>) -> usize {
        let actions = self
            .diagnostic_action_driver
            .as_mut()
            .map(|driver| driver.due_actions(now))
            .unwrap_or_default();
        let action_count = actions.len();
        for action in actions {
            self.record_diagnostic_action("start", action, 0);
            let start = Instant::now();
            self.apply_mux_key_action(action.mux_action());
            self.record_diagnostic_action("done", action, start.elapsed().as_micros());
            effects.push(AppEffect::RequestRepaint);
        }
        action_count
    }

    #[cfg(not(debug_assertions))]
    fn drive_diagnostic_actions(&mut self, _now: Instant, _effects: &mut Vec<AppEffect>) -> usize {
        0
    }

    #[cfg(debug_assertions)]
    fn record_diagnostic_action(
        &mut self,
        phase: &str,
        action: DiagnosticAction,
        action_elapsed_us: u128,
    ) {
        let selected_session = self.binding.mux.selected_session().map(str::to_owned);
        let selected_window = self.binding.mux.selected_window().map(str::to_owned);
        let pane_count = self.binding.mux.selected_window_panes().len();
        let last_error = self.last_error.clone();
        if let Some(driver) = &mut self.diagnostic_action_driver {
            driver.record(DiagnosticRecord {
                phase,
                action,
                action_elapsed_us,
                selected_session: selected_session.as_deref(),
                selected_window: selected_window.as_deref(),
                pane_count,
                last_error: last_error.as_deref(),
            });
        }
    }

    pub fn update_frame(&mut self, inputs: FrameInputs) -> Vec<AppEffect> {
        let frame_started = crate::diagnostics::latency_start();
        let FrameInputs {
            now,
            stable_dt_ms,
            events,
            dropped_file_paths,
            modifiers,
            hover_pos,
            pressed_mouse_button,
            viewport,
            window_focused,
            renderer_metrics,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
            terminal_view_transform,
        } = inputs;
        let mut effects = Vec::new();

        self.drain_app_commands(viewport, &mut effects);
        self.collect_backend_automation_events();
        if !self
            .binding_runtimes()
            .any(|binding| binding.automation_event_refresh_pending)
            && let Err(error) = self.refresh_automation_event_sources(false)
        {
            self.last_error = Some(error.to_string());
        }

        // A command-palette choice from the previous frame runs as soon as viewport/effects are
        // available, before mux refresh can retarget selected-window actions back to backend-active.
        if let Some(invocation) = self.pending_command.take() {
            let _ = self.dispatch_command(invocation, viewport, &mut effects);
        }

        self.sync_macos_non_native_fullscreen_presentation();
        // Drain the focused pane plus every live sibling in the active native window so background
        // panes keep processing output. For non-native this is just the single attach surface.
        self.last_drain = self.binding.terminal.drain_native_window();
        for binding in &mut self.inactive_bindings {
            binding.terminal.drain_native_window();
            binding.discard_terminal_side_effects();
        }
        for space in &mut self.inactive_spaces {
            for binding in space.bindings_mut() {
                binding.terminal.drain_native_window();
                binding.discard_terminal_side_effects();
            }
        }
        if let Some(owner) = &mut self.parked_native_terminal {
            owner.drain_inactive();
        }
        self.drain_terminal_side_effects(
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        if self.has_degraded_remote() && self.network_change_detector.changed(now) {
            self.reset_remote_reconnects(now);
        }
        // A shell exiting closes its pane, collapsing the split (or cascading to the tab when it was
        // the last pane). On native, any pane's shell can exit, not just the focused one.
        if self.is_native() {
            for pane in self.binding.terminal.native_exited_panes() {
                self.close_pane(&pane);
            }
        } else {
            match self.binding.terminal.child_exited() {
                Ok(true) => self.handle_attach_client_exit(now),
                Ok(false) => self.note_attach_client_alive(now),
                Err(error) => self.last_error = Some(error.to_string()),
            }
            self.start_due_reattach(now, &mut effects);
        }

        if let Some(Err(_)) = self.binding.mux.poll_command() {
            self.binding.pending_generated_names.clear();
        }
        for binding in &mut self.inactive_bindings {
            if let Some(Err(_)) = binding.mux.poll_command() {
                binding.pending_generated_names.clear();
            }
        }
        for space in &mut self.inactive_spaces {
            for binding in space.bindings_mut() {
                if let Some(Err(_)) = binding.mux.poll_command() {
                    binding.pending_generated_names.clear();
                }
            }
        }
        let active_config = self.binding.multiplexer.clone();
        self.binding
            .mux
            .set_refresh_interval(mux_session_refresh_interval(window_focused));
        let active_refresh_failed = self
            .binding
            .mux
            .refresh_sessions(&self.repaint, &active_config)
            .is_some();
        if active_refresh_failed && self.binding.automation_event_refresh_pending {
            self.binding.mux.refresh_on_next_frame();
        }
        let active_refresh_completed = self.binding.mux.take_refresh_completed();
        let active_persisted_sessions_restored = match self
            .binding
            .restore_persisted_sessions(active_refresh_completed, &self.repaint)
        {
            Ok(restored) => restored,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        };
        if active_refresh_completed {
            self.prune_native_terminal_snapshot();
        }
        self.resolve_remote_attach_exit_after_refresh(active_refresh_completed);
        let (
            inactive_refresh_completed,
            inactive_persisted_sessions_restored,
            mut refreshed_event_scopes,
        ) = self.refresh_inactive_bindings_for_frame();
        if active_refresh_completed && self.binding.automation_event_refresh_pending {
            refreshed_event_scopes.push(self.binding.scope);
        }
        let sources_refreshed = if active_persisted_sessions_restored
            || inactive_persisted_sessions_restored
            || active_refresh_completed
            || inactive_refresh_completed
        {
            match self.refresh_automation_event_sources(true) {
                Ok(()) => true,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    false
                }
            }
        } else {
            false
        };
        if sources_refreshed {
            if let Err(error) =
                self.reconcile_refreshed_binding_automation_events(&refreshed_event_scopes)
            {
                self.last_error = Some(error.to_string());
            }
        } else {
            for &scope in &refreshed_event_scopes {
                self.retry_binding_automation_event_refresh(scope);
            }
        }
        if let Err(error) = self.publish_backend_automation_events() {
            self.last_error = Some(error.to_string());
        }
        let mux_refresh_after = mux_refresh_repaint_after(&active_config, window_focused);
        let active_scope = self.binding.scope;
        for binding in self
            .binding_runtimes_mut()
            .filter(|binding| binding.scope != active_scope)
        {
            if let Err(error) = binding.sync_session_order() {
                binding.mux.set_error(Some(error.to_string()));
            }
        }
        self.record_restored_persisted_launch_claims();
        if let Some(after) = mux_refresh_after {
            effects.push(AppEffect::RepaintAfter(after));
        }
        self.sync_generated_session_names();
        self.sync_session_order();
        let phase = crate::diagnostics::latency_start();
        let waiting_to_reattach = self
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started);
        if !waiting_to_reattach && let Err(error) = self.sync_terminal_panes() {
            if self.binding.multiplexer.remote.is_some() {
                self.handle_attach_start_failure(now, &error.to_string());
            } else {
                self.last_error = Some(error.to_string());
            }
        }
        crate::diagnostics::trace_slow("frame.sync_terminal_panes", phase, 4.0);
        self.hot_reload_config_if_changed(&mut effects, now);
        self.terminal_view_transform = terminal_view_transform;
        self.restore_mouse_pointer_after_pointer_moved(&events, hover_pos, &mut effects);
        let input_commands = self.handle_direct_input(viewport, &mut effects)
            + self.handle_egui_input(
                events,
                modifiers,
                hover_pos,
                pressed_mouse_button,
                viewport,
                &mut effects,
            )
            + self.handle_dropped_file_paths(dropped_file_paths)
            + self.drive_diagnostic_actions(now, &mut effects);
        // Pane reconciliation and input actions can start a native runtime after the frame's
        // initial PTY drain. Finish against the binding that remains active.
        self.drain_terminal_side_effects(
            &mut effects,
            terminal_cell_width,
            terminal_cell_height,
            terminal_scale_factor,
        );
        self.last_frame_dt_ms = stable_dt_ms;

        let pending_pty_bytes = self.binding.terminal.pending_pty_len();
        let (cols, rows) = self.binding.terminal.grid_size();
        if let Some(trace) = &mut self.stability_trace {
            trace.record(StabilityTraceSample {
                elapsed_ms: trace.started_at.elapsed().as_millis(),
                selected_session: self.binding.mux.selected_session(),
                cols,
                rows,
                pending_pty_bytes,
                drain_bytes: self.last_drain.bytes,
                drain_elapsed_us: self.last_drain.elapsed_us,
                text_runs: renderer_metrics.text_runs,
                last_error: self.last_error.as_deref(),
            });
        }
        if now.duration_since(self.last_status_metrics_sample) >= STATUS_METRICS_SAMPLE_INTERVAL {
            self.status_metrics = StatusMetrics {
                drain: self.last_drain,
                renderer: renderer_metrics,
                cols,
                rows,
            };
            self.last_status_metrics_sample = now;
        }
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes: self.last_drain.bytes,
            drain_elapsed_us: self.last_drain.elapsed_us,
            pending_bytes: pending_pty_bytes,
            dirty_rows: renderer_metrics.dirty_rows,
            cursor_blinking: renderer_metrics.cursor_blinking,
            input_commands,
        });
        let repaint_after = repaint.after.min(CONFIG_HOT_RELOAD_INTERVAL);
        if repaint_after.is_zero() {
            if !effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::RequestRepaint))
            {
                effects.push(AppEffect::RequestRepaint);
            }
        } else {
            effects.push(AppEffect::RepaintAfter(repaint_after));
        }
        crate::diagnostics::trace_slow("frame.update_frame", frame_started, 8.0);
        effects
    }

    /// Only one floating dialog is shown at a time; opening one closes the rest.
    fn close_overlay_dialogs(&mut self) -> bool {
        let restored_preview = self.restore_theme_picker_preview();
        self.theme_picker_restore_config = None;
        self.new_mux_session_dialog = None;
        self.session_picker_dialog = None;
        self.rename_session_dialog = None;
        self.rename_tab_dialog = None;
        self.ditch_session_dialog = None;
        self.keybind_help_dialog = None;
        self.command_palette_dialog = None;
        self.theme_picker_dialog = None;
        self.space_editor_dialog = None;
        self.terminal_find_dialog = None;
        self.terminal_find_return_focus_after_search = false;
        restored_preview
    }

    fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.new_mux_session_dialog = Some(
            self.active_multiplexer()
                .remote
                .clone()
                .map(|remote| NewMuxSessionDialog::open_remote(remote, self.repaint.clone()))
                .unwrap_or_else(NewMuxSessionDialog::open),
        );
        self.input_focus = InputFocus::Picker;
    }
    pub fn open_create_space_dialog_from_ui(&mut self) -> bool {
        self.close_overlay_dialogs();
        let existing_icons = self
            .space_summaries()
            .into_iter()
            .map(|space| space.icon)
            .collect::<Vec<_>>();
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.space_editor_dialog = Some(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter()),
        );
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_edit_space_dialog_from_ui(&mut self, space_id: SpaceId) -> bool {
        let backend = self.space_backend_override(space_id);
        let Some((space, backend)) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
            .zip(backend)
        else {
            return false;
        };
        self.close_overlay_dialogs();
        // Save only this Space's remote override.
        let remote = self
            .space_remote_override(space.id)
            .expect("a listed Space has a remote source");
        let profiles = self
            .config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect::<Vec<_>>();
        self.space_editor_dialog = Some(
            SpaceEditorDialog::edit_space(
                space.id,
                space.name,
                space.icon,
                space.color,
                space.tint_sidebar,
                SpaceMuxOverride { backend, remote },
            )
            .with_profiles(profiles.into_iter()),
        );
        self.input_focus = InputFocus::Picker;
        true
    }

    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }

    fn open_session_picker_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.session_picker_dialog = Some(SessionPickerDialog::open());
        self.input_focus = InputFocus::Picker;
    }

    pub fn open_session_picker_dialog_from_ui(&mut self) -> bool {
        self.open_session_picker_dialog();
        true
    }

    fn toggle_session_picker_dialog(&mut self) {
        if self.session_picker_dialog.is_some() {
            self.session_picker_dialog = None;
            self.input_focus = InputFocus::Terminal;
        } else {
            self.open_session_picker_dialog();
        }
    }

    fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return;
        };
        self.open_rename_session_dialog_for(&selected);
    }

    pub fn open_rename_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, name)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| {
                // Prefill what bootty shows, so a backend-only uniqueness suffix is not something
                // the user has to delete out of the field.
                let name = self
                    .binding
                    .session_names
                    .display_name(&session.id)
                    .unwrap_or(session.name.as_str())
                    .to_owned();
                (session.id.clone(), name)
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.rename_session_dialog = Some(RenameSessionDialog::open(session_id, name));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn open_rename_tab_dialog(&mut self) {
        let Some((session_id, window_id, _)) = self.selected_window_for_rename() else {
            return;
        };
        self.open_rename_tab_dialog_for(&session_id, &window_id);
    }

    pub fn open_rename_tab_dialog_for(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, name)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .map(|window| (session.id.clone(), window.id.clone(), window.name.clone()))
            })
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.rename_tab_dialog = Some(RenameTabDialog::open(session_id, window_id, name));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn selected_window_for_rename(&self) -> Option<(String, String, String)> {
        let selected = self.binding.mux.selected_session()?;
        let session = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == selected || session.name == selected)?;
        let window_id = self
            .binding
            .mux
            .selected_window()
            .or(session.active_window_id.as_deref());
        let window = window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())?;
        Some((session.id.clone(), window.id.clone(), window.name.clone()))
    }

    fn open_ditch_session_dialog(&mut self) {
        let Some(selected) = self.binding.mux.selected_session().map(str::to_owned) else {
            return;
        };
        self.open_ditch_session_dialog_for(&selected);
    }

    pub fn open_ditch_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, cwd)) = self
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id || session.name == session_id)
            .map(|session| (session.id.clone(), session.anchor.cwd.clone()))
        else {
            return false;
        };
        self.close_overlay_dialogs();
        self.ditch_session_dialog = Some(DitchSessionDialog::open(session_id, cwd));
        self.input_focus = InputFocus::Picker;
        true
    }

    fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.keybind_help_dialog = Some(KeybindHelpDialog::open(&bindings));
        self.input_focus = InputFocus::Picker;
    }

    fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.binding.multiplexer.backend);
        self.close_overlay_dialogs();
        self.command_palette_dialog = Some(CommandPaletteDialog::open(&bindings));
        self.input_focus = InputFocus::Picker;
    }

    fn open_terminal_find_dialog(&mut self) {
        self.open_terminal_find_dialog_with_direction(TerminalSearchDirection::Next);
    }

    fn open_terminal_find_dialog_with_direction(&mut self, direction: TerminalSearchDirection) {
        let query = self.last_terminal_search.clone();
        self.close_overlay_dialogs();
        let mut dialog = TerminalFindDialog::open_with_direction(query.clone(), direction);
        if !query.trim().is_empty() {
            let result = self.search_terminal(&query, TerminalSearchDirection::Current);
            dialog.set_result(result);
        }
        self.terminal_find_dialog = Some(dialog);
        self.terminal_find_return_focus_after_search = false;
        self.input_focus = InputFocus::Picker;
    }

    fn open_theme_picker_dialog(&mut self) {
        let config = self.config();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "Light appearance",
            AppearanceVariant::Dark => "Dark appearance",
        };
        let current = config
            .theme_for_appearance(self.active_appearance_variant)
            .map(str::to_owned);
        let config_path = config.config_path.clone();
        let restore_config = config.clone();
        self.close_overlay_dialogs();
        self.theme_picker_restore_config = Some(restore_config);
        self.theme_picker_dialog = Some(ThemePickerDialog::open(
            &config_path,
            current.as_deref(),
            branch,
        ));
        self.input_focus = InputFocus::Picker;
    }

    fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input()
            && self.new_mux_session_dialog.is_none()
            && self.session_picker_dialog.is_none()
            && self.rename_session_dialog.is_none()
            && self.rename_tab_dialog.is_none()
            && self.ditch_session_dialog.is_none()
            && self.keybind_help_dialog.is_none()
            && self.command_palette_dialog.is_none()
            && self.theme_picker_dialog.is_none()
            && self.space_editor_dialog.is_none()
            && !self.lua_window_open
            && !self.settings_open
    }

    fn prepare_profile_bindings(
        &self,
        config: &BoottyConfig,
    ) -> Result<Vec<(MuxScope, BindingRuntime)>> {
        let repaint = self.repaint.clone();
        let variant = self.active_appearance_variant;
        let specs = self
            .binding_runtimes()
            .filter(|binding| matches!(binding.remote_override, SpaceRemoteOverride::Profile(_)))
            .map(|binding| {
                (
                    binding.scope,
                    binding.label.clone(),
                    binding.backend_override,
                    binding.remote_override.clone(),
                )
            })
            .collect::<Vec<_>>();
        specs
            .into_iter()
            .map(|(scope, label, backend_override, remote_override)| {
                binding_runtime_for_multiplexer(BindingRuntimeSpec {
                    config,
                    scope,
                    label,
                    backend_override,
                    remote_override,
                    variant,
                    repaint: repaint.clone(),
                    register_namespace: false,
                    restore_sessions: false,
                })
                .map(|binding| (scope, binding))
            })
            .collect()
    }

    fn commit_profile_bindings(&mut self, replacements: Vec<(MuxScope, BindingRuntime)>) {
        for (scope, replacement) in replacements {
            if let Some(binding) = self.binding_runtime_mut(scope) {
                *binding = replacement;
            }
        }
    }

    #[cfg(test)]
    fn rebuild_profile_bindings(&mut self, config: &BoottyConfig) -> Result<()> {
        let replacements = self.prepare_profile_bindings(config)?;
        if !replacements.is_empty() {
            let path = self.config().config_path.clone();
            let namespaces = replacements.iter().map(|(scope, binding)| {
                (
                    scope.binding_id().persistence_value(),
                    namespace_for_binding(*scope, &binding.multiplexer),
                )
            });
            SessionOrderStore::register_namespaces(&path, namespaces)?;
        }
        self.commit_profile_bindings(replacements);
        Ok(())
    }

    fn reload_config(&mut self, effects: &mut Vec<AppEffect>) -> bool {
        let previous = self.config().clone();
        let path = previous.config_path.clone();
        let next = match load_config_from_path(&path) {
            Ok(config) => config,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        if next.multiplexer != previous.multiplexer {
            let error =
                "live multiplexer changes are unsupported; restart to apply the new backend";
            self.config_state.reject(error);
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        let compatibility_warning = (!next.compatibility_warnings.is_empty())
            .then(|| next.compatibility_warnings.join("; "));
        let modifier_remaps = match next.input.modifier_remaps() {
            Ok(remaps) => remaps,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let keybinds = next
            .input
            .keybinds_for_backend(self.binding.multiplexer.backend);
        let app_key_bindings = match AppKeyBindings::from_keybinds(&keybinds) {
            Ok(bindings) => bindings,
            Err(error) => {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        };
        let sidebar_key_bindings =
            match SidebarKeyBindings::from_keybinds(&next.input.sidebar_keybind) {
                Ok(bindings) => bindings,
                Err(error) => {
                    self.config_state.reject(error.to_string());
                    self.last_error = self.config_state.last_error().map(str::to_owned);
                    return false;
                }
            };

        let mut profile_replacements = if previous.ssh_profiles != next.ssh_profiles {
            match self.prepare_profile_bindings(&next) {
                Ok(replacements) => replacements,
                Err(error) => {
                    self.config_state.reject(error.to_string());
                    self.last_error = self.config_state.last_error().map(str::to_owned);
                    return false;
                }
            }
        } else {
            Vec::new()
        };
        // A profile replacement may still fail at the namespace commit. Defer all active order
        // writes in that case so a rejected profile reload cannot alter unrelated session state.
        let active_order = if !profile_replacements.is_empty() {
            None
        } else {
            let binding_id = self.binding.scope.binding_id().persistence_value();
            let namespace = namespace_for_binding(self.binding.scope, &self.binding.multiplexer);
            if let Err(error) =
                SessionOrderStore::for_binding_preflight(&path, binding_id, namespace)
            {
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
            let session_order = self.binding.session_order.clone();
            let ordered_alive = self
                .binding
                .mux
                .all_sessions()
                .iter()
                .map(|session| session.name.clone())
                .collect::<Vec<_>>();
            Some((session_order, ordered_alive))
        };

        if let Err(error) = self.apply_terminal_reload(&previous, &next) {
            self.config_state.reject(error.to_string());
            self.last_error = self.config_state.last_error().map(str::to_owned);
            return false;
        }
        let active_order = match active_order {
            Some((mut session_order, ordered_alive)) => {
                match session_order
                    .sync_sessions(ordered_alive.iter().map(|session| session.as_str()))
                {
                    Ok(ordered_names) => Some((session_order, ordered_names)),
                    Err(error) => {
                        #[cfg(test)]
                        self.binding.session_order.clear_save_failure_for_test();
                        let _ = self.apply_terminal_reload(&next, &previous);
                        self.config_state.reject(error.to_string());
                        self.last_error = self.config_state.last_error().map(str::to_owned);
                        return false;
                    }
                }
            }
            None => None,
        };

        if !profile_replacements.is_empty() {
            let namespaces = profile_replacements.iter().map(|(scope, binding)| {
                (
                    scope.binding_id().persistence_value(),
                    namespace_for_binding(*scope, &binding.multiplexer),
                )
            });
            if let Err(error) = SessionOrderStore::register_namespaces(&path, namespaces) {
                let _ = self.apply_terminal_reload(&next, &previous);
                self.config_state.reject(error.to_string());
                self.last_error = self.config_state.last_error().map(str::to_owned);
                return false;
            }
        }

        self.commit_profile_bindings(std::mem::take(&mut profile_replacements));
        let active_appearance_variant = self.active_appearance_variant;
        for binding in self.binding_runtimes_mut() {
            let mut binding_config = next.clone();
            binding_config.multiplexer = binding.multiplexer.clone();
            let session_config = terminal_session_config_with_side_effects(
                &binding_config,
                active_appearance_variant,
                &binding.terminal_side_effect_tx,
            );
            binding.terminal.set_terminal_config(session_config);
        }
        if let Some(owner) = &mut self.parked_native_terminal {
            let mut owner_config = next.clone();
            owner_config.multiplexer.backend = crate::config::MultiplexerBackendConfig::Native;
            let session_config = terminal_session_config_with_side_effects(
                &owner_config,
                active_appearance_variant,
                &owner.terminal_side_effect_tx,
            );
            owner.terminal.set_terminal_config(session_config);
        }

        if let Some((session_order, ordered_names)) = active_order {
            let config_path = next.config_path.clone();
            let binding_id = self.binding.scope.binding_id().persistence_value();
            self.binding.session_names = SessionNameStore::for_binding(&config_path, binding_id);
            self.binding.pending_generated_names.clear();
            self.binding.session_order = session_order;
            self.binding.mux.apply_session_order(&ordered_names);
        }
        let new_session_changes = new_session_only_config_changed(&previous, &next);
        self.config_state.accept(next);
        self.modifier_remaps = modifier_remaps;
        self.macos_option_as_alt = self.config().input.macos_option_as_alt.into();
        self.app_key_bindings = app_key_bindings;
        self.sidebar_key_bindings = sidebar_key_bindings;
        self.has_new_session_config_changes =
            new_session_changes || self.has_new_session_config_changes;
        if previous.font != self.config().font {
            effects.push(AppEffect::SetTerminalTextConfig(
                self.config().font.terminal_text_config(),
            ));
            if previous.font.ui_families() != self.config().font.ui_families() {
                effects.push(AppEffect::SetUiFonts(
                    self.config().font.ui_families().to_vec(),
                ));
            }
        }
        if previous.window.title != self.config().window.title {
            effects.push(AppEffect::SetWindowTitle(
                self.config().window.title.clone(),
            ));
        }
        if previous.diagnostics != self.config().diagnostics {
            self.stability_trace = StabilityTrace::from_config(self.config());
        }
        self.set_mouse_pointer_hidden_while_typing(self.mouse_pointer_hidden_while_typing, effects);
        self.last_error = match (self.has_new_session_config_changes, compatibility_warning) {
            (true, Some(warning)) => Some(format!(
                "config reloaded; session/window settings require a new window or restart; {warning}"
            )),
            (true, None) => Some(
                "config reloaded; session/window settings require a new window or restart"
                    .to_owned(),
            ),
            (false, warning) => warning,
        };
        effects.push(AppEffect::RequestRepaint);
        true
    }

    fn hot_reload_config_if_changed(&mut self, effects: &mut Vec<AppEffect>, now: Instant) {
        if !self.config_hot_reload.changed(now) {
            return;
        }
        let path = self.config().config_path.clone();
        if self.reload_config(effects) {
            self.config_hot_reload.refresh_after_reload(&path);
        }
    }

    fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<CommandInvocation>) {
        split_app_actions_for_bindings_with_modifier_sides(
            &mut self.app_key_bindings,
            events,
            self.modifier_sides,
        )
    }

    /// While the command palette is open, find and remove the configure-keybinding
    /// chord (`cmd+shift+,` on macOS, `ctrl+shift+,` elsewhere) from `events` so it
    /// doesn't also trigger whatever global binding shares that chord. Returns
    /// whether one was consumed.
    fn take_configure_keybind_chord(&self, events: &mut Vec<egui::Event>) -> bool {
        if self.command_palette_dialog.is_none() {
            return false;
        }
        let macos = cfg!(target_os = "macos");
        let Some(index) = events.iter().position(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Comma,
                    pressed: true,
                    modifiers,
                    ..
                } if if macos {
                    modifiers.shift && (modifiers.command || modifiers.mac_cmd)
                        && !modifiers.alt && !modifiers.ctrl
                } else {
                    modifiers.shift && modifiers.ctrl && !modifiers.alt
                }
            )
        }) else {
            return false;
        };
        events.remove(index);
        true
    }

    fn terminal_mouse_tracking_for_selection(
        &mut self,
        events: &[egui::Event],
        terminal_input_enabled: bool,
        pressed_mouse_button: Option<MouseButton>,
    ) -> bool {
        let primary_drag_active = pressed_mouse_button == Some(MouseButton::Left);
        if !terminal_input_enabled
            || !events.iter().any(|event| match event {
                egui::Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    ..
                } => true,
                egui::Event::PointerMoved(_) => primary_drag_active,
                _ => false,
            })
        {
            return false;
        }

        match TerminalRenderSource::is_mouse_tracking(self.binding.terminal.as_mut()) {
            Ok(mouse_tracking) => mouse_tracking,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn apply_terminal_selection_actions(
        &mut self,
        actions: Vec<TerminalSelectionAction>,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let count = actions.len();
        for action in actions {
            let copy_on_select = self.config().input.copy_on_select
                && matches!(&action, TerminalSelectionAction::End(_));
            let result = match action {
                TerminalSelectionAction::Begin(event) => {
                    TerminalRenderSource::begin_selection(self.binding.terminal.as_mut(), event)
                }
                TerminalSelectionAction::Scroll(delta) => {
                    TerminalRenderSource::scroll_viewport_delta(
                        self.binding.terminal.as_mut(),
                        delta,
                    )
                }
                TerminalSelectionAction::Update(event) => {
                    TerminalRenderSource::update_selection(self.binding.terminal.as_mut(), event)
                }
                TerminalSelectionAction::End(event) => {
                    TerminalRenderSource::end_selection(self.binding.terminal.as_mut(), event)
                }
            };
            match result {
                Ok(()) => {
                    effects.push(AppEffect::RequestRepaint);
                    if copy_on_select {
                        self.copy_terminal_selection_if_any(CopyToClipboard::Mixed);
                    }
                }
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
        count
    }

    fn terminal_copy_mode_active(&mut self) -> bool {
        match TerminalRenderSource::copy_mode_active(self.binding.terminal.as_mut()) {
            Ok(active) => active,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        match TerminalRenderSource::enter_copy_mode(self.binding.terminal.as_mut()) {
            Ok(()) => effects.push(AppEffect::RequestRepaint),
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn apply_copy_mode_key_action(
        &mut self,
        action: CopyModeKeyAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        match action {
            CopyModeKeyAction::Terminal(action) => {
                self.apply_terminal_copy_mode_action(action, effects)
            }
            CopyModeKeyAction::SearchPrompt(direction) => {
                self.record_terminal_search_direction(direction);
                self.open_terminal_find_dialog_with_direction(direction);
                self.terminal_find_return_focus_after_search = true;
                effects.push(AppEffect::RequestRepaint);
                true
            }
            CopyModeKeyAction::SearchWord(direction) => self.apply_terminal_copy_mode_action(
                TerminalCopyModeAction::SearchWord(direction),
                effects,
            ),
            CopyModeKeyAction::SearchRepeat(repeat) => {
                let direction = repeat.direction(self.last_terminal_search_direction);
                let query = self.last_terminal_search.clone();
                if !query.trim().is_empty() {
                    let result =
                        self.search_terminal_with_direction_recording(&query, direction, false);
                    if let Some(dialog) = self.terminal_find_dialog.as_mut() {
                        dialog.set_result(result);
                    }
                    effects.push(AppEffect::RequestRepaint);
                }
                true
            }
        }
    }

    fn record_terminal_search_direction(&mut self, direction: TerminalSearchDirection) {
        if direction != TerminalSearchDirection::Current {
            self.last_terminal_search_direction = direction;
        }
    }

    fn apply_terminal_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
        effects: &mut Vec<AppEffect>,
    ) -> bool {
        let search_direction = match &action {
            TerminalCopyModeAction::Search { direction, .. }
            | TerminalCopyModeAction::SearchWord(direction) => Some(*direction),
            _ => None,
        };
        match TerminalRenderSource::handle_copy_mode_action(self.binding.terminal.as_mut(), action)
        {
            Ok(outcome) => {
                if let Some(bytes) = outcome.copied {
                    let text = String::from_utf8_lossy(&bytes);
                    if let Err(error) = write_clipboard_text(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                let search_result = outcome.search.map(|search| {
                    self.last_terminal_search = search.query;
                    if let Some(direction) = search_direction {
                        self.record_terminal_search_direction(direction);
                    }
                    self.terminal_find_result_from_frame(search.found)
                });
                if let Some(result) = search_result
                    && let Some(dialog) = self.terminal_find_dialog.as_mut()
                {
                    dialog.set_result(result);
                }
                effects.push(AppEffect::RequestRepaint);
                outcome.active
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn consume_copy_mode_egui_events(
        &mut self,
        events: &mut Vec<egui::Event>,
        effects: &mut Vec<AppEffect>,
        terminal_input_enabled: bool,
    ) -> usize {
        if !terminal_input_enabled
            || (self.terminal_find_dialog.is_some() && self.input_focus != InputFocus::Terminal)
            || !copy_mode_key_input_present(events)
            || !self.terminal_copy_mode_active()
        {
            return 0;
        }

        let mut count = 0;
        let mut retained = Vec::with_capacity(events.len());
        let mut suppress_next_text = false;
        let mut pass_next_text_to_app = false;
        for event in events.drain(..) {
            match &event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } if copy_mode_egui_key_should_pass_to_app(*key, *modifiers) => {
                    pass_next_text_to_app = copy_mode_egui_key_may_emit_text(*key);
                    retained.push(event);
                }
                egui::Event::Text(_) if std::mem::take(&mut pass_next_text_to_app) => {
                    retained.push(event);
                }
                _ if matches!(event, egui::Event::Key { .. } | egui::Event::Text(_)) => {
                    pass_next_text_to_app = false;
                    count += 1;
                    if let Some(action) =
                        copy_mode_action_for_egui_event(&event, &mut suppress_next_text)
                    {
                        self.apply_copy_mode_key_action(action, effects);
                    }
                }
                _ => {
                    pass_next_text_to_app = false;
                    retained.push(event);
                }
            }
        }
        *events = retained;
        count
    }

    fn handle_egui_input(
        &mut self,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
        hover_pos: Option<Pos2>,
        pressed_mouse_button: Option<MouseButton>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let suppress_next_egui_paste = std::mem::take(&mut self.suppress_next_egui_paste);
        let mut events = events;
        if suppress_next_egui_paste {
            remove_first_paste_event(&mut events);
        }
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let selection_surface = terminal_input_enabled
            .then_some(self.terminal_surface)
            .flatten();
        let mouse_tracking = self.terminal_mouse_tracking_for_selection(
            &events,
            terminal_input_enabled,
            pressed_mouse_button,
        );
        let mut chrome_handle_rects = self.chrome_handle_rects.clone();
        if let Some(rect) = self
            .terminal_find_dialog
            .as_ref()
            .and_then(TerminalFindDialog::last_rect)
        {
            chrome_handle_rects.push(rect);
        }
        let (mut events, mut selection_actions) = self.terminal_selection.route_events(
            events,
            TerminalSelectionRouteContext {
                surface: selection_surface,
                view: self.terminal_view_transform,
                mouse_tracking,
                frame_modifiers: modifiers,
                chrome_handle_rects: &chrome_handle_rects,
            },
        );
        selection_actions.extend(self.terminal_selection.autoscroll_actions(
            selection_surface,
            self.terminal_view_transform,
            modifiers,
        ));
        let selection_count = self.apply_terminal_selection_actions(selection_actions, effects);
        let copy_mode_count =
            self.consume_copy_mode_egui_events(&mut events, effects, terminal_input_enabled);
        let copy_selection_count = self.consume_copy_shortcut_for_terminal_selection(&mut events);
        // `cmd+shift+,` over a palette row jumps to that command's keybinding editor.
        // Consume it here so it doesn't also fire its own global binding.
        if self.take_configure_keybind_chord(&mut events) {
            let action = self
                .command_palette_dialog
                .as_ref()
                .and_then(CommandPaletteDialog::current_action)
                .map(str::to_owned);
            self.close_overlay_dialogs();
            self.input_focus = InputFocus::Terminal;
            if let Some(action) = action {
                effects.push(AppEffect::ConfigureKeybind(action));
            }
        }
        let (events, actions) = self.split_app_actions(events);
        let routed = if self.terminal_find_dialog.is_some() {
            route_find_modeless_events(
                self.input_focus,
                events,
                self.terminal_find_dialog
                    .as_ref()
                    .and_then(TerminalFindDialog::last_rect),
                hover_pos,
            )
        } else {
            route_events(self.input_focus, events)
        };
        let sidebar_count = self.handle_sidebar_input(routed.ui_events, viewport, effects);
        let events = if terminal_input_enabled || self.terminal_find_dialog.is_some() {
            routed.terminal_events
        } else {
            Vec::new()
        };
        let snapshot = InputSnapshot {
            events,
            modifiers,
            modifier_sides: self.modifier_sides,
            hover_pos,
            pressed_mouse_button,
            surface: self.terminal_surface,
            mouse_exclusion: self
                .terminal_surface
                .map(crate::renderer::scrollbar_hit_rect),
            view: self.terminal_view_transform,
        };
        let commands = terminal_input_commands_with_wheel_state(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
            &mut self.wheel_scroll_state,
        );
        let count = commands.len()
            + actions.len()
            + sidebar_count
            + selection_count
            + copy_mode_count
            + copy_selection_count;

        for invocation in actions {
            let _ = self.dispatch_command(invocation, viewport, effects);
        }

        for command in commands {
            self.apply_terminal_input(command, effects);
        }

        count
    }

    fn handle_dropped_file_paths(&mut self, paths: Vec<PathBuf>) -> usize {
        if !self.direct_terminal_input_enabled() {
            return 0;
        }
        if paths.is_empty() {
            return 0;
        }
        if self.binding.multiplexer.remote.is_some() {
            self.last_error = Some("File handoff to remote Spaces is not supported.".to_owned());
            return 0;
        }
        let text = match local_file_handoff(&paths) {
            LocalFileHandoff::Ready(text) => text,
            LocalFileHandoff::Rejected(message) => {
                self.last_error = Some(message.to_owned());
                return 0;
            }
        };
        if let Err(error) = self.binding.terminal.write_paste(&text) {
            self.last_error = Some(error.to_string());
            return 0;
        }
        1
    }

    fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        // While settings is open, leave the pending direct input untouched so the keybind recorder
        // can read it in the UI pass; the terminal behind settings must not consume it.
        if self.settings_open {
            return self.pending_direct_input.len();
        }
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if count == 0 {
            return 0;
        }
        if !self.direct_terminal_input_enabled() {
            return count;
        }

        let mut copy_mode_active = self.terminal_copy_mode_active();
        for input in inputs {
            let mut input = input.input();
            input.mods = self.modifier_remaps.apply(input.mods);
            if copy_mode_active {
                if let Some(action) = copy_mode_action_for_input(input) {
                    copy_mode_active = self.apply_copy_mode_key_action(action, effects);
                    continue;
                }
                if !copy_mode_input_should_pass_to_app(input) {
                    continue;
                }
            }
            if direct_copy_shortcut_pressed(input)
                && self.copy_terminal_selection_if_any(CopyToClipboard::Mixed)
            {
                continue;
            }
            if let Some(invocation) = self.app_key_bindings.invocation_for_input(input) {
                if invocation.command == "paste_from_clipboard" {
                    self.suppress_next_egui_paste = true;
                }
                let _ = self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if let Some(invocation) = builtin_app_invocation_for_direct_key(input) {
                self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if copy_mode_active {
                continue;
            }
            if input.mods.command {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input), effects);
        }
        count
    }

    fn handle_sidebar_input(
        &mut self,
        events: Vec<egui::Event>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if self.input_focus != InputFocus::Sidebar {
            return 0;
        }
        self.ensure_sidebar_hovered_session();
        let mut count = 0;
        for event in events {
            count += 1;
            let egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = event
            else {
                continue;
            };
            let Some(invocation) = self.sidebar_key_bindings.invocation_for_key(key, modifiers)
            else {
                continue;
            };
            self.dispatch_command(invocation, viewport, effects);
        }
        count
    }

    /// Returns a non-blocking sender for producers outside the UI-owner call stack.
    ///
    /// UI code dispatches directly and must not synchronously wait on this channel's response.
    pub fn app_command_sender(&self, caller: Caller) -> BoundAppCommandSender {
        self.app_command_tx.for_caller(caller)
    }

    fn drain_reconciliation_completions(&mut self) -> bool {
        let mut completed = false;
        while let Ok(reconciliation) = self.reconciliation_rx.try_recv() {
            completed = true;
            match reconciliation {
                ShutdownReconciliationCompletion::Mux {
                    request_id,
                    command_id,
                    command,
                    origin,
                    binding_identity,
                    binding_generation,
                    namespace,
                    target,
                    completion,
                    result,
                } => {
                    let outcome = self.command_outcome_for_mux_result(
                        MuxCompletionContext {
                            command_id: &command_id,
                            origin,
                            binding_identity: &binding_identity,
                            binding_generation,
                            namespace: &namespace,
                            command: &command,
                            rename: None,
                        },
                        *result,
                    );
                    let outcome = bounded_command_outcome(outcome);
                    if let Some(message) = command_outcome_message(&outcome) {
                        self.last_error = Some(message);
                    }
                    self.publish_reconciled_command_completion(
                        request_id,
                        &command_id,
                        target.as_ref(),
                        completion.as_ref(),
                        &outcome,
                    );
                }
                ShutdownReconciliationCompletion::Extension {
                    request_id,
                    command_id,
                    invocation,
                    extension_id,
                    generation,
                    target,
                    completion,
                    result,
                } => {
                    let outcome = if self
                        .extension_runtime
                        .generation_is_active(&extension_id, generation)
                    {
                        result
                    } else {
                        CommandOutcome::Failed {
                            code: "stale_generation".to_owned(),
                            message: format!(
                                "extension command generation was reloaded while reconciling {}",
                                invocation.command
                            ),
                        }
                    };
                    let outcome = bounded_command_outcome(outcome);
                    if let Some(message) = command_outcome_message(&outcome) {
                        self.last_error = Some(message);
                    }
                    self.publish_reconciled_command_completion(
                        request_id,
                        &command_id,
                        target.as_ref(),
                        completion.as_ref(),
                        &outcome,
                    );
                }
            }
        }
        completed
    }

    fn drain_app_commands(&mut self, viewport: ViewportSnapshot, effects: &mut Vec<AppEffect>) {
        self.retry_pending_completion_publications();
        let reconciled = self.drain_reconciliation_completions();
        let mux_completed = self.drain_pending_app_commands(Instant::now());
        let extension_completed = self.drain_pending_extension_commands(Instant::now());
        if reconciled || mux_completed || extension_completed {
            effects.push(AppEffect::RequestRepaint);
        }
        let mut drained = 0;
        for _ in 0..32 {
            let request = match self.app_command_rx.try_recv() {
                Ok(request) => request,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            };
            drained += 1;
            let now = Instant::now();
            let request_id = next_app_command_reconciliation_id();
            let request_command_id = request.invocation.command.clone();
            let completion_target = self.completion_target_for_invocation(&request.invocation);
            let mut completion = request.completion;
            if let Some(context) = completion.as_mut()
                && completion_target.is_some()
            {
                context.target = completion_target.clone();
            }
            let started_deadline = now >= request.deadline && request.cancellation.is_started();
            if started_deadline {
                let indeterminate = CommandOutcome::completion_indeterminate();
                if request.response.clone().send(indeterminate.clone()).is_ok() {
                    if let Some(completion) = completion.as_ref() {
                        self.publish_command_completion_event(
                            request_id,
                            &request_command_id,
                            completion_target.as_ref(),
                            Some(completion),
                            &indeterminate,
                            false,
                        );
                    }
                    continue;
                }
            }
            let dispatch = if request.cancellation.is_cancelled() {
                CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "command was cancelled".to_owned(),
                })
            // The cancellation CAS is the commit gate: only a still-pending request may time out.
            } else if now >= request.deadline && request.cancellation.cancel() {
                CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "command deadline expired".to_owned(),
                })
            } else {
                self.dispatch_command_with_execution(
                    request.invocation,
                    viewport,
                    effects,
                    Some((request.deadline, request.cancellation.clone())),
                )
            };
            match dispatch {
                CommandDispatch::Complete(outcome) => {
                    let outcome = bounded_command_outcome(outcome);
                    let disconnected = request.response.send(outcome.clone()).is_err();
                    if (!disconnected || request.cancellation.is_started()) && completion.is_some()
                    {
                        if disconnected {
                            self.publish_reconciled_command_completion(
                                request_id,
                                &request_command_id,
                                completion_target.as_ref(),
                                completion.as_ref(),
                                &outcome,
                            );
                        } else {
                            self.publish_command_completion_event(
                                request_id,
                                &request_command_id,
                                completion_target.as_ref(),
                                completion.as_ref(),
                                &outcome,
                                false,
                            );
                        }
                    }
                }
                CommandDispatch::Pending {
                    command,
                    command_id,
                    origin,
                    binding_identity,
                    binding_generation,
                    namespace,
                    target,
                    deadline,
                    cancellation,
                    result,
                } => {
                    self.pending_app_commands.push(PendingAppCommand {
                        request_id,
                        command,
                        command_id,
                        origin,
                        binding_identity,
                        binding_generation,
                        namespace,
                        target,
                        deadline,
                        cancellation,
                        response: Some(request.response),
                        completion,
                        rename: None,
                        result,
                    });
                }
                CommandDispatch::ExtensionPending {
                    invocation,
                    extension_id,
                    generation,
                    target,
                    deadline,
                    cancellation,
                    result,
                } => {
                    self.pending_extension_commands
                        .push(PendingExtensionCommand {
                            request_id,
                            invocation,
                            extension_id,
                            generation,
                            target,
                            deadline,
                            cancellation,
                            response: Some(request.response),
                            completion,
                            result,
                        });
                }
            }
        }
        if drained == 32 {
            effects.push(AppEffect::RequestRepaint);
        }
    }

    fn drain_pending_app_commands(&mut self, now: Instant) -> bool {
        let mut completed = false;
        for mut pending in std::mem::take(&mut self.pending_app_commands) {
            if now >= pending.deadline && pending.cancellation.is_started() {
                pending.cancellation.request_cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::completion_indeterminate());
                }
                enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Mux(
                    ShutdownMuxReconciliation {
                        request_id: pending.request_id,
                        command_id: pending.command_id,
                        command: pending.command,
                        origin: pending.origin,
                        binding_identity: pending.binding_identity,
                        binding_generation: pending.binding_generation,
                        namespace: pending.namespace,
                        result: pending.result,
                        deadline: pending
                            .deadline
                            .checked_add(SHUTDOWN_RECONCILIATION_GRACE)
                            .unwrap_or_else(Instant::now),
                        cancellation: pending.cancellation,
                        target: pending.target,
                        completion: pending.completion,
                        reconciliation: self.reconciliation_tx.clone(),
                        automation: self.automation.clone(),
                        scope: automation_event_scope(pending.origin),
                        fallback_scope: format!("instance:{}", self.command_instance_handle),
                    },
                ));
                completed = true;
                continue;
            }
            let outcome = if pending.cancellation.is_cancelled() {
                let outcome = self.finalize_failed_session_launch(
                    pending.origin,
                    &pending.namespace,
                    &pending.command,
                    CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message: "command was cancelled".to_owned(),
                    },
                );
                self.clear_failed_session_rename(
                    pending.origin,
                    &pending.command_id,
                    pending.rename.as_ref(),
                )
                .unwrap_or(outcome)
            } else if now >= pending.deadline && pending.cancellation.cancel() {
                let outcome = self.finalize_failed_session_launch(
                    pending.origin,
                    &pending.namespace,
                    &pending.command,
                    CommandOutcome::Failed {
                        code: "deadline_exceeded".to_owned(),
                        message: "command deadline expired".to_owned(),
                    },
                );
                self.clear_failed_session_rename(
                    pending.origin,
                    &pending.command_id,
                    pending.rename.as_ref(),
                )
                .unwrap_or(outcome)
            } else {
                match pending.result.try_recv() {
                    Ok(result) => self.command_outcome_for_mux_result(
                        MuxCompletionContext {
                            command_id: &pending.command_id,
                            origin: pending.origin,
                            binding_identity: &pending.binding_identity,
                            binding_generation: pending.binding_generation,
                            namespace: &pending.namespace,
                            command: &pending.command,
                            rename: pending.rename.as_ref(),
                        },
                        result,
                    ),
                    Err(mpsc::TryRecvError::Empty) => {
                        self.pending_app_commands.push(pending);
                        continue;
                    }
                    Err(mpsc::TryRecvError::Disconnected) => {
                        let outcome = self.finalize_failed_session_launch(
                            pending.origin,
                            &pending.namespace,
                            &pending.command,
                            CommandOutcome::Failed {
                                code: "backend_worker_stopped".to_owned(),
                                message: "mux command worker stopped".to_owned(),
                            },
                        );
                        self.clear_failed_session_rename(
                            pending.origin,
                            &pending.command_id,
                            pending.rename.as_ref(),
                        )
                        .unwrap_or(outcome)
                    }
                }
            };
            let outcome = bounded_command_outcome(outcome);
            completed = true;
            if pending.response.is_none()
                && let Some(message) = command_outcome_message(&outcome)
            {
                self.last_error = Some(message);
            }
            if let Some(response) = pending.response {
                let disconnected = response.send(outcome.clone()).is_err();
                if (!disconnected || pending.cancellation.is_started())
                    && pending.completion.is_some()
                {
                    if disconnected {
                        self.publish_reconciled_command_completion(
                            pending.request_id,
                            &pending.command_id,
                            pending.target.as_ref(),
                            pending.completion.as_ref(),
                            &outcome,
                        );
                    } else {
                        self.publish_command_completion_event(
                            pending.request_id,
                            &pending.command_id,
                            pending.target.as_ref(),
                            pending.completion.as_ref(),
                            &outcome,
                            false,
                        );
                    }
                }
            } else if pending.completion.is_some() {
                self.publish_reconciled_command_completion(
                    pending.request_id,
                    &pending.command_id,
                    pending.target.as_ref(),
                    pending.completion.as_ref(),
                    &outcome,
                );
            }
        }
        completed
    }
    fn drain_pending_extension_commands(&mut self, now: Instant) -> bool {
        let mut completed = false;
        for mut pending in std::mem::take(&mut self.pending_extension_commands) {
            let generation_active = self
                .extension_runtime
                .generation_is_active(&pending.extension_id, pending.generation);
            if now >= pending.deadline && pending.cancellation.is_started() {
                pending.cancellation.request_cancel();
                if let Some(response) = pending.response.take() {
                    let _ = response.send(CommandOutcome::completion_indeterminate());
                }
                enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Extension(
                    ShutdownExtensionReconciliation {
                        request_id: pending.request_id,
                        command_id: pending.invocation.command.clone(),
                        invocation: pending.invocation,
                        extension_id: pending.extension_id,
                        generation: pending.generation,
                        result: pending.result,
                        deadline: pending
                            .deadline
                            .checked_add(SHUTDOWN_RECONCILIATION_GRACE)
                            .unwrap_or_else(Instant::now),
                        cancellation: pending.cancellation,
                        target: pending.target,
                        completion: pending.completion,
                        reconciliation: self.reconciliation_tx.clone(),
                        automation: self.automation.clone(),
                        scope: format!("instance:{}", self.command_instance_handle),
                        fallback_scope: format!("instance:{}", self.command_instance_handle),
                    },
                ));
                completed = true;
                continue;
            }
            let outcome = if !generation_active {
                CommandOutcome::Failed {
                    code: "stale_generation".to_owned(),
                    message: "extension command generation was reloaded".to_owned(),
                }
            } else if pending.cancellation.is_cancelled() {
                CommandOutcome::Failed {
                    code: "cancelled".to_owned(),
                    message: "extension command was cancelled".to_owned(),
                }
            } else if now >= pending.deadline && pending.cancellation.cancel() {
                CommandOutcome::Failed {
                    code: "deadline_exceeded".to_owned(),
                    message: "extension command deadline expired".to_owned(),
                }
            } else {
                match pending.result.try_recv() {
                    Ok(outcome) => outcome,
                    Err(mpsc::TryRecvError::Empty) => {
                        if !self
                            .extension_runtime
                            .generation_is_active(&pending.extension_id, pending.generation)
                        {
                            CommandOutcome::Failed {
                                code: "stale_generation".to_owned(),
                                message: "extension command generation was reloaded".to_owned(),
                            }
                        } else {
                            self.pending_extension_commands.push(pending);
                            continue;
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected) => CommandOutcome::Failed {
                        code: "extension_worker_stopped".to_owned(),
                        message: "extension command worker stopped".to_owned(),
                    },
                }
            };
            let outcome = if self
                .extension_runtime
                .generation_is_active(&pending.extension_id, pending.generation)
            {
                outcome
            } else {
                CommandOutcome::Failed {
                    code: "stale_generation".to_owned(),
                    message: "extension command generation was reloaded".to_owned(),
                }
            };
            let outcome = bounded_command_outcome(outcome);
            completed = true;
            if pending.response.is_none()
                && let Some(message) = command_outcome_message(&outcome)
            {
                self.last_error = Some(message);
            }
            if let Some(response) = pending.response {
                let disconnected = response.send(outcome.clone()).is_err();
                if (!disconnected || pending.cancellation.is_started())
                    && pending.completion.is_some()
                {
                    if disconnected {
                        self.publish_reconciled_command_completion(
                            pending.request_id,
                            &pending.invocation.command,
                            pending.target.as_ref(),
                            pending.completion.as_ref(),
                            &outcome,
                        );
                    } else {
                        self.publish_command_completion_event(
                            pending.request_id,
                            &pending.invocation.command,
                            pending.target.as_ref(),
                            pending.completion.as_ref(),
                            &outcome,
                            false,
                        );
                    }
                }
            } else if pending.completion.is_some() {
                self.publish_reconciled_command_completion(
                    pending.request_id,
                    &pending.invocation.command,
                    pending.target.as_ref(),
                    pending.completion.as_ref(),
                    &outcome,
                );
            }
        }
        completed
    }

    fn enqueue_authoritative_mux_command(
        &mut self,
        command_id: impl Into<String>,
        command: MuxCommand,
        origin: MuxScope,
        target: Option<CommandTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = execution.unwrap_or_else(|| {
            (
                Instant::now() + Duration::from_secs(10),
                CommandCancellation::new(),
            )
        });
        let Some(binding) = self.binding_runtime(origin) else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            });
        };
        if let Some(message) = binding.mux.unavailable_reason() {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        if let Some(outcome) = command_outcome_for_binding_operation(
            binding
                .mux
                .operation_outcome(&binding.multiplexer, command.operation()),
        ) {
            return CommandDispatch::Complete(outcome);
        }
        let binding_identity = self.binding_identity(binding);
        let binding_generation = binding.mux.binding_generation();
        let namespace = namespace_for_binding(binding.scope, &binding.multiplexer);
        let repaint = self.repaint.clone();
        let Some(binding) = self.binding_runtime_mut(origin) else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            });
        };
        let config = binding.multiplexer.clone();
        let selected_session_before = matches!(command, MuxCommand::RenameSession { .. })
            .then(|| binding.mux.selected_session().map(str::to_owned))
            .flatten();
        let result = binding.mux.execute_command_authoritatively(
            &repaint,
            &config,
            command.clone(),
            deadline,
            cancellation.clone(),
        );
        if let Some(selected_session) = selected_session_before {
            binding.mux.activate_session(&selected_session);
        }
        CommandDispatch::Pending {
            command,
            command_id: command_id.into(),
            origin,
            binding_identity,
            binding_generation,
            namespace,
            target,
            deadline,
            cancellation,
            result,
        }
    }

    fn dispatch_command(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> CommandOutcome {
        match self.dispatch_command_with_execution(invocation, viewport, effects, None) {
            CommandDispatch::Complete(outcome) => outcome,
            CommandDispatch::Pending {
                command,
                command_id,
                origin,
                binding_identity,
                binding_generation,
                namespace,
                target,
                deadline,
                cancellation,
                result,
            } => {
                let outcome = CommandOutcome::pending(target.clone());
                self.pending_app_commands.push(PendingAppCommand {
                    request_id: next_app_command_reconciliation_id(),
                    command,
                    command_id,
                    origin,
                    binding_identity,
                    binding_generation,
                    namespace,
                    target,
                    deadline,
                    cancellation,
                    response: None,
                    completion: None,
                    rename: None,
                    result,
                });
                effects.push(AppEffect::RequestRepaint);
                outcome
            }
            CommandDispatch::ExtensionPending {
                invocation,
                extension_id,
                generation,
                target,
                deadline,
                cancellation,
                result,
            } => {
                let outcome = CommandOutcome::pending(target.clone());
                self.pending_extension_commands
                    .push(PendingExtensionCommand {
                        request_id: next_app_command_reconciliation_id(),
                        invocation,
                        extension_id,
                        generation,
                        target,
                        deadline,
                        cancellation,
                        response: None,
                        completion: None,
                        result,
                    });
                outcome
            }
        }
    }

    fn dispatch_command_with_execution(
        &mut self,
        invocation: CommandInvocation,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if invocation.command == "new_window"
            && matches!(
                invocation.caller,
                Caller::Keybinding | Caller::BuiltinKeybinding | Caller::CommandPalette
            )
        {
            if invocation.target.is_some() {
                return CommandDispatch::Complete(CommandOutcome::Unsupported {
                    message: "new_window is a local UI action and does not accept a target"
                        .to_owned(),
                });
            }
            if let Err(outcome) = Self::begin_synchronous_command(execution) {
                return CommandDispatch::Complete(outcome);
            }
            // Explicitly source-manifested as unsupported: it is not an
            // automation command, but remains a local typed keybinding action.
            self.apply_keybind_action(KeybindAction::App(AppAction::NewWindow), viewport, effects);
            return CommandDispatch::Complete(CommandOutcome::success());
        }

        let mut resolved = match self
            .extension_runtime
            .command_registry()
            .resolve(invocation)
        {
            Ok(resolved) => resolved,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        let target = match self.resolve_command_target(
            resolved.descriptor.id.as_str(),
            resolved.descriptor.target,
            resolved.invocation.target.as_ref(),
        ) {
            Ok(target) => target,
            Err(outcome) => {
                self.last_error = command_outcome_message(&outcome);
                return CommandDispatch::Complete(outcome);
            }
        };
        resolved.invocation.target = target.as_ref().map(|target| target.target.clone());
        let mux_scope = target.as_ref().and_then(|target| target.mux_scope);
        if let Some(outcome) = self.preflight_command(&resolved.executor, mux_scope) {
            self.last_error = command_outcome_message(&outcome);
            return CommandDispatch::Complete(outcome);
        }
        if resolved.descriptor.mutation == MutationClass::Destructive
            && !matches!(
                &resolved.executor,
                CoreCommandExecutor::WorktreeRemove { .. }
            )
            && matches!(
                resolved.invocation.caller,
                Caller::Cli | Caller::Socket | Caller::Luau
            )
            && resolved.invocation.confirmation.as_ref()
                != Some(&resolved.invocation.confirmation())
        {
            return CommandDispatch::Complete(CommandOutcome::ConfirmationRequired {
                confirmation: Box::new(resolved.invocation.confirmation()),
            });
        }
        let invocation = resolved.invocation.clone();
        let target = resolved.invocation.target;
        let context = ResolvedCommandContext {
            target: target.as_ref(),
            mux_scope,
            caller: invocation.caller,
            viewport,
            execution,
            invocation,
        };
        self.dispatch_resolved_command(resolved.descriptor.id, resolved.executor, context, effects)
    }

    fn preflight_command(
        &self,
        executor: &CoreCommandExecutor,
        mux_scope: Option<MuxScope>,
    ) -> Option<CommandOutcome> {
        let scope = mux_scope?;
        let binding = self.binding_runtime(scope)?;
        let operation = match executor {
            CoreCommandExecutor::Mux(MuxCommandSpec::SelectPane { .. }) => {
                BindingOperation::NavigatePane
            }
            CoreCommandExecutor::Mux(MuxCommandSpec::SelectLastPane) => BindingOperation::LastPane,
            CoreCommandExecutor::Mux(MuxCommandSpec::ResizePane { .. }) => {
                BindingOperation::ResizePane
            }
            CoreCommandExecutor::Keybind(KeybindAction::Mux(action)) => {
                Self::mux_operation_for_action_for_binding(*action, binding)?
            }
            CoreCommandExecutor::SessionCreate(_) => BindingOperation::CreateProjectSession,
            _ => return None,
        };
        if let Some(message) = binding.mux.unavailable_reason() {
            return Some(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        command_outcome_for_binding_operation(
            binding
                .mux
                .operation_outcome(&binding.multiplexer, operation),
        )
    }

    fn resolve_command_target(
        &self,
        command: &str,
        expected: Option<ResourceKind>,
        supplied: Option<&CommandTarget>,
    ) -> Result<Option<ResolvedCommandTarget>, CommandOutcome> {
        let Some(expected) = expected else {
            return if supplied.is_none() {
                Ok(None)
            } else {
                Err(CommandOutcome::Denied {
                    message: "command does not accept a target".to_owned(),
                })
            };
        };
        if supplied.is_some_and(|target| target.kind != expected) {
            return Err(CommandOutcome::Denied {
                message: format!("command requires a {expected:?} target"),
            });
        }
        if let Some(target) = supplied {
            return self
                .resolve_supplied_command_target(command, target)
                .map(Some);
        }

        let Some(target) = self.current_command_target_for(command, expected) else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {expected:?} target is available"),
            });
        };
        let mux_scope = self.target_mux_scope(&target)?;
        Ok(Some(ResolvedCommandTarget { target, mux_scope }))
    }

    fn current_command_target_for(
        &self,
        command: &str,
        kind: ResourceKind,
    ) -> Option<CommandTarget> {
        let target = self.current_command_target(kind);
        if target.is_some() || command != "window.create" || kind != ResourceKind::Session {
            return target;
        }
        self.current_command_target(ResourceKind::Binding)
            .map(|binding| CommandTarget {
                kind,
                handle: serde_json::to_string(&("no-session", &binding.handle))
                    .expect("serialize empty session target"),
                generation: binding.generation,
            })
    }

    fn current_command_target(&self, kind: ResourceKind) -> Option<CommandTarget> {
        let process = self.command_instance_handle.clone();
        let window = &self.window_state_key;
        let scope = self.binding.scope;
        let space = scope.space_id().persistence_value().to_string();
        let binding = scope.binding_id().persistence_value().to_string();
        let binding_generation = self.binding.mux.binding_generation();
        let binding_handle = serde_json::to_string(&(
            &process,
            window,
            self.command_window_generation,
            &space,
            &binding,
            binding_generation,
        ))
        .expect("serialize target");
        let (session, mux_window, pane, terminal) = self.selected_mux_resource_path();
        let target = match kind {
            ResourceKind::Instance => CommandTarget {
                kind,
                handle: process,
                generation: self.command_instance_generation,
            },
            ResourceKind::ApplicationWindow => CommandTarget {
                kind,
                handle: serde_json::to_string(&[&process, window]).expect("serialize target"),
                generation: self.command_window_generation,
            },
            ResourceKind::Binding => CommandTarget {
                kind,
                handle: binding_handle,
                generation: binding_generation,
            },
            ResourceKind::Space => self.space_command_target(self.active_space_id),
            ResourceKind::Session => {
                let session = session?;
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session])
                        .expect("serialize target"),
                    generation: self.binding.mux.session_generation(&session)?,
                }
            }
            ResourceKind::MuxWindow => {
                let (session, mux_window) = (session?, mux_window?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window])
                        .expect("serialize target"),
                    generation: self.binding.mux.window_generation(&session, &mux_window)?,
                }
            }
            ResourceKind::Pane => {
                let (session, mux_window, pane) = (session?, mux_window?, pane?);
                CommandTarget {
                    kind,
                    handle: serde_json::to_string(&[&binding_handle, &session, &mux_window, &pane])
                        .expect("serialize target"),
                    generation: self
                        .binding
                        .mux
                        .pane_generation(&session, &mux_window, &pane)?,
                }
            }
            ResourceKind::Terminal => {
                let (handle, generation) = match (session, mux_window, pane, terminal) {
                    (Some(session), Some(mux_window), Some(pane), Some(terminal)) => (
                        serde_json::to_string(&(
                            &binding_handle,
                            &session,
                            &mux_window,
                            &pane,
                            &terminal,
                        ))
                        .expect("serialize target"),
                        self.binding
                            .mux
                            .terminal_generation(&session, &mux_window, &terminal)?,
                    ),
                    (Some(session), _, _, _) => (
                        serde_json::to_string(&(&binding_handle, &session))
                            .expect("serialize target"),
                        self.binding.mux.session_generation(&session)?,
                    ),
                    (None, _, _, _) => (
                        serde_json::to_string(&(&binding_handle, "active_terminal"))
                            .expect("serialize target"),
                        binding_generation,
                    ),
                };
                CommandTarget {
                    kind,
                    handle,
                    generation,
                }
            }
            ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => return None,
        };
        Some(target)
    }

    fn selected_mux_resource_path(
        &self,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        let Some(anchor) = self.binding.mux.selected_session_anchor() else {
            return (None, None, None, None);
        };
        let session = anchor.session_id.clone();
        let mux_window = self
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .or_else(|| {
                self.binding
                    .mux
                    .sessions()
                    .iter()
                    .find(|candidate| candidate.id == session)
                    .and_then(|candidate| candidate.active_window_id.clone())
            });
        let pane = if self.uses_native_terminal_layout() {
            self.binding.terminal.focused_pane_id().map(|pane_id| {
                decode_scoped_pane_id(pane_id).map_or_else(
                    || pane_id.to_owned(),
                    |(scope, pane_id)| {
                        debug_assert_eq!(scope, self.binding.scope);
                        pane_id
                    },
                )
            })
        } else {
            anchor.pane_id.clone()
        };
        let terminal = match (mux_window.as_deref(), pane.as_deref()) {
            (Some(window_id), Some(pane_id)) => self
                .binding
                .mux
                .terminal_id_for_pane(&session, window_id, pane_id)
                .map(str::to_owned),
            _ => None,
        };
        (Some(session), mux_window, pane, terminal)
    }

    fn resolves_app_target_kind(kind: ResourceKind) -> bool {
        match kind {
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Binding
            | ResourceKind::Space
            | ResourceKind::Session
            | ResourceKind::MuxWindow
            | ResourceKind::Pane
            | ResourceKind::Terminal => true,
            ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => false,
        }
    }

    fn resolve_supplied_command_target(
        &self,
        command: &str,
        target: &CommandTarget,
    ) -> Result<ResolvedCommandTarget, CommandOutcome> {
        if !Self::resolves_app_target_kind(target.kind) {
            return Err(CommandOutcome::Unsupported {
                message: format!("{:?} targets are not routed by AppState", target.kind),
            });
        }
        let mux_scope = match target.kind {
            ResourceKind::Instance | ResourceKind::ApplicationWindow => {
                let current = self.current_command_target(target.kind).ok_or_else(|| {
                    CommandOutcome::Unavailable {
                        message: format!("no current {:?} target is available", target.kind),
                    }
                })?;
                if target != &current {
                    return Err(CommandOutcome::StaleTarget {
                        message: format!("the {:?} target is stale", target.kind),
                    });
                }
                None
            }
            ResourceKind::Binding
            | ResourceKind::Session
            | ResourceKind::MuxWindow
            | ResourceKind::Pane
            | ResourceKind::Terminal => Some(self.validate_mux_target(command, target)?),
            ResourceKind::Space => {
                self.validate_space_target(target)?;
                None
            }
            ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => {
                return Err(CommandOutcome::Unsupported {
                    message: format!("{:?} targets are not routed by AppState", target.kind),
                });
            }
        };
        Ok(ResolvedCommandTarget {
            target: target.clone(),
            mux_scope,
        })
    }

    fn target_mux_scope(&self, target: &CommandTarget) -> Result<Option<MuxScope>, CommandOutcome> {
        match target.kind {
            ResourceKind::Instance | ResourceKind::ApplicationWindow => Ok(None),
            ResourceKind::Space => {
                self.validate_space_target(target)?;
                Ok(None)
            }
            ResourceKind::Binding
            | ResourceKind::Session
            | ResourceKind::MuxWindow
            | ResourceKind::Pane
            | ResourceKind::Terminal => self.validate_mux_target("window.create", target).map(Some),
            ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => Err(CommandOutcome::Unsupported {
                message: format!("{:?} targets are not routed by AppState", target.kind),
            }),
        }
    }

    fn validate_mux_target(
        &self,
        command: &str,
        target: &CommandTarget,
    ) -> Result<MuxScope, CommandOutcome> {
        let (scope, path) = self.mux_target_scope_and_path(target)?;
        let binding = self
            .binding_runtime(scope)
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            })?;
        let generation = match target.kind {
            ResourceKind::Binding => binding.mux.binding_generation(),
            ResourceKind::Session => match path.as_slice() {
                [binding_handle, _]
                    if binding_handle.as_str() == "no-session" && command == "window.create" =>
                {
                    if !binding.mux.sessions().is_empty() {
                        return Err(CommandOutcome::StaleTarget {
                            message: "the empty session target is stale".to_owned(),
                        });
                    }
                    binding.mux.binding_generation()
                }
                [_, session_id] => binding.mux.session_generation(session_id).ok_or_else(|| {
                    CommandOutcome::Unavailable {
                        message: "the target session is unavailable".to_owned(),
                    }
                })?,
                _ => return Err(invalid_opaque_target()),
            },
            ResourceKind::MuxWindow => match path.as_slice() {
                [_, session_id, window_id] => binding
                    .mux
                    .window_generation(session_id, window_id)
                    .ok_or_else(|| CommandOutcome::Unavailable {
                        message: "the target mux window is unavailable".to_owned(),
                    })?,
                _ => return Err(invalid_opaque_target()),
            },
            ResourceKind::Pane => match path.as_slice() {
                [_, session_id, window_id, pane_id] => binding
                    .mux
                    .pane_generation(session_id, window_id, pane_id)
                    .ok_or_else(|| CommandOutcome::Unavailable {
                        message: "the target pane is unavailable".to_owned(),
                    })?,
                _ => return Err(invalid_opaque_target()),
            },
            ResourceKind::Terminal => match path.as_slice() {
                [_, session_id, window_id, pane_id, terminal_id] => {
                    if binding
                        .mux
                        .terminal_id_for_pane(session_id, window_id, pane_id)
                        != Some(terminal_id.as_str())
                    {
                        return Err(CommandOutcome::Unavailable {
                            message: "the target terminal is unavailable".to_owned(),
                        });
                    }
                    binding
                        .mux
                        .terminal_generation(session_id, window_id, terminal_id)
                        .ok_or_else(|| CommandOutcome::Unavailable {
                            message: "the target terminal is unavailable".to_owned(),
                        })?
                }
                [_, terminal_id] if terminal_id.as_str() == "active_terminal" => {
                    binding.mux.binding_generation()
                }
                [_, session_id] => binding.mux.session_generation(session_id).ok_or_else(|| {
                    CommandOutcome::Unavailable {
                        message: "the target terminal is unavailable".to_owned(),
                    }
                })?,
                _ => return Err(invalid_opaque_target()),
            },
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Space
            | ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => {
                return Err(CommandOutcome::Unsupported {
                    message: format!("{:?} targets are not mux-scoped", target.kind),
                });
            }
        };
        if target.generation != generation {
            return Err(CommandOutcome::StaleTarget {
                message: format!("the {:?} target is stale", target.kind),
            });
        }
        Ok(scope)
    }

    fn mux_target_scope_and_path(
        &self,
        target: &CommandTarget,
    ) -> Result<(MuxScope, Vec<String>), CommandOutcome> {
        let path = match target.kind {
            ResourceKind::Binding => Vec::new(),
            ResourceKind::Session
            | ResourceKind::MuxWindow
            | ResourceKind::Pane
            | ResourceKind::Terminal => serde_json::from_str::<Vec<String>>(&target.handle)
                .map_err(|_| invalid_opaque_target())?,
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Space
            | ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => {
                return Err(CommandOutcome::Unsupported {
                    message: format!("{:?} targets are not mux-scoped", target.kind),
                });
            }
        };
        let binding_handle = match target.kind {
            ResourceKind::Binding => target.handle.as_str(),
            ResourceKind::Session if path.first().is_some_and(|part| part == "no-session") => {
                path.get(1).ok_or_else(invalid_opaque_target)?
            }
            ResourceKind::Session
            | ResourceKind::MuxWindow
            | ResourceKind::Pane
            | ResourceKind::Terminal => path.first().ok_or_else(invalid_opaque_target)?,
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Space
            | ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => {
                return Err(CommandOutcome::Unsupported {
                    message: format!("{:?} targets are not mux-scoped", target.kind),
                });
            }
        };
        Ok((self.mux_scope_from_binding_handle(binding_handle)?, path))
    }

    fn mux_scope_from_binding_handle(&self, handle: &str) -> Result<MuxScope, CommandOutcome> {
        let (process, window, window_generation, space, binding, binding_generation) =
            serde_json::from_str::<(String, String, u64, String, String, u64)>(handle)
                .map_err(|_| invalid_opaque_target())?;
        if process != self.command_instance_handle
            || window != self.window_state_key
            || window_generation != self.command_window_generation
        {
            return Err(CommandOutcome::StaleTarget {
                message: "the target application window is stale".to_owned(),
            });
        }
        let scope = MuxScope::new(
            SpaceId::from_persistence(space.parse().map_err(|_| invalid_opaque_target())?),
            BindingId::from_persistence(binding.parse().map_err(|_| invalid_opaque_target())?),
        );
        let runtime = self
            .binding_runtime(scope)
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            })?;
        if runtime.mux.binding_generation() != binding_generation {
            return Err(CommandOutcome::StaleTarget {
                message: "the target binding is stale".to_owned(),
            });
        }
        Ok(scope)
    }

    fn space_command_target(&self, space_id: SpaceId) -> CommandTarget {
        CommandTarget {
            kind: ResourceKind::Space,
            handle: serde_json::to_string(&(
                &self.command_instance_handle,
                &self.window_state_key,
                self.command_window_generation,
                space_id.persistence_value().to_string(),
            ))
            .expect("serialize space target"),
            generation: self.command_window_generation,
        }
    }

    fn validate_space_target(&self, target: &CommandTarget) -> Result<SpaceId, CommandOutcome> {
        let (process, window, window_generation, space) =
            serde_json::from_str::<(String, String, u64, String)>(&target.handle)
                .map_err(|_| invalid_opaque_target())?;
        if process != self.command_instance_handle
            || window != self.window_state_key
            || window_generation != self.command_window_generation
            || target.generation != self.command_window_generation
        {
            return Err(CommandOutcome::StaleTarget {
                message: "the target application window is stale".to_owned(),
            });
        }
        let space_id =
            SpaceId::from_persistence(space.parse().map_err(|_| invalid_opaque_target())?);
        if space_id != self.active_space_id
            && !self
                .inactive_spaces
                .iter()
                .any(|space| space.id == space_id)
        {
            return Err(CommandOutcome::StaleTarget {
                message: "the space target is stale".to_owned(),
            });
        }
        Ok(space_id)
    }

    fn read_active_terminal(&mut self) -> CommandOutcome {
        match self.binding.terminal.extract_frame() {
            Ok(frame) => CommandOutcome::Success {
                value: serde_json::json!({
                    "cols": frame.cols,
                    "rows": frame.rows,
                    "text": frame.text_rows().join("\n"),
                    "cursor": frame.cursor.map(|cursor| serde_json::json!({
                        "x": cursor.x,
                        "y": cursor.y,
                    })),
                }),
                warnings: Vec::new(),
            },
            Err(error) => CommandOutcome::Failed {
                code: "terminal_read_failed".to_owned(),
                message: error.to_string(),
            },
        }
    }

    fn dispatch_worktree_query<T>(
        &self,
        execution: Option<(Instant, CommandCancellation)>,
        operation: impl FnOnce(&crate::git::WorktreeService) -> Result<T, WorktreeServiceError>,
    ) -> CommandDispatch
    where
        T: serde::Serialize,
    {
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let service = self.worktree_service();
        CommandDispatch::Complete(match operation(&service) {
            Ok(value) => command_success(value),
            Err(error) => worktree_service_failure(error),
        })
    }

    fn dispatch_worktree_create(
        &mut self,
        repository_path: String,
        branch: String,
        managed_by_bootty: bool,
        caller: Caller,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let service = self.worktree_service();
        let details = match service.create(WorktreeCreateRequest {
            repository_path: PathBuf::from(repository_path),
            branch,
            managed_by_bootty,
            caller: caller_name(caller).to_owned(),
        }) {
            Ok(details) => details,
            Err(error) => return CommandDispatch::Complete(worktree_service_failure(error)),
        };
        let mut outcome = command_success(&details);
        if let Err(error) = publish_worktree_changed(
            &self.automation,
            &service,
            &self.binding,
            &details.worktree,
            json!({ "action": "created", "worktree": &details }),
        ) && let CommandOutcome::Success { warnings, .. } = &mut outcome
        {
            warnings.push(CommandWarning {
                code: "worktree_event_failed".to_owned(),
                message: error.to_string(),
            });
        }
        CommandDispatch::Complete(outcome)
    }

    fn dispatch_worktree_remove(
        &mut self,
        command_id: String,
        path: String,
        force: bool,
        confirmation: Option<WorktreeRemovalConfirmation>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let requester_session = self
            .binding
            .mux
            .selected_session()
            .map(|session_id| self.directory_session_ref(session_id));
        let service = self.worktree_service();
        let assessment = match service.assess_removal(&path, requester_session.as_ref()) {
            Ok(assessment) => assessment,
            Err(error) => return CommandDispatch::Complete(worktree_service_failure(error)),
        };
        let bound_confirmation = assessment.bound_confirmation();
        if bound_confirmation.as_ref() != confirmation.as_ref() {
            return CommandDispatch::Complete(match bound_confirmation {
                Some(bound_confirmation) => {
                    worktree_removal_confirmation(command_id, path, force, bound_confirmation)
                }
                None => CommandOutcome::StaleTarget {
                    message: "worktree removal confirmation is no longer required".to_owned(),
                },
            });
        }

        let outcome = match service.remove(WorktreeRemoveRequest {
            worktree: assessment.worktree,
            force,
            requester_session,
            confirmation: bound_confirmation,
        }) {
            Ok(assessment) => {
                let mut outcome = command_success(&assessment);
                if let Err(error) = publish_worktree_changed(
                    &self.automation,
                    &service,
                    &self.binding,
                    &assessment.worktree,
                    json!({ "action": "removed", "assessment": &assessment }),
                ) && let CommandOutcome::Success { warnings, .. } = &mut outcome
                {
                    warnings.push(CommandWarning {
                        code: "worktree_event_failed".to_owned(),
                        message: error.to_string(),
                    });
                }
                outcome
            }
            Err(error) => {
                let recheck_confirmation = match &error {
                    WorktreeServiceError::Claims(
                        crate::automation::directory::DirectoryClaimsError::ConfirmationRequired {
                            assessment,
                        }
                        | crate::automation::directory::DirectoryClaimsError::StaleConfirmation {
                            assessment,
                        },
                    ) => assessment.bound_confirmation().map(|bound_confirmation| {
                        worktree_removal_confirmation(
                            command_id.clone(),
                            path.clone(),
                            force,
                            bound_confirmation,
                        )
                    }),
                    _ => None,
                };
                recheck_confirmation.unwrap_or_else(|| worktree_service_failure(error))
            }
        };
        CommandDispatch::Complete(outcome)
    }

    fn dispatch_resolved_command(
        &mut self,
        command_id: String,
        executor: CoreCommandExecutor,
        context: ResolvedCommandContext<'_>,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        match executor {
            CoreCommandExecutor::Mux(specification) => self.dispatch_mux_command_spec(
                command_id,
                specification,
                context.target,
                context.mux_scope,
                context.execution,
            ),
            CoreCommandExecutor::Keybind(KeybindAction::App(AppAction::ReloadConfig)) => {
                if let Err(outcome) = Self::begin_synchronous_command(context.execution) {
                    return CommandDispatch::Complete(outcome);
                }
                let reloaded = self.reload_config(effects);
                let outcome = if reloaded {
                    let path = self.config().config_path.clone();
                    self.config_hot_reload.refresh_after_reload(&path);
                    self.last_error
                        .clone()
                        .map_or_else(CommandOutcome::success, |warning| {
                            CommandOutcome::success_with_warning("configuration_warning", warning)
                        })
                } else {
                    CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message: self
                            .last_error
                            .clone()
                            .unwrap_or_else(|| "configuration reload failed".to_owned()),
                    }
                };
                CommandDispatch::Complete(outcome)
            }
            CoreCommandExecutor::Keybind(action) => {
                self.dispatch_resolved_keybind_command(command_id, action, context, effects)
            }
            CoreCommandExecutor::Sidebar(action) => {
                if command_id == "pane.focus" {
                    let Some(target) = context.target else {
                        return CommandDispatch::Complete(CommandOutcome::Unavailable {
                            message: "no current pane target is available".to_owned(),
                        });
                    };
                    if self.current_command_target(ResourceKind::Pane).as_ref() != Some(target) {
                        return CommandDispatch::Complete(CommandOutcome::Unsupported {
                            message: "pane.focus only supports the current pane target".to_owned(),
                        });
                    }
                }
                if let Err(outcome) = Self::begin_synchronous_command(context.execution) {
                    return CommandDispatch::Complete(outcome);
                }
                self.apply_sidebar_action(action);
                CommandDispatch::Complete(CommandOutcome::success())
            }
            CoreCommandExecutor::SessionSelect { selector } => {
                let Some(origin) = context.mux_scope else {
                    return CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: "the target binding is unavailable".to_owned(),
                    });
                };
                let Some(binding_target) = context.target else {
                    return CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: "no binding target is available".to_owned(),
                    });
                };
                self.dispatch_session_select(selector, origin, binding_target, context.execution)
            }
            CoreCommandExecutor::SessionCreate(descriptor) => self
                .dispatch_session_create_descriptor(
                    command_id,
                    descriptor,
                    context.mux_scope.unwrap_or(self.binding.scope),
                    context.target.cloned(),
                    context.execution,
                ),
            CoreCommandExecutor::DirectoryResolve { path } => self
                .dispatch_worktree_query(context.execution, move |service| service.resolve(path)),
            CoreCommandExecutor::DirectoryUsageList { path } => {
                self.dispatch_worktree_query(context.execution, move |service| service.usage(path))
            }
            CoreCommandExecutor::WorktreeList { path } => {
                self.dispatch_worktree_query(context.execution, move |service| service.list(path))
            }
            CoreCommandExecutor::WorktreeGet { path } => {
                self.dispatch_worktree_query(context.execution, move |service| service.get(path))
            }
            CoreCommandExecutor::WorktreeCreate {
                repository_path,
                branch,
                managed_by_bootty,
            } => self.dispatch_worktree_create(
                repository_path,
                branch,
                managed_by_bootty,
                context.caller,
                context.execution,
            ),
            CoreCommandExecutor::WorktreeRemove {
                path,
                force,
                confirmation,
            } => self.dispatch_worktree_remove(
                command_id,
                path,
                force,
                confirmation,
                context.execution,
            ),
            CoreCommandExecutor::Extension {
                command_id,
                extension_id,
                generation,
            } => {
                let (deadline, cancellation) = context.execution.unwrap_or_else(|| {
                    (
                        Instant::now() + Duration::from_secs(10),
                        CommandCancellation::new(),
                    )
                });
                let mut invocation = context.invocation;
                invocation.command.clone_from(&command_id);
                let target = invocation.target.clone();
                let result = self.extension_runtime.invoke_async_exact(
                    invocation.clone(),
                    &extension_id,
                    generation,
                    deadline,
                    cancellation.clone(),
                );
                CommandDispatch::ExtensionPending {
                    invocation,
                    extension_id,
                    generation,
                    target,
                    deadline,
                    cancellation,
                    result,
                }
            }
            CoreCommandExecutor::ReadTerminal => {
                if let Err(outcome) = Self::begin_synchronous_command(context.execution) {
                    return CommandDispatch::Complete(outcome);
                }
                let Some(target) = context.target else {
                    return CommandDispatch::Complete(CommandOutcome::Unavailable {
                        message: "no active terminal target is available".to_owned(),
                    });
                };
                if self.current_command_target(ResourceKind::Terminal).as_ref() != Some(target) {
                    return CommandDispatch::Complete(CommandOutcome::Unsupported {
                        message: "terminal.read only supports the active terminal target"
                            .to_owned(),
                    });
                }
                CommandDispatch::Complete(self.read_active_terminal())
            }
        }
    }

    fn dispatch_session_select(
        &mut self,
        selector: String,
        origin: MuxScope,
        binding_target: &CommandTarget,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let resolution = match self.binding_runtime(origin) {
            Some(binding) => binding.mux.resolve_session_selector(&selector),
            None => {
                return CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: "the target binding is unavailable".to_owned(),
                });
            }
        };
        let session_id = match resolution {
            SessionSelectorResolution::Missing => {
                return CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: format!("no session matches selector {selector:?}"),
                });
            }
            SessionSelectorResolution::Ambiguous { session_ids } => {
                let candidates = match session_ids
                    .iter()
                    .map(|session_id| {
                        self.session_target_for_binding_selector(binding_target, origin, session_id)
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
                {
                    Ok(candidates) => candidates,
                    Err(outcome) => return CommandDispatch::Complete(outcome),
                };
                return CommandDispatch::Complete(CommandOutcome::Ambiguous {
                    message: format!("session selector {selector:?} matches multiple sessions"),
                    candidates,
                });
            }
            SessionSelectorResolution::Resolved { session_id } => session_id,
        };
        let session_target =
            match self.session_target_for_binding_selector(binding_target, origin, &session_id) {
                Ok(target) => target,
                Err(outcome) => return CommandDispatch::Complete(outcome),
            };
        let scope = match self.validate_mux_target("session.select", &session_target) {
            Ok(scope) => scope,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        if scope != origin {
            return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                message: "the selected session resolved outside the target binding".to_owned(),
            });
        }
        {
            let Some(binding) = self.binding_runtime_mut(scope) else {
                return CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: "the target binding is unavailable".to_owned(),
                });
            };
            binding.mux.activate_session(&session_id);
        }
        if scope == self.binding.scope {
            self.persist_rmux_restore_state();
            self.sync_native_layout_terminal_now();
        }
        CommandDispatch::Complete(CommandOutcome::Success {
            value: json!({ "focused": session_target }),
            warnings: Vec::new(),
        })
    }

    fn session_target_for_binding_selector(
        &self,
        binding_target: &CommandTarget,
        scope: MuxScope,
        session_id: &str,
    ) -> Result<CommandTarget, CommandOutcome> {
        if binding_target.kind != ResourceKind::Binding {
            return Err(CommandOutcome::Denied {
                message: "session.select requires a binding target".to_owned(),
            });
        }
        let binding = self
            .binding_runtime(scope)
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            })?;
        let generation = binding.mux.session_generation(session_id).ok_or_else(|| {
            CommandOutcome::StaleTarget {
                message: "the selected session is no longer current".to_owned(),
            }
        })?;
        Ok(CommandTarget {
            kind: ResourceKind::Session,
            handle: serde_json::to_string(&[&binding_target.handle, session_id])
                .expect("serialize session target"),
            generation,
        })
    }

    fn dispatch_mux_command_spec(
        &mut self,
        command_id: String,
        specification: MuxCommandSpec,
        target: Option<&CommandTarget>,
        mux_scope: Option<MuxScope>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(target) = target else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "no mux command target is available".to_owned(),
            });
        };
        let Some(origin) = mux_scope else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            });
        };
        let command = match Self::mux_command_for_spec(specification, target) {
            Ok(command) => command,
            Err(outcome) => return CommandDispatch::Complete(outcome),
        };
        self.enqueue_authoritative_mux_command(
            command_id,
            command,
            origin,
            Some(target.clone()),
            execution,
        )
    }

    fn mux_command_for_spec(
        specification: MuxCommandSpec,
        target: &CommandTarget,
    ) -> Result<MuxCommand, CommandOutcome> {
        let path = serde_json::from_str::<Vec<String>>(&target.handle)
            .map_err(|_| invalid_opaque_target())?;
        match (specification, target.kind, path.as_slice()) {
            (
                MuxCommandSpec::SelectPane { direction },
                ResourceKind::Pane,
                [_, session_id, window_id, _pane_id],
            ) => Ok(MuxCommand::SelectPane {
                session_id: session_id.clone(),
                window_id: Some(window_id.clone()),
                direction,
            }),
            (
                MuxCommandSpec::SelectLastPane,
                ResourceKind::MuxWindow,
                [_, session_id, window_id],
            ) => Ok(MuxCommand::SelectLastPane {
                session_id: session_id.clone(),
                window_id: Some(window_id.clone()),
            }),
            (
                MuxCommandSpec::ResizePane { adjustment },
                ResourceKind::Pane,
                [_, session_id, _, pane_id],
            ) => Ok(MuxCommand::ResizePane {
                session_id: session_id.clone(),
                pane_id: Some(pane_id.clone()),
                adjustment,
            }),
            _ => Err(invalid_opaque_target()),
        }
    }

    /// Resolves a recursive launch descriptor into one authoritative mux command.
    fn dispatch_session_create_descriptor(
        &mut self,
        command_id: String,
        descriptor: SessionLaunchDescriptor,
        origin: MuxScope,
        target: Option<CommandTarget>,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let Some(binding) = self.binding_runtime(origin) else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            });
        };
        let mux_config = binding.multiplexer.clone();
        let remote = mux_config.remote.is_some();
        if let Some(message) = binding.mux.unavailable_reason() {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: message.to_owned(),
            });
        }
        if let Some(outcome) = command_outcome_for_binding_operation(
            binding
                .mux
                .operation_outcome(&mux_config, BindingOperation::CreateProjectSession),
        ) {
            return CommandDispatch::Complete(outcome);
        }

        let normalized = match Self::normalize_session_launch_descriptor(&descriptor, remote) {
            Ok(normalized) => normalized,
            Err(error) => {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "invalid_launch".to_owned(),
                    message: error.to_string(),
                });
            }
        };
        let existing_names = self.taken_session_names(None);
        let generated_name = normalized.name().is_none();
        let (session_id, display_name) = if let Some(name) = normalized.name() {
            if existing_names.iter().any(|existing| existing == name) {
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "session_exists".to_owned(),
                    message: format!("a session named {name:?} already exists"),
                });
            }
            (name.to_owned(), name.to_owned())
        } else {
            let display_name = Self::suggested_session_name(normalized.default_cwd(), remote);
            let session_id = crate::strings::unique_session_name(
                &display_name,
                existing_names.iter().map(String::as_str),
            );
            (session_id, display_name)
        };
        let cwd = normalized.default_cwd().to_owned();
        let plan = normalized.mux_plan(session_id.clone());
        if let Some(outcome) =
            Self::session_launch_preflight_outcome(&binding.mux, &mux_config, &plan)
        {
            return CommandDispatch::Complete(outcome);
        }
        if let Err(error) = persist_session_launch_plan(
            &self.config().config_path,
            origin.binding_id().persistence_value(),
            &plan,
        ) {
            return CommandDispatch::Complete(CommandOutcome::Failed {
                code: "session_launch_persistence_failed".to_owned(),
                message: format!("persisting session launch plan failed: {error}"),
            });
        }
        let plan_id = plan.session_id.clone();
        let dispatch = self.enqueue_authoritative_mux_command(
            command_id,
            MuxCommand::CreateSession { plan },
            origin,
            target,
            execution,
        );
        if matches!(&dispatch, CommandDispatch::Complete(_)) {
            let cleanup_error = delete_session_launch_plan(
                &self.config().config_path,
                origin.binding_id().persistence_value(),
                &plan_id,
            )
            .err()
            .map(|error| error.to_string());
            if generated_name {
                SessionNameStore::for_binding(
                    &self.config().config_path,
                    origin.binding_id().persistence_value(),
                )
                .discard_generated(&plan_id);
            }
            if let Some(binding) = self.binding_runtime_mut(origin) {
                binding.session_order.forget_session_cache(&plan_id);
            }
            if let Some(error) = cleanup_error {
                let original = match &dispatch {
                    CommandDispatch::Complete(outcome) => command_outcome_message(outcome),
                    CommandDispatch::Pending { .. } | CommandDispatch::ExtensionPending { .. } => {
                        None
                    }
                }
                .unwrap_or_else(|| "session launch failed".to_owned());
                return CommandDispatch::Complete(CommandOutcome::Failed {
                    code: "session_launch_cleanup_failed".to_owned(),
                    message: format!("{original}; session launch cleanup failed: {error}"),
                });
            }
        }
        if matches!(&dispatch, CommandDispatch::Pending { .. }) && generated_name {
            let Some(binding) = self.binding_runtime_mut(origin) else {
                return CommandDispatch::Complete(CommandOutcome::Unavailable {
                    message: "the target binding is unavailable".to_owned(),
                });
            };
            binding.pending_generated_names.insert(
                session_id.clone(),
                PendingGeneratedName {
                    cwd: cwd.clone(),
                    name: session_id.clone(),
                    display_name: display_name.clone(),
                    previous_display_name: None,
                },
            );
            binding
                .session_names
                .remember_generated(&session_id, &cwd, &session_id, &display_name);
        }
        dispatch
    }

    fn begin_synchronous_command(
        execution: Option<(Instant, CommandCancellation)>,
    ) -> Result<(), CommandOutcome> {
        let Some((deadline, cancellation)) = execution else {
            return Ok(());
        };
        if Instant::now() >= deadline && cancellation.cancel() {
            return Err(CommandOutcome::Failed {
                code: "deadline_exceeded".to_owned(),
                message: "command deadline expired".to_owned(),
            });
        }
        if !cancellation.try_start() {
            return Err(CommandOutcome::Failed {
                code: "cancelled".to_owned(),
                message: "command was cancelled".to_owned(),
            });
        }
        Ok(())
    }

    fn dispatch_resolved_keybind_command(
        &mut self,
        command_id: String,
        action: KeybindAction,
        context: ResolvedCommandContext<'_>,
        effects: &mut Vec<AppEffect>,
    ) -> CommandDispatch {
        let ResolvedCommandContext {
            target,
            mux_scope,
            caller: _,
            viewport,
            execution,
            invocation: _,
        } = context;
        if let Err(outcome) = self.validate_local_keybind_target(&action, target) {
            return CommandDispatch::Complete(outcome);
        }
        let mut return_native_mux_focus = false;
        if let KeybindAction::Mux(mux_action) = action {
            let origin = mux_scope.unwrap_or(self.binding.scope);
            if let Some(kind) = Self::mux_action_target_kind(mux_action)
                && let Err(outcome) = self.validate_current_command_target(kind, target)
            {
                return CommandDispatch::Complete(outcome);
            }
            let native_local_action = self.binding_runtime(origin).is_some_and(|binding| {
                selected_backend(&binding.multiplexer) == MultiplexerBackendConfig::Native
            }) && Self::native_mux_action_uses_local_layout(mux_action);
            if native_local_action {
                if origin != self.binding.scope {
                    return CommandDispatch::Complete(CommandOutcome::StaleTarget {
                        message: "local mux actions require the active binding target".to_owned(),
                    });
                }
                if let Some(kind) = Self::native_local_mux_target_kind(mux_action)
                    && let Err(outcome) = self.validate_current_command_target(kind, target)
                {
                    return CommandDispatch::Complete(outcome);
                }
                return_native_mux_focus = true;
            } else {
                match self.mux_command_for_command(mux_action, target, origin) {
                    Ok(Some(command)) => {
                        return self.enqueue_authoritative_mux_command(
                            command_id,
                            command,
                            origin,
                            target.cloned(),
                            execution,
                        );
                    }
                    Ok(None) => {}
                    Err(outcome) => {
                        self.last_error = command_outcome_message(&outcome);
                        return CommandDispatch::Complete(outcome);
                    }
                }
            }
        }
        if let KeybindAction::App(app_action) = action
            && let Err(outcome) = self.validate_app_action_target(app_action, target)
        {
            return CommandDispatch::Complete(outcome);
        }
        if let Err(outcome) = Self::begin_synchronous_command(execution) {
            return CommandDispatch::Complete(outcome);
        }
        let previous_error = self.last_error.take();
        self.apply_resolved_keybind_action(action, target, mux_scope, viewport, effects);
        let outcome = match self.last_error.clone() {
            Some(message) => CommandOutcome::Failed {
                code: "execution_failed".to_owned(),
                message,
            },
            None => {
                self.last_error = previous_error;
                if return_native_mux_focus {
                    CommandOutcome::Success {
                        value: self.current_mux_focus_value(),
                        warnings: Vec::new(),
                    }
                } else {
                    CommandOutcome::success()
                }
            }
        };
        CommandDispatch::Complete(outcome)
    }

    fn validate_local_keybind_target(
        &self,
        action: &KeybindAction,
        target: Option<&CommandTarget>,
    ) -> Result<(), CommandOutcome> {
        if Self::keybind_action_uses_active_terminal(action) {
            return self.validate_current_command_target(ResourceKind::Terminal, target);
        }
        Ok(())
    }

    fn keybind_action_uses_active_terminal(action: &KeybindAction) -> bool {
        matches!(
            action,
            KeybindAction::Scroll(_)
                | KeybindAction::Write(_)
                | KeybindAction::Find(_)
                | KeybindAction::CopyToClipboard(_)
                | KeybindAction::CopyMode
                | KeybindAction::PasteFromClipboard
        )
    }

    fn mux_action_target_kind(action: MuxKeyAction) -> Option<ResourceKind> {
        match action {
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_) => Some(ResourceKind::Binding),
            MuxKeyAction::MoveSession(_) => Some(ResourceKind::Session),
            _ => None,
        }
    }

    fn native_local_mux_target_kind(action: MuxKeyAction) -> Option<ResourceKind> {
        match action {
            MuxKeyAction::SplitPane(_)
            | MuxKeyAction::KillPane
            | MuxKeyAction::ClosePane
            | MuxKeyAction::TogglePaneZoom => Some(ResourceKind::Pane),
            MuxKeyAction::SelectPane(_) | MuxKeyAction::NextPane | MuxKeyAction::PreviousPane => {
                Some(ResourceKind::Pane)
            }
            _ => None,
        }
    }

    fn validate_current_command_target(
        &self,
        kind: ResourceKind,
        target: Option<&CommandTarget>,
    ) -> Result<(), CommandOutcome> {
        let Some(target) = target else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {kind:?} target is available"),
            });
        };
        let Some(current) = self.current_command_target(kind) else {
            return Err(CommandOutcome::Unavailable {
                message: format!("no current {kind:?} target is available"),
            });
        };
        if target.kind != kind || target != &current {
            return Err(CommandOutcome::StaleTarget {
                message: format!("the {kind:?} target is not current"),
            });
        }
        Ok(())
    }

    fn validate_app_action_target(
        &self,
        action: AppAction,
        target: Option<&CommandTarget>,
    ) -> Result<(), CommandOutcome> {
        let Some(target) = target else {
            return Ok(());
        };
        if matches!(
            (action, target.kind),
            (
                AppAction::EditSpace | AppAction::CloseSpace,
                ResourceKind::Binding | ResourceKind::Space
            ) | (
                AppAction::NextSpace | AppAction::PreviousSpace | AppAction::SelectSpace(_),
                ResourceKind::Space
            )
        ) {
            return Ok(());
        }
        if self.current_command_target(target.kind).as_ref() == Some(target) {
            return Ok(());
        }
        Err(CommandOutcome::Unsupported {
            message: format!("command only supports its current {:?} target", target.kind),
        })
    }

    fn native_mux_action_uses_local_layout(action: MuxKeyAction) -> bool {
        matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
                | MuxKeyAction::SplitPane(_)
                | MuxKeyAction::SelectPane(_)
                | MuxKeyAction::NextPane
                | MuxKeyAction::PreviousPane
                | MuxKeyAction::KillPane
                | MuxKeyAction::ClosePane
        )
    }

    fn current_mux_focus_value(&self) -> serde_json::Value {
        let focused = self
            .current_command_target(ResourceKind::Pane)
            .or_else(|| self.current_command_target(ResourceKind::MuxWindow))
            .or_else(|| self.current_command_target(ResourceKind::Session));
        focused.map_or_else(
            || serde_json::json!({}),
            |focused| serde_json::json!({ "focused": focused }),
        )
    }

    fn mux_command_for_command(
        &mut self,
        action: MuxKeyAction,
        target: Option<&CommandTarget>,
        origin: MuxScope,
    ) -> Result<Option<MuxCommand>, CommandOutcome> {
        if matches!(
            action,
            MuxKeyAction::NextSession
                | MuxKeyAction::PreviousSession
                | MuxKeyAction::LastSession
                | MuxKeyAction::SelectSession(_)
                | MuxKeyAction::MoveSession(_)
        ) {
            return Ok(None);
        }

        let target = target.expect("mux command target was resolved");
        let path = serde_json::from_str::<Vec<String>>(&target.handle)
            .expect("resolved mux command target has a resource path");
        let remote = self
            .binding_runtime(origin)
            .ok_or_else(|| CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            })?
            .multiplexer
            .remote
            .is_some();
        if action == MuxKeyAction::NewTab && path.first().is_some_and(|part| part == "no-session") {
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            let cwd = Self::session_cwd(&cwd, remote);
            let display_name = Self::suggested_session_name(&cwd, remote);
            let session_id = crate::strings::unique_session_name(
                &display_name,
                self.taken_session_names(None).iter().map(String::as_str),
            );
            return Ok(Some(MuxCommand::CreateProjectSession { session_id, cwd }));
        }

        let session_id = path.get(1).cloned().ok_or_else(invalid_opaque_target)?;
        let window_id = matches!(
            target.kind,
            ResourceKind::MuxWindow | ResourceKind::Pane | ResourceKind::Terminal
        )
        .then(|| {
            path.get(2)
                .expect("window, pane, and terminal targets include a window")
                .clone()
        });
        let pane_id =
            matches!(target.kind, ResourceKind::Pane | ResourceKind::Terminal).then(|| {
                path.get(3)
                    .expect("pane and terminal targets include a pane")
                    .clone()
            });
        let target_window_id = window_id.as_deref();
        let target_pane_id = matches!(target.kind, ResourceKind::Pane | ResourceKind::Terminal)
            .then(|| path.get(3).map(String::as_str))
            .flatten();
        let anchor_cwd = {
            let binding =
                self.binding_runtime(origin)
                    .ok_or_else(|| CommandOutcome::Unavailable {
                        message: "the target binding is unavailable".to_owned(),
                    })?;
            let session = binding
                .mux
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .ok_or_else(|| CommandOutcome::Unavailable {
                    message: "the target session is unavailable".to_owned(),
                })?;
            let window = target_window_id
                .and_then(|window_id| session.windows.iter().find(|window| window.id == window_id))
                .or_else(|| {
                    session.active_window_id.as_deref().and_then(|window_id| {
                        session.windows.iter().find(|window| window.id == window_id)
                    })
                })
                .or_else(|| session.windows.iter().find(|window| window.active))
                .or_else(|| session.windows.first());
            target_pane_id
                .and_then(|pane_id| {
                    window.and_then(|window| {
                        window
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id.as_deref() == Some(pane_id))
                            .or_else(|| {
                                (window.anchor.pane_id.as_deref() == Some(pane_id))
                                    .then_some(&window.anchor)
                            })
                            .and_then(|pane| pane.cwd.clone())
                    })
                })
                .or_else(|| window.and_then(|window| window.anchor.cwd.clone()))
                .or_else(|| session.anchor.cwd.clone())
        };
        let live_terminal_cwd = (target.kind == ResourceKind::Terminal
            && self.current_command_target(ResourceKind::Terminal).as_ref() == Some(target))
        .then(|| {
            self.binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten()
        })
        .flatten();
        let cwd = terminal_cwd_for_mux_command(live_terminal_cwd, anchor_cwd);
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow { session_id, cwd },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow { session_id },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow { session_id },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow { session_id },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex { session_id, index },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id,
                window_id,
                delta,
            },
            MuxKeyAction::SplitPane(direction) => MuxCommand::SplitPane {
                session_id,
                pane_id,
                direction: mux_split_direction(direction),
            },
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id,
                window_id,
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id,
                window_id,
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id,
                window_id,
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id,
                pane_id,
            },
            MuxKeyAction::ClosePane => MuxCommand::ClosePane {
                session_id,
                pane_id,
            },
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id,
                pane_id,
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => unreachable!("handled before command construction"),
        };
        Ok(Some(command))
    }

    fn reconcile_stale_mux_completion(
        &mut self,
        origin: MuxScope,
        namespace: &BackendConnectionNamespace,
        command: &MuxCommand,
        result: &MuxCommandResult,
        outcome: CommandOutcome,
    ) -> CommandOutcome {
        let MuxCommand::CreateSession { .. } = command else {
            return self.finalize_failed_session_launch(origin, namespace, command, outcome);
        };
        let Some(allocated) = result
            .as_ref()
            .ok()
            .and_then(MuxCommandCompletion::allocated)
        else {
            return self.finalize_failed_session_launch(origin, namespace, command, outcome);
        };

        let allocation_error = if let Some(binding) = self.binding_runtime_mut(origin) {
            let current_namespace = namespace_for_binding(binding.scope, &binding.multiplexer);
            if current_namespace != *namespace {
                Some("the target backend connection changed before cleanup".to_owned())
            } else {
                let config = binding.multiplexer.clone();
                binding
                    .mux
                    .compensate_created_session(&allocated.session_id, &config)
                    .err()
                    .map(|error| error.to_string())
            }
        } else {
            Some("the target binding is unavailable".to_owned())
        };
        if let Some(error) = allocation_error {
            let original = command_outcome_message(&outcome).unwrap_or_else(|| {
                "the target binding changed before command completion".to_owned()
            });
            let message = format!(
                "{original}; session allocation {} could not be reconciled: {error}; \
                 durable launch intent was retained for authoritative recovery",
                allocated.session_id
            );
            self.last_error = Some(message.clone());
            return CommandOutcome::Failed {
                code: "completion_indeterminate".to_owned(),
                message,
            };
        }

        if let Err(error) = self.discard_failed_session_launch(origin, namespace, command) {
            let original = command_outcome_message(&outcome).unwrap_or_else(|| {
                "the target binding changed before command completion".to_owned()
            });
            let message = format!(
                "{original}; session allocation {} was compensated, but durable launch intent \
                 cleanup failed: {error}",
                allocated.session_id
            );
            self.last_error = Some(message.clone());
            return CommandOutcome::Failed {
                code: "session_launch_cleanup_failed".to_owned(),
                message,
            };
        }
        self.last_error = command_outcome_message(&outcome);
        outcome
    }

    fn command_outcome_for_mux_result(
        &mut self,
        context: MuxCompletionContext<'_>,
        result: MuxCommandResult,
    ) -> CommandOutcome {
        let MuxCompletionContext {
            command_id,
            origin,
            binding_identity,
            binding_generation,
            namespace,
            command,
            rename,
        } = context;
        let Some((config, current_identity, current_generation, current_namespace)) =
            self.binding_runtime(origin).map(|binding| {
                (
                    binding.multiplexer.clone(),
                    self.binding_identity(binding),
                    binding.mux.binding_generation(),
                    namespace_for_binding(binding.scope, &binding.multiplexer),
                )
            })
        else {
            return self.reconcile_stale_mux_completion(
                origin,
                namespace,
                command,
                &result,
                CommandOutcome::Unavailable {
                    message: "the target binding is unavailable".to_owned(),
                },
            );
        };
        if current_identity != *binding_identity
            || current_generation != binding_generation
            || current_namespace != *namespace
        {
            return self.reconcile_stale_mux_completion(
                origin,
                namespace,
                command,
                &result,
                CommandOutcome::StaleTarget {
                    message: "the target binding changed before command completion".to_owned(),
                },
            );
        }
        let completed = {
            let Some(binding) = self.binding_runtime_mut(origin) else {
                return self.reconcile_stale_mux_completion(
                    origin,
                    namespace,
                    command,
                    &result,
                    CommandOutcome::Unavailable {
                        message: "the target binding is unavailable".to_owned(),
                    },
                );
            };
            let selected_session_before = binding.mux.selected_session().map(str::to_owned);
            let completed = binding.mux.complete_authoritative_command(result, &config);
            if rename.is_some()
                && let Some(selected_session) = selected_session_before
            {
                binding.mux.activate_session(&selected_session);
            }
            completed
        };
        match completed {
            Ok(completion) => {
                let mut outcome = CommandOutcome::Success {
                    value: self.mux_command_completion_value(origin, command, &completion),
                    warnings: Vec::new(),
                };
                if let Err(outcome) =
                    self.record_completed_session_launch(origin, namespace, command, &completion)
                {
                    let outcome = self.compensate_completed_session_launch(
                        origin,
                        namespace,
                        command,
                        &completion,
                        outcome,
                    );
                    return outcome;
                }
                if let Some(rename) = rename
                    && let Err(outcome) =
                        self.record_completed_session_rename(origin, command_id, rename)
                {
                    return outcome;
                }
                self.sync_native_focus_from_completion(origin, &completion);
                self.record_authoritative_directory_claims(
                    origin,
                    command,
                    &completion,
                    &mut outcome,
                );
                if origin == self.binding.scope {
                    self.sync_native_layout_terminal_now();
                }
                outcome
            }
            Err(error) => {
                let outcome = match error {
                    MuxCommandError::Cancelled => CommandOutcome::Failed {
                        code: "cancelled".to_owned(),
                        message: "command was cancelled".to_owned(),
                    },
                    MuxCommandError::DeadlineExceeded => CommandOutcome::Failed {
                        code: "deadline_exceeded".to_owned(),
                        message: "command deadline expired".to_owned(),
                    },
                    MuxCommandError::Unsupported => CommandOutcome::Unsupported {
                        message: "mux operation is unsupported".to_owned(),
                    },
                    MuxCommandError::Unavailable => CommandOutcome::Unavailable {
                        message: "mux operation is unavailable".to_owned(),
                    },
                    MuxCommandError::Denied => CommandOutcome::Denied {
                        message: "mux operation was denied".to_owned(),
                    },
                    MuxCommandError::Stale => CommandOutcome::StaleTarget {
                        message: "mux operation capability is stale".to_owned(),
                    },
                    MuxCommandError::Failed(message) => CommandOutcome::Failed {
                        code: "execution_failed".to_owned(),
                        message,
                    },
                };
                let outcome =
                    self.finalize_failed_session_launch(origin, namespace, command, outcome);
                let outcome = self
                    .clear_failed_session_rename(origin, command_id, rename)
                    .unwrap_or(outcome);
                self.last_error = command_outcome_message(&outcome);

                outcome
            }
        }
    }
    fn record_completed_session_rename(
        &mut self,
        origin: MuxScope,
        command_id: &str,
        rename: &PendingSessionRename,
    ) -> std::result::Result<(), CommandOutcome> {
        let Some(binding) = self.binding_runtime_mut(origin) else {
            return Err(CommandOutcome::Unavailable {
                message: "the target binding is unavailable".to_owned(),
            });
        };
        let plan_ids = [rename.old_name.as_str(), rename.session_id.as_str()];
        rename_session_membership_and_launch_plans(
            &binding.workspace_config_path,
            origin.binding_id().persistence_value(),
            &rename.old_name,
            &rename.new_name,
            &plan_ids,
        )
        .map_err(|error| CommandOutcome::Failed {
            code: "session_rename_persistence_failed".to_owned(),
            message: format!(
                "session rename completed in the backend, but workspace persistence failed: \
                 {error}"
            ),
        })?;
        binding
            .session_order
            .rename_session_cache(&rename.old_name, &rename.new_name);
        binding.session_names.mark_explicit(
            &rename.session_id,
            &rename.new_name,
            &rename.display_name,
            &rename.cwd,
        );
        binding.pending_generated_names.remove(&rename.new_name);
        clear_pending_session_rename(
            &binding.workspace_config_path,
            origin.binding_id().persistence_value(),
            command_id,
        )
        .map_err(|error| CommandOutcome::Failed {
            code: "session_rename_persistence_failed".to_owned(),
            message: format!(
                "session rename completed and workspace state changed, but pending intent \
                 cleanup failed: {error}"
            ),
        })?;
        Ok(())
    }
    fn clear_failed_session_rename(
        &mut self,
        origin: MuxScope,
        command_id: &str,
        rename: Option<&PendingSessionRename>,
    ) -> Option<CommandOutcome> {
        let rename = rename?;
        let binding = self.binding_runtime_mut(origin)?;
        match clear_pending_session_rename(
            &binding.workspace_config_path,
            origin.binding_id().persistence_value(),
            command_id,
        ) {
            Ok(()) => {
                let previous_display_name = binding
                    .pending_generated_names
                    .get(&rename.new_name)
                    .and_then(|pending| pending.previous_display_name.clone());
                if let Some(previous_display_name) = previous_display_name {
                    binding
                        .session_names
                        .set_display_name(&rename.session_id, &previous_display_name);
                }
                binding.pending_generated_names.remove(&rename.new_name);
                None
            }
            Err(error) => Some(CommandOutcome::Failed {
                code: "session_rename_cleanup_failed".to_owned(),
                message: format!(
                    "backend session rename failed and pending intent cleanup failed: {error}"
                ),
            }),
        }
    }

    fn record_completed_session_launch(
        &mut self,
        origin: MuxScope,
        _namespace: &BackendConnectionNamespace,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> std::result::Result<(), CommandOutcome> {
        if let MuxCommand::DitchSession { session_id } = command {
            if let Some(binding) = self.binding_runtime_mut(origin) {
                let observed_name = binding
                    .session_names
                    .last_observed_name(session_id)
                    .unwrap_or(session_id)
                    .to_owned();
                let plan_ids = [session_id.as_str(), observed_name.as_str()];
                remove_session_membership_and_launch_plan(
                    &binding.workspace_config_path,
                    origin.binding_id().persistence_value(),
                    &observed_name,
                    &plan_ids,
                )
                .map_err(|error| CommandOutcome::Failed {
                    code: "session_ditch_persistence_failed".to_owned(),
                    message: format!(
                        "session ditch completed, but membership/launch-plan cleanup failed: \
                         {error}"
                    ),
                })?;
                binding.session_order.forget_session_cache(&observed_name);
                if observed_name != session_id.as_str() {
                    binding.session_order.forget_session_cache(session_id);
                }
            }
            self.persist_rmux_restore_state();
            return Ok(());
        }
        let (requested_session_id, focus, launch_plan) = match command {
            MuxCommand::CreateSession { plan } => {
                (plan.session_id.as_str(), plan.focus, Some(plan))
            }
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => {
                (session_id.as_str(), false, None)
            }
            _ => return Ok(()),
        };
        if let Some(binding) = self.binding_runtime_mut(origin) {
            if let Some(plan) = launch_plan {
                persist_session_launch_plan(
                    &binding.workspace_config_path,
                    origin.binding_id().persistence_value(),
                    plan,
                )
                .map_err(|error| CommandOutcome::Failed {
                    code: "session_launch_persistence_failed".to_owned(),
                    message: format!(
                        "session creation completed, but launch-plan persistence failed: {error}"
                    ),
                })?;
            }
            binding
                .session_order
                .add_session(requested_session_id)
                .map_err(|error| CommandOutcome::Failed {
                    code: "session_membership_persistence_failed".to_owned(),
                    message: format!(
                        "session creation completed in the backend, but membership persistence \
                         failed: {error}"
                    ),
                })?;
            if let Some(plan) = launch_plan
                && selected_backend(&binding.multiplexer) == MultiplexerBackendConfig::Native
            {
                let allocated = completion
                    .allocated()
                    .ok_or_else(|| CommandOutcome::Failed {
                        code: "invalid_native_allocation".to_owned(),
                        message: "native launch completed without authoritative allocation"
                            .to_owned(),
                    })?;
                let terminal_ids = allocated
                    .windows
                    .iter()
                    .map(|window| {
                        window
                            .pane_ids
                            .iter()
                            .map(|pane_id| {
                                binding
                                    .mux
                                    .terminal_id_for_pane(
                                        &allocated.session_id,
                                        &window.window_id,
                                        pane_id,
                                    )
                                    .map(|terminal_id| (pane_id.clone(), terminal_id.to_owned()))
                            })
                            .collect::<Option<HashMap<_, _>>>()
                            .map(|pane_terminal_ids| (window.window_id.clone(), pane_terminal_ids))
                    })
                    .collect::<Option<HashMap<_, _>>>()
                    .ok_or_else(|| CommandOutcome::Failed {
                        code: "invalid_native_allocation".to_owned(),
                        message: "native allocation snapshot omitted a terminal identity"
                            .to_owned(),
                    })?;
                binding
                    .terminal
                    .register_native_session_launch(origin, plan, allocated, &terminal_ids)
                    .map_err(|error| CommandOutcome::Failed {
                        code: "invalid_native_allocation".to_owned(),
                        message: error.to_string(),
                    })?;
            }
            if launch_plan.is_some()
                && let Some(allocated) = completion.allocated()
                && allocated.session_id != requested_session_id
            {
                rekey_session_launch_plan(
                    &binding.workspace_config_path,
                    origin.binding_id().persistence_value(),
                    requested_session_id,
                    &allocated.session_id,
                )
                .map_err(|error| CommandOutcome::Failed {
                    code: "session_launch_persistence_failed".to_owned(),
                    message: format!(
                        "session creation completed, but launch-plan identity persistence failed: \
                         {error}"
                    ),
                })?;
            }
        }
        if focus && origin == self.binding.scope {
            self.input_focus = InputFocus::Terminal;
        }
        self.persist_rmux_restore_state();
        Ok(())
    }

    fn discard_failed_session_launch(
        &mut self,
        origin: MuxScope,
        namespace: &BackendConnectionNamespace,
        command: &MuxCommand,
    ) -> Result<()> {
        let MuxCommand::CreateSession { plan } = command else {
            return Ok(());
        };
        let config_path = self.config().config_path.clone();
        let binding_id = origin.binding_id().persistence_value();
        let mut cleanup_error = None;
        let mut membership_removed = false;
        if let Some(binding) = self.binding_runtime_mut(origin) {
            match binding.session_order.remove_session(&plan.session_id) {
                Ok(_) => membership_removed = true,
                Err(error) => cleanup_error = Some(error),
            }
        }
        if let Err(error) = remove_session_membership_and_launch_plan(
            &config_path,
            binding_id,
            &plan.session_id,
            &[plan.session_id.as_str()],
        ) && cleanup_error.is_none()
        {
            cleanup_error = Some(error);
        }

        let same_namespace = self.binding_runtime(origin).is_some_and(|binding| {
            namespace_for_binding(binding.scope, &binding.multiplexer) == *namespace
        });
        if same_namespace {
            SessionNameStore::for_binding(&config_path, binding_id)
                .discard_generated(&plan.session_id);
            if let Some(binding) = self.binding_runtime_mut(origin) {
                binding.pending_generated_names.remove(&plan.session_id);
                binding.session_names.discard_generated(&plan.session_id);
                if membership_removed {
                    binding.session_order.forget_session_cache(&plan.session_id);
                }
            }
        } else if self.binding_runtime(origin).is_none() {
            SessionNameStore::for_binding(&config_path, binding_id)
                .discard_generated(&plan.session_id);
        }
        if let Some(error) = cleanup_error {
            Err(error.into())
        } else {
            Ok(())
        }
    }

    fn finalize_failed_session_launch(
        &mut self,
        origin: MuxScope,
        namespace: &BackendConnectionNamespace,
        command: &MuxCommand,
        outcome: CommandOutcome,
    ) -> CommandOutcome {
        if let MuxCommand::DitchSession { session_id } = command
            && let Some(binding) = self.binding_runtime(origin)
        {
            let _ = clear_pending_ditch(
                &binding.workspace_config_path,
                origin.binding_id().persistence_value(),
                session_id,
            );
        }
        let Err(error) = self.discard_failed_session_launch(origin, namespace, command) else {
            return outcome;
        };
        let original = command_outcome_message(&outcome)
            .unwrap_or_else(|| "mux command did not complete".to_owned());
        let message = format!("{original}; session membership cleanup failed: {error}");
        self.last_error = Some(message.clone());
        CommandOutcome::Failed {
            code: "session_membership_cleanup_failed".to_owned(),
            message,
        }
    }
    fn compensate_completed_session_launch(
        &mut self,
        origin: MuxScope,
        namespace: &BackendConnectionNamespace,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
        outcome: CommandOutcome,
    ) -> CommandOutcome {
        let Some(allocated) = completion.allocated() else {
            if matches!(command, MuxCommand::DitchSession { .. }) {
                // Backend ditch succeeded; the durable cleanup failure is retriable from the
                // pending intent and must not be treated as an unstarted command.
                return outcome;
            }
            return self.finalize_failed_session_launch(origin, namespace, command, outcome);
        };
        let membership_error = self
            .discard_failed_session_launch(origin, namespace, command)
            .err()
            .map(|error| error.to_string());
        let original = command_outcome_message(&outcome)
            .unwrap_or_else(|| "session launch finalization failed".to_owned());
        let allocation_error = match self.binding_runtime_mut(origin) {
            Some(binding) => {
                let config = binding.multiplexer.clone();
                binding
                    .mux
                    .compensate_created_session(&allocated.session_id, &config)
                    .err()
                    .map(|error| error.to_string())
            }
            None => Some("its binding vanished".to_owned()),
        };

        match (membership_error, allocation_error) {
            (None, None) => outcome,
            (Some(error), None) => CommandOutcome::Failed {
                code: "session_membership_cleanup_failed".to_owned(),
                message: format!(
                    "{original}; session membership cleanup failed for {}: {error}",
                    allocated.session_id
                ),
            },
            (None, Some(error)) => CommandOutcome::Failed {
                code: "session_allocation_cleanup_failed".to_owned(),
                message: format!(
                    "{original}; authoritative session cleanup failed for {}: {error}",
                    allocated.session_id
                ),
            },
            (Some(membership_error), Some(allocation_error)) => CommandOutcome::Failed {
                code: "session_membership_cleanup_failed".to_owned(),
                message: format!(
                    "{original}; session membership cleanup failed for {}: {membership_error}; \
                     authoritative session cleanup also failed: {allocation_error}",
                    allocated.session_id
                ),
            },
        }
    }

    fn mux_command_completion_value(
        &self,
        origin: MuxScope,
        command: &MuxCommand,
        completion: &MuxCommandCompletion,
    ) -> serde_json::Value {
        let mut value = serde_json::Map::new();
        if let MuxCommand::CreateSession { plan } = command {
            value.insert(
                "launch".to_owned(),
                serde_json::to_value(plan).expect("serialize immutable launch plan"),
            );
        }
        if let Some(target) = completion
            .resolved_target()
            .and_then(|target| self.mux_command_target_from_event(origin, target))
        {
            value.insert(
                "resolved_target".to_owned(),
                serde_json::to_value(target).expect("serialize resolved command target"),
            );
        }
        let requested_session_id = match command {
            MuxCommand::CreateSession { plan } => Some(plan.session_id.as_str()),
            MuxCommand::CreateProjectSession { session_id, .. }
            | MuxCommand::CreateWorktreeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        };
        if let Some(allocated) = completion.allocated() {
            let session = self
                .mux_resource_target_for_scope(
                    origin,
                    ResourceKind::Session,
                    &allocated.session_id,
                    None,
                )
                .expect("authoritative allocation session generation");
            if requested_session_id.is_some() {
                value.insert(
                    "created".to_owned(),
                    serde_json::to_value(&session).expect("serialize command target"),
                );
            }
            // A flat tmux snapshot reports only its attach anchor. The authoritative recursive
            // allocation is one result, so descendants use the created session's allocation
            // generation rather than a per-pane generation assigned while bookkeeping that
            // snapshot.
            let allocation_generation =
                matches!(command, MuxCommand::CreateSession { .. }).then_some(session.generation);
            let windows = allocated
                .windows
                .iter()
                .map(|window| {
                    let mut target = self
                        .mux_resource_target_for_scope(
                            origin,
                            ResourceKind::MuxWindow,
                            &allocated.session_id,
                            Some(&window.window_id),
                        )
                        .expect("authoritative allocation window generation");
                    if let Some(generation) = allocation_generation {
                        target.generation = generation;
                    }
                    let panes = window
                        .pane_ids
                        .iter()
                        .map(|pane_id| {
                            let mut target = self
                                .mux_pane_resource_target_for_scope(
                                    origin,
                                    &allocated.session_id,
                                    &window.window_id,
                                    pane_id,
                                )
                                .expect("authoritative allocation pane generation");
                            if let Some(generation) = allocation_generation {
                                target.generation = generation;
                            }
                            target
                        })
                        .collect::<Vec<_>>();
                    serde_json::json!({
                        "window": target,
                        "panes": panes,
                    })
                })
                .collect::<Vec<_>>();
            value.insert(
                "allocated".to_owned(),
                serde_json::json!({
                    "session": session,
                    "windows": windows,
                }),
            );
        } else if let Some(session_id) = requested_session_id
            && let Some(target) =
                self.mux_resource_target_for_scope(origin, ResourceKind::Session, session_id, None)
        {
            value.insert(
                "created".to_owned(),
                serde_json::to_value(target).expect("serialize command target"),
            );
        }
        if let (Some(session_id), Some(window_id)) = (
            completion.selected_session.as_deref(),
            completion.selected_window.as_deref(),
        ) && let Some(target) = self.mux_resource_target_for_scope(
            origin,
            ResourceKind::MuxWindow,
            session_id,
            Some(window_id),
        ) {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(target).expect("serialize command target"),
            );
        }
        if !value.contains_key("focused")
            && let Some(session_id) = completion.selected_session.as_deref()
            && let Some(target) =
                self.mux_resource_target_for_scope(origin, ResourceKind::Session, session_id, None)
        {
            value.insert(
                "focused".to_owned(),
                serde_json::to_value(target).expect("serialize command target"),
            );
        }
        serde_json::Value::Object(value)
    }
    fn mux_command_target_from_event(
        &self,
        origin: MuxScope,
        target: &MuxEventTarget,
    ) -> Option<CommandTarget> {
        let session_id = target.session_id.as_deref()?;
        match (
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
            target.terminal_id.as_deref(),
        ) {
            (Some(window_id), Some(pane_id), Some(terminal_id)) => self
                .mux_terminal_resource_target_for_scope(
                    origin,
                    session_id,
                    window_id,
                    pane_id,
                    terminal_id,
                ),
            (Some(window_id), Some(pane_id), None) => {
                self.mux_pane_resource_target_for_scope(origin, session_id, window_id, pane_id)
            }
            (Some(window_id), None, _) => self.mux_resource_target_for_scope(
                origin,
                ResourceKind::MuxWindow,
                session_id,
                Some(window_id),
            ),
            (None, None, _) => {
                self.mux_resource_target_for_scope(origin, ResourceKind::Session, session_id, None)
            }
            (None, Some(_), _) => None,
        }
    }

    fn sync_native_focus_from_completion(
        &mut self,
        origin: MuxScope,
        completion: &MuxCommandCompletion,
    ) {
        if origin != self.binding.scope || !self.uses_native_terminal_layout() {
            return;
        }
        let Some(target) = completion.resolved_target() else {
            return;
        };
        let (Some(session_id), Some(window_id), Some(pane_id)) = (
            target.session_id.as_deref(),
            target.window_id.as_deref(),
            target.pane_id.as_deref(),
        ) else {
            return;
        };
        let (selected_session, selected_window, _, _) = self.selected_mux_resource_path();
        if selected_session.as_deref() == Some(session_id)
            && selected_window.as_deref() == Some(window_id)
        {
            self.focus_pane(pane_id);
        }
    }

    fn mux_resource_target_for_scope(
        &self,
        scope: MuxScope,
        kind: ResourceKind,
        session_id: &str,
        window_id: Option<&str>,
    ) -> Option<CommandTarget> {
        let runtime = self.binding_runtime(scope)?;
        let space = scope.space_id().persistence_value().to_string();
        let binding_id = scope.binding_id().persistence_value().to_string();
        let binding_generation = runtime.mux.binding_generation();
        let binding = serde_json::to_string(&(
            &self.command_instance_handle,
            &self.window_state_key,
            self.command_window_generation,
            &space,
            &binding_id,
            binding_generation,
        ))
        .expect("serialize target");
        let generation = match kind {
            ResourceKind::Session => runtime.mux.session_generation(session_id)?,
            ResourceKind::MuxWindow => runtime.mux.window_generation(session_id, window_id?)?,
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Binding
            | ResourceKind::Space
            | ResourceKind::Pane
            | ResourceKind::Terminal
            | ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => return None,
        };
        let handle = match kind {
            ResourceKind::Session => {
                serde_json::to_string(&[&binding, session_id]).expect("serialize target")
            }
            ResourceKind::MuxWindow => serde_json::to_string(&[
                &binding,
                session_id,
                window_id.expect("mux window target requires a window id"),
            ])
            .expect("serialize target"),
            ResourceKind::Instance
            | ResourceKind::ApplicationWindow
            | ResourceKind::Binding
            | ResourceKind::Space
            | ResourceKind::Pane
            | ResourceKind::Terminal
            | ResourceKind::Client
            | ResourceKind::Directory
            | ResourceKind::Worktree
            | ResourceKind::Task
            | ResourceKind::Subscription
            | ResourceKind::Surface
            | ResourceKind::Extension => return None,
        };
        Some(CommandTarget {
            kind,
            handle,
            generation,
        })
    }

    fn mux_pane_resource_target_for_scope(
        &self,
        scope: MuxScope,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
    ) -> Option<CommandTarget> {
        let runtime = self.binding_runtime(scope)?;
        let space = scope.space_id().persistence_value().to_string();
        let binding_id = scope.binding_id().persistence_value().to_string();
        let binding = serde_json::to_string(&(
            &self.command_instance_handle,
            &self.window_state_key,
            self.command_window_generation,
            &space,
            &binding_id,
            runtime.mux.binding_generation(),
        ))
        .expect("serialize target");
        Some(CommandTarget {
            kind: ResourceKind::Pane,
            handle: serde_json::to_string(&[&binding, session_id, window_id, pane_id])
                .expect("serialize target"),
            generation: runtime
                .mux
                .pane_generation(session_id, window_id, pane_id)?,
        })
    }
    fn mux_terminal_resource_target_for_scope(
        &self,
        scope: MuxScope,
        session_id: &str,
        window_id: &str,
        pane_id: &str,
        terminal_id: &str,
    ) -> Option<CommandTarget> {
        let runtime = self.binding_runtime(scope)?;
        let space = scope.space_id().persistence_value().to_string();
        let binding_id = scope.binding_id().persistence_value().to_string();
        let binding = serde_json::to_string(&(
            &self.command_instance_handle,
            &self.window_state_key,
            self.command_window_generation,
            &space,
            &binding_id,
            runtime.mux.binding_generation(),
        ))
        .expect("serialize target");
        if runtime
            .mux
            .terminal_id_for_pane(session_id, window_id, pane_id)
            != Some(terminal_id)
        {
            return None;
        }
        Some(CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::to_string(&[&binding, session_id, window_id, pane_id, terminal_id])
                .expect("serialize target"),
            generation: runtime
                .mux
                .terminal_generation(session_id, window_id, terminal_id)?,
        })
    }

    fn apply_resolved_keybind_action(
        &mut self,
        action: KeybindAction,
        target: Option<&CommandTarget>,
        mux_scope: Option<MuxScope>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        let target_space = target
            .filter(|target| target.kind == ResourceKind::Space)
            .map(|target| {
                self.validate_space_target(target)
                    .expect("resolved space target remains valid")
            });
        match (action, mux_scope, target_space) {
            (KeybindAction::Mux(action), _, _) => {
                let target_id = |target: &CommandTarget| {
                    serde_json::from_str::<Vec<String>>(&target.handle)
                        .expect("validated mux target")
                        .pop()
                        .expect("mux target identity")
                };
                let window_id = target
                    .filter(|target| target.kind == ResourceKind::MuxWindow)
                    .map(target_id);
                let pane_id = target
                    .filter(|target| target.kind == ResourceKind::Pane)
                    .map(target_id);
                self.apply_mux_key_action_to_target(action, window_id, pane_id);
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::EditSpace), _, Some(space_id)) => {
                if !self.open_edit_space_dialog_from_ui(space_id) {
                    self.last_error = Some("the target space is unavailable".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::CloseSpace), _, Some(space_id)) => {
                if !self.close_space_from_ui(space_id) {
                    self.last_error = Some("the target space cannot be closed".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::NextSpace), _, Some(space_id)) => {
                if !self.activate_relative_space_from(space_id, 1) {
                    self.last_error = Some("no next space is available".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::PreviousSpace), _, Some(space_id)) => {
                if !self.activate_relative_space_from(space_id, -1) {
                    self.last_error = Some("no previous space is available".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::SelectSpace(_)), _, Some(space_id)) => {
                if !self.activate_space_target(space_id) {
                    self.last_error = Some("the target space is unavailable".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::EditSpace), Some(scope), None) => {
                self.open_edit_space_dialog_from_ui(scope.space_id());
                effects.push(AppEffect::RequestRepaint);
            }
            (KeybindAction::App(AppAction::CloseSpace), Some(scope), None) => {
                if !self.close_space_from_ui(scope.space_id()) {
                    self.last_error = Some("the last space cannot be closed".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            (action, _, _) => self.apply_keybind_action(action, viewport, effects),
        }
    }

    fn apply_sidebar_action(&mut self, action: SidebarAction) -> bool {
        match action {
            SidebarAction::Ignore => {}
            SidebarAction::PreviousSession => self.move_sidebar_hover(-1),
            SidebarAction::NextSession => self.move_sidebar_hover(1),
            SidebarAction::ActivateSession => return self.activate_sidebar_hovered_session(),
            SidebarAction::FocusTerminal => self.input_focus = InputFocus::Terminal,
        }
        true
    }

    fn apply_keybind_action(
        &mut self,
        action: KeybindAction,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            KeybindAction::App(AppAction::ReloadConfig) => {
                if self.reload_config(effects) {
                    let path = self.config().config_path.clone();
                    self.config_hot_reload.refresh_after_reload(&path);
                }
            }
            KeybindAction::App(AppAction::Ignore) => {}
            KeybindAction::App(AppAction::NewWindow | AppAction::NewMuxSession) => {
                self.open_new_mux_session_dialog();
            }

            KeybindAction::App(AppAction::SessionPicker) => {
                self.toggle_session_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::CommandPalette) => {
                self.open_command_palette_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ChangeAppearance(mode)) => {
                self.persist_appearance_mode(mode, effects);
            }
            KeybindAction::App(AppAction::SwitchTheme) => {
                self.open_theme_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::RenameSession) => {
                self.open_rename_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::RenameTab) => {
                self.open_rename_tab_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::DitchSession) => {
                self.open_ditch_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::EditSpace) => {
                self.open_edit_space_dialog_from_ui(self.active_space_id);
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Quit) => {
                effects.push(AppEffect::QuitApplication);
            }
            KeybindAction::App(AppAction::CreateSpace) => {
                self.open_create_space_dialog_from_ui();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::CloseSpace) => {
                if !self.close_space_from_ui(self.active_space_id) {
                    self.last_error = Some("the last space cannot be closed".to_owned());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::NextSpace) => {
                if self.activate_relative_space(1) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some("no other space is available".to_owned());
                }
            }
            KeybindAction::App(AppAction::PreviousSpace) => {
                if self.activate_relative_space(-1) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some("no other space is available".to_owned());
                }
            }
            KeybindAction::App(AppAction::SelectSpace(index)) => {
                if self.select_space(index) {
                    effects.push(AppEffect::RequestRepaint);
                } else {
                    self.last_error = Some(format!("space {index} is unavailable"));
                }
            }
            KeybindAction::App(AppAction::ShowKeybinds) => {
                self.open_keybind_help_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::Close) => {
                effects.push(AppEffect::CloseWindow);
            }
            KeybindAction::App(AppAction::OpenSettings) => {
                effects.push(AppEffect::OpenSettings);
            }
            KeybindAction::App(AppAction::ToggleFullscreen) => {
                if should_toggle_native_fullscreen(&self.config().window) {
                    effects.push(AppEffect::SetFullscreen(!viewport.fullscreen));
                } else {
                    let next_maximized = next_non_native_fullscreen_state(
                        macos_handles_non_native_fullscreen_frame(&self.config().window),
                        self.macos_non_native_fullscreen_active,
                        viewport.maximized,
                    );
                    self.macos_non_native_fullscreen_active = next_maximized;
                    if next_maximized {
                        self.macos_non_native_fullscreen_pending_apply =
                            !apply_macos_non_native_fullscreen_presentation(&self.config().window);
                    } else {
                        restore_macos_presentation();
                        self.macos_non_native_fullscreen_pending_apply = false;
                    }
                    effects.push(AppEffect::SetFullscreen(false));
                    if !macos_handles_non_native_fullscreen_frame(&self.config().window) {
                        effects.push(AppEffect::SetMaximized(next_maximized));
                    }
                }
            }
            KeybindAction::App(AppAction::ToggleSidebarFocus) => {
                self.close_overlay_dialogs();
                if self.input_focus == InputFocus::Sidebar {
                    self.input_focus = InputFocus::Terminal;
                } else {
                    self.config_state.current_mut().chrome.sidebar = true;
                    self.input_focus = InputFocus::Sidebar;
                    self.sidebar_hovered_session = self
                        .binding
                        .mux
                        .selected_session()
                        .and_then(|selected| self.session_target_matching(selected))
                        .or_else(|| self.session_navigation_targets().into_iter().next());
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::App(AppAction::ToggleSidebarVisibility) => {
                let chrome = &mut self.config_state.current_mut().chrome;
                chrome.sidebar = !chrome.sidebar;
                if !chrome.sidebar {
                    self.input_focus = InputFocus::Terminal;
                }
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Mux(action) => {
                self.apply_mux_key_action(action);
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self.binding.terminal.write_input(&bytes) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::Find(action) => self.apply_terminal_find_action(action, effects),
            KeybindAction::CopyToClipboard(format) => {
                self.copy_terminal_selection_or_request_copy(format, effects);
            }
            KeybindAction::CopyMode => {
                self.enter_terminal_copy_mode(effects);
            }
            KeybindAction::PasteFromClipboard => match read_clipboard_text() {
                Ok(Some(text)) => {
                    if let Err(error) = self.binding.terminal.write_paste(&text) {
                        self.last_error = Some(error.to_string());
                    }
                }
                Ok(None) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            },
        }
    }

    fn consume_copy_shortcut_for_terminal_selection(
        &mut self,
        events: &mut Vec<egui::Event>,
    ) -> usize {
        let Some(index) = events.iter().position(copy_shortcut_pressed) else {
            return 0;
        };
        if !self.copy_terminal_selection_if_any(CopyToClipboard::Mixed) {
            return 0;
        }
        events.remove(index);
        1
    }

    fn write_terminal_selection_to_clipboard(&mut self, format: CopyToClipboard) -> Result<bool> {
        let mut selection = |format| self.binding.terminal.format_selection(format);
        match format {
            CopyToClipboard::Plain => {
                let Some(bytes) = selection(TerminalSelectionFormat::PlainText)? else {
                    return Ok(false);
                };
                write_clipboard_text(&String::from_utf8_lossy(&bytes))?;
            }
            CopyToClipboard::Vt => {
                let Some(bytes) = selection(TerminalSelectionFormat::Vt)? else {
                    return Ok(false);
                };
                write_clipboard_text(&String::from_utf8_lossy(&bytes))?;
            }
            CopyToClipboard::Html => {
                let Some(bytes) = selection(TerminalSelectionFormat::Html)? else {
                    return Ok(false);
                };
                write_clipboard_html(&String::from_utf8_lossy(&bytes), None)?;
            }
            CopyToClipboard::Mixed => {
                let Some(plain) = selection(TerminalSelectionFormat::PlainText)? else {
                    return Ok(false);
                };
                let Some(html) = selection(TerminalSelectionFormat::Html)? else {
                    return Ok(false);
                };
                write_clipboard_html(
                    &String::from_utf8_lossy(&html),
                    Some(&String::from_utf8_lossy(&plain)),
                )?;
            }
        }
        Ok(true)
    }

    fn copy_terminal_selection_if_any(&mut self, format: CopyToClipboard) -> bool {
        match self.write_terminal_selection_to_clipboard(format) {
            Ok(copied) => copied,
            Err(error) => {
                self.last_error = Some(error.to_string());
                false
            }
        }
    }

    fn copy_terminal_selection_or_request_copy(
        &mut self,
        format: CopyToClipboard,
        effects: &mut Vec<AppEffect>,
    ) {
        if !self.copy_terminal_selection_if_any(format) {
            effects.push(AppEffect::RequestCopy);
        }
    }

    /// The attach client exited. For a local binding that means the pane it was showing ended, so
    /// the pane closes. For a remote one it means either that or a dropped connection, and the two
    /// look identical from here — so bootty reconnects instead of closing. The sessions live on the
    /// other host and outlive the link; closing on a network blip would kill work the user still
    /// has. A pane that really did end is gone from the next snapshot, which closes it properly.
    fn handle_attach_client_exit(&mut self, now: Instant) {
        let Some(remote) = self.binding.multiplexer.remote.clone() else {
            self.close_active_pane();
            return;
        };
        if self
            .binding
            .reattach
            .is_some_and(|reattach| !reattach.started)
        {
            return;
        }
        let attached_for = self
            .binding
            .remote_attach_started
            .map(|started| now.saturating_duration_since(started));
        let reattach = RemoteReattach::after_failure(self.binding.reattach, attached_for, now);
        let error = format!(
            "lost the connection to {}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.binding.mux.set_availability_error(Some(error));
        self.binding.reattach = Some(reattach);
    }

    fn handle_attach_start_failure(&mut self, now: Instant, detail: &str) {
        let Some(remote) = self.binding.multiplexer.remote.clone() else {
            return;
        };
        let reattach = RemoteReattach::after_failure(self.binding.reattach, None, now);
        let error = format!(
            "could not connect to {}: {detail}; reconnecting (attempt {})",
            remote.host, reattach.attempts
        );
        self.last_error = Some(error.clone());
        self.binding.mux.set_availability_error(Some(error));
        self.binding.reattach = Some(reattach);
    }

    fn resolve_remote_attach_exit_after_refresh(&mut self, refresh_completed: bool) {
        if self
            .binding
            .resolve_empty_remote_after_attach_exit(refresh_completed)
            && self
                .last_error
                .as_deref()
                .is_some_and(|error| error.starts_with("lost the connection to "))
        {
            self.last_error = None;
        }
    }
    /// A remote attach client that has been alive long enough proves the connection is back, so the
    /// next outage starts its backoff from the beginning rather than from where this one left off.
    fn note_attach_client_alive(&mut self, now: Instant) {
        let established = self.binding.remote_attach_started.is_some_and(|started| {
            now.saturating_duration_since(started) >= RemoteReattach::STABLE_AFTER
        });
        if established
            && self
                .binding
                .reattach
                .is_some_and(|reattach| reattach.started)
        {
            self.binding.reattach = None;
            self.binding.mux.set_availability_error(None);
        }
    }

    /// Drop the dead attach client once its backoff has passed. Clearing the pane's target is what
    /// asks for a new one: this frame's pane sync starts a fresh client for the same session.
    fn start_due_reattach(&mut self, now: Instant, effects: &mut Vec<AppEffect>) {
        let Some(mut reattach) = self.binding.reattach else {
            return;
        };
        if !reattach.due(now) {
            // Nothing else is guaranteed to wake the frame loop while a pane sits disconnected, so
            // the wait itself asks for the frame that ends it.
            if !reattach.started {
                effects.push(AppEffect::RepaintAfter(
                    reattach.retry_at.saturating_duration_since(now),
                ));
            }
            return;
        }
        reattach.started = true;
        self.binding.reattach = Some(reattach);
        self.binding.remote_attach_started = Some(now);
        self.binding.terminal.discard_active_pane();
    }

    pub fn reconnect_space_from_ui(&mut self, space_id: SpaceId) -> bool {
        let now = Instant::now();
        if space_id == self.active_space_id {
            let mut restarted = Self::restart_remote_binding(&mut self.binding, now);
            for binding in &mut self.inactive_bindings {
                restarted |= Self::restart_remote_binding(binding, now);
            }
            return restarted;
        }
        let Some(space) = self
            .inactive_spaces
            .iter_mut()
            .find(|space| space.id == space_id)
        else {
            return false;
        };
        let mut restarted = false;
        for binding in space.bindings_mut() {
            restarted |= Self::restart_remote_binding(binding, now);
        }
        restarted
    }

    fn has_degraded_remote(&self) -> bool {
        self.binding.reattach.is_some()
            || self
                .inactive_bindings
                .iter()
                .any(|binding| binding.reattach.is_some())
            || self
                .inactive_spaces
                .iter()
                .flat_map(SpaceRuntime::bindings)
                .any(|binding| binding.reattach.is_some())
    }

    fn reset_remote_reconnects(&mut self, now: Instant) {
        if self.binding.reattach.is_some() {
            Self::restart_remote_binding(&mut self.binding, now);
        }
        for binding in &mut self.inactive_bindings {
            if binding.reattach.is_some() {
                Self::restart_remote_binding(binding, now);
            }
        }
        for space in &mut self.inactive_spaces {
            for binding in space.bindings_mut() {
                if binding.reattach.is_some() {
                    Self::restart_remote_binding(binding, now);
                }
            }
        }
    }

    fn restart_remote_binding(binding: &mut BindingRuntime, now: Instant) -> bool {
        let Some(remote) = binding.multiplexer.remote.as_ref() else {
            return false;
        };
        binding.reattach = Some(RemoteReattach {
            retry_at: now,
            attempts: 1,
            started: true,
        });
        binding.remote_attach_started = Some(now);
        binding
            .mux
            .set_availability_error(Some(format!("reconnecting to {}", remote.host)));
        binding.terminal.discard_active_pane();
        true
    }
    // Close the focused pane (cmd+w or its shell exiting) and let the mux cascade to the tab. The
    // active terminal is dropped here so its PTY is reaped; sync_mux_anchor then attaches whatever
    // pane the mux selected next (or idle when the session has no tabs left).
    fn close_active_pane(&mut self) {
        self.close_target_pane(None);
    }

    fn close_target_pane(&mut self, target_pane_id: Option<&str>) {
        if self.uses_native_terminal_layout() {
            if let Some(pane_id) = target_pane_id
                .map(str::to_owned)
                .or_else(|| self.focused_pane())
            {
                self.close_pane(&pane_id);
            }
            return;
        }
        let session_id = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: target_pane_id.map(str::to_owned),
            },
        );
        self.binding.terminal.discard_active_pane();
    }

    /// Close a specific native pane: remove it from the backend window, kill its PTY, collapse the
    /// split layout, and re-activate the surviving focused pane this frame so it doesn't flash idle.
    fn close_pane(&mut self, pane_id: &str) {
        let session_id = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let mux_config = self.active_multiplexer().clone();
        self.binding.mux.execute_command(
            &self.repaint,
            &mux_config,
            MuxCommand::ClosePane {
                session_id,
                pane_id: Some(pane_id.to_owned()),
            },
        );
        self.binding
            .terminal
            .discard_scoped_pane(self.binding.scope, pane_id);
        let key = self.current_window_key();
        if let Some(layout) = self.binding.pane_layouts.get_mut(&key) {
            layout.remove(pane_id);
        }
        let _ = self.sync_terminal_panes();
    }

    fn mux_operation_for_action(action: MuxKeyAction) -> Option<BindingOperation> {
        match action {
            MuxKeyAction::NewTab => Some(BindingOperation::CreateWindow),
            MuxKeyAction::NextTab
            | MuxKeyAction::PreviousTab
            | MuxKeyAction::LastTab
            | MuxKeyAction::SelectTab(_) => Some(BindingOperation::NavigateWindow),
            MuxKeyAction::MoveTab(_) => Some(BindingOperation::MoveWindow),
            MuxKeyAction::SplitPane(_) => Some(BindingOperation::SplitPane),
            MuxKeyAction::SelectPane(_) | MuxKeyAction::NextPane | MuxKeyAction::PreviousPane => {
                Some(BindingOperation::NavigatePane)
            }
            MuxKeyAction::KillPane | MuxKeyAction::ClosePane => Some(BindingOperation::ClosePane),
            MuxKeyAction::TogglePaneZoom => Some(BindingOperation::TogglePaneZoom),
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => None,
        }
    }

    fn mux_operation_for_action_for_binding(
        action: MuxKeyAction,
        binding: &BindingRuntime,
    ) -> Option<BindingOperation> {
        if action == MuxKeyAction::NewTab && binding.mux.selected_session().is_none() {
            Some(BindingOperation::CreateProjectSession)
        } else {
            Self::mux_operation_for_action(action)
        }
    }

    fn apply_mux_key_action(&mut self, action: MuxKeyAction) {
        self.apply_mux_key_action_to_target(action, None, None);
    }

    fn apply_mux_key_action_to_target(
        &mut self,
        action: MuxKeyAction,
        target_window_id: Option<String>,
        target_pane_id: Option<String>,
    ) {
        if self.apply_session_navigation_action(action) {
            return;
        }
        if let MuxKeyAction::MoveSession(delta) = action {
            self.move_selected_session(delta);
            return;
        }
        if matches!(action, MuxKeyAction::ClosePane) {
            self.close_target_pane(target_pane_id.as_deref());
            return;
        }
        // On the native engine, killing a pane means removing the focused split leaf and collapsing
        // the layout, same as closing it. Other backends keep tmux/zellij kill-pane semantics.
        if self.uses_native_terminal_layout() && matches!(action, MuxKeyAction::KillPane) {
            self.close_target_pane(target_pane_id.as_deref());
            return;
        }
        if let MuxKeyAction::SplitPane(direction) = action {
            self.split_focused_pane(direction, target_pane_id.as_deref());
            return;
        }
        // On the native engine, directional pane selection moves focus geometrically across the
        // egui split layout. Other backends keep their own (cycling) pane selection.
        if let MuxKeyAction::SelectPane(direction) = action
            && self.uses_native_terminal_layout()
        {
            self.focus_pane_neighbor(layout_direction(direction));
            return;
        }
        // Likewise next/previous pane cycle focus across the split layout's leaves; the mux-state
        // pane selection the command path mutates is invisible to the native layout.
        if self.uses_native_terminal_layout() {
            let delta = match action {
                MuxKeyAction::NextPane => Some(1),
                MuxKeyAction::PreviousPane => Some(-1),
                _ => None,
            };
            if let Some(delta) = delta {
                self.focus_pane_relative(delta);
                return;
            }
        }
        if matches!(action, MuxKeyAction::NewTab) && self.binding.mux.selected_session().is_none() {
            let cwd = new_mux_session_request_with_name(self.config(), "").cwd;
            self.create_project_session_for_cwd(cwd);
            self.sync_native_layout_terminal_now();
            return;
        }
        let selected_session = self
            .binding
            .mux
            .selected_session()
            .unwrap_or("local")
            .to_owned();
        let selected_cwd = terminal_cwd_for_mux_command(
            self.binding
                .terminal
                .current_working_directory()
                .ok()
                .flatten(),
            self.binding
                .mux
                .selected_session_anchor()
                .and_then(|anchor| anchor.cwd.clone()),
        );
        let command = match action {
            MuxKeyAction::NewTab => MuxCommand::NewWindow {
                session_id: selected_session,
                cwd: selected_cwd,
            },
            MuxKeyAction::NextTab => MuxCommand::ActivateNextWindow {
                session_id: selected_session,
            },
            MuxKeyAction::PreviousTab => MuxCommand::ActivatePreviousWindow {
                session_id: selected_session,
            },
            MuxKeyAction::LastTab => MuxCommand::ActivateLastWindow {
                session_id: selected_session,
            },
            MuxKeyAction::SelectTab(index) => MuxCommand::ActivateWindowIndex {
                session_id: selected_session,
                index,
            },
            MuxKeyAction::MoveTab(delta) => MuxCommand::MoveWindow {
                session_id: selected_session,
                window_id: self.binding.mux.selected_window().map(str::to_owned),
                delta,
            },
            MuxKeyAction::SplitPane(_) => {
                unreachable!("split pane is handled before the command match")
            }
            MuxKeyAction::SelectPane(direction) => MuxCommand::SelectPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
                direction,
            },
            MuxKeyAction::NextPane => MuxCommand::SelectNextPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
            },
            MuxKeyAction::PreviousPane => MuxCommand::SelectPreviousPane {
                session_id: selected_session,
                window_id: target_window_id.clone(),
            },
            MuxKeyAction::KillPane => MuxCommand::KillPane {
                session_id: selected_session,
                pane_id: target_pane_id.clone(),
            },
            MuxKeyAction::ClosePane => {
                unreachable!("close pane is handled before the command match")
            }
            MuxKeyAction::TogglePaneZoom => MuxCommand::TogglePaneZoom {
                session_id: selected_session,
                pane_id: target_pane_id.clone(),
            },
            MuxKeyAction::NextSession
            | MuxKeyAction::PreviousSession
            | MuxKeyAction::LastSession
            | MuxKeyAction::SelectSession(_)
            | MuxKeyAction::MoveSession(_) => {
                unreachable!("session actions are handled by Bootty state")
            }
        };
        let mux_config = self.active_multiplexer().clone();
        self.binding
            .mux
            .execute_command(&self.repaint, &mux_config, command);
        self.sync_native_layout_terminal_now();
    }

    fn ensure_sidebar_hovered_session(&mut self) {
        if self.sidebar_hovered_index().is_some() {
            return;
        }
        self.sidebar_hovered_session = self
            .binding
            .mux
            .selected_session()
            .and_then(|selected| self.session_target_matching(selected))
            .or_else(|| self.session_navigation_targets().into_iter().next());
    }

    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let targets = self.session_navigation_targets();
        let Some(current) = self.sidebar_hovered_index() else {
            return;
        };
        let next = (current as isize + delta).rem_euclid(targets.len() as isize) as usize;
        self.sidebar_hovered_session = targets.get(next).cloned();
    }

    fn activate_sidebar_hovered_session(&mut self) -> bool {
        self.ensure_sidebar_hovered_session();
        let activated = self
            .sidebar_hovered_session
            .clone()
            .is_some_and(|target| self.activate_scoped_session_from_ui(&target));
        self.input_focus = InputFocus::Terminal;
        activated
    }

    fn sidebar_hovered_index(&self) -> Option<usize> {
        let hovered = self.sidebar_hovered_session.as_ref()?;
        self.session_navigation_targets()
            .iter()
            .position(|target| target == hovered)
    }

    fn session_navigation_targets(&self) -> Vec<ScopedSessionTarget> {
        self.binding_session_groups()
            .into_iter()
            .flat_map(|group| {
                group
                    .sessions
                    .into_iter()
                    .map(move |session| ScopedSessionTarget::new(group.scope, session.id))
            })
            .collect()
    }

    fn session_target_matching(&self, value: &str) -> Option<ScopedSessionTarget> {
        self.binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == value || session.name == value)
            .map(|session| ScopedSessionTarget::new(self.binding.scope, session.id.clone()))
    }

    fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .binding
                .mux
                .sessions()
                .get(index.saturating_sub(1) as usize)
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(1),
            MuxKeyAction::PreviousSession => self.relative_session(-1),
            MuxKeyAction::LastSession => self
                .binding
                .mux
                .previous_selected_session()
                .map(str::to_owned),
            // Not a session-navigation action: let the caller route it.
            _ => return false,
        };
        // Activate when there is a target, but always report the action as handled. Missing a
        // target (e.g. last_session with no prior session) is a no-op here; falling through would
        // reach the command builder's `unreachable!` for these Bootty-owned actions and panic.
        if let Some(target) = target {
            self.binding.mux.activate_session(&target);
            self.persist_rmux_restore_state();
            self.sync_native_layout_terminal_now();
        }
        true
    }

    fn relative_session(&self, delta: isize) -> Option<String> {
        let sessions = self.binding.mux.sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.binding.mux.selected_session();
        let current = selected
            .and_then(|selected| {
                sessions
                    .iter()
                    .position(|session| session.id == selected || session.name == selected)
            })
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(sessions.len() as isize) as usize;
        sessions.get(next).map(|session| session.id.clone())
    }

    fn apply_terminal_find_action(
        &mut self,
        action: TerminalFindAction,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            TerminalFindAction::Prompt => {
                self.open_terminal_find_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Close => {
                self.terminal_find_dialog = None;
                self.clear_terminal_search();
                self.input_focus = InputFocus::Terminal;
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Search(query) => {
                self.search_terminal(&query, TerminalSearchDirection::Current);
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::SearchSelection => {
                if let Some(query) = self.selected_terminal_text() {
                    self.search_terminal(&query, TerminalSearchDirection::Current);
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            TerminalFindAction::Previous => {
                let query = self.last_terminal_search.clone();
                if query.is_empty() {
                    self.open_terminal_find_dialog();
                } else {
                    self.search_terminal(&query, TerminalSearchDirection::Previous);
                }
                effects.push(AppEffect::RequestRepaint);
            }
            TerminalFindAction::Next => {
                let query = self.last_terminal_search.clone();
                if query.is_empty() {
                    self.open_terminal_find_dialog();
                } else {
                    self.search_terminal(&query, TerminalSearchDirection::Next);
                }
                effects.push(AppEffect::RequestRepaint);
            }
        }
    }

    fn selected_terminal_text(&mut self) -> Option<String> {
        match self
            .binding
            .terminal
            .format_selection(TerminalSelectionFormat::PlainText)
        {
            Ok(Some(bytes)) => Some(String::from_utf8_lossy(&bytes).trim().to_owned())
                .filter(|text| !text.is_empty()),
            Ok(None) => None,
            Err(error) => {
                self.last_error = Some(error.to_string());
                None
            }
        }
    }

    fn clear_terminal_search(&mut self) {
        if let Err(error) = self
            .binding
            .terminal
            .search_viewport("", TerminalSearchDirection::Current)
        {
            self.last_error = Some(error.to_string());
        }
    }

    fn search_terminal(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        self.search_terminal_with_direction_recording(query, direction, true)
    }

    fn search_terminal_with_direction_recording(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
        record_direction: bool,
    ) -> TerminalFindResult {
        let query = query.trim();
        if query.is_empty() {
            self.clear_terminal_search();
            return TerminalFindResult::default();
        }
        self.last_terminal_search = query.to_owned();
        if record_direction {
            self.record_terminal_search_direction(direction);
        }
        if self.terminal_copy_mode_active() {
            return self.search_copy_mode_terminal(query, direction);
        }
        match self.search_focused_terminal_runtime(query, direction) {
            Ok(result) => result,
            Err(error) => {
                self.last_error = Some(error.to_string());
                TerminalFindResult::default()
            }
        }
    }

    fn search_focused_terminal_runtime(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<TerminalFindResult> {
        if let Some(pane_id) = self.focused_pane()
            && let Some(source) = self.binding.terminal.focused_render_source(&pane_id)
        {
            let found = source.search_viewport(query, direction)?;
            let frame = source.extract_frame()?;
            return Ok(TerminalFindResult {
                found,
                active_index: frame.active_search_match_index,
                match_count: frame.search_match_count,
            });
        }

        let found = self.binding.terminal.search_viewport(query, direction)?;
        let frame = self.binding.terminal.extract_frame()?;
        Ok(TerminalFindResult {
            found,
            active_index: frame.active_search_match_index,
            match_count: frame.search_match_count,
        })
    }

    fn search_copy_mode_terminal(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> TerminalFindResult {
        match TerminalRenderSource::handle_copy_mode_action(
            self.binding.terminal.as_mut(),
            TerminalCopyModeAction::Search {
                query: query.to_owned(),
                direction,
            },
        ) {
            Ok(outcome) => outcome
                .search
                .map_or_else(TerminalFindResult::default, |search| {
                    self.terminal_find_result_from_frame(search.found)
                }),
            Err(error) => {
                self.last_error = Some(error.to_string());
                TerminalFindResult::default()
            }
        }
    }

    fn terminal_find_result_from_frame(&mut self, found: bool) -> TerminalFindResult {
        let (active_index, match_count) = self
            .binding
            .terminal
            .extract_frame()
            .map(|frame| (frame.active_search_match_index, frame.search_match_count))
            .unwrap_or_else(|error| {
                self.last_error = Some(error.to_string());
                (None, 0)
            });
        TerminalFindResult {
            found,
            active_index,
            match_count,
        }
    }

    fn apply_terminal_scroll_action(&mut self, action: TerminalScrollAction) {
        let delta = match action {
            TerminalScrollAction::Top => -1_000_000,
            TerminalScrollAction::Bottom => 1_000_000,
            TerminalScrollAction::PageUp => -(self.binding.terminal.grid_size().1 as isize),
            TerminalScrollAction::PageDown => self.binding.terminal.grid_size().1 as isize,
            TerminalScrollAction::Lines(lines) => isize::from(lines),
        };
        if let Err(error) = self.binding.terminal.scroll_viewport_delta(delta) {
            self.last_error = Some(error.to_string());
        }
    }

    fn apply_terminal_input(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) {
        match command {
            TerminalInputCommand::Text(text) => {
                if let Err(error) = self.binding.terminal.write_input(text.as_bytes()) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Paste(text) => {
                if let Err(error) = self.binding.terminal.write_paste(&text) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Focus(focused) => {
                if let Err(error) = self.binding.terminal.encode_focus(focused) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::Key(input) => {
                if let Err(error) = self.binding.terminal.encode_key(input) {
                    self.last_error = Some(error.to_string());
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            TerminalInputCommand::Mouse(input) => {
                if let Err(error) = self.binding.terminal.encode_mouse(input) {
                    self.last_error = Some(error.to_string());
                }
            }
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => {
                if let Err(error) = self
                    .binding
                    .terminal
                    .handle_mouse_wheel(input, scroll_delta)
                {
                    self.last_error = Some(error.to_string());
                }
            }
        }
    }

    fn apply_font_size_action(&mut self, action: FontSizeAction, effects: &mut Vec<AppEffect>) {
        let default_size = BoottyConfig::default().font.size;
        let current_size = self.config().font.size;
        let next_size = match action {
            FontSizeAction::Increase(delta) => current_size + delta,
            FontSizeAction::Decrease(delta) => current_size - delta,
            FontSizeAction::Reset => default_size,
            FontSizeAction::Set(size) => size,
        }
        .max(1.0);
        self.config_state.current_mut().font.size = next_size;
        let text_config = self.config().font.terminal_text_config();
        if let Some(existing) = effects.iter_mut().rev().find_map(|effect| match effect {
            AppEffect::SetTerminalTextConfig(existing) => Some(existing),
            _ => None,
        }) {
            *existing = text_config;
        } else {
            effects.push(AppEffect::SetTerminalTextConfig(text_config));
        }
    }
}

fn should_toggle_native_fullscreen(window: &WindowConfig) -> bool {
    !window.non_native_fullscreen_enabled()
}

fn next_non_native_fullscreen_state(
    macos_handles_frame: bool,
    tracked_active: bool,
    viewport_maximized: bool,
) -> bool {
    if macos_handles_frame {
        !tracked_active
    } else {
        !viewport_maximized
    }
}

enum DitchCleanupError {
    ConfirmationRequired(Box<WorktreeRemovalConfirmation>),
    Failed(String),
    PartialCleanup(String),
    StaleTarget(String),
}

impl std::fmt::Display for DitchCleanupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfirmationRequired(_) => formatter.write_str(
                "worktree removal is shared with active sessions and needs explicit confirmation",
            ),
            Self::Failed(message) | Self::PartialCleanup(message) | Self::StaleTarget(message) => {
                formatter.write_str(message)
            }
        }
    }
}

/// Run the Git side of a ditch before the session is killed. Linked-worktree
/// removal is routed through the same independent safety service exposed to
/// commands, so an unchecked UI path cannot bypass the locked final recheck.
fn run_ditch_cleanup(
    state: &AppState,
    session_id: &str,
    cwd: Option<&str>,
    action: &DitchAction,
    confirmation: Option<&WorktreeRemovalConfirmation>,
) -> Result<(), DitchCleanupError> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    match action {
        DitchAction::KillOnly => Ok(()),
        DitchAction::DetachWorktree => {
            crate::git::detach_head(cwd).map_err(DitchCleanupError::Failed)
        }
        DitchAction::RemoveWorktree { force } => {
            remove_ditch_worktree(state, session_id, cwd, *force, confirmation.cloned())
        }
        DitchAction::RemoveWorktreeAndBranch {
            force,
            branch,
            repo,
        } => {
            let branch_target = crate::git::capture_branch_removal_target(repo, branch)
                .map_err(DitchCleanupError::Failed)?;
            // A retry may only need the branch deletion when the worktree was
            // removed by a prior claim-safe invocation.
            if std::path::Path::new(cwd).exists() {
                remove_ditch_worktree(state, session_id, cwd, *force, confirmation.cloned())?;
            }
            crate::git::delete_branch_if_unchanged(repo, &branch_target)
                .map_err(DitchCleanupError::PartialCleanup)
        }
    }
}

fn remove_ditch_worktree(
    state: &AppState,
    session_id: &str,
    cwd: &str,
    force: bool,
    confirmation: Option<WorktreeRemovalConfirmation>,
) -> Result<(), DitchCleanupError> {
    let service = state.worktree_service();
    let details = service
        .get(cwd)
        .map_err(|error| DitchCleanupError::Failed(error.to_string()))?;
    let worktree = details.worktree;
    let assessment = match service.remove(crate::git::WorktreeRemoveRequest {
        worktree: worktree.clone(),
        force,
        requester_session: Some(state.directory_session_ref(session_id)),
        confirmation,
    }) {
        Ok(assessment) => assessment,
        Err(WorktreeServiceError::Claims(
            crate::automation::directory::DirectoryClaimsError::StaleRemovalTarget {
                expected, ..
            },
        )) => {
            return Err(DitchCleanupError::StaleTarget(format!(
                "worktree removal target changed before deletion at {:?}; no cleanup was applied; \
                 fresh confirmation is required",
                expected.path
            )));
        }
        Err(WorktreeServiceError::Claims(
            crate::automation::directory::DirectoryClaimsError::ConfirmationRequired { assessment }
            | crate::automation::directory::DirectoryClaimsError::StaleConfirmation { assessment },
        )) => {
            let Some(confirmation) = assessment.bound_confirmation() else {
                return Err(DitchCleanupError::Failed(
                    "worktree removal confirmation no longer matches active claims".to_owned(),
                ));
            };
            return Err(DitchCleanupError::ConfirmationRequired(Box::new(
                confirmation,
            )));
        }
        Err(error) => return Err(DitchCleanupError::Failed(error.to_string())),
    };
    if let Err(error) = publish_worktree_changed(
        &state.automation,
        &service,
        &state.binding,
        &worktree,
        json!({
            "change": "removed",
            "worktree": worktree,
            "assessment": assessment,
        }),
    ) {
        // The worktree deletion is already committed and cannot be rolled back. Reporting this
        // as a cleanup failure would keep the session open and make retry target a deleted path.
        eprintln!("worktree removed, but its lifecycle event could not be published: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandRegistry;
    use crate::config::{MultiplexerBackendConfig, WindowFullscreen};
    use crate::mux::{
        backend::{
            MuxAllocatedResources, MuxAllocatedWindow, MuxBackend, MuxBackendCommandCompletion,
        },
        capability::{BindingCapabilityDescriptor, BindingOperationAvailability},
        command::{
            MuxCommand, MuxDirection, MuxPaneLaunch, MuxPaneLaunchPlan, MuxPaneResize,
            MuxSessionLaunchPlan, MuxSplitDirection, MuxSplitLaunch, MuxWindowLaunchPlan,
        },
        native::NativeBackend,
        snapshot::MuxSnapshot,
    };
    use anyhow::Context;
    use std::{
        sync::Arc,
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::{Duration, Instant},
    };

    #[test]
    fn recorded_chord_lowercases_letters_but_keeps_named_keys() {
        // The physical-key serializer emits uppercase letters; recorded chords are lowercased to
        // match the default keybind convention (cmd+alt+x), while named keys keep their casing so
        // they still parse and match.
        assert_eq!(
            normalize_recorded_chord("cmd+alt+X".to_owned()),
            "cmd+alt+x"
        );
        assert_eq!(normalize_recorded_chord("cmd+V".to_owned()), "cmd+v");
        assert_eq!(normalize_recorded_chord("ctrl+KeyV".to_owned()), "ctrl+v");
        assert_eq!(
            normalize_recorded_chord("ctrl+shift+Digit1".to_owned()),
            "ctrl+shift+1"
        );
        assert_eq!(normalize_recorded_chord("ctrl+Tab".to_owned()), "ctrl+Tab");
        assert_eq!(normalize_recorded_chord("cmd+F5".to_owned()), "cmd+F5");
        assert_eq!(normalize_recorded_chord("cmd+=".to_owned()), "cmd+=");
    }

    static TEST_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_test_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let sequence = TEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{nanos}-{sequence}")
    }
    fn ensure_test_binding(
        config_path: &std::path::Path,
        scope: MuxScope,
        backend: MultiplexerBackendConfig,
    ) {
        let workspace = WorkspaceStore::for_config_path(config_path);
        let conn =
            crate::workspace::open_db(workspace.path()).expect("open test workspace database");
        let space_id = scope.space_id().persistence_value();
        let binding_id = scope.binding_id().persistence_value();
        let space_exists = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM workspace_spaces WHERE id = ?1)",
                [space_id],
                |row| row.get::<_, bool>(0),
            )
            .expect("check test workspace space");
        if !space_exists {
            let position = conn
                .query_row(
                    "SELECT COALESCE(MAX(position) + 1, 0) FROM workspace_spaces",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .expect("allocate test workspace space position");
            conn.execute(
                "INSERT INTO workspace_spaces
                    (id, remote_id, name, icon, color, tint_sidebar, position)
                 VALUES (?1, ?2, ?3, 'folder', '#7AA2F7', 0, ?4)",
                rusqlite::params![
                    space_id,
                    format!("test-space-{space_id}"),
                    format!("Test Space {space_id}"),
                    position
                ],
            )
            .expect("insert test workspace space");
        }
        let backend = match backend {
            MultiplexerBackendConfig::Native => "native",
            MultiplexerBackendConfig::Rmux => "rmux",
            MultiplexerBackendConfig::Tmux => "tmux",
            MultiplexerBackendConfig::Zellij => "zellij",
        };
        conn.execute(
            "INSERT OR IGNORE INTO workspace_bindings
                (id, space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4, 0)",
            rusqlite::params![
                binding_id,
                space_id,
                format!("Test Binding {binding_id}"),
                backend
            ],
        )
        .expect("insert test workspace binding");
        conn.execute(
            "INSERT OR IGNORE INTO workspace_session_groups (binding_id, name, position)
             VALUES (?1, '', 0)",
            [binding_id],
        )
        .expect("insert test workspace session group");
    }

    fn route_selection_test_events(
        events: Vec<egui::Event>,
        context: TerminalSelectionRouteContext<'_>,
    ) -> (
        Vec<egui::Event>,
        Vec<TerminalSelectionAction>,
        TerminalSelectionRouter,
    ) {
        let mut router = TerminalSelectionRouter::default();
        let (terminal_events, selection_actions) = router.route_events(events, context);
        (terminal_events, selection_actions, router)
    }

    #[test]
    fn remove_first_paste_event_removes_only_one_paste_event() {
        let mut events = vec![
            egui::Event::Text("before".to_owned()),
            egui::Event::Paste("first".to_owned()),
            egui::Event::Paste("second".to_owned()),
        ];

        assert!(remove_first_paste_event(&mut events));
        assert_eq!(
            events,
            vec![
                egui::Event::Text("before".to_owned()),
                egui::Event::Paste("second".to_owned())
            ]
        );
    }

    #[test]
    fn find_bar_focus_keeps_text_in_ui_but_routes_terminal_pointer_events() {
        let find_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 40.0));
        let outside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(120.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let routed = route_find_modeless_events(
            InputFocus::Picker,
            vec![outside_press.clone(), egui::Event::Text("a".to_owned())],
            Some(find_rect),
            None,
        );

        assert_eq!(routed.terminal_events, vec![outside_press]);
        assert_eq!(routed.ui_events, vec![egui::Event::Text("a".to_owned())]);
    }

    #[test]
    fn terminal_focus_does_not_route_find_bar_pointer_events_to_terminal() {
        let find_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 40.0));
        let inside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(20.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let outside_press = egui::Event::PointerButton {
            pos: egui::Pos2::new(120.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let routed = route_find_modeless_events(
            InputFocus::Terminal,
            vec![inside_press.clone(), outside_press.clone()],
            Some(find_rect),
            None,
        );

        assert_eq!(routed.ui_events, vec![inside_press]);
        assert_eq!(routed.terminal_events, vec![outside_press]);
    }

    #[test]
    fn bootty_selection_drag_is_not_sent_to_terminal_input() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, 10.0)),
            egui::Event::PointerButton {
                pos: egui::Pos2::new(20.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::Text("x".to_owned()),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, vec![egui::Event::Text("x".to_owned())]);
        assert_eq!(selection_actions.len(), 3);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(matches!(
            selection_actions[1],
            TerminalSelectionAction::Update(_)
        ));
        assert!(matches!(
            selection_actions[2],
            TerminalSelectionAction::End(_)
        ));
        assert!(!router.is_active());
    }

    #[test]
    fn selection_drag_above_terminal_scrolls_and_updates_at_viewport_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 10.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, -25.0)),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert!(terminal_events.is_empty());
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert_eq!(selection_actions.len(), 1);
        let scroll_actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );
        assert_eq!(scroll_actions[0], TerminalSelectionAction::Scroll(-2));
        let TerminalSelectionAction::Update(event) = scroll_actions[1] else {
            panic!("expected edge update after scroll");
        };
        assert_eq!(event.position.y, 0.0);
        assert!(router.is_active());
    }

    #[test]
    fn selection_drag_below_terminal_scrolls_and_updates_at_viewport_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![
            egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 30.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            },
            egui::Event::PointerMoved(egui::Pos2::new(20.0, 205.0)),
        ];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert!(terminal_events.is_empty());
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert_eq!(selection_actions.len(), 1);
        let scroll_actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );
        assert_eq!(scroll_actions[0], TerminalSelectionAction::Scroll(3));
        let TerminalSelectionAction::Update(event) = scroll_actions[1] else {
            panic!("expected edge update after scroll");
        };
        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
        assert!(router.is_active());
    }

    #[test]
    fn held_selection_below_terminal_repeats_downward_scroll_without_pointer_motion() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );

        let mut router = TerminalSelectionRouter::default();
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let _ = router.route_events(
            vec![
                egui::Event::PointerButton {
                    pos: egui::Pos2::new(10.0, 30.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: shift,
                },
                egui::Event::PointerMoved(egui::Pos2::new(20.0, 205.0)),
            ],
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );
        let actions = router.autoscroll_actions(
            Some(surface),
            ViewTransform::IDENTITY,
            egui::Modifiers::default(),
        );

        assert_eq!(actions[0], TerminalSelectionAction::Scroll(3));
        let TerminalSelectionAction::Update(event) = actions[1] else {
            panic!("expected edge update after repeated scroll");
        };
        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
    }

    #[test]
    fn selection_press_only_near_edge_does_not_autoscroll_until_drag_moves() {
        let mut state = test_state();
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        state.record_surface(surface);

        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let effects = state.update_frame(test_frame_inputs(
            vec![egui::Event::PointerButton {
                pos: egui::Pos2::new(10.0, 155.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: shift,
            }],
            Some(egui::Pos2::new(10.0, 155.0)),
        ));

        assert!(state.terminal_selection.is_active());
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            1
        );
    }

    #[test]
    fn copy_mode_key_layer_supports_tmux_vim_navigation_and_selection() {
        fn terminal(action: TerminalCopyModeAction) -> Option<CopyModeKeyAction> {
            Some(CopyModeKeyAction::Terminal(action))
        }

        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::J, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Down))
        );
        assert_eq!(
            copy_mode_action_for_char('n'),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::SameDirection
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('N'),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::OppositeDirection
            ))
        );
        assert_eq!(
            copy_mode_action_for_input(KeyInput {
                key: TerminalKey::N,
                mods: crate::terminal::KeyMods::default(),
                repeat: false,
                utf8: Some("n"),
                unshifted: Some('n'),
            }),
            Some(CopyModeKeyAction::SearchRepeat(
                CopyModeSearchRepeat::SameDirection
            ))
        );

        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(egui::Key::J, egui::Modifiers::default()),
                &mut suppress_next_text,
            ),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Down))
        );
        assert_eq!(
            copy_mode_action_for_egui_event(
                &egui::Event::Text("j".to_owned()),
                &mut suppress_next_text,
            ),
            None
        );
        assert_eq!(
            copy_mode_action_for_egui_event(
                &egui::Event::Text("/".to_owned()),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Next
            ))
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::ArrowUp, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::Move(TerminalCopyModeMotion::Up))
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::Space, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::BeginSelection)
        );
        assert_eq!(
            copy_mode_action_for_egui_key(egui::Key::V, egui::Modifiers::default()),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_egui_key(
                egui::Key::V,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            terminal(TerminalCopyModeAction::ToggleRectangle)
        );

        assert_eq!(
            copy_mode_action_for_egui_key(
                egui::Key::V,
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                }
            ),
            terminal(TerminalCopyModeAction::SelectLine)
        );
        assert_eq!(
            copy_mode_action_for_char('v'),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_input(KeyInput {
                key: TerminalKey::V,
                mods: crate::terminal::KeyMods::default(),
                repeat: false,
                utf8: Some("v"),
                unshifted: Some('v'),
            }),
            terminal(TerminalCopyModeAction::ToggleSelection)
        );
        assert_eq!(
            copy_mode_action_for_char('o'),
            terminal(TerminalCopyModeAction::ToggleSelectionEnd)
        );
        assert_eq!(
            copy_mode_action_for_input(KeyInput {
                key: TerminalKey::O,
                mods: crate::terminal::KeyMods::default(),
                repeat: false,
                utf8: Some("o"),
                unshifted: Some('o'),
            }),
            terminal(TerminalCopyModeAction::ToggleSelectionEnd)
        );
        assert_eq!(
            copy_mode_action_for_char('$'),
            terminal(TerminalCopyModeAction::Move(
                TerminalCopyModeMotion::EndOfLine
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('/'),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Next
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('?'),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );
        assert_eq!(
            copy_mode_action_for_char('*'),
            Some(CopyModeKeyAction::SearchWord(TerminalSearchDirection::Next))
        );
        assert_eq!(
            copy_mode_action_for_char('#'),
            Some(CopyModeKeyAction::SearchWord(
                TerminalSearchDirection::Previous
            ))
        );
    }

    #[test]
    fn copy_mode_search_repeat_uses_the_direction_that_started_search() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous
        );

        state.last_terminal_search = "needle".to_owned();
        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::SameDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::OppositeDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "opposite repeat must not change the sticky search mode"
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchRepeat(CopyModeSearchRepeat::SameDirection),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "next same-direction repeat should still follow the original backward search mode"
        );

        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Next),
            &mut effects,
        );
        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Next,
            "a new explicit search prompt should replace the sticky search mode"
        );
    }

    #[test]
    fn copy_mode_egui_question_mark_opens_backward_search_prompt() {
        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(egui::Key::Questionmark, egui::Modifiers::default()),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );

        let mut suppress_next_text = false;
        assert_eq!(
            copy_mode_action_for_egui_event(
                &key_event(
                    egui::Key::Slash,
                    egui::Modifiers {
                        shift: true,
                        ..Default::default()
                    },
                ),
                &mut suppress_next_text,
            ),
            Some(CopyModeKeyAction::SearchPrompt(
                TerminalSearchDirection::Previous
            ))
        );
    }

    #[test]
    fn copy_mode_search_submit_returns_focus_to_terminal_for_repeat_keys() {
        let mut state = test_state();
        let mut effects = Vec::new();
        state.apply_copy_mode_key_action(
            CopyModeKeyAction::SearchPrompt(TerminalSearchDirection::Previous),
            &mut effects,
        );
        assert_eq!(state.input_focus, InputFocus::Picker);

        state.apply_terminal_find_event(
            TerminalFindDialog::open_with_direction(
                "needle".to_owned(),
                TerminalSearchDirection::Previous,
            ),
            TerminalFindEvent::Search {
                query: "needle".to_owned(),
                direction: TerminalSearchDirection::Previous,
            },
        );
        assert_eq!(state.input_focus, InputFocus::Terminal);
        assert!(state.terminal_find_dialog.is_some());

        assert_eq!(
            state.last_terminal_search_direction,
            TerminalSearchDirection::Previous,
            "submitting a backward copy-mode search keeps backward repeat mode"
        );
    }

    #[test]
    fn default_app_bindings_leave_alt_s_and_alt_enter_for_terminal_input() {
        let mut bindings = AppKeyBindings::from_config(&BoottyConfig::default().input)
            .expect("default app bindings");
        let alt = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let (terminal_events, actions) = split_app_actions_for_bindings_with_modifier_sides(
            &mut bindings,
            vec![
                key_event(egui::Key::S, alt),
                egui::Event::Text("s".to_owned()),
                key_event(egui::Key::Enter, alt),
            ],
            ModifierSideState {
                left_alt: true,
                ..Default::default()
            },
        );

        assert!(actions.is_empty());
        assert_eq!(terminal_events.len(), 3);
    }

    #[test]
    fn selection_drag_inside_bottom_hot_zone_scrolls_downward() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );

        assert_eq!(
            selection_drag_scroll_delta(surface, egui::Pos2::new(20.0, 155.0)),
            1
        );
        assert_eq!(
            selection_drag_scroll_delta(surface, egui::Pos2::new(20.0, 150.0)),
            0
        );
    }

    #[test]
    fn update_frame_repeats_selection_downscroll_without_new_pointer_events() {
        let mut state = test_state();
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 160.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        state.record_surface(surface);
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };

        let first_frame = state.update_frame(test_frame_inputs(
            vec![
                egui::Event::PointerButton {
                    pos: egui::Pos2::new(10.0, 30.0),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: shift,
                },
                egui::Event::PointerMoved(egui::Pos2::new(20.0, 155.0)),
            ],
            Some(egui::Pos2::new(20.0, 155.0)),
        ));
        assert!(state.terminal_selection.is_active());
        assert_eq!(
            first_frame
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            3
        );

        let repeat_frame = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(
            repeat_frame
                .iter()
                .filter(|effect| matches!(effect, AppEffect::RequestRepaint))
                .count(),
            2
        );
    }

    #[test]
    fn selection_drag_into_partial_bottom_cell_scrolls_downward() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(220.0, 165.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let pos = egui::Pos2::new(20.0, 162.0);

        assert_eq!(selection_drag_scroll_delta(surface, pos), 1);
        let event = terminal_selection_event_clamped(surface, ViewTransform::IDENTITY, pos, false)
            .expect("clamped selection event");

        assert!(event.position.y < 160.0);
        assert!(event.position.y >= 140.0);
    }

    #[test]
    fn selection_drag_below_small_pane_uses_widget_edge_not_minimum_grid_edge() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let pos = egui::Pos2::new(20.0, 125.0);

        assert_eq!(selection_drag_scroll_delta(surface, pos), 3);
        let event = terminal_selection_event_clamped(surface, ViewTransform::IDENTITY, pos, false)
            .expect("clamped selection event");

        assert!(event.position.y < 80.0);
        assert!(event.position.y >= 60.0);
    }

    #[test]
    fn press_over_chrome_handle_does_not_begin_selection() {
        // Dragging a resize handle (sidebar edge / pane divider) that overlaps the terminal must
        // not start a text selection, even with no mouse tracking active.
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let handle =
            egui::Rect::from_min_size(egui::Pos2::new(4.0, 0.0), egui::Vec2::new(8.0, 80.0));
        let press_pos = egui::Pos2::new(8.0, 10.0);
        assert!(surface.rect.contains(press_pos));
        assert!(handle.contains(press_pos));
        let events = vec![
            egui::Event::PointerButton {
                pos: press_pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerMoved(egui::Pos2::new(40.0, 10.0)),
        ];

        let (_, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[handle],
            },
        );

        assert!(selection_actions.is_empty());
        assert!(!router.is_active());
    }

    #[test]
    fn plain_mouse_drag_stays_available_for_terminal_mouse_reporting() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];
        let original = events.clone();

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, original);
        assert!(selection_actions.is_empty());
        assert!(!router.is_active());
    }

    #[test]
    fn plain_mouse_drag_starts_selection_when_mouse_reporting_is_inactive() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let press = egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        };
        let motion = egui::Event::PointerMoved(egui::Pos2::new(20.0, 10.0));
        let release = egui::Event::PointerButton {
            pos: egui::Pos2::new(20.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        };
        let events = vec![press.clone(), motion.clone(), release.clone()];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: false,
                frame_modifiers: egui::Modifiers::default(),
                chrome_handle_rects: &[],
            },
        );

        assert_eq!(terminal_events, vec![press, motion, release]);
        assert_eq!(selection_actions.len(), 3);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(matches!(
            selection_actions[2],
            TerminalSelectionAction::End(_)
        ));
        assert!(!router.is_active());
    }

    #[test]
    fn shift_drag_overrides_mouse_reporting_for_bootty_selection() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: shift,
        }];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: shift,
                chrome_handle_rects: &[],
            },
        );
        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(router.is_active());
    }

    #[test]
    fn frame_shift_overrides_mouse_reporting_when_pointer_event_lacks_modifiers() {
        let surface = TerminalSurface::for_rect(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(100.0, 80.0)),
            crate::geometry::CellMetrics::new(10.0, 20.0),
        );
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        let events = vec![egui::Event::PointerButton {
            pos: egui::Pos2::new(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }];

        let (terminal_events, selection_actions, router) = route_selection_test_events(
            events,
            TerminalSelectionRouteContext {
                surface: Some(surface),
                view: ViewTransform::IDENTITY,
                mouse_tracking: true,
                frame_modifiers: shift,
                chrome_handle_rects: &[],
            },
        );
        assert!(terminal_events.is_empty());
        assert_eq!(selection_actions.len(), 1);
        assert!(matches!(
            selection_actions[0],
            TerminalSelectionAction::Begin(_)
        ));
        assert!(router.is_active());
    }

    #[test]
    fn command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                mac_cmd: true,
                ..Default::default()
            },
        }));
        assert!(!copy_shortcut_pressed(&egui::Event::Key {
            key: egui::Key::C,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }));
    }

    #[test]
    fn direct_command_c_is_detected_as_copy_shortcut_for_selection_override() {
        assert!(direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
        assert!(!direct_copy_shortcut_pressed(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("c"),
            unshifted: Some('c'),
        }));
    }

    #[test]
    fn mouse_shape_side_effect_maps_common_cursor_names() {
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("shape=pointing_hand"),
            Some(egui::CursorIcon::PointingHand)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("ew-resize"),
            Some(egui::CursorIcon::ResizeHorizontal)
        );
        assert_eq!(
            terminal_cursor_icon_for_mouse_shape("not-a-known-cursor"),
            None
        );
    }

    #[test]
    fn terminal_typing_hides_mouse_pointer_until_pointer_moves() {
        let mut state = test_state();
        state.terminal_cursor_icon = egui::CursorIcon::PointingHand;
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(egui::CursorIcon::None)]
        );

        effects.clear();
        state.restore_mouse_pointer_after_pointer_moved(
            &[egui::Event::PointerMoved(egui::Pos2::new(1.0, 1.0))],
            Some(egui::Pos2::new(1.0, 1.0)),
            &mut effects,
        );

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(
                egui::CursorIcon::PointingHand
            )]
        );
    }

    #[test]
    fn terminal_typing_restores_mouse_pointer_when_hover_position_changes_without_event() {
        let mut state = test_state();
        state.terminal_cursor_icon = egui::CursorIcon::Text;
        state.last_mouse_hover_pos = Some(egui::Pos2::new(1.0, 1.0));
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);
        effects.clear();

        state.restore_mouse_pointer_after_pointer_moved(
            &[],
            Some(egui::Pos2::new(2.0, 1.0)),
            &mut effects,
        );

        assert_eq!(
            effects,
            vec![AppEffect::SetTerminalCursorIcon(egui::CursorIcon::Text)]
        );
    }

    #[test]
    fn hide_mouse_pointer_while_typing_setting_can_disable_typing_hide() {
        let mut state = test_state_with_config(|config| {
            config.input.hide_mouse_pointer_while_typing = false;
        });
        let mut effects = Vec::new();

        state.apply_terminal_input(TerminalInputCommand::Text("x".to_owned()), &mut effects);

        assert!(effects.is_empty());
    }

    #[test]
    fn bell_side_effect_requests_host_bell() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::Bell),
            &mut effects,
            10.0,
            20.0,
            1.0,
        );

        assert_eq!(effects, vec![AppEffect::Bell]);
    }

    #[test]
    fn report_variable_response_returns_selected_session_name() {
        assert_eq!(
            terminal_report_variable_response("session.name", Some("local")),
            Some(encode_iterm2_report_variable("local"))
        );
    }

    #[test]
    fn report_variable_response_ignores_unknown_variables() {
        assert_eq!(
            terminal_report_variable_response("user.missing", Some("local")),
            None
        );
    }

    #[test]
    fn default_fullscreen_config_toggles_native_fullscreen() {
        let config = BoottyConfig::default();

        assert!(should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn appkit_handled_non_native_fullscreen_toggles_tracked_state() {
        assert!(!next_non_native_fullscreen_state(true, true, false));
        assert!(next_non_native_fullscreen_state(true, false, false));
    }

    #[test]
    fn viewport_handled_non_native_fullscreen_toggles_maximized_state() {
        assert!(!next_non_native_fullscreen_state(false, false, true));
        assert!(next_non_native_fullscreen_state(false, true, false));
    }

    #[test]
    fn non_native_fullscreen_config_toggles_non_native_fullscreen() {
        let mut config = BoottyConfig::default();
        config.window.fullscreen = WindowFullscreen::NonNative;

        assert!(!should_toggle_native_fullscreen(&config.window));
    }

    #[test]
    fn external_mux_backends_schedule_frequent_refresh_repaints() {
        let mut config = BoottyConfig::default();
        assert_eq!(mux_refresh_repaint_after(&config.multiplexer, true), None);

        config.multiplexer.backend = MultiplexerBackendConfig::Zellij;

        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer, true),
            Some(MUX_SESSION_REFRESH_INTERVAL)
        );
        assert!(MUX_SESSION_REFRESH_INTERVAL <= Duration::from_millis(500));

        config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer, true),
            if cfg!(windows) {
                None
            } else {
                Some(MUX_SESSION_REFRESH_INTERVAL)
            }
        );
    }

    #[test]
    fn unfocused_windows_stop_waking_up_to_poll_for_sessions() {
        let mut config = BoottyConfig::default();
        config.multiplexer.backend = MultiplexerBackendConfig::Zellij;

        // Each poll spawns a backend client and forces a frame, so an unfocused window pays the
        // full cadence for a sidebar nobody is reading.
        assert_eq!(
            mux_refresh_repaint_after(&config.multiplexer, false),
            Some(MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED)
        );
        assert!(MUX_SESSION_REFRESH_INTERVAL_UNFOCUSED >= MUX_SESSION_REFRESH_INTERVAL * 4);

        config.multiplexer.backend = MultiplexerBackendConfig::Native;
        assert_eq!(mux_refresh_repaint_after(&config.multiplexer, false), None);
    }

    #[test]
    fn new_mux_session_request_uses_configured_working_directory() {
        let mut config = BoottyConfig::default();
        config.session.working_directory = Some("tmp/bootty-project".into());

        let request = new_mux_session_request_with_name(&config, "review-session");

        assert_eq!(request.session_id, "review-session");
        assert_eq!(request.cwd, "tmp/bootty-project");
    }

    #[test]
    fn new_mux_session_request_defaults_to_home_working_directory() {
        let config = BoottyConfig::default();
        let expected_home = crate::config::default_working_directory()
            .expect("home directory should be discoverable");

        let request = new_mux_session_request_with_name(&config, "home-session");

        assert_eq!(request.session_id, "home-session");
        assert_eq!(request.cwd, expected_home.to_string_lossy().as_ref());
    }

    #[test]
    fn mux_command_cwd_prefers_live_osc7_directory_over_snapshot_anchor() {
        assert_eq!(
            terminal_cwd_for_mux_command(
                Some("file://host/Users/me/project%20space".to_owned()),
                Some("/old".to_owned()),
            ),
            Some("/Users/me/project space".to_owned())
        );
        assert_eq!(
            terminal_cwd_for_mux_command(None, Some("/fallback".to_owned())),
            Some("/fallback".to_owned())
        );
    }

    #[test]
    fn new_window_action_opens_new_session_picker() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::NewWindow),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert!(state.take_dialog().is_some());
    }

    fn test_frame_inputs(events: Vec<egui::Event>, hover_pos: Option<egui::Pos2>) -> FrameInputs {
        FrameInputs {
            now: Instant::now(),
            stable_dt_ms: 16.0,
            events,
            dropped_file_paths: Vec::new(),
            modifiers: egui::Modifiers::default(),
            hover_pos,
            pressed_mouse_button: None,
            viewport: ViewportSnapshot::default(),
            window_focused: true,
            renderer_metrics: RendererMetrics::default(),
            terminal_cell_width: 10.0,
            terminal_cell_height: 20.0,
            terminal_scale_factor: 1.0,
            terminal_view_transform: ViewTransform::IDENTITY,
        }
    }

    fn test_state() -> AppState {
        test_state_with_config(|_| {})
    }

    fn await_authoritative_commands(state: &mut AppState) {
        for _ in 0..100 {
            let _ = state.drain_pending_app_commands(Instant::now());
            if state.pending_app_commands.is_empty() {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("authoritative mux command did not complete");
    }

    fn session_with(id: &str, name: &str, cwd: &str) -> MuxSession {
        MuxSession {
            id: id.to_owned(),
            name: name.to_owned(),
            active: true,
            anchor: MuxPaneAnchor {
                session_id: id.to_owned(),
                pane_id: Some(format!("{id}-pane")),
                terminal_id: Some(format!("{id}-terminal")),
                occupant_id: None,
                pane_pid: None,
                cwd: Some(cwd.to_owned()),
                process: None,
            },
            active_window_id: None,
            windows: Vec::new(),
        }
    }

    fn session_with_window_and_pane(id: &str, name: &str, cwd: &str) -> MuxSession {
        let mut session = session_with(id, name, cwd);
        let window_id = format!("{id}-window");
        let anchor = session.anchor.clone();
        session.active_window_id = Some(window_id.clone());
        session.windows = vec![MuxWindow {
            id: window_id,
            index: 0,
            name: "window".to_owned(),
            active: true,
            anchor: anchor.clone(),
            panes: vec![anchor],
            layout: None,
            progress: None,
        }];
        session
    }

    #[test]
    fn creating_a_session_focuses_the_terminal_after_authoritative_completion() {
        let mut state = test_state();
        state.input_focus = InputFocus::Sidebar;

        state.create_project_session_for_cwd(std::env::temp_dir().to_string_lossy().into_owned());
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        for _ in 0..100 {
            let _ = state.drain_pending_app_commands(Instant::now());
            if state.input_focus == InputFocus::Terminal {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        assert_eq!(state.input_focus, InputFocus::Terminal);
    }

    #[test]
    fn remote_session_creation_preserves_the_remote_path() {
        let mut state = test_state();
        state.binding.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        ScriptedBackend::with(Vec::new()).install(&mut state.binding);
        state.binding.multiplexer.remote = Some(SshRemoteConfig::for_host("devbox"));
        let cwd = r"C:\Users\developer\project";

        state.create_project_session_for_cwd(cwd.to_owned());

        let pending = state
            .binding
            .pending_generated_names
            .values()
            .next()
            .expect("pending remote session");
        assert_eq!(pending.cwd, cwd);
        assert_eq!(pending.display_name, "project");
    }

    #[test]
    fn remote_session_reconciliation_preserves_posix_and_windows_paths_without_client_git() {
        let mut state = test_state();
        state.binding.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        state.binding.multiplexer.remote = Some(SshRemoteConfig::for_host("devbox"));
        let posix = "/srv/projects/alpha";
        let windows = r"C:\Users\developer\beta";
        ScriptedBackend::with(vec![
            session_with("$1", "alpha", posix),
            session_with("$2", "beta", windows),
        ])
        .install(&mut state.binding);
        state
            .binding
            .session_order
            .add_session("alpha")
            .expect("persist session order");
        state
            .binding
            .session_order
            .add_session("beta")
            .expect("persist session order");

        let _guard = bootty_runtime::perf::guard_frame_path();
        let config = state.active_multiplexer().clone();
        state.binding.mux.refresh_on_next_frame();
        state.binding.mux.refresh_sessions(&state.repaint, &config);
        for _ in 0..50 {
            if state.binding.mux.all_sessions().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
            state.binding.mux.refresh_sessions(&state.repaint, &config);
        }
        state.sync_generated_session_names();
        assert_eq!(state.binding.mux.all_sessions().len(), 2);

        assert!(
            state
                .binding
                .session_names
                .observe_session("$1", "alpha", posix)
                .is_some()
        );
        assert!(
            state
                .binding
                .session_names
                .observe_session("$2", "beta", windows)
                .is_some()
        );
    }

    #[test]
    fn persisted_session_restore_waits_for_incomplete_refresh_then_reconciles_each_plan() {
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Native, false),
            PersistedSessionRestoreDecision::Restore
        );
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Rmux, false),
            PersistedSessionRestoreDecision::Wait
        );
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Zellij, false),
            PersistedSessionRestoreDecision::Wait
        );
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Rmux, true),
            PersistedSessionRestoreDecision::Restore
        );
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Tmux, true),
            PersistedSessionRestoreDecision::Restore
        );
        assert_eq!(
            persisted_session_restore_decision(MultiplexerBackendConfig::Zellij, true),
            PersistedSessionRestoreDecision::Restore
        );
    }
    #[test]
    fn rmux_session_activation_persists_last_focused_session() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        });
        let config_path = state.config().config_path.clone();

        state.activate_session_from_ui("last-focused");

        let workspace = WorkspaceStore::for_config_path(&config_path);
        assert_eq!(
            workspace
                .binding()
                .and_then(|binding| binding.selection())
                .map(|selection| selection.session_id()),
            Some("last-focused")
        );
    }

    #[test]
    fn generated_name_sync_skips_unchanged_sessions_and_reruns_on_change() {
        // Guards the fix for the per-frame `git` fork: the reconciler must not repeat its
        // per-session worktree lookups while the session set is unchanged, but must re-run when a
        // session's name or cwd changes so generated names stay current.
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let backend = ScriptedBackend::with(vec![session_with("s1", "alpha", "/repo/alpha")])
            .install(&mut state.binding);

        assert!(
            frames_reconciled_names(&mut state),
            "first observation of a session set must reconcile"
        );
        assert!(
            !frames_reconciled_names(&mut state),
            "an unchanged session set must be skipped (no per-frame git forks)"
        );

        backend.set(vec![session_with("s1", "beta", "/repo/alpha")]);
        assert!(
            frames_reconciled_names(&mut state),
            "a session rename must trigger reconciliation"
        );

        backend.set(vec![session_with("s1", "beta", "/repo/beta")]);
        assert!(
            frames_reconciled_names(&mut state),
            "a session cwd change must trigger reconciliation"
        );
        assert!(
            !frames_reconciled_names(&mut state),
            "reconciliation must settle again once the session set stops changing"
        );
    }

    /// Run a refreshing frame and then an idle one, and report whether either reconciled generated
    /// names.
    ///
    /// Drives the real `update_frame` rather than replaying the calls it makes, so reordering them
    /// cannot leave this passing while covering nothing. Both frames are needed: real refreshes are
    /// 250ms apart, so most frames fall between them, and it is the *idle* frame that sees
    /// `mux.sessions()` narrowed back down. Refreshing on every frame hides that entirely.
    fn frames_reconciled_names(state: &mut AppState) -> bool {
        let before = state.binding.generated_names_signature;
        state.binding.mux.refresh_on_next_frame();
        state.update_frame(test_frame_inputs(Vec::new(), None));
        let after_refresh = state.binding.generated_names_signature;
        state.update_frame(test_frame_inputs(Vec::new(), None));
        after_refresh != before || state.binding.generated_names_signature != after_refresh
    }

    /// Run one frame that refreshes the mux, so it reconciles names and re-narrows membership.
    fn reconcile_frame(state: &mut AppState) {
        state.binding.mux.refresh_on_next_frame();
        state.update_frame(test_frame_inputs(Vec::new(), None));
    }

    /// A generated-name rename has to take this binding's membership with it. Membership is keyed by
    /// session name, so once the backend reports the new name the old entry prunes away — and nothing
    /// added the new one, so the session belonged to no Space at all: gone from the sidebar while
    /// still running.
    #[test]
    fn a_generated_rename_keeps_the_session_in_its_space() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let backend = ScriptedBackend::with(vec![session_with("s1", "stale", "/repo/alpha")])
            .install(&mut state.binding);
        // The reconciler only renames a name it generated itself, and the name it suggests for this
        // cwd is "alpha".
        state
            .binding
            .session_names
            .remember_generated("s1", "/repo/alpha", "stale", "stale");
        state
            .binding
            .session_order
            .add_session("stale")
            .expect("persist session order");

        reconcile_frame(&mut state);
        assert_eq!(state.binding.session_order.session_names(), ["stale"]);

        // The rename reaches the backend (ScriptedBackend ignores commands, so the test applies it).
        backend.set(vec![session_with("s1", "alpha", "/repo/alpha")]);
        reconcile_frame(&mut state);

        assert_eq!(state.binding.session_order.session_names(), ["alpha"]);
        assert_eq!(
            state
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha"],
        );
    }

    #[test]
    fn switching_to_a_space_keeps_a_session_renamed_while_inactive() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let home_space = state.active_space_id();
        assert!(state.create_space_from_ui(
            "Work",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let work_space = state.active_space_id();
        let backend = ScriptedBackend::with(vec![session_with("s1", "before", "/repo/work")])
            .install(&mut state.binding);
        state
            .binding
            .session_names
            .mark_explicit("s1", "before", "before", "/repo/work");
        state
            .binding
            .session_order
            .add_session("before")
            .expect("persist session order");
        reconcile_frame(&mut state);

        assert!(state.activate_space_from_ui(home_space));
        backend.set(vec![session_with("s1", "after", "/repo/work")]);
        assert!(state.activate_space_from_ui(work_space));

        assert_eq!(state.binding.session_order.session_names(), ["after"]);
        assert_eq!(
            state
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            ["after"],
        );
    }

    /// Focus lands on the created session *and* shows there: the sidebar marks its current row by
    /// session id, so a selection still carrying the name bootty asked the backend for left the
    /// focused session unhighlighted.
    #[test]
    fn a_created_session_is_the_current_sidebar_row() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let backend = ScriptedBackend::with(Vec::new()).install(&mut state.binding);
        let dir = std::env::temp_dir().join(format!("bootty-current-row-{}", unique_test_id()));
        std::fs::create_dir_all(&dir).expect("create session cwd");
        let cwd = dir.to_string_lossy().into_owned();
        let name = crate::git::suggested_session_name(&AppState::session_root(&cwd));

        state.create_project_session_for_cwd(cwd);
        await_authoritative_commands(&mut state);
        // Replace the backend's authoritative create result with its opaque session identity.
        backend.set(vec![session_with(
            "s1",
            &name,
            dir.to_str().expect("utf-8 cwd"),
        )]);
        reconcile_frame(&mut state);

        assert_eq!(
            state.binding.mux.selected_session(),
            Some("s1"),
            "the selection resolves to the session id the sidebar marks rows by"
        );
    }

    /// A UI rename records the new name as pending so membership and uniqueness hold it while the
    /// backend catches up. That entry is keyed by the name rather than by a session id, so the id
    /// lookup in the reconciler never pruned it: the name stayed reserved for the rest of the run,
    /// and the next session for that project was pushed onto a "-2" suffix by it.
    #[test]
    fn a_landed_ui_rename_releases_its_pending_name() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let backend = ScriptedBackend::with(vec![session_with("s1", "alpha", "/repo/alpha")])
            .install(&mut state.binding);
        state
            .binding
            .session_names
            .remember_generated("s1", "/repo/alpha", "alpha", "alpha");
        state
            .binding
            .session_order
            .add_session("alpha")
            .expect("persist session order");
        reconcile_frame(&mut state);

        state.apply_rename_session_event(
            RenameSessionDialog::open("s1".to_owned(), "alpha".to_owned()),
            RenameSessionEvent::Rename {
                session_id: "s1".to_owned(),
                name: "release".to_owned(),
            },
        );
        assert!(
            state
                .binding
                .pending_generated_names
                .contains_key("release"),
            "the new name is held until the backend reports it"
        );

        // The rename reaches the backend (ScriptedBackend ignores commands, so the test applies it).
        backend.set(vec![session_with("s1", "release", "/repo/alpha")]);
        reconcile_frame(&mut state);

        assert!(
            state.binding.pending_generated_names.is_empty(),
            "a pending name the backend now reports must be released"
        );
        assert_eq!(state.binding.session_order.session_names(), ["release"]);
    }

    /// A backend name has to clear every session on a shared server, bootty's or not. The suffix that
    /// takes is the backend's business: the sidebar shows the name bootty meant.
    #[test]
    fn a_name_taken_on_the_backend_only_suffixes_the_backend_name() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let dir = std::env::temp_dir().join(format!("bootty-display-name-{}", unique_test_id()));
        std::fs::create_dir_all(&dir).expect("create session cwd");
        let cwd = dir.to_string_lossy().into_owned();
        let wanted = crate::git::suggested_session_name(&AppState::session_root(&cwd));
        // A session this Space does not own already answers to that name on the shared backend.
        ScriptedBackend::with(vec![session_with("foreign", &wanted, "/repo/foreign")])
            .install(&mut state.binding);
        reconcile_frame(&mut state);

        state.create_project_session_for_cwd(cwd);

        let backend_name = format!("{wanted}-2");
        assert!(
            state
                .binding
                .pending_generated_names
                .contains_key(&backend_name),
            "the backend is asked for a name no other session holds"
        );
        assert_eq!(
            state.binding.session_names.display_name(&backend_name),
            Some(wanted.as_str()),
            "bootty shows the name it meant, without the backend's suffix"
        );
    }

    /// Bootty asking the backend for `agents/main-2` and then reading that back as somebody's rename
    /// is how these sessions became "explicit": the suffix froze into the name shown everywhere, and
    /// an explicit name is one bootty will not second-guess. Only records from before display names
    /// existed are read this way — a name typed since then carries its own display name.
    #[test]
    fn a_legacy_generated_suffix_is_not_read_as_someone_elses_rename() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let dir = std::env::temp_dir().join(format!("bootty-suffix-{}", unique_test_id()));
        std::fs::create_dir_all(&dir).expect("create session cwd");
        let root = AppState::session_root(&dir.to_string_lossy());
        let wanted = crate::git::suggested_session_name(&root);
        let backend_name = format!("{wanted}-2");
        // The record the old reconciler left: generated under the clean name, then marked explicit
        // because the backend reported the suffixed one, and with no display name of its own.
        state
            .binding
            .session_names
            .remember_generated("s1", &root, &wanted, "");
        state
            .binding
            .session_names
            .mark_explicit("s1", &backend_name, "", &root);
        state
            .binding
            .session_order
            .add_session(&backend_name)
            .expect("persist session order");
        ScriptedBackend::with(vec![session_with("s1", &backend_name, &root)])
            .install(&mut state.binding);

        reconcile_frame(&mut state);

        assert_eq!(
            state.binding.session_names.display_name("s1"),
            Some(wanted.as_str()),
            "the suffix bootty added is not part of the name it shows"
        );
    }

    /// A suffix-shaped name someone typed is theirs. Nothing may re-derive it, or bootty would rename
    /// the session back to the name it would have generated.
    #[test]
    fn a_typed_name_that_looks_like_a_generated_suffix_is_left_alone() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let dir = std::env::temp_dir().join(format!("bootty-typed-{}", unique_test_id()));
        std::fs::create_dir_all(&dir).expect("create session cwd");
        let root = AppState::session_root(&dir.to_string_lossy());
        let wanted = crate::git::suggested_session_name(&root);
        let typed = format!("{wanted}-2");
        state
            .binding
            .session_names
            .remember_generated("s1", &root, &wanted, &wanted);
        // What the rename dialog records: the typed name, shown as typed.
        state
            .binding
            .session_names
            .mark_explicit("s1", &typed, &typed, &root);
        state
            .binding
            .session_order
            .add_session(&typed)
            .expect("persist session order");
        ScriptedBackend::with(vec![session_with("s1", &typed, &root)]).install(&mut state.binding);

        reconcile_frame(&mut state);

        assert_eq!(
            state.binding.session_names.display_name("s1"),
            Some(typed.as_str()),
            "a typed name stands, suffix-shaped or not"
        );
        assert_eq!(
            state.binding.mux.sessions()[0].name,
            typed,
            "and no rename is attempted back to the generated name"
        );
    }

    /// Sessions that predate display names have none recorded, so they kept showing the backend's
    /// name — including the suffix bootty only ever added to clear the server's namespace.
    #[test]
    fn sessions_recorded_before_display_names_get_one() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let dir = std::env::temp_dir().join(format!("bootty-backfill-{}", unique_test_id()));
        std::fs::create_dir_all(&dir).expect("create session cwd");
        let cwd = dir.to_string_lossy().into_owned();
        let root = AppState::session_root(&cwd);
        let wanted = crate::git::suggested_session_name(&root);
        let backend_name = format!("{wanted}-2");
        // What the old code left behind: a generated record with no display name of its own, on a
        // session whose backend name carries the suffix that cleared a foreign session.
        state
            .binding
            .session_names
            .remember_generated("s1", &root, &backend_name, "");
        state
            .binding
            .session_order
            .add_session(&backend_name)
            .expect("persist session order");
        ScriptedBackend::with(vec![
            session_with("s1", &backend_name, &root),
            session_with("foreign", &wanted, "/repo/foreign"),
        ])
        .install(&mut state.binding);

        reconcile_frame(&mut state);

        assert_eq!(
            state.binding.session_names.display_name("s1"),
            Some(wanted.as_str()),
            "the upgrade fills in the name bootty would have shown"
        );
        assert_eq!(
            state.binding.mux.sessions()[0].name,
            backend_name,
            "and asks the backend for nothing: the foreign session still holds the clean name"
        );
    }

    /// Two members that would show the same name are the one case the suffix has to stay: it is all
    /// that tells them apart.
    #[test]
    fn members_that_would_show_the_same_name_keep_their_backend_names() {
        let mut state = test_state();
        state.binding.session_names.remember_generated(
            "s1",
            "/repo/a",
            "agents/main",
            "agents/main",
        );
        state.binding.session_names.remember_generated(
            "s2",
            "/repo/b",
            "agents/main-2",
            "agents/main",
        );
        let sessions = vec![
            session_with("s1", "agents/main", "/repo/a"),
            session_with("s2", "agents/main-2", "/repo/b"),
        ];

        assert_eq!(
            state.session_display_names(&sessions),
            ["agents/main", "agents/main-2"]
        );
        assert_eq!(
            state.session_display_names(&sessions[1..]),
            ["agents/main"],
            "on its own, that session shows the name bootty meant"
        );
    }

    /// Renaming onto a name some other session on the server holds used to be a rename the backend
    /// rejected. The typed name is bootty's to show; the backend gets a unique one.
    #[test]
    fn renaming_onto_a_name_the_backend_holds_keeps_the_typed_name() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        ScriptedBackend::with(vec![
            session_with("s1", "alpha", "/repo/alpha"),
            session_with("foreign", "release", "/repo/foreign"),
        ])
        .install(&mut state.binding);
        state
            .binding
            .session_names
            .remember_generated("s1", "/repo/alpha", "alpha", "alpha");
        state
            .binding
            .session_order
            .add_session("alpha")
            .expect("persist session order");
        reconcile_frame(&mut state);

        state.apply_rename_session_event(
            RenameSessionDialog::open("s1".to_owned(), "alpha".to_owned()),
            RenameSessionEvent::Rename {
                session_id: "s1".to_owned(),
                name: "release".to_owned(),
            },
        );

        assert_eq!(
            state.binding.session_names.display_name("s1"),
            Some("release"),
            "the typed name is what bootty shows"
        );
        assert!(
            state
                .binding
                .pending_generated_names
                .contains_key("release-2"),
            "the backend is asked for a name the foreign session does not hold"
        );
    }

    /// The finder reaches every Space, so it has to say which Space each session belongs to, and
    /// selecting one has to mean what the grouping implies: switch to the owning Space, or adopt an
    /// unclaimed session into the current one.
    #[test]
    fn the_session_finder_groups_sessions_by_owning_space() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let home_space = state.active_space_id();
        // Every binding answers from this one list: the Spaces share a backend, and a real native
        // backend would report whatever other tests in this process happen to have running.
        let backend = ScriptedBackend::with(vec![
            session_with("s1", "home-session", "/repo/home"),
            session_with("s2", "work-session", "/repo/work"),
            session_with("s3", "unclaimed", "/repo/unclaimed"),
        ]);
        backend.clone().install(&mut state.binding);
        // Seeded before any sync: a fresh store adopts every session it is shown.
        state
            .binding
            .session_order
            .add_session("home-session")
            .expect("persist session order");
        assert!(state.create_space_from_ui(
            "Work",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let work_scope = state.binding.scope;
        backend.clone().install(&mut state.binding);
        state
            .binding
            .session_order
            .add_session("work-session")
            .expect("persist session order");
        assert!(state.activate_space_from_ui(home_space));
        reconcile_frame(&mut state);

        let groups = state
            .session_finder_groups()
            .into_iter()
            .map(|group| {
                (
                    group.label,
                    group
                        .sessions
                        .iter()
                        .map(|session| session.name.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            groups,
            vec![
                ("Default Space".to_owned(), vec!["home-session".to_owned()]),
                ("Work".to_owned(), vec!["work-session".to_owned()]),
                (
                    UNCLAIMED_SESSIONS_LABEL.to_owned(),
                    vec!["unclaimed".to_owned()]
                ),
            ]
        );

        state.apply_session_picker_event(
            SessionPickerDialog::open(),
            SessionPickerEvent::ActivateSession(ScopedSessionTarget::new(
                state.binding.scope,
                "s3",
            )),
        );
        assert_eq!(state.active_space_id(), home_space);
        assert_eq!(
            state.binding.session_order.session_names(),
            ["home-session", "unclaimed"],
            "an unclaimed session must be adopted by the Space that activated it"
        );

        state.apply_session_picker_event(
            SessionPickerDialog::open(),
            SessionPickerEvent::ActivateSession(ScopedSessionTarget::new(work_scope, "s2")),
        );
        assert_eq!(
            state.active_space_id(),
            work_scope.space_id(),
            "a session that belongs to another Space must be switched to there"
        );
        assert_eq!(state.mux().selected_session(), Some("s2"));
    }

    /// A backend whose session list the test owns, so a refresh can be made to report a change or
    /// to report the same thing again.
    #[derive(Clone)]
    struct ScriptedBackend {
        sessions: Arc<std::sync::Mutex<Vec<MuxSession>>>,
        commands: Arc<std::sync::Mutex<Vec<MuxCommand>>>,
        events: crate::mux::backend::MuxEventQueue,
        operations: Vec<BindingOperation>,
        failure: Option<String>,
    }

    impl ScriptedBackend {
        fn default_operations() -> Vec<BindingOperation> {
            vec![
                BindingOperation::ActivateWindow,
                BindingOperation::CreateWindow,
                BindingOperation::RenameWindow,
                BindingOperation::NavigateWindow,
                BindingOperation::MoveWindow,
                BindingOperation::SplitPane,
                BindingOperation::NavigatePane,
                BindingOperation::ClosePane,
                BindingOperation::TogglePaneZoom,
                BindingOperation::CreateProjectSession,
                BindingOperation::CreateWorktreeSession,
                BindingOperation::RenameSession,
                BindingOperation::DitchSession,
            ]
        }

        fn with(sessions: Vec<MuxSession>) -> Self {
            Self::with_operations(sessions, Self::default_operations())
        }

        fn with_operations(
            sessions: Vec<MuxSession>,
            operations: impl IntoIterator<Item = BindingOperation>,
        ) -> Self {
            Self {
                sessions: Arc::new(std::sync::Mutex::new(sessions)),
                commands: Arc::new(std::sync::Mutex::new(Vec::new())),
                events: crate::mux::backend::MuxEventQueue::for_backend("scripted"),
                operations: operations.into_iter().collect(),
                failure: None,
            }
        }

        fn failing(sessions: Vec<MuxSession>, message: &str) -> Self {
            let mut backend = Self::with(sessions);
            backend.failure = Some(message.to_owned());
            backend
        }

        fn set(&self, sessions: Vec<MuxSession>) {
            *self.sessions.lock().expect("scripted sessions") = sessions;
        }

        fn publish_event(&self, event: crate::mux::backend::MuxEventDraft) {
            self.events.publish(event);
        }

        fn executed_commands(&self) -> Vec<MuxCommand> {
            self.commands.lock().expect("scripted commands").clone()
        }

        /// Installs itself on a binding and returns a handle the test keeps for later `set` calls.
        fn install(self, binding: &mut BindingRuntime) -> Self {
            let backend = self.clone();
            binding
                .mux
                .set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
            self
        }
    }

    impl MuxBackend for ScriptedBackend {
        fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
            let sessions = self.sessions.lock().expect("scripted sessions").clone();
            Ok(MuxSnapshot {
                active_session_id: sessions.first().map(|session| session.id.clone()),
                sessions,
            })
        }

        fn execute(&mut self, command: MuxCommand) -> anyhow::Result<()> {
            self.commands
                .lock()
                .expect("scripted commands")
                .push(command);
            if let Some(message) = &self.failure {
                anyhow::bail!("{message}");
            }
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: MuxScope,
            command: MuxCommand,
            _precondition: Option<&crate::mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let descriptor = self.capabilities(scope);
            descriptor.invoke(
                descriptor.request(command.operation()),
                BindingOperationAvailability::Available,
                || self.execute(command),
            )
        }

        fn drain_events(&mut self, scope: MuxScope, maximum: usize) -> Vec<MuxEvent> {
            self.events.drain(scope, maximum)
        }

        fn session_launch_capability(
            &self,
            plan: &MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<()> {
            if plan
                .windows
                .iter()
                .all(|window| matches!(&window.layout, MuxPaneLaunchPlan::Pane(_)))
            {
                BindingOperationOutcome::Supported(())
            } else {
                BindingOperationOutcome::Unsupported
            }
        }

        fn execute_session_launch(
            &mut self,
            plan: MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            if !matches!(
                self.session_launch_capability(&plan),
                BindingOperationOutcome::Supported(())
            ) {
                return BindingOperationOutcome::Unsupported;
            }
            if let Some(message) = &self.failure {
                return BindingOperationOutcome::Supported(Err(anyhow::anyhow!(message.clone())));
            }

            let session_id = plan.session_id.clone();
            let mut windows = Vec::with_capacity(plan.windows.len());
            for (index, window) in plan.windows.iter().enumerate() {
                let MuxPaneLaunchPlan::Pane(pane) = &window.layout else {
                    return BindingOperationOutcome::Unsupported;
                };
                let window_id = format!("{session_id}-window-{index}");
                let pane_id = format!("{window_id}-pane");
                let terminal_id = format!("{window_id}-terminal");
                let anchor = MuxPaneAnchor {
                    session_id: session_id.clone(),
                    pane_id: Some(pane_id),
                    terminal_id: Some(terminal_id),
                    cwd: Some(pane.cwd.clone()),
                    ..Default::default()
                };
                windows.push(MuxWindow {
                    id: window_id,
                    index: index as u32,
                    name: window
                        .name
                        .clone()
                        .unwrap_or_else(|| format!("window-{index}")),
                    active: index == plan.focused_window,
                    anchor: anchor.clone(),
                    panes: vec![anchor],
                    layout: None,
                    progress: None,
                });
            }
            let anchor = windows
                .get(plan.focused_window)
                .map(|window| window.anchor.clone())
                .expect("launch plans always contain a focused window");
            self.sessions
                .lock()
                .expect("scripted sessions")
                .push(MuxSession {
                    id: session_id.clone(),
                    name: session_id,
                    active: plan.focus,
                    anchor,
                    active_window_id: windows
                        .get(plan.focused_window)
                        .map(|window| window.id.clone()),
                    windows,
                });
            BindingOperationOutcome::Supported(Ok(()))
        }

        fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
            BindingCapabilityDescriptor::new(scope, self.operations.iter().copied())
        }
    }

    #[derive(Clone)]
    struct FlatRecursiveAllocationBackend {
        sessions: Arc<std::sync::Mutex<Vec<MuxSession>>>,
    }

    impl FlatRecursiveAllocationBackend {
        fn allocation() -> MuxAllocatedResources {
            MuxAllocatedResources {
                session_id: "$recursive".to_owned(),
                windows: vec![MuxAllocatedWindow {
                    window_id: "@recursive".to_owned(),
                    pane_ids: vec!["%first".to_owned(), "%second".to_owned()],
                }],
            }
        }

        fn install(self, binding: &mut BindingRuntime) {
            let backend = self.clone();
            binding
                .mux
                .set_backend_factory(Arc::new(move |_| Box::new(backend.clone())));
        }
    }

    impl MuxBackend for FlatRecursiveAllocationBackend {
        fn snapshot(&self) -> anyhow::Result<MuxSnapshot> {
            let sessions = self.sessions.lock().expect("recursive sessions").clone();
            Ok(MuxSnapshot {
                active_session_id: sessions.first().map(|session| session.id.clone()),
                sessions,
            })
        }

        fn execute(&mut self, _command: MuxCommand) -> anyhow::Result<()> {
            Ok(())
        }

        fn execute_checked(
            &mut self,
            scope: MuxScope,
            command: MuxCommand,
            _precondition: Option<&crate::mux::backend::MuxScopedExecutionPrecondition>,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let descriptor = self.capabilities(scope);
            descriptor.invoke(
                descriptor.request(command.operation()),
                BindingOperationAvailability::Available,
                || self.execute(command),
            )
        }

        fn session_launch_capability(
            &self,
            _plan: &MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<()> {
            BindingOperationOutcome::Supported(())
        }

        fn execute_session_launch(
            &mut self,
            plan: MuxSessionLaunchPlan,
        ) -> BindingOperationOutcome<anyhow::Result<()>> {
            let allocation = Self::allocation();
            let anchor = MuxPaneAnchor {
                session_id: allocation.session_id.clone(),
                pane_id: allocation.windows[0].pane_ids.last().cloned(),
                terminal_id: allocation.windows[0]
                    .pane_ids
                    .last()
                    .map(|pane_id| format!("{pane_id}-terminal")),
                cwd: Some(plan.default_cwd),
                ..Default::default()
            };
            *self.sessions.lock().expect("recursive sessions") = vec![MuxSession {
                id: allocation.session_id.clone(),
                name: plan.session_id,
                active: true,
                anchor: anchor.clone(),
                active_window_id: Some(allocation.windows[0].window_id.clone()),
                windows: vec![MuxWindow {
                    id: allocation.windows[0].window_id.clone(),
                    index: 0,
                    name: "recursive".to_owned(),
                    active: true,
                    anchor: anchor.clone(),
                    // tmux only reports the attach anchor; the completion has the full DFS map.
                    panes: vec![anchor],
                    layout: None,
                    progress: None,
                }],
            }];
            BindingOperationOutcome::Supported(Ok(()))
        }

        fn take_authoritative_completion(&mut self) -> Option<MuxBackendCommandCompletion> {
            Some(MuxBackendCommandCompletion {
                allocated: Some(Self::allocation()),
                target: None,
            })
        }

        fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
            BindingCapabilityDescriptor::new(scope, [BindingOperation::CreateProjectSession])
        }
    }

    /// A frame that changes nothing must not fork a subprocess. `update_frame`'s `sync_*` helpers
    /// resolve session cwds through `git`, which costs tens of milliseconds per spawn on the frame
    /// thread; when that landed on every frame it stalled the window 60-207ms at a time.
    ///
    /// Asserting no spawn rather than a duration keeps this deterministic on a loaded CI runner.
    /// Refreshes alternate so the guard covers both a snapshot-applying frame and an idle one.
    #[test]
    fn steady_state_frames_do_not_fork_subprocesses() {
        let sessions = (0..7)
            .map(|index| {
                session_with(
                    &format!("${index}"),
                    &format!("session-{index}"),
                    &format!("/tmp/bootty-steady-{index}"),
                )
            })
            .collect::<Vec<_>>();
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        ScriptedBackend::with(sessions).install(&mut state.binding);
        // One of the seven belongs to this binding, which is what makes `mux.sessions()` unstable:
        // a refresh resets it to all seven and `sync_session_order` narrows it back to one later in
        // the same frame. Fingerprinting that list flips the signature on every refresh.
        state
            .binding
            .session_order
            .add_session("session-0")
            .expect("persist session order");
        // Settle: the frames that first observe these sessions are entitled to resolve their cwds.
        for _ in 0..3 {
            state.binding.mux.refresh_on_next_frame();
            state.update_frame(test_frame_inputs(Vec::new(), None));
        }
        assert_eq!(state.binding.mux.all_sessions().len(), 7);
        assert_eq!(state.binding.mux.sessions().len(), 1);
        // Without this the loop below is vacuous: an early return added ahead of the `git` call
        // would skip the reconciler entirely and the guard would have nothing to complain about.
        assert!(state.binding.generated_names_signature.is_some());

        let _guard = bootty_runtime::perf::guard_frame_path();
        for frame in 0..8 {
            if frame % 2 == 0 {
                state.binding.mux.refresh_on_next_frame();
            }
            state.update_frame(test_frame_inputs(Vec::new(), None));
        }
    }

    #[test]
    fn rmux_skips_generated_name_reconciliation() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        });

        state.sync_generated_session_names();

        assert_eq!(state.binding.generated_names_signature, None);
    }

    fn test_state_with_config(mutate: impl FnOnce(&mut BoottyConfig)) -> AppState {
        test_state_with_config_and_automation(mutate, AutomationHub::new())
    }

    fn test_state_with_config_and_automation(
        mutate: impl FnOnce(&mut BoottyConfig),
        automation: AutomationHub,
    ) -> AppState {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-test-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create app state test config dir");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        mutate(&mut config);
        AppState::new_for_window_with_automation(
            config,
            PRIMARY_WINDOW_STATE_KEY.to_owned(),
            repaint,
            None,
            None,
            automation,
        )
        .expect("state")
    }

    fn test_state_for_window_instance(window_state_key: &str, instance: InstanceRef) -> AppState {
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-window-state-test-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create app state test config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        AppState::new_for_window_with_automation_and_instance(
            config,
            window_state_key.to_owned(),
            repaint,
            None,
            None,
            AutomationHub::new(),
            instance,
        )
        .expect("state")
    }

    #[test]
    fn app_state_retains_the_supplied_automation_hub() {
        let hub = AutomationHub::new();
        let state = test_state_with_config_and_automation(|_| {}, hub.clone());

        assert!(state.automation_hub().shares_state_with(&hub));
    }

    #[test]
    fn dropping_app_state_releases_only_its_exact_window_claims() {
        let unique = unique_test_id();
        let instance = InstanceRef {
            instance_id: format!("window-owner-{unique}"),
            generation: 1,
        };
        let mut closing = test_state_for_window_instance("closing", instance.clone());
        let mut sibling = test_state_for_window_instance("sibling", instance.clone());
        let claims_root = std::env::temp_dir().join(format!("bootty-window-claims-{unique}"));
        let claims = DirectoryClaims::at(
            &claims_root,
            ClaimOwner::current(format!("window-claims-{unique}")).expect("claim owner"),
        )
        .expect("isolated claims");
        closing.directory_claims = claims.clone();
        sibling.directory_claims = claims.clone();
        let automation = closing.automation_hub();
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["directory.usage_changed".to_owned()].into_iter().collect(),
                automation_event_scope(closing.binding.scope),
            )
            .expect("subscribe to usage changes")
            .subscription;

        let closing_context = DirectoryClaimsContext {
            instance: instance.clone(),
            window_id: closing.window_state_key.clone(),
        };
        let sibling_context = DirectoryClaimsContext {
            instance,
            window_id: sibling.window_state_key.clone(),
        };
        let closing_claimant = directory_claimant_for_pane_at_generation(
            &closing_context,
            &closing.binding,
            "closing-session",
            "closing-pane",
            "closing-terminal",
            closing.binding.mux.binding_generation(),
            1,
        );
        let sibling_claimant = directory_claimant_for_pane_at_generation(
            &sibling_context,
            &sibling.binding,
            "sibling-session",
            "sibling-pane",
            "sibling-terminal",
            sibling.binding.mux.binding_generation(),
            1,
        );
        std::fs::create_dir_all(&claims_root).expect("create claims directory");
        let directory = DirectoryRef::resolve(&claims_root).expect("resolve claim directory");
        claims
            .record_launch(closing_claimant.clone(), directory.clone())
            .expect("record closing launch");
        claims
            .observe_cwd(closing_claimant.clone(), directory.clone())
            .expect("record closing cwd");
        claims
            .record_launch(sibling_claimant.clone(), directory.clone())
            .expect("record sibling launch");
        claims
            .observe_cwd(sibling_claimant.clone(), directory)
            .expect("record sibling cwd");
        let closing_scope = automation_event_scope(closing.binding.scope);
        let closing_window = WindowRef {
            instance: closing_context.instance.clone(),
            window_id: closing_context.window_id.clone(),
        };
        let closing_claims = claims.clone();
        let before = claims.snapshot().expect("claims before teardown");

        drop(closing);
        for _ in 0..512 {
            enqueue_window_claim_release(
                closing_claims.clone(),
                closing_window.clone(),
                automation.clone(),
                vec![closing_scope.clone()],
            );
        }

        let mut snapshot = claims.snapshot().expect("claims after teardown");
        for _ in 0..100 {
            if snapshot.revision > before.revision {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
            snapshot = claims.snapshot().expect("claims after teardown");
        }
        assert_eq!(snapshot.revision, before.revision + 1);
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.terminal.binding.window
                    != closing_claimant.terminal.binding.window)
        );
        assert!(
            snapshot
                .claims
                .iter()
                .any(|claim| claim.terminal == sibling_claimant.terminal)
        );
        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("read usage change");
        assert_eq!(delivery.events.len(), 1);
        assert_eq!(delivery.events[0].topic, "directory.usage_changed");
        assert_eq!(delivery.events[0].payload["reason"], "window_closed");
    }

    fn test_binding_runtime(scope: MuxScope) -> BindingRuntime {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-binding-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create binding test config directory");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        ensure_test_binding(
            &config.config_path,
            scope,
            selected_backend(&config.multiplexer),
        );
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        BindingRuntime::new(scope, &config, AppearanceVariant::Dark, repaint)
            .expect("create test binding")
    }

    #[test]
    fn persisted_recursive_launch_restores_exact_plan_and_topology() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-recursive-restore-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create recursive restore config directory");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;

        let root_path = config_dir.join("root");
        let first_path = root_path.join("first");
        let second_path = root_path.join("second");
        let logs_path = root_path.join("logs");
        for path in [&first_path, &second_path, &logs_path] {
            std::fs::create_dir_all(path).expect("create launch pane directory");
        }
        let root = root_path.to_string_lossy().into_owned();
        let first_cwd = first_path.to_string_lossy().into_owned();
        let second_cwd = second_path.to_string_lossy().into_owned();
        let logs_cwd = logs_path.to_string_lossy().into_owned();
        let session_id = format!("recursive-restore-{unique}");

        let mut session_environment = std::collections::BTreeMap::new();
        session_environment.insert("SESSION".to_owned(), "persisted".to_owned());
        let mut first_environment = std::collections::BTreeMap::new();
        first_environment.insert("PANE".to_owned(), "first".to_owned());
        let first = MuxPaneLaunch {
            cwd: first_cwd.clone(),
            command: Some("printf first".to_owned()),
            argv: None,
            environment: first_environment,
            title: Some("First pane".to_owned()),
        };
        let second = MuxPaneLaunch {
            cwd: second_cwd.clone(),
            command: None,
            argv: Some(vec!["printf".to_owned(), "second".to_owned()]),
            environment: std::collections::BTreeMap::new(),
            title: Some("Second pane".to_owned()),
        };
        let logs = MuxPaneLaunch {
            cwd: logs_cwd.clone(),
            command: Some("printf logs".to_owned()),
            argv: None,
            environment: std::collections::BTreeMap::new(),
            title: Some("Logs".to_owned()),
        };
        let plan = MuxSessionLaunchPlan {
            session_id: session_id.clone(),
            focus: true,
            default_cwd: root.clone(),
            environment: session_environment,
            windows: vec![
                MuxWindowLaunchPlan {
                    name: Some("work".to_owned()),
                    focus: true,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 620,
                        first: Box::new(MuxPaneLaunchPlan::Pane(first)),
                        second: Box::new(MuxPaneLaunchPlan::Pane(second)),
                    }),
                },
                MuxWindowLaunchPlan {
                    name: Some("logs".to_owned()),
                    focus: false,
                    layout: MuxPaneLaunchPlan::Pane(logs),
                },
            ],
            focused_window: 0,
        };

        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let scope = workspace.binding().expect("default binding").mux_scope();
        let binding_id = scope.binding_id().persistence_value();
        let mut names = SessionNameStore::for_binding(&config.config_path, binding_id);
        names.remember_generated(&session_id, &root, &session_id, &session_id);
        let mut order = SessionOrderStore::for_binding(
            &config.config_path,
            binding_id,
            namespace_for_binding(scope, &config.multiplexer),
        )
        .expect("open recursive restore order");
        order
            .add_session(&session_id)
            .expect("persist recursive session");
        persist_session_launch_plan(&config.config_path, binding_id, &plan)
            .expect("persist recursive plan");

        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let binding = BindingRuntime::new(scope, &config, AppearanceVariant::Dark, repaint)
            .expect("restore recursive binding");
        let restored = binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("restored recursive session");
        assert_eq!(restored.windows.len(), 2);
        assert_eq!(restored.windows[0].name, "work");
        assert_eq!(restored.windows[0].panes.len(), 2);
        assert_eq!(
            restored.windows[0].panes[0].cwd.as_deref(),
            Some(first_cwd.as_str())
        );
        assert_eq!(
            restored.windows[0].panes[1].cwd.as_deref(),
            Some(second_cwd.as_str())
        );
        assert_eq!(restored.windows[1].name, "logs");
        assert_eq!(restored.windows[1].panes.len(), 1);
        assert_eq!(
            restored.windows[1].panes[0].cwd.as_deref(),
            Some(logs_cwd.as_str())
        );
        assert_eq!(
            load_session_launch_plans(&config.config_path, binding_id)
                .expect("reload recursive plan"),
            vec![(session_id, plan)]
        );
    }

    #[test]
    fn persisted_restore_reconciles_two_plans_when_one_session_is_already_live() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-partial-restore-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create partial restore config directory");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;

        let cwd = config_dir.join("cwd").to_string_lossy().into_owned();
        std::fs::create_dir_all(&cwd).expect("create partial restore cwd");
        let live_id = format!("partial-live-{unique}");
        let missing_id = format!("partial-missing-{unique}");
        let live_plan = simple_session_launch_plan(&live_id, &cwd);
        let missing_plan = simple_session_launch_plan(&missing_id, &cwd);

        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let scope = workspace.binding().expect("default binding").mux_scope();
        let binding_id = scope.binding_id().persistence_value();
        {
            let mut names = SessionNameStore::for_binding(&config.config_path, binding_id);
            names.remember_generated(&live_id, &cwd, &live_id, &live_id);
            names.remember_generated(&missing_id, &cwd, &missing_id, &missing_id);
            let mut order = SessionOrderStore::for_binding(
                &config.config_path,
                binding_id,
                namespace_for_binding(scope, &config.multiplexer),
            )
            .expect("open partial restore order");
            order.add_session(&live_id).expect("persist live session");
            order
                .add_session(&missing_id)
                .expect("persist missing session");
            persist_session_launch_plan(&config.config_path, binding_id, &live_plan)
                .expect("persist live plan");
            persist_session_launch_plan(&config.config_path, binding_id, &missing_plan)
                .expect("persist missing plan");
        }

        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        {
            let mut live_binding = BindingRuntime::new_with_backend_override(
                scope,
                &config,
                None,
                SpaceRemoteOverride::Inherit,
                AppearanceVariant::Dark,
                repaint.clone(),
                true,
            )
            .expect("create pre-existing binding");
            live_binding
                .mux
                .create_session(live_plan.clone(), &repaint, &live_binding.multiplexer);
            assert!(
                live_binding
                    .mux
                    .sessions()
                    .iter()
                    .any(|session| session.id == live_id)
            );
        }

        let binding = BindingRuntime::new(scope, &config, AppearanceVariant::Dark, repaint)
            .expect("restore partial binding");
        let sessions = binding.mux.sessions();
        assert!(sessions.iter().any(|session| session.id == live_id));
        assert!(sessions.iter().any(|session| session.id == missing_id));
        assert_eq!(
            load_session_launch_plans(&config.config_path, binding_id)
                .expect("reload partial plans"),
            vec![(live_id, live_plan), (missing_id, missing_plan)]
        );
    }

    #[test]
    fn reopened_binding_reconciles_committed_rename_without_duplicate_plan() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-rename-recovery-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create rename recovery config directory");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;
        let cwd = config_dir.join("cwd").to_string_lossy().into_owned();
        std::fs::create_dir_all(&cwd).expect("create rename recovery cwd");
        let plan = simple_session_launch_plan("alpha", &cwd);
        let rename = PendingSessionRename {
            session_id: "alpha".to_owned(),
            old_name: "alpha".to_owned(),
            new_name: "release".to_owned(),
            display_name: "release".to_owned(),
            cwd: cwd.clone(),
        };

        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let scope = workspace.binding().expect("default binding").mux_scope();
        let binding_id = scope.binding_id().persistence_value();
        {
            let mut names = SessionNameStore::for_binding(&config.config_path, binding_id);
            names.remember_generated("alpha", &cwd, "alpha", "alpha");
            let mut order = SessionOrderStore::for_binding(
                &config.config_path,
                binding_id,
                namespace_for_binding(scope, &config.multiplexer),
            )
            .expect("open rename recovery order");
            order
                .add_session("alpha")
                .expect("persist source membership");
            persist_session_launch_plan(&config.config_path, binding_id, &plan)
                .expect("persist source launch plan");
            persist_pending_session_rename(
                &config.config_path,
                binding_id,
                "rename-recovery-token",
                &rename,
            )
            .expect("persist pending rename");
        }

        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        {
            let mut live_binding = BindingRuntime::new_with_backend_override(
                scope,
                &config,
                None,
                SpaceRemoteOverride::Inherit,
                AppearanceVariant::Dark,
                repaint.clone(),
                true,
            )
            .expect("create live rename binding");
            live_binding
                .mux
                .create_session(plan.clone(), &repaint, &live_binding.multiplexer);
            assert!(
                live_binding
                    .mux
                    .sessions()
                    .iter()
                    .any(|session| session.id == "alpha")
            );

            assert!(
                rename_session_membership_and_launch_plans(
                    &config.config_path,
                    binding_id,
                    &rename.old_name,
                    &rename.new_name,
                    &[rename.session_id.as_str()],
                )
                .expect("commit durable rename before simulated crash")
            );
            let conn =
                crate::workspace::open_db(workspace.path()).expect("open rename recovery database");
            conn.execute(
                "UPDATE workspace_session_name_metadata
                 SET session_name = 'release', display_name = 'release', explicit = 1
                 WHERE binding_id = ?1 AND session_id = 'alpha'",
                [binding_id],
            )
            .expect("persist renamed metadata before simulated crash");
            drop(conn);

            live_binding.mux.rename_session(
                "alpha",
                "release".to_owned(),
                &repaint,
                &live_binding.multiplexer,
            );
            assert!(
                live_binding
                    .mux
                    .sessions()
                    .iter()
                    .any(|session| session.id == "alpha" && session.name == "release")
            );
        }

        let binding = BindingRuntime::new(scope, &config, AppearanceVariant::Dark, repaint)
            .expect("reopen rename recovery binding");
        assert!(
            load_pending_session_renames(&config.config_path, binding_id)
                .expect("load recovered pending renames")
                .is_empty()
        );
        assert_eq!(
            load_session_launch_plans(&config.config_path, binding_id)
                .expect("load recovered launch plan"),
            vec![(
                "release".to_owned(),
                simple_session_launch_plan("release", &cwd)
            )]
        );
        assert_eq!(
            binding
                .mux
                .sessions()
                .iter()
                .filter(|session| session.id == "alpha" && session.name == "release")
                .count(),
            1
        );
    }

    #[derive(Clone, Copy)]
    struct ObservationGenerations {
        binding: u64,
        target: Option<u64>,
        retired_target: Option<u64>,
    }

    fn observed_mux_event(
        scope: MuxScope,
        revision: u64,
        topic: MuxEventTopic,
        target: Option<MuxEventTarget>,
        payload: MuxEventPayload,
        generations: ObservationGenerations,
    ) -> MuxEventObservation {
        MuxEventObservation {
            event: MuxEvent {
                backend_identity: "test".to_owned(),
                scope,
                revision,
                cursor: None,
                topic,
                provenance: crate::mux::backend::MuxEventProvenance::Queue,
                target,
                payload,
            },
            binding_generation: generations.binding,
            target_generation: generations.target,
            retired_target_generation: generations.retired_target,
        }
    }

    #[test]
    fn unresolved_resource_events_do_not_retarget_the_binding() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(97),
            BindingId::from_persistence(97),
        );
        let binding = test_binding_runtime(scope);
        let context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let unresolved = observed_mux_event(
            scope,
            1,
            MuxEventTopic::TerminalOutput,
            Some(MuxEventTarget::pane("$1", "@1", "%1", "t1", None)),
            MuxEventPayload::Output {
                bytes: b"foreign".to_vec(),
            },
            ObservationGenerations {
                binding: binding.mux.binding_generation(),
                target: None,
                retired_target: None,
            },
        );
        assert_eq!(
            automation_target_from_mux_event(&binding, &context, &unresolved),
            None
        );

        let targetless = observed_mux_event(
            scope,
            2,
            MuxEventTopic::TopologyChanged,
            None,
            MuxEventPayload::Topology {
                change: crate::mux::backend::MuxTopologyChange::Invalidated,
            },
            ObservationGenerations {
                binding: binding.mux.binding_generation(),
                target: None,
                retired_target: None,
            },
        );
        assert!(matches!(
            automation_target_from_mux_event(&binding, &context, &targetless),
            Some(CommandTarget {
                kind: ResourceKind::Binding,
                ..
            })
        ));
    }

    #[test]
    fn authoritative_cwd_clear_releases_the_observed_directory_claim() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(98),
            BindingId::from_persistence(98),
        );
        let mut binding = test_binding_runtime(scope);
        let unique = unique_test_id();
        let directory = std::env::temp_dir().join(format!("bootty-cwd-clear-{unique}"));
        std::fs::create_dir_all(&directory).expect("claim directory");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let _backend = ScriptedBackend::with(vec![session_with_window_and_pane(
            "live",
            "live",
            &directory.to_string_lossy(),
        )])
        .install(&mut binding);
        refresh_selector_test_sessions(&mut binding, &repaint, 1);
        let binding_generation = binding.mux.binding_generation();
        let window_id = "live-window";
        let pane_id = "live-pane";
        let terminal_id = "live-terminal";
        let target_generation = binding
            .mux
            .terminal_generation("live", window_id, terminal_id)
            .expect("live test generation");
        let claims = DirectoryClaims::at(
            directory.join("claims"),
            ClaimOwner::current(format!("cwd-clear-{unique}")).expect("claim owner"),
        )
        .expect("isolated claims");
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: format!("cwd-clear-{unique}"),
                generation: 1,
            },
            window_id: state.window_state_key.clone(),
        };
        let claimant = directory_claimant_for_pane_at_generation(
            &claims_context,
            &binding,
            "live",
            pane_id,
            terminal_id,
            binding_generation,
            target_generation,
        );
        claims
            .observe_cwd(
                claimant,
                DirectoryRef::resolve(&directory).expect("resolve claim directory"),
            )
            .expect("record observed cwd");
        let observation = observed_mux_event(
            scope,
            1,
            MuxEventTopic::PaneCwdChanged,
            Some(MuxEventTarget::pane(
                "live",
                window_id,
                pane_id,
                terminal_id,
                None,
            )),
            MuxEventPayload::Cwd {
                old_cwd: Some(directory.to_string_lossy().into_owned()),
                new_cwd: None,
            },
            ObservationGenerations {
                binding: binding_generation,
                target: Some(target_generation),
                retired_target: None,
            },
        );
        consume_directory_claim_event(
            &claims,
            &claims_context,
            &state.automation,
            &binding,
            &AutomationTargetContext {
                process: state.command_instance_handle.clone(),
                window_state_key: state.window_state_key.clone(),
                window_generation: state.command_window_generation,
            },
            &observation,
        )
        .expect("consume cwd clear");

        assert!(claims.snapshot().expect("claims").claims.is_empty());
    }
    #[test]
    fn stale_cwd_after_binding_rebase_does_not_recreate_retired_claim() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(100),
            BindingId::from_persistence(100),
        );
        let mut binding = test_binding_runtime(scope);
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let unique = unique_test_id();
        let root = std::env::temp_dir().join(format!("bootty-stale-cwd-{unique}"));
        std::fs::create_dir_all(&root).expect("stale cwd directory");
        let cwd = root.to_string_lossy().into_owned();
        let backend =
            ScriptedBackend::with(vec![session_with_window_and_pane("live", "live", &cwd)])
                .install(&mut binding);
        refresh_selector_test_sessions(&mut binding, &repaint, 1);
        let old_binding_generation = binding.mux.binding_generation();
        let window_id = "live-window";
        let pane_id = "live-pane";
        let terminal_id = "live-terminal";
        let target_generation = binding
            .mux
            .terminal_generation("live", window_id, terminal_id)
            .expect("live stale-cwd generation");
        let automation = state.automation_hub();
        let claims = DirectoryClaims::at(
            root.join("claims"),
            ClaimOwner::current(format!("stale-cwd-{unique}")).expect("claim owner"),
        )
        .expect("stale cwd claims");
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: format!("stale-cwd-{unique}"),
                generation: 1,
            },
            window_id: state.window_state_key.clone(),
        };
        let old_claimant = directory_claimant_for_pane_at_generation(
            &claims_context,
            &binding,
            "live",
            pane_id,
            terminal_id,
            old_binding_generation,
            target_generation,
        );
        claims
            .observe_cwd(
                old_claimant.clone(),
                DirectoryRef::resolve(&root).expect("resolve stale cwd"),
            )
            .expect("record stale cwd claim");
        let old_observation = observed_mux_event(
            scope,
            1,
            MuxEventTopic::PaneCwdChanged,
            Some(MuxEventTarget::pane(
                "live",
                window_id,
                pane_id,
                terminal_id,
                None,
            )),
            MuxEventPayload::Cwd {
                old_cwd: None,
                new_cwd: Some(root.to_string_lossy().into_owned()),
            },
            ObservationGenerations {
                binding: old_binding_generation,
                target: Some(target_generation),
                retired_target: None,
            },
        );

        assert!(
            binding
                .mux
                .drain_events(&binding.multiplexer, 16)
                .is_empty()
        );
        backend.publish_event(crate::mux::backend::MuxEventDraft::rebase(
            crate::mux::backend::MuxEventProvenance::Queue,
            crate::mux::backend::MuxRebaseReason::Reconnect,
        ));
        let rebased = binding.mux.drain_events(&binding.multiplexer, 16);
        assert_eq!(rebased.len(), 1);
        assert_ne!(binding.mux.binding_generation(), old_binding_generation);
        claims
            .release_observed_claimant(&old_claimant)
            .expect("retire old cwd claim");
        assert!(claims.snapshot().expect("retired claims").claims.is_empty());

        let target_context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        automation
            .events()
            .replace_live_binding_scopes([automation_event_scope(scope)]);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                [automation_event_topic(MuxEventTopic::PaneCwdChanged).to_owned()]
                    .into_iter()
                    .collect(),
                automation_event_scope(scope),
            )
            .expect("subscribe stale cwd event")
            .subscription;
        binding.pending_automation_events.push_back(old_observation);
        assert_eq!(
            publish_pending_binding_automation_events(
                &mut binding,
                &automation,
                &target_context,
                &claims,
                &claims_context,
            )
            .expect("publish stale cwd event"),
            1
        );
        assert!(
            claims
                .snapshot()
                .expect("post-event claims")
                .claims
                .is_empty()
        );
        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("poll stale cwd event");
        assert_eq!(delivery.events.len(), 1);
        assert_eq!(
            delivery.events[0].topic,
            automation_event_topic(MuxEventTopic::PaneCwdChanged)
        );
    }

    #[test]
    fn batched_occupant_replacements_publish_their_own_target_generations() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(99),
            BindingId::from_persistence(99),
        );
        let mut binding = test_binding_runtime(scope);
        let automation = state.automation_hub();
        let unique = unique_test_id();
        let claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-event-claims-{unique}")),
            ClaimOwner::current(format!("event-claims-{unique}")).expect("claim owner"),
        )
        .expect("isolated claims");
        let target_context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: format!("event-claims-{unique}"),
                generation: 1,
            },
            window_id: state.window_state_key.clone(),
        };
        let event_scope = automation_event_scope(scope);
        automation
            .events()
            .replace_live_binding_scopes([event_scope.clone()]);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["terminal.occupant_replaced".to_owned()]
                    .into_iter()
                    .collect(),
                event_scope,
            )
            .expect("subscribe")
            .subscription;
        let binding_generation = binding.mux.binding_generation();
        let target = || MuxEventTarget::pane("$1", "@1", "%1", "t1", None);
        binding.pending_automation_events.extend([
            observed_mux_event(
                scope,
                1,
                MuxEventTopic::PaneOccupantReplaced,
                Some(target()),
                MuxEventPayload::OccupantReplaced {
                    old_occupant: None,
                    new_occupant: None,
                },
                ObservationGenerations {
                    binding: binding_generation,
                    target: Some(1),
                    retired_target: None,
                },
            ),
            observed_mux_event(
                scope,
                2,
                MuxEventTopic::PaneOccupantReplaced,
                Some(target()),
                MuxEventPayload::OccupantReplaced {
                    old_occupant: None,
                    new_occupant: None,
                },
                ObservationGenerations {
                    binding: binding_generation,
                    target: Some(2),
                    retired_target: Some(1),
                },
            ),
        ]);

        assert_eq!(
            publish_pending_binding_automation_events(
                &mut binding,
                &automation,
                &target_context,
                &claims,
                &claims_context,
            )
            .expect("publish pending events"),
            2
        );
        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("read lifecycle events");

        assert_eq!(
            delivery
                .events
                .iter()
                .map(|event| {
                    (
                        event.target.as_ref().expect("terminal target").kind,
                        event.target.as_ref().expect("terminal target").generation,
                    )
                })
                .collect::<Vec<_>>(),
            vec![(ResourceKind::Terminal, 1), (ResourceKind::Terminal, 2)]
        );
    }

    #[test]
    fn failed_pending_event_retains_it_and_later_events_for_ordered_retry() {
        let scope = MuxScope::new(SpaceId::from_persistence(1), BindingId::from_persistence(1));
        let mut pending = VecDeque::new();
        for revision in [1, 2] {
            pending.push_back(observed_mux_event(
                scope,
                revision,
                MuxEventTopic::BackendDisconnected,
                None,
                MuxEventPayload::Disconnected {
                    reason: "test".to_owned(),
                },
                ObservationGenerations {
                    binding: 1,
                    target: None,
                    retired_target: None,
                },
            ));
        }

        assert_eq!(
            consume_pending_automation_events(
                &mut pending,
                |_| true,
                |_| Err(AutomationError::new(-32000, "publication failed")),
            )
            .unwrap_err()
            .code,
            -32000
        );
        assert_eq!(
            pending
                .iter()
                .map(|observation| observation.event.revision)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );

        let mut retried = Vec::new();
        assert_eq!(
            consume_pending_automation_events(
                &mut pending,
                |_| true,
                |observation| {
                    retried.push(observation.event.revision);
                    Ok(())
                },
            )
            .expect("retry pending events"),
            2
        );
        assert_eq!(retried, vec![1, 2]);
        assert!(pending.is_empty());
    }

    #[test]
    fn rebase_event_waits_for_refreshed_sources_before_publication() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(SpaceId::from_persistence(8), BindingId::from_persistence(8));
        let mut binding = test_binding_runtime(scope);
        let automation = state.automation_hub();
        let unique = unique_test_id();
        let claims_instance = format!("rebase-event-claims-{unique}");
        let claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-rebase-event-claims-{unique}")),
            ClaimOwner::current(claims_instance.clone()).expect("claim owner"),
        )
        .expect("isolated claims");
        let target_context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: claims_instance,
                generation: 1,
            },
            window_id: state.window_state_key.clone(),
        };
        let event_scope = automation_event_scope(scope);
        automation
            .events()
            .replace_live_binding_scopes([event_scope.clone()]);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                [
                    "backend.connection_changed".to_owned(),
                    "backend.rebased".to_owned(),
                ]
                .into_iter()
                .collect(),
                event_scope,
            )
            .expect("subscribe")
            .subscription;
        binding.pending_automation_events.extend([
            observed_mux_event(
                scope,
                1,
                MuxEventTopic::BackendDisconnected,
                None,
                MuxEventPayload::Disconnected {
                    reason: "before rebase".to_owned(),
                },
                ObservationGenerations {
                    binding: binding.mux.binding_generation(),
                    target: None,
                    retired_target: None,
                },
            ),
            observed_mux_event(
                scope,
                2,
                MuxEventTopic::SnapshotRebased,
                None,
                MuxEventPayload::Rebase {
                    reason: crate::mux::backend::MuxRebaseReason::Reconnect,
                },
                ObservationGenerations {
                    binding: binding.mux.binding_generation(),
                    target: None,
                    retired_target: None,
                },
            ),
        ]);
        binding.automation_event_refresh_pending = true;

        assert_eq!(
            publish_pending_binding_automation_events(
                &mut binding,
                &automation,
                &target_context,
                &claims,
                &claims_context,
            )
            .expect("hold rebase"),
            0
        );
        assert_eq!(binding.pending_automation_events.len(), 2);
        assert!(
            automation
                .events()
                .poll(&subscription, &owner, 0)
                .expect("no early rebase")
                .events
                .is_empty()
        );

        install_binding_automation_sources(
            &automation,
            &claims,
            &mut binding,
            &target_context,
            true,
        )
        .expect("install refreshed sources");
        reconcile_directory_claims_after_authoritative_refresh(
            &claims,
            &claims_context,
            &automation,
            &binding,
            &target_context,
        )
        .expect("reconcile refreshed claims");
        binding.automation_event_refresh_pending = false;

        assert_eq!(
            publish_pending_binding_automation_events(
                &mut binding,
                &automation,
                &target_context,
                &claims,
                &claims_context,
            )
            .expect("publish refreshed events"),
            2
        );
        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("read refreshed events");
        assert_eq!(
            delivery
                .events
                .iter()
                .map(|event| event.topic.as_str())
                .collect::<Vec<_>>(),
            ["backend.connection_changed", "backend.rebased"]
        );
    }

    #[test]
    fn inactive_binding_rebase_refreshes_sources_and_claims_before_publication() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let unique = unique_test_id();
        let claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-inactive-rebase-claims-{unique}")),
            ClaimOwner::current(state.command_instance_handle.clone()).expect("claim owner"),
        )
        .expect("isolated claims");
        state.directory_claims = claims.clone();

        let scope = MuxScope::new(state.active_space_id(), BindingId::from_persistence(99));
        let config = state.config().clone();
        ensure_test_binding(
            &config.config_path,
            scope,
            selected_backend(&config.multiplexer),
        );

        let mut binding = BindingRuntime::new(
            scope,
            &config,
            AppearanceVariant::Dark,
            state.repaint.clone(),
        )
        .expect("create inactive binding");
        let root = state
            .config()
            .config_path
            .parent()
            .expect("test config has a parent")
            .to_path_buf();
        let cwd = root.display().to_string();
        let live_session = session_with_window_and_pane("live", "live", &cwd);
        let closed_session = session_with_window_and_pane("closed", "closed", &cwd);
        let backend = ScriptedBackend::with(vec![live_session.clone(), closed_session.clone()])
            .install(&mut binding);
        refresh_selector_test_sessions(&mut binding, &state.repaint, 2);
        let _ = binding.mux.take_refresh_completed();
        binding.automation_generation = Some(binding.mux.binding_generation());

        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: state.command_instance_handle.clone(),
                generation: state.command_instance_generation,
            },
            window_id: state.window_state_key.clone(),
        };
        let live_claimant = directory_claimant_for_pane(
            &claims_context,
            &binding,
            "live",
            "live-window",
            "live-pane",
            "live-terminal",
        )
        .expect("live claimant");
        let closed_claimant = directory_claimant_for_pane(
            &claims_context,
            &binding,
            "closed",
            "closed-window",
            "closed-pane",
            "closed-terminal",
        )
        .expect("closed claimant");
        let directory = DirectoryRef::resolve(&root).expect("resolve claim directory");
        claims
            .record_launch(live_claimant.clone(), directory.clone())
            .expect("record live launch");
        claims
            .record_launch(closed_claimant.clone(), directory)
            .expect("record closed launch");

        state.inactive_bindings.push(binding);
        state.synchronize_live_binding_event_scopes();
        let automation = state.automation_hub();
        let event_scope = automation_event_scope(scope);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                [
                    "directory.usage_changed".to_owned(),
                    "backend.rebased".to_owned(),
                ]
                .into_iter()
                .collect(),
                event_scope.clone(),
            )
            .expect("subscribe")
            .subscription;

        backend.set(vec![live_session]);
        backend.publish_event(crate::mux::backend::MuxEventDraft::rebase(
            crate::mux::backend::MuxEventProvenance::Queue,
            crate::mux::backend::MuxRebaseReason::Reconnect,
        ));
        state.update_frame(test_frame_inputs(Vec::new(), None));

        let binding = state
            .binding_runtime(scope)
            .expect("inactive binding remains live");
        assert!(
            !binding.automation_event_refresh_pending,
            "a refreshed inactive binding must not keep its event queue blocked"
        );
        assert!(binding.pending_automation_events.is_empty());

        let source_topics = std::collections::BTreeSet::from([
            "topology.changed".to_owned(),
            "directory.usage_changed".to_owned(),
        ]);
        let source = automation
            .events()
            .snapshot(&event_scope, &source_topics)
            .expect("refreshed source snapshot");
        let sessions = source.snapshots["topology.changed"]["sessions"]
            .as_array()
            .expect("topology sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], json!("live"));

        let claims_snapshot = claims.snapshot().expect("claim snapshot");
        assert!(
            !claims_snapshot
                .claims
                .iter()
                .any(|claim| claim.terminal == closed_claimant.terminal),
            "the vanished terminal claim must be reconciled before publication"
        );
        assert!(claims_snapshot.claims.iter().any(|claim| {
            claim.session.session_id == live_claimant.session.session_id
                && claim.terminal.binding.generation == binding.mux.binding_generation()
        }));

        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("read ordered refresh events");
        assert_eq!(
            delivery
                .events
                .iter()
                .map(|event| event.topic.as_str())
                .collect::<Vec<_>>(),
            ["directory.usage_changed", "backend.rebased"]
        );
    }

    #[test]
    fn occupant_lifecycle_purges_the_exact_retired_terminal_output_target() {
        let scope = MuxScope::new(SpaceId::from_persistence(7), BindingId::from_persistence(7));
        let binding = test_binding_runtime(scope);
        let automation = AutomationHub::new();
        let target_context = AutomationTargetContext {
            process: "test".to_owned(),
            window_state_key: "main".to_owned(),
            window_generation: 1,
        };
        let event_scope = automation_event_scope(scope);
        let binding_generation = binding.mux.binding_generation();
        let first_target = automation_terminal_target_for_generation(
            &binding,
            &target_context,
            AutomationTerminalIdentity {
                session_id: "$1",
                window_id: "@1",
                pane_id: "%p",
                terminal_id: "t1",
            },
            binding_generation,
            1,
        );
        let first_path =
            serde_json::from_str::<Vec<String>>(&first_target.handle).expect("terminal handle");
        assert_eq!(
            first_path
                .iter()
                .skip(1)
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["$1", "@1", "%p", "t1"]
        );
        automation
            .publish_terminal_output(
                event_scope.clone(),
                json!({"source": "test"}),
                first_target.clone(),
                json!({"data": "first"}),
            )
            .expect("publish first output");
        let replacement = observed_mux_event(
            scope,
            1,
            MuxEventTopic::PaneOccupantReplaced,
            Some(MuxEventTarget::pane("$1", "@1", "%p", "t1", None)),
            MuxEventPayload::OccupantReplaced {
                old_occupant: None,
                new_occupant: None,
            },
            ObservationGenerations {
                binding: binding_generation,
                target: Some(2),
                retired_target: Some(1),
            },
        );

        purge_retired_terminal_output(&automation, &binding, &target_context, &replacement)
            .expect("purge replaced output");
        assert!(
            automation
                .terminal_output_after(&event_scope, &first_target, 0)
                .is_err()
        );

        let second_target = automation_terminal_target_for_generation(
            &binding,
            &target_context,
            AutomationTerminalIdentity {
                session_id: "$1",
                window_id: "@1",
                pane_id: "%p",
                terminal_id: "t1",
            },
            binding_generation,
            2,
        );
        automation
            .publish_terminal_output(
                event_scope.clone(),
                json!({"source": "test"}),
                second_target.clone(),
                json!({"data": "second"}),
            )
            .expect("publish second output");
        let closed = observed_mux_event(
            scope,
            2,
            MuxEventTopic::PaneClosed,
            Some(MuxEventTarget::pane("$1", "@1", "%p", "t1", None)),
            MuxEventPayload::Closed {
                reason: "closed".to_owned(),
            },
            ObservationGenerations {
                binding: binding_generation,
                target: Some(2),
                retired_target: Some(2),
            },
        );

        purge_retired_terminal_output(&automation, &binding, &target_context, &closed)
            .expect("purge closed output");
        assert!(
            automation
                .terminal_output_after(&event_scope, &second_target, 0)
                .is_err()
        );
    }

    #[test]
    fn rmux_reconnect_rebases_live_launch_claims_and_publishes_one_directory_update() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        });
        let unique = unique_test_id();
        let claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-rmux-rebase-claims-{unique}")),
            ClaimOwner::current(state.command_instance_handle.clone()).expect("claim owner"),
        )
        .expect("isolated claims");
        state.directory_claims = claims.clone();
        let automation = state.automation_hub();
        let scope = automation_event_scope(state.binding.scope);
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: state.command_instance_handle.clone(),
                generation: state.command_instance_generation,
            },
            window_id: state.window_state_key.clone(),
        };
        let root = state
            .config()
            .config_path
            .parent()
            .expect("test config has a parent")
            .to_path_buf();
        let cwd = root.display().to_string();
        let set_occupant = |session: &mut MuxSession, occupant: &str| {
            let occupant = Some(occupant.to_owned());
            session.anchor.occupant_id.clone_from(&occupant);
            for window in &mut session.windows {
                window.anchor.occupant_id.clone_from(&occupant);
                for pane in &mut window.panes {
                    pane.occupant_id.clone_from(&occupant);
                }
            }
        };
        let mut live_session = session_with_window_and_pane("live", "live", &cwd);
        set_occupant(&mut live_session, "old");
        let mut closed_session = session_with_window_and_pane("closed", "closed", &cwd);
        set_occupant(&mut closed_session, "closed");
        let backend = ScriptedBackend::with(vec![live_session.clone(), closed_session])
            .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 2);

        let live_before = directory_claimant_for_pane(
            &claims_context,
            &state.binding,
            "live",
            "live-window",
            "live-pane",
            "live-terminal",
        )
        .expect("live claimant before reconnect");
        let closed_before = directory_claimant_for_pane(
            &claims_context,
            &state.binding,
            "closed",
            "closed-window",
            "closed-pane",
            "closed-terminal",
        )
        .expect("closed claimant before reconnect");
        let current_generation = state.binding.mux.binding_generation();
        let retired_generation = if current_generation == 0 {
            1
        } else {
            current_generation - 1
        };
        let retired_live = directory_claimant_for_pane_at_generation(
            &claims_context,
            &state.binding,
            "live",
            "live-pane",
            "live-terminal",
            retired_generation,
            live_before.terminal.occupant_generation,
        );
        let retired_closed = directory_claimant_for_pane_at_generation(
            &claims_context,
            &state.binding,
            "closed",
            "closed-pane",
            "closed-terminal",
            retired_generation,
            closed_before.terminal.occupant_generation,
        );
        let launch_directory = DirectoryRef::resolve(&root).expect("resolve launch directory");
        claims
            .record_launch(retired_live.clone(), launch_directory.clone())
            .expect("record live launch");
        claims
            .observe_cwd(
                retired_live.clone(),
                DirectoryRef::resolve(root.join("live-observed")).expect("resolve observed cwd"),
            )
            .expect("record live observed cwd");
        claims
            .record_launch(
                retired_closed.clone(),
                DirectoryRef::resolve(root.join("closed-launch")).expect("resolve closed launch"),
            )
            .expect("record closed launch");
        claims
            .observe_cwd(
                retired_closed.clone(),
                DirectoryRef::resolve(root.join("closed-observed"))
                    .expect("resolve closed observed cwd"),
            )
            .expect("record closed observed cwd");
        let revision_before_reconnect = claims.snapshot().expect("claim snapshot").revision;

        let target_context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let retired_terminal_target = CommandTarget {
            kind: ResourceKind::Terminal,
            handle: serde_json::to_string(&[
                automation_binding_handle_for_generation(
                    &state.binding,
                    &target_context,
                    retired_generation,
                ),
                "live".to_owned(),
                "live-window".to_owned(),
                "live-pane".to_owned(),
                "live-terminal".to_owned(),
            ])
            .expect("serialize retired terminal target"),
            generation: retired_live.terminal.occupant_generation,
        };
        automation
            .publish_terminal_output(
                scope.clone(),
                json!({"source": "test"}),
                retired_terminal_target.clone(),
                json!({"data": "retired"}),
            )
            .expect("publish retired output");

        let mut reconnected_live = live_session;
        set_occupant(&mut reconnected_live, "new");
        backend.set(vec![reconnected_live]);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 1);
        let live_after = directory_claimant_for_pane(
            &claims_context,
            &state.binding,
            "live",
            "live-window",
            "live-pane",
            "live-terminal",
        )
        .expect("live claimant after reconnect");
        assert_ne!(
            live_after.terminal.occupant_generation,
            retired_live.terminal.occupant_generation
        );
        state.binding.automation_generation = Some(retired_generation);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["directory.usage_changed".to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .expect("subscribe")
            .subscription;

        state
            .refresh_automation_event_sources(true)
            .expect("refresh reconnected automation sources");

        let snapshot = claims.snapshot().expect("claim snapshot");
        assert_eq!(snapshot.revision, revision_before_reconnect + 1);
        assert_eq!(snapshot.claims.len(), 1);
        let claim = &snapshot.claims[0];
        assert_eq!(
            claim.source,
            crate::automation::directory::DirectoryClaimSource::Launch
        );
        assert_eq!(claim.directory, launch_directory);
        assert_eq!(claim.session, live_after.session);
        assert_eq!(claim.pane, live_after.pane);
        assert_eq!(claim.terminal, live_after.terminal);
        assert_eq!(claim.since_revision, 1);
        assert!(snapshot.claims.iter().all(|claim| {
            claim.source != crate::automation::directory::DirectoryClaimSource::Observed
        }));
        assert!(snapshot.claims.iter().all(|claim| {
            claim.terminal != retired_live.terminal && claim.terminal != retired_closed.terminal
        }));
        assert!(
            automation
                .events()
                .terminal_output_after(&scope, &retired_terminal_target, 0)
                .is_err()
        );

        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("read directory update");
        assert_eq!(delivery.events.len(), 1);
        let update = &delivery.events[0];
        assert_eq!(update.topic, "directory.usage_changed");
        assert_eq!(update.payload["revision"], json!(snapshot.revision));
        assert_eq!(
            update.target.as_ref().map(|target| target.generation),
            Some(current_generation)
        );

        assert_eq!(
            reconcile_directory_claims_after_authoritative_refresh(
                &claims,
                &claims_context,
                &automation,
                &state.binding,
                &target_context,
            )
            .expect("reconcile refreshed topology"),
            None
        );
        assert_eq!(
            claims.snapshot().expect("claim snapshot").revision,
            snapshot.revision
        );
        assert!(
            automation
                .events()
                .poll(&subscription, &owner, delivery.cursor)
                .expect("read no duplicate update")
                .events
                .is_empty()
        );
    }

    #[test]
    fn vanished_completion_origin_discards_durable_generated_launch_reservations() {
        let mut state = test_state();
        let origin = MuxScope::new(state.active_space_id(), BindingId::from_persistence(99));
        assert!(state.binding_runtime(origin).is_none());
        let session_id = format!("late-completion-{}", unique_test_id());
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let config_path = state.config().config_path.clone();
        let binding_id = origin.binding_id().persistence_value();
        let workspace = WorkspaceStore::for_config_path(&config_path);
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_bindings (id, space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                binding_id,
                state.active_space_id().persistence_value(),
                "Vanished completion",
                "native",
                0_i64
            ],
        )
        .expect("insert vanished completion binding");
        let mut names = SessionNameStore::for_binding(&config_path, binding_id);
        names.remember_generated(&session_id, &cwd, &session_id, &session_id);
        let mut order = SessionOrderStore::for_binding(
            &config_path,
            binding_id,
            BackendConnectionNamespace::new(MultiplexerBackendConfig::Native, None),
        )
        .expect("open session order");
        order
            .add_session(&session_id)
            .expect("persist generated session");

        let command = MuxCommand::CreateSession {
            plan: MuxSessionLaunchPlan {
                session_id: session_id.clone(),
                focus: false,
                default_cwd: cwd,
                environment: std::collections::BTreeMap::new(),
                windows: Vec::new(),
                focused_window: 0,
            },
        };
        let namespace = BackendConnectionNamespace::new(MultiplexerBackendConfig::Native, None);
        let binding_identity = BindingRef {
            window: WindowRef {
                instance: InstanceRef {
                    instance_id: "vanished-completion-test".to_owned(),
                    generation: 0,
                },
                window_id: "vanished-completion-test".to_owned(),
            },
            space_id: origin.space_id().persistence_value().to_string(),
            binding_id: binding_id.to_string(),
            generation: 0,
        };
        let outcome = state.command_outcome_for_mux_result(
            MuxCompletionContext {
                command_id: "late-completion",
                origin,
                binding_identity: &binding_identity,
                binding_generation: 0,
                namespace: &namespace,
                command: &command,
                rename: None,
            },
            Ok(MuxCommandCompletion::default()),
        );

        assert!(matches!(outcome, CommandOutcome::Unavailable { .. }));
        assert_eq!(
            SessionNameStore::for_binding(&config_path, binding_id).display_name(&session_id),
            None
        );
        assert!(
            SessionOrderStore::for_binding(
                &config_path,
                binding_id,
                BackendConnectionNamespace::new(MultiplexerBackendConfig::Native, None),
            )
            .expect("reload session order")
            .session_names()
            .is_empty()
        );
    }

    fn completed_native_launch_fixture()
    -> (AppState, MuxScope, MuxCommand, MuxCommandCompletion, String) {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let origin = state.binding.scope;
        let session_id = format!("persistence-failure-{}", unique_test_id());
        let cwd = state
            .config()
            .config_path
            .parent()
            .expect("test config parent")
            .to_string_lossy()
            .into_owned();
        let descriptor = SessionLaunchDescriptor::simple(session_id.clone(), cwd.clone());
        let plan = AppState::normalize_session_launch_descriptor(&descriptor, false)
            .expect("normalize native launch")
            .mux_plan(session_id.clone());
        let command = MuxCommand::CreateSession { plan };
        state.binding.pending_generated_names.insert(
            session_id.clone(),
            PendingGeneratedName {
                cwd: cwd.clone(),
                name: session_id.clone(),
                display_name: session_id.clone(),
                previous_display_name: None,
            },
        );
        state
            .binding
            .session_names
            .remember_generated(&session_id, &cwd, &session_id, &session_id);
        let config = state.binding.multiplexer.clone();
        let completion = state
            .binding
            .mux
            .execute_command_authoritatively(
                &state.repaint,
                &config,
                command.clone(),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
            .recv_timeout(Duration::from_secs(1))
            .expect("native launch completion")
            .expect("native launch succeeds");
        (state, origin, command, completion, session_id)
    }

    fn persist_fixture_launch_plan(state: &AppState, command: &MuxCommand) {
        let MuxCommand::CreateSession { plan } = command else {
            panic!("launch fixture must be a CreateSession command");
        };
        persist_session_launch_plan(
            &state.config().config_path,
            state.binding.scope.binding_id().persistence_value(),
            plan,
        )
        .expect("persist launch plan");
    }

    #[test]
    fn stale_allocated_create_compensates_before_discarding_plan() {
        let (mut state, origin, command, completion, session_id) =
            completed_native_launch_fixture();
        persist_fixture_launch_plan(&state, &command);
        let binding_identity = state.binding_identity(&state.binding);
        let binding_generation = state.binding.mux.binding_generation();
        let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);

        let outcome = state.command_outcome_for_mux_result(
            MuxCompletionContext {
                command_id: "stale-create",
                origin,
                binding_identity: &binding_identity,
                binding_generation: binding_generation + 1,
                namespace: &namespace,
                command: &command,
                rename: None,
            },
            Ok(completion),
        );

        assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
        assert!(
            load_session_launch_plans(
                &state.config().config_path,
                origin.binding_id().persistence_value()
            )
            .expect("reload launch plans")
            .is_empty(),
            "the plan is removed only after authoritative compensation"
        );
        assert!(
            state
                .binding
                .mux
                .all_sessions()
                .iter()
                .all(|session| session.id != session_id),
            "stale completion compensation must remove the backend allocation"
        );
    }

    #[test]
    fn stale_allocated_create_retains_plan_when_backend_namespace_changed() {
        let (mut state, origin, command, completion, session_id) =
            completed_native_launch_fixture();
        persist_fixture_launch_plan(&state, &command);
        let binding_identity = state.binding_identity(&state.binding);
        let namespace = BackendConnectionNamespace::new(MultiplexerBackendConfig::Tmux, None);

        let outcome = state.command_outcome_for_mux_result(
            MuxCompletionContext {
                command_id: "stale-create-namespace",
                origin,
                binding_identity: &binding_identity,
                binding_generation: state.binding.mux.binding_generation(),
                namespace: &namespace,
                command: &command,
                rename: None,
            },
            Ok(completion),
        );

        assert!(matches!(
            outcome,
            CommandOutcome::Failed { ref code, .. } if code == "completion_indeterminate"
        ));
        assert_eq!(
            load_session_launch_plans(
                &state.config().config_path,
                origin.binding_id().persistence_value()
            )
            .expect("reload launch plans")
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
            vec![session_id.as_str()],
            "the plan remains durable when cleanup cannot target the original backend"
        );
        assert!(
            state
                .binding
                .mux
                .all_sessions()
                .iter()
                .any(|session| session.id == session_id),
            "the allocation remains for authoritative recovery"
        );
    }

    #[test]
    fn stale_allocated_create_retains_plan_when_compensation_fails() {
        let (mut state, origin, command, completion, session_id) =
            completed_native_launch_fixture();
        persist_fixture_launch_plan(&state, &command);
        ScriptedBackend::failing(Vec::new(), "injected stale compensation failure")
            .install(&mut state.binding);
        let binding_identity = state.binding_identity(&state.binding);
        let binding_generation = state.binding.mux.binding_generation();
        let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);

        let outcome = state.command_outcome_for_mux_result(
            MuxCompletionContext {
                command_id: "stale-create-compensation-failure",
                origin,
                binding_identity: &binding_identity,
                binding_generation: binding_generation + 1,
                namespace: &namespace,
                command: &command,
                rename: None,
            },
            Ok(completion),
        );

        assert!(matches!(
            outcome,
            CommandOutcome::Failed { ref code, .. } if code == "completion_indeterminate"
        ));
        assert_eq!(
            load_session_launch_plans(
                &state.config().config_path,
                origin.binding_id().persistence_value()
            )
            .expect("reload launch plans")
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
            vec![session_id.as_str()],
            "a failed compensation must not discard durable recovery intent"
        );
    }

    #[test]
    fn completed_native_launch_persistence_failure_cleans_allocation_and_reservations() {
        let (mut state, origin, command, completion, session_id) =
            completed_native_launch_fixture();
        state.binding.session_order.fail_next_save_for_test();
        let binding_identity = state.binding_identity(&state.binding);
        let binding_generation = state.binding.mux.binding_generation();
        let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);

        let outcome = state.command_outcome_for_mux_result(
            MuxCompletionContext {
                command_id: "test",
                origin,
                binding_identity: &binding_identity,
                binding_generation,
                namespace: &namespace,
                command: &command,
                rename: None,
            },
            Ok(completion),
        );

        assert!(matches!(
            outcome,
            CommandOutcome::Failed { ref code, .. }
                if code == "session_membership_persistence_failed"
        ));
        assert!(
            !state
                .binding
                .pending_generated_names
                .contains_key(&session_id)
        );
        assert_eq!(state.binding.session_names.display_name(&session_id), None);
        assert!(
            !state
                .binding
                .session_order
                .session_names()
                .contains(&session_id)
        );
        assert!(
            state
                .binding
                .mux
                .all_sessions()
                .iter()
                .all(|session| session.id != session_id)
        );
    }

    #[test]
    fn membership_cleanup_failure_does_not_skip_native_allocation_compensation() {
        let (mut state, origin, command, completion, session_id) =
            completed_native_launch_fixture();
        state
            .binding
            .session_order
            .add_session(&session_id)
            .expect("persist allocated session membership");
        state.binding.session_order.fail_next_save_for_test();

        let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);
        let outcome = state.compensate_completed_session_launch(
            origin,
            &namespace,
            &command,
            &completion,
            CommandOutcome::Failed {
                code: "session_membership_persistence_failed".to_owned(),
                message: "injected launch finalization failure".to_owned(),
            },
        );

        assert!(matches!(
            outcome,
            CommandOutcome::Failed { ref code, .. }
                if code == "session_membership_cleanup_failed"
        ));
        assert!(
            state
                .binding
                .session_order
                .session_names()
                .contains(&session_id),
            "the injected failure must reach membership cleanup"
        );
        assert!(
            !state
                .binding
                .pending_generated_names
                .contains_key(&session_id)
        );
        assert_eq!(state.binding.session_names.display_name(&session_id), None);
        assert!(
            state
                .binding
                .mux
                .all_sessions()
                .iter()
                .all(|session| session.id != session_id),
            "backend compensation must run despite the persistence failure"
        );
    }

    #[test]
    fn authoritative_project_and_worktree_launches_record_exact_allocated_directory_claims() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let root = state
            .config()
            .config_path
            .parent()
            .expect("test config has a parent")
            .to_path_buf();
        let unique = unique_test_id();
        state.directory_claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-authoritative-launch-claims-{unique}")),
            ClaimOwner::current(format!("authoritative-launch-{unique}")).expect("claim owner"),
        )
        .expect("isolated directory claims");

        for (kind, cwd) in [
            ("project", root.join("project")),
            ("worktree", root.join("worktree")),
        ] {
            std::fs::create_dir_all(&cwd).expect("create launch cwd");
            let session_id = format!("{kind}-{unique}");
            let cwd = cwd.to_string_lossy().into_owned();
            let command = if kind == "project" {
                MuxCommand::CreateProjectSession {
                    session_id: session_id.clone(),
                    cwd: cwd.clone(),
                }
            } else {
                MuxCommand::CreateWorktreeSession {
                    session_id: session_id.clone(),
                    cwd: cwd.clone(),
                }
            };
            let origin = state.binding.scope;
            let config = state.binding.multiplexer.clone();
            let repaint = state.repaint.clone();
            let result = state
                .binding
                .mux
                .execute_command_authoritatively(
                    &repaint,
                    &config,
                    command.clone(),
                    Instant::now() + Duration::from_secs(1),
                    CommandCancellation::new(),
                )
                .recv_timeout(Duration::from_secs(1))
                .expect("native command completion");
            let allocated = result
                .as_ref()
                .expect("native command succeeds")
                .allocated()
                .cloned()
                .expect("new native session has authoritative allocation");
            let binding_identity = state.binding_identity(&state.binding);
            let binding_generation = state.binding.mux.binding_generation();
            let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);
            let outcome = state.command_outcome_for_mux_result(
                MuxCompletionContext {
                    command_id: "test",
                    origin,
                    binding_identity: &binding_identity,
                    binding_generation,
                    namespace: &namespace,
                    command: &command,
                    rename: None,
                },
                result,
            );
            assert!(matches!(outcome, CommandOutcome::Success { .. }));

            let session = state
                .binding
                .mux
                .all_sessions()
                .iter()
                .find(|session| session.id == session_id)
                .expect("allocated session");
            let window = session.windows.first().expect("allocated window");
            let pane_id = window.anchor.pane_id.as_deref().expect("allocated pane");
            assert_eq!(allocated.session_id, session.id);
            assert_eq!(
                allocated.windows,
                vec![MuxAllocatedWindow {
                    window_id: window.id.clone(),
                    pane_ids: vec![pane_id.to_owned()],
                }]
            );
            let allocated_window = allocated.windows.first().expect("allocated window");
            let allocated_pane_id = allocated_window.pane_ids.first().expect("allocated pane");
            let terminal_id = state
                .binding
                .mux
                .terminal_id_for_pane(
                    &allocated.session_id,
                    &allocated_window.window_id,
                    allocated_pane_id,
                )
                .expect("allocated terminal identity");
            let occupant_generation = state
                .binding
                .mux
                .terminal_generation(
                    &allocated.session_id,
                    &allocated_window.window_id,
                    terminal_id,
                )
                .expect("allocated terminal generation");
            let directory = DirectoryRef::resolve(&cwd).expect("resolve launch cwd");
            let claim = state
                .directory_claims
                .snapshot()
                .expect("directory claim snapshot")
                .claims
                .into_iter()
                .find(|claim| {
                    claim.source == crate::automation::directory::DirectoryClaimSource::Launch
                        && claim.session.session_id == session.id
                })
                .expect("launch claim");
            assert_eq!(claim.directory, directory);
            assert_eq!(claim.session.session_id, allocated.session_id);
            assert_eq!(claim.pane.pane_id, allocated_pane_id.as_str());
            assert_eq!(claim.terminal.terminal_id, terminal_id);
            assert_eq!(claim.terminal.occupant_generation, occupant_generation);
        }
    }

    #[test]
    fn dropped_close_after_rebase_reconciles_claims_against_refreshed_live_slots() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        let unique = unique_test_id();
        state.directory_claims = DirectoryClaims::at(
            std::env::temp_dir().join(format!("bootty-rebase-claims-{unique}")),
            ClaimOwner::current(format!("rebase-claims-{unique}")).expect("claim owner"),
        )
        .expect("isolated directory claims");
        let root = state
            .config()
            .config_path
            .parent()
            .expect("test config has a parent")
            .to_path_buf();
        let cwd = root.display().to_string();
        let live_session = session_with_window_and_pane("live", "live", &cwd);
        let closed_session = session_with_window_and_pane("closed", "closed", &cwd);
        let backend = ScriptedBackend::with(vec![live_session.clone(), closed_session])
            .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 2);

        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: state.command_instance_handle.clone(),
                generation: state.command_instance_generation,
            },
            window_id: state.window_state_key.clone(),
        };
        let live = directory_claimant_for_pane(
            &claims_context,
            &state.binding,
            "live",
            "live-window",
            "live-pane",
            "live-terminal",
        )
        .expect("live claimant");
        let closed = directory_claimant_for_pane(
            &claims_context,
            &state.binding,
            "closed",
            "closed-window",
            "closed-pane",
            "closed-terminal",
        )
        .expect("closed claimant");
        let directory = DirectoryRef::resolve(&root).expect("resolve claim directory");
        state
            .directory_claims
            .record_launch(live.clone(), directory.clone())
            .expect("record live launch");
        state
            .directory_claims
            .observe_cwd(live.clone(), directory.clone())
            .expect("record live cwd");
        state
            .directory_claims
            .record_launch(closed.clone(), directory.clone())
            .expect("record closed launch");
        state
            .directory_claims
            .observe_cwd(closed.clone(), directory)
            .expect("record closed cwd");

        backend.set(vec![live_session]);
        state.binding.mux.refresh_on_next_frame();
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 1);
        let target_context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        assert!(
            reconcile_directory_claims_after_authoritative_refresh(
                &state.directory_claims,
                &claims_context,
                &state.automation,
                &state.binding,
                &target_context,
            )
            .expect("reconcile after rebase")
            .is_some()
        );

        let snapshot = state.directory_claims.snapshot().expect("claim snapshot");
        assert!(
            snapshot
                .claims
                .iter()
                .all(|claim| claim.terminal != closed.terminal)
        );
        assert!(snapshot.claims.iter().any(|claim| {
            claim.terminal == live.terminal
                && claim.source == crate::automation::directory::DirectoryClaimSource::Launch
        }));
        assert!(snapshot.claims.iter().any(|claim| {
            claim.terminal == live.terminal
                && claim.source == crate::automation::directory::DirectoryClaimSource::Observed
        }));
    }

    #[test]
    fn binding_runtimes_isolate_overlapping_layout_progress_and_terminal_target_identity() {
        let mut first = test_binding_runtime(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(10),
        ));
        let mut second = test_binding_runtime(MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(20),
        ));
        let first_window = first.window_id("$1".to_owned(), "@1".to_owned());
        let second_window = second.window_id("$1".to_owned(), "@1".to_owned());
        let first_pane = first.pane_id(first_window.clone(), "%1");
        let second_pane = second.pane_id(second_window.clone(), "%1");
        let first_transition = scoped_terminal_transition_key(
            first.scope,
            MultiplexerBackendConfig::Tmux,
            "$1",
            Some("%1"),
        );
        let second_transition = scoped_terminal_transition_key(
            second.scope,
            MultiplexerBackendConfig::Tmux,
            "$1",
            Some("%1"),
        );

        first
            .terminal_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some("%1".to_owned()),
                TerminalSideEffect::WindowTitle("first title".to_owned()),
            ))
            .expect("send first binding side effect");

        first
            .pane_layouts
            .insert(first_window.clone(), PaneLayout::single("%1".to_owned()));
        second
            .pane_layouts
            .insert(second_window.clone(), PaneLayout::single("%1".to_owned()));
        first.terminal_progress.insert(
            first_pane.clone(),
            TerminalProgress::from_conemu("normal", Some(25)).expect("progress"),
        );
        second.terminal_progress.insert(
            second_pane.clone(),
            TerminalProgress::from_conemu("error", Some(75)).expect("progress"),
        );
        first.mux.set_error(Some("first failed".to_owned()));
        assert_eq!(
            first
                .terminal_side_effect_rx
                .try_recv()
                .expect("first binding side effect")
                .effect,
            TerminalSideEffect::WindowTitle("first title".to_owned())
        );

        assert!(matches!(
            second.terminal_side_effect_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        first.pane_layouts.remove(&first_window);
        first.terminal_progress.remove(&first_pane);

        assert_ne!(first_window, second_window);
        assert_ne!(first_pane, second_pane);
        assert_ne!(first_transition, second_transition);
        assert!(first.pane_layouts.is_empty());
        assert!(first.terminal_progress.is_empty());
        assert!(second.pane_layouts.contains_key(&second_window));
        assert_eq!(second.terminal_progress[&second_pane].percent(), Some(75));
        assert_eq!(first.mux.last_error(), Some("first failed"));
        assert_eq!(second.mux.last_error(), None);
    }

    #[test]
    fn switching_bindings_updates_backend_specific_keybindings_and_render_policy() {
        let mut state = test_state_with_config(|config| {
            config.input.keybind.clear();
            config.input.backend_keybinds.native = vec!["f1=next_tab".to_owned()];
            config.input.backend_keybinds.tmux = vec!["f1=previous_tab".to_owned()];
        });
        assert_eq!(
            state
                .app_key_bindings
                .invocation_for_key_with_modifier_sides(
                    egui::Key::F1,
                    egui::Modifiers::NONE,
                    ModifierSideState::default(),
                )
                .expect("native binding")
                .command,
            "next_tab"
        );
        let remote_scope = MuxScope::new(
            state.binding.scope.space_id(),
            BindingId::from_persistence(
                state
                    .binding
                    .scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        ensure_test_binding(
            &state.config().config_path,
            remote_scope,
            selected_backend(&state.config().multiplexer),
        );

        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        )
        .expect("create remote binding");
        let native_config = remote.multiplexer.clone();
        remote.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &native_config,
        );
        remote.multiplexer.backend = crate::config::MultiplexerBackendConfig::Tmux;
        state.inactive_bindings.push(remote);

        assert!(
            state.activate_scoped_session_from_ui(&ScopedSessionTarget::new(remote_scope, "$1",))
        );

        assert_eq!(
            state
                .app_key_bindings
                .invocation_for_key_with_modifier_sides(
                    egui::Key::F1,
                    egui::Modifiers::NONE,
                    ModifierSideState::default(),
                )
                .expect("tmux binding")
                .command,
            "previous_tab"
        );
        assert_eq!(
            state.multiplexer_backend(),
            crate::config::MultiplexerBackendConfig::Tmux
        );
        assert!(!state.uses_native_terminal_layout());
    }

    #[test]
    fn inactive_binding_refresh_applies_its_own_persisted_session_order() {
        let mut state = test_state();
        let active_order_before = state.binding.session_order.session_names();
        let remote_scope = MuxScope::new(
            state.binding.scope.space_id(),
            BindingId::from_persistence(
                state
                    .binding
                    .scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        let config_path = state.config().config_path.clone();
        let workspace = WorkspaceStore::for_config_path(&config_path);
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_bindings (id, space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                remote_scope.binding_id().persistence_value(),
                remote_scope.space_id().persistence_value(),
                "Inactive order binding",
                "native",
                0_i64,
            ],
        )
        .expect("insert inactive order binding");

        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        )
        .expect("create remote binding");
        let first = format!("inactive-order-a-{}", unique_test_id());
        let second = format!("inactive-order-b-{}", unique_test_id());
        let remote_config = remote.multiplexer.clone();
        for session_id in [&first, &second] {
            remote.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: session_id.clone(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &state.repaint,
                &remote_config,
            );
        }
        assert!(
            remote
                .session_order
                .move_session(&second, -1, [first.as_str(), second.as_str()])
                .expect("persist remote session order")
        );
        let remote_namespace = namespace_for_binding(remote.scope, &remote.multiplexer);
        state.inactive_bindings.push(remote);

        state.update_frame(test_frame_inputs(Vec::new(), None));

        let remote_sessions = state
            .binding_session_groups()
            .into_iter()
            .find(|group| group.scope == remote_scope)
            .expect("remote binding group")
            .sessions;
        let first_index = remote_sessions
            .iter()
            .position(|session| session.id == first)
            .expect("first session");
        let second_index = remote_sessions
            .iter()
            .position(|session| session.id == second)
            .expect("second session");
        assert!(second_index < first_index);
        assert_eq!(
            SessionOrderStore::for_binding(
                &config_path,
                remote_scope.binding_id().persistence_value(),
                remote_namespace,
            )
            .expect("reload remote session order")
            .session_names(),
            vec![second, first],
        );
        assert_eq!(
            SessionOrderStore::for_binding(
                &config_path,
                state.binding.scope.binding_id().persistence_value(),
                namespace_for_binding(state.binding.scope, &state.binding.multiplexer,),
            )
            .expect("reload active session order")
            .session_names(),
            active_order_before,
            "inactive session ordering must not alter the active binding",
        );
    }

    #[test]
    fn scoped_sidebar_navigation_routes_colliding_ids_without_resetting_other_binding() {
        let mut state = test_state();
        state.binding.label = "Local".to_owned();
        let local_scope = state.binding.scope;
        let remote_scope = MuxScope::new(
            local_scope.space_id(),
            BindingId::from_persistence(
                local_scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1000),
            ),
        );
        ensure_test_binding(
            &state.config().config_path,
            remote_scope,
            selected_backend(&state.config().multiplexer),
        );

        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        )
        .expect("create remote binding");
        remote.label = "Remote".to_owned();
        let local_config = state.binding.multiplexer.clone();
        for session_id in ["$1", "$2"] {
            state.binding.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: session_id.to_owned(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &state.repaint,
                &local_config,
            );
        }
        state.binding.mux.activate_session("$2");
        let remote_config = remote.multiplexer.clone();
        remote.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &remote_config,
        );
        state.inactive_bindings.push(remote);

        let groups = state.binding_session_groups();
        assert_eq!(groups.len(), 2);
        assert!(
            groups
                .iter()
                .all(|group| group.sessions.iter().any(|session| session.id == "$1"))
        );
        assert!(
            groups
                .iter()
                .find(|group| group.scope == local_scope)
                .is_some_and(|group| group.can_return_to_last_session)
        );
        assert!(
            groups
                .iter()
                .find(|group| group.scope == remote_scope)
                .is_some_and(|group| !group.can_return_to_last_session)
        );

        let remote_target = ScopedSessionTarget::new(remote_scope, "$1");
        assert!(state.activate_scoped_session_from_ui(&remote_target));
        assert_eq!(state.mux_scope(), remote_scope);
        assert_eq!(state.mux().selected_session(), Some("$1"));
        let local = state
            .inactive_bindings
            .iter()
            .find(|binding| binding.scope == local_scope)
            .expect("local binding remains live");
        assert_eq!(local.mux.selected_session(), Some("$2"));

        let targets = state.session_navigation_targets();
        let remote_index = targets
            .iter()
            .position(|target| target == &remote_target)
            .expect("remote target is keyboard navigable");
        let previous_index = (remote_index + targets.len() - 1) % targets.len();
        state.sidebar_hovered_session = Some(targets[previous_index].clone());
        state.move_sidebar_hover(1);
        assert_eq!(state.sidebar_hovered_session.as_ref(), Some(&remote_target));
        assert!(state.activate_sidebar_hovered_session());
        assert_eq!(state.mux_scope(), remote_scope);
    }

    #[test]
    fn startup_restores_all_bindings_for_grouped_navigation() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-multi-binding-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let space_id = workspace
            .binding()
            .expect("default binding")
            .mux_scope()
            .space_id()
            .persistence_value();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![space_id, "Default Binding", "native", 0_i64],
        )
        .expect("insert remote binding");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});

        let state = AppState::new(config, repaint, None, None).expect("state");
        let groups = state.binding_session_groups();

        assert_eq!(state.binding_count(), 2);
        assert_eq!(groups.len(), 2);
        assert!(groups[0].label.starts_with("Default Binding / Binding "));
        assert!(groups[1].label.starts_with("Default Binding / Binding "));
        assert_ne!(groups[0].label, groups[1].label);
        assert!(groups[0].active);
        assert!(!groups[1].active);
    }

    #[test]
    fn creating_space_activates_it_and_survives_state_recreation() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-create-space-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config.clone(), repaint.clone(), None, None).expect("state");
        let first_space = state.active_space_id();

        assert!(!state.create_space_from_ui(
            "   ",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        assert!(state.create_space_from_ui(
            "Review",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let review_space = state.active_space_id();
        assert_ne!(review_space, first_space);
        assert_eq!(state.mux_scope().space_id(), review_space);
        assert_eq!(
            state
                .space_summaries()
                .iter()
                .map(|space| (space.name.as_str(), space.active))
                .collect::<Vec<_>>(),
            vec![("Default Space", false), ("Review", true)]
        );

        drop(state);
        let reopened = AppState::new(config, repaint, None, None).expect("reopened state");
        assert_eq!(
            reopened
                .space_summaries()
                .iter()
                .map(|space| space.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Default Space", "Review"]
        );
        assert_eq!(reopened.active_space_id(), review_space);
        assert_eq!(reopened.mux_scope().space_id(), review_space);
    }

    #[test]
    fn space_editor_events_create_and_edit_persist_through_recreation() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-space-edit-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config.clone(), repaint.clone(), None, None).expect("state");
        let default_space = state.active_space_id();

        state.apply_space_editor_event(
            SpaceEditorDialog::new_space(
                "phosphor:alarm".to_owned(),
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Native),
                    remote: SpaceRemoteOverride::Inherit,
                },
            ),
            SpaceEditorEvent::Save {
                space_id: None,
                name: "Review".to_owned(),
                icon: "terminal".to_owned(),
                color: [1, 2, 3],
                tint_sidebar: true,
                mux: SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Rmux),
                    remote: SpaceRemoteOverride::Inherit,
                },
            },
        );
        let review_space = state.active_space_id();
        assert_eq!(state.multiplexer_backend(), MultiplexerBackendConfig::Rmux);
        state.apply_space_editor_event(
            SpaceEditorDialog::edit_space(
                review_space,
                "Review".to_owned(),
                "terminal".to_owned(),
                [1, 2, 3],
                true,
                SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Rmux),
                    remote: SpaceRemoteOverride::Inherit,
                },
            ),
            SpaceEditorEvent::Save {
                space_id: Some(review_space),
                name: "Planning".to_owned(),
                icon: "calendar".to_owned(),
                color: [4, 5, 6],
                tint_sidebar: false,
                mux: SpaceMuxOverride {
                    backend: Some(MultiplexerBackendConfig::Zellij),
                    remote: SpaceRemoteOverride::Inherit,
                },
            },
        );
        assert_eq!(
            state
                .space_summaries()
                .iter()
                .find(|space| space.id == review_space)
                .map(|space| {
                    (
                        space.name.as_str(),
                        space.icon.as_str(),
                        space.color,
                        space.tint_sidebar,
                    )
                }),
            Some(("Planning", "calendar", [4, 5, 6], false))
        );
        assert_eq!(
            state.multiplexer_backend(),
            MultiplexerBackendConfig::Zellij
        );

        drop(state);
        let mut reopened = AppState::new(config, repaint, None, None).expect("reopened state");
        assert_eq!(
            reopened
                .space_summaries()
                .iter()
                .find(|space| space.id == review_space)
                .map(|space| {
                    (
                        space.name.as_str(),
                        space.icon.as_str(),
                        space.color,
                        space.tint_sidebar,
                    )
                }),
            Some(("Planning", "calendar", [4, 5, 6], false))
        );
        assert_eq!(reopened.active_space_id(), review_space);
        assert_eq!(
            reopened.multiplexer_backend(),
            MultiplexerBackendConfig::Zellij
        );
        let claims = DirectoryClaims::at(
            config_dir.join("close-space-claims"),
            ClaimOwner::current(format!("close-space-{unique}")).expect("claim owner"),
        )
        .expect("isolated claims");
        reopened.directory_claims = claims.clone();
        let claims_context = DirectoryClaimsContext {
            instance: InstanceRef {
                instance_id: reopened.command_instance_handle.clone(),
                generation: reopened.command_instance_generation,
            },
            window_id: reopened.window_state_key.clone(),
        };
        let claimant = directory_claimant_for_pane_at_generation(
            &claims_context,
            &reopened.binding,
            "review-session",
            "review-pane",
            "review-terminal",
            reopened.binding.mux.binding_generation(),
            1,
        );
        claims
            .record_launch(
                claimant,
                DirectoryRef::resolve(&config_dir).expect("claim directory"),
            )
            .expect("record Space claim");
        assert!(reopened.close_space_from_ui(review_space));
        assert!(
            claims
                .snapshot()
                .expect("claims after Space close")
                .claims
                .is_empty(),
            "closing a Space must release its binding claims"
        );
        assert_eq!(reopened.active_space_id(), default_space);
        assert!(!reopened.close_space_from_ui(default_space));
    }

    #[test]
    fn space_lifecycle_refuses_started_command_transition() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        assert!(state.create_space_from_ui(
            "Second",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let target = state.active_space_id();
        let origin = state.binding.scope;
        let binding_identity = state.binding_identity(&state.binding);
        let binding_generation = state.binding.mux.binding_generation();
        let namespace = namespace_for_binding(state.binding.scope, &state.binding.multiplexer);
        let command = MuxCommand::CreateSession {
            plan: MuxSessionLaunchPlan {
                session_id: "in-flight".to_owned(),
                focus: false,
                default_cwd: "/tmp".to_owned(),
                environment: BTreeMap::new(),
                windows: Vec::new(),
                focused_window: 0,
            },
        };
        let cancellation = CommandCancellation::new();
        assert!(cancellation.try_start());
        let (result_sender, result) = mpsc::channel();
        state.pending_app_commands.push(PendingAppCommand {
            request_id: 1,
            command,
            command_id: "in-flight".to_owned(),
            origin,
            binding_identity,
            binding_generation,
            namespace,
            target: None,
            deadline: Instant::now() + Duration::from_secs(30),
            cancellation: cancellation.clone(),
            response: None,
            completion: None,
            rename: None,
            result,
        });
        let before = state.space_summaries();

        assert!(!state.close_space_from_ui(target));
        assert_eq!(state.active_space_id(), target);
        assert_eq!(state.space_summaries(), before);
        assert!(cancellation.is_cancel_requested());
        assert!(
            state
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("commands_in_flight"))
        );

        assert!(state.update_space_from_ui(
            target,
            "Changed",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
        ));
        let after_rename = state.space_summaries();
        assert_eq!(
            after_rename
                .iter()
                .find(|space| space.id == target)
                .map(|space| space.name.as_str()),
            Some("Changed")
        );
        assert_eq!(state.pending_app_commands.len(), 1);
        assert!(!state.update_space_from_ui(
            target,
            "Backend Attempt",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inherit,
            },
        ));
        assert_eq!(state.space_summaries(), after_rename);
        result_sender
            .send(Err(MuxCommandError::Cancelled))
            .expect("deliver old-backend cancellation");
        assert!(state.drain_pending_app_commands(Instant::now()));
        assert!(
            state.pending_app_commands.is_empty(),
            "the transition may retry only after terminal reconciliation"
        );
        assert!(state.update_space_from_ui(
            target,
            "Changed",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inherit,
            },
        ));
        assert_eq!(
            state
                .space_summaries()
                .iter()
                .find(|space| space.id == target)
                .map(|space| space.name.as_str()),
            Some("Changed")
        );
        assert!(
            state.close_space_from_ui(target),
            "a reconciled binding must be closable"
        );
        assert!(
            state
                .space_summaries()
                .iter()
                .all(|space| space.id != target),
            "close must remove the reconciled Space"
        );
        assert_ne!(
            state.active_space_id(),
            target,
            "close must leave the surviving Space active"
        );
    }

    /// A dropped connection has to reconnect, not close: the sessions are on the other host, and
    /// closing the pane sends the backend a kill that would destroy work the user still has. The
    /// pane's target survives so the next sync attaches the same session again.
    #[test]
    fn a_lost_remote_connection_reconnects_instead_of_killing_the_pane() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-reattach-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");
        let space = state.active_space_id();
        assert!(state.update_space_from_ui(
            space,
            "Remote",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inline(SshRemoteConfig::for_host("devbox")),
            },
        ));
        let now = Instant::now();

        state.handle_attach_client_exit(now);

        let reattach = state
            .binding
            .reattach
            .expect("a lost connection schedules a reconnect");
        assert_eq!(reattach.attempts, 1);
        assert!(!reattach.started);
        assert!(reattach.retry_at > now);
        assert!(
            state
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("devbox"))
        );
        assert!(
            state.space_summaries()[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("devbox"))
        );
        assert!(state.reconnect_space_from_ui(space));
        let manual = state.binding.reattach.expect("manual reconnect");
        assert!(manual.started);
        assert_eq!(manual.attempts, 1);

        state.resolve_remote_attach_exit_after_refresh(true);

        assert!(
            state.binding.reattach.is_none(),
            "an empty successful snapshot means the session ended normally"
        );
        assert!(state.binding.mux.last_error().is_none());
        assert!(state.last_error.is_none());
    }

    /// Backoff grows while one outage keeps ending clients, and starts over once a connection has
    /// lasted — otherwise a host that drops out for an hour would still be waiting the maximum
    /// delay the next time it blips, long after it came back.
    #[test]
    fn reconnect_backoff_grows_during_an_outage_and_resets_after_a_connection_lasts() {
        let now = Instant::now();
        let first = RemoteReattach::after_failure(None, None, now);
        let second = RemoteReattach::after_failure(Some(first), Some(Duration::from_secs(1)), now);
        let third = RemoteReattach::after_failure(Some(second), Some(Duration::from_secs(1)), now);

        assert_eq!((first.attempts, second.attempts, third.attempts), (1, 2, 3));
        assert!(RemoteReattach::delay(1) < RemoteReattach::delay(2));
        assert!(RemoteReattach::delay(2) < RemoteReattach::delay(3));
        assert_eq!(RemoteReattach::delay(99), RemoteReattach::MAX_DELAY);

        let after_a_long_session = RemoteReattach::after_failure(
            Some(third),
            Some(RemoteReattach::STABLE_AFTER + Duration::from_secs(1)),
            now,
        );
        assert_eq!(after_a_long_session.attempts, 1);
    }

    #[test]
    fn network_address_change_resets_only_after_the_poll_interval() {
        let now = Instant::now();
        let first = Some("10.0.0.1".parse().expect("address"));
        let second = Some("192.168.1.7".parse().expect("address"));
        let mut detector = NetworkChangeDetector {
            next_check: now,
            signature: first,
        };

        assert!(!detector.changed_to(now, first));
        assert!(!detector.changed_to(now + Duration::from_secs(1), second));
        assert!(detector.changed_to(now + Duration::from_secs(2), second));
    }

    /// A space's host reaches the binding that attaches it, and stops being carried the moment the
    /// space moves to a backend that keeps its terminals in this process — otherwise the binding
    /// would hold a host it can never dial while rendering local shells.
    #[test]
    fn a_space_carries_its_host_only_while_its_backend_can_reach_one() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-space-remote-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");
        let space = state.active_space_id();
        let remote = SshRemoteConfig::for_host("devbox");

        assert!(state.update_space_from_ui(
            space,
            "Remote",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Inline(remote.clone()),
            },
        ));
        assert_eq!(state.binding.multiplexer.remote.as_ref(), Some(&remote));

        assert!(state.update_space_from_ui(
            space,
            "Remote",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Inline(remote),
            },
        ));
        assert_eq!(state.binding.multiplexer.remote, None);
    }

    #[test]
    fn remote_space_reference_resolves_its_named_profile() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-profile-space-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let profile = SshProfileConfig {
            name: "Lab".to_owned(),
            host: "lab-host".to_owned(),
            user: Some("developer".to_owned()),
            port: Some(2222),
            authentication: Default::default(),
            host_key_policy: Default::default(),
            identity_file: None,
            proxy_jump: None,
            program: "ssh".to_owned(),
            args: Vec::new(),
        };
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config
            .ssh_profiles
            .insert("lab".to_owned(), profile.clone());
        let mut changed = config.clone();
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");

        assert!(state.update_space_from_ui(
            state.active_space_id(),
            "Remote",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Tmux),
                remote: SpaceRemoteOverride::Profile(crate::workspace::RemoteSpaceRef {
                    profile_id: "lab".to_owned(),
                    remote_space_id: "remote-7".to_owned(),
                    remote_space_name: "Production".to_owned(),
                    backend: MultiplexerBackendConfig::Tmux,
                }),
            },
        ));
        assert_eq!(state.binding.multiplexer.remote, Some(profile.to_remote()));
        assert_eq!(
            state.binding.multiplexer.remote_space_id,
            Some("remote-7".to_owned())
        );
        assert_eq!(
            state.binding.multiplexer.backend,
            MultiplexerBackendConfig::Tmux
        );
        changed.ssh_profiles.get_mut("lab").unwrap().host = "new-lab-host".to_owned();
        state.rebuild_profile_bindings(&changed).unwrap();
        assert_eq!(
            state
                .binding
                .multiplexer
                .remote
                .as_ref()
                .map(|remote| remote.host.as_str()),
            Some("new-lab-host")
        );

        changed.ssh_profiles.remove("lab");
        state.rebuild_profile_bindings(&changed).unwrap();
        assert_eq!(state.binding.multiplexer.remote, None);
        assert_eq!(
            state.binding.degraded_error().as_deref(),
            Some("SSH profile 'lab' is unavailable")
        );
    }

    #[test]
    fn inherited_space_backend_resolves_the_current_global_backend_after_restart() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-inherit-space-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state =
            AppState::new(config.clone(), repaint.clone(), None, None).expect("native state");
        let default_space = state.active_space_id();

        assert!(state.create_space_from_ui(
            "Override",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let override_space = state.active_space_id();
        assert!(state.update_space_from_ui(
            override_space,
            "Override",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride {
                backend: Some(MultiplexerBackendConfig::Native),
                remote: SpaceRemoteOverride::Inherit,
            },
        ));
        drop(state);

        config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        let mut reopened = AppState::new(config, repaint, None, None).expect("tmux state");
        assert_eq!(reopened.active_space_id(), override_space);
        assert_eq!(
            reopened.multiplexer_backend(),
            MultiplexerBackendConfig::Native
        );
        assert!(reopened.activate_space_from_ui(default_space));
        assert_eq!(
            reopened.multiplexer_backend(),
            MultiplexerBackendConfig::Tmux
        );
        assert!(reopened.activate_space_from_ui(override_space));
        assert_eq!(
            reopened.multiplexer_backend(),
            MultiplexerBackendConfig::Native
        );
        assert!(reopened.update_space_from_ui(
            override_space,
            "Override",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
            SpaceMuxOverride::default(),
        ));
        assert_eq!(
            reopened.multiplexer_backend(),
            MultiplexerBackendConfig::Tmux
        );
    }

    #[test]
    fn native_sessions_rebuild_from_binding_metadata_without_cross_space_adoption() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-native-restore-{unique}"));
        let cwd = config_dir.join("shared");
        std::fs::create_dir_all(&cwd).expect("create shared cwd");
        let mut config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        config.multiplexer.backend = MultiplexerBackendConfig::Native;
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state =
            AppState::new(config.clone(), repaint.clone(), None, None).expect("native state");
        let first_space = state.active_space_id();
        state.create_project_session_for_cwd(cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        let first_session = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| Some(session.id.as_str()) == state.binding.mux.selected_session())
            .expect("selected first session")
            .clone();

        assert!(state.create_space_from_ui(
            "Second",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let second_space = state.active_space_id();
        state.create_project_session_for_cwd(cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        let second_session = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| Some(session.id.as_str()) == state.binding.mux.selected_session())
            .expect("selected second session")
            .clone();
        assert_ne!(first_session.id, second_session.id);
        drop(state);

        let mut native = NativeBackend::for_workspace(&config.config_path);
        for session_id in [&first_session.id, &second_session.id] {
            native
                .execute(MuxCommand::DitchSession {
                    session_id: session_id.clone(),
                })
                .expect("clear process-local native session");
        }

        let mut reopened = AppState::new(config, repaint, None, None).expect("restored state");
        assert_eq!(
            reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec![second_session.id.as_str()]
        );
        assert!(reopened.activate_space_from_ui(first_space));
        assert_eq!(
            reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec![first_session.id.as_str()]
        );
        assert!(reopened.activate_space_from_ui(second_space));
    }

    #[test]
    fn space_transition_progresses_deterministically() {
        let started = Instant::now();
        let transition = SpaceTransition {
            from: SpaceId::from_persistence(1),
            to: SpaceId::from_persistence(2),
            started,
        };

        assert_eq!(transition.progress_at(started), 0.0);
        assert!(
            (transition.progress_at(started + SpaceTransition::DURATION / 2) - 0.5).abs() < 0.01
        );
        assert_eq!(
            transition.progress_at(started + SpaceTransition::DURATION * 2),
            1.0
        );
    }

    #[test]
    fn empty_new_space_ignores_shared_backend_sessions_after_refresh_and_recreation() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-empty-space-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config.clone(), repaint.clone(), None, None).expect("state");

        let session_cwd = config_dir.join("existing-session");
        std::fs::create_dir_all(&session_cwd).expect("create existing session directory");
        state.create_project_session_for_cwd(session_cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        state.sync_session_order();

        assert!(state.create_space_from_ui(
            "Empty",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let empty_space = state.active_space_id();
        state.update_frame(test_frame_inputs(Vec::new(), None));
        assert!(state.binding.mux.sessions().is_empty());

        drop(state);
        let mut reopened = AppState::new(config, repaint, None, None).expect("reopened state");
        assert_eq!(reopened.active_space_id(), empty_space);
        reopened.update_frame(test_frame_inputs(Vec::new(), None));
        assert!(reopened.binding.mux.sessions().is_empty());
    }

    #[test]
    fn space_actions_follow_order_without_wrapping() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-space-actions-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let first_space = workspace.spaces()[0].id();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 2)",
            ["Last Space"],
        )
        .expect("insert last space");
        let last_space = SpaceId::from_persistence(conn.last_insert_rowid());
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Middle Space"],
        )
        .expect("insert middle space");
        let middle_space = SpaceId::from_persistence(conn.last_insert_rowid());
        for (space_id, name) in [
            (middle_space, "Middle Binding"),
            (last_space, "Last Binding"),
        ] {
            conn.execute(
                "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![space_id.persistence_value(), name, "native", 0_i64],
            )
            .expect("insert space binding");
        }

        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");
        let mut effects = Vec::new();
        assert_eq!(
            state
                .space_summaries()
                .into_iter()
                .map(|space| space.id)
                .collect::<Vec<_>>(),
            vec![first_space, middle_space, last_space]
        );
        state.apply_keybind_action(
            KeybindAction::App(AppAction::EditSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert!(state.take_space_editor_dialog().is_some());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::PreviousSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert_eq!(state.active_space_id(), first_space);

        state.apply_keybind_action(
            KeybindAction::App(AppAction::NextSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert_eq!(state.active_space_id(), middle_space);
        state.apply_keybind_action(
            KeybindAction::App(AppAction::NextSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert_eq!(state.active_space_id(), last_space);
        state.apply_keybind_action(
            KeybindAction::App(AppAction::NextSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert_eq!(state.active_space_id(), last_space);
        state.apply_keybind_action(
            KeybindAction::App(AppAction::PreviousSpace),
            ViewportSnapshot::default(),
            &mut effects,
        );
        assert_eq!(state.active_space_id(), middle_space);
    }

    #[test]
    fn switching_spaces_replaces_the_full_window_binding_context() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-multi-space-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let first_space = workspace.spaces()[0].id();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Review Space"],
        )
        .expect("insert second space");
        let second_space = SpaceId::from_persistence(conn.last_insert_rowid());
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                second_space.persistence_value(),
                "Review Binding",
                "native",
                0_i64
            ],
        )
        .expect("insert second space binding");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let other_window = AppState::new_for_window(
            config.clone(),
            "window-a".to_owned(),
            repaint.clone(),
            None,
            None,
        )
        .expect("other state");
        let mut state = AppState::new_for_window(
            config.clone(),
            "window-b".to_owned(),
            repaint.clone(),
            None,
            None,
        )
        .expect("state");
        let first_scope = state.binding.scope;
        let first_config = state.binding.multiplexer.clone();
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &first_config,
        );
        state
            .binding
            .session_order
            .add_session("$1")
            .expect("record first Space session");
        let second_runtime = state
            .inactive_spaces
            .iter_mut()
            .find(|space| space.id == second_space)
            .expect("second space runtime");
        let second_scope = second_runtime.binding.scope;
        let second_config = second_runtime.binding.multiplexer.clone();
        second_runtime.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$2".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &second_config,
        );
        second_runtime
            .binding
            .session_order
            .add_session("$2")
            .expect("record second Space session");
        second_runtime
            .binding
            .terminal_side_effect_tx
            .send(TerminalSideEffectEvent::new(None, TerminalSideEffect::Bell))
            .expect("queue inactive Space side effect");

        let spaces = state.space_summaries();
        assert_eq!(spaces.len(), 2);
        assert_eq!(spaces[0].id, first_space);
        assert_eq!(spaces[0].name, "Default Space");
        assert!(spaces[0].active);
        assert_eq!(spaces[1].id, second_space);
        assert_eq!(spaces[1].name, "Review Space");
        assert!(!spaces[1].active);
        assert!(
            state
                .binding_session_groups()
                .iter()
                .all(|group| group.scope.space_id() == first_space)
        );
        assert_eq!(state.binding_session_groups()[0].scope, first_scope);
        assert!(
            state.binding_session_groups()[0]
                .sessions
                .iter()
                .any(|session| session.id == "$1")
        );

        assert!(state.open_ditch_session_dialog_for("$1"));
        assert!(state.ditch_session_dialog.is_some());
        assert!(state.activate_space_from_ui(second_space));
        assert!(state.ditch_session_dialog.is_none());
        state
            .binding
            .terminal_side_effect_tx
            .send(TerminalSideEffectEvent::new(None, TerminalSideEffect::Bell))
            .expect("queue active Space side effect");
        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, AppEffect::Bell))
                .count(),
            1,
            "inactive Space side effects must not replay after activation"
        );

        assert_eq!(state.active_space_id(), second_space);
        assert!(
            state
                .binding_session_groups()
                .iter()
                .all(|group| group.scope.space_id() == second_space)
        );
        assert_eq!(state.binding_session_groups()[0].scope, second_scope);
        assert!(
            state.binding_session_groups()[0]
                .sessions
                .iter()
                .any(|session| session.id == "$2")
        );
        assert_eq!(other_window.active_space_id(), first_space);
        let persisted = WorkspaceStore::try_for_config_path(&config.config_path)
            .expect("reopen workspace selection");
        assert_eq!(
            persisted.selected_space("window-a").expect("window a"),
            Some(first_space)
        );
        assert_eq!(
            persisted.selected_space("window-b").expect("window b"),
            Some(second_space)
        );
        assert!(state.binding.mux.poll_command().is_none());
        assert!(
            state
                .inactive_spaces
                .iter_mut()
                .find(|space| space.id == first_space)
                .expect("first Space remains available")
                .bindings_mut()
                .all(|binding| binding.mux.poll_command().is_none())
        );
        assert_eq!(state.binding_count(), 1);
    }

    #[test]
    fn spaces_filter_shared_backend_sessions_by_persisted_binding_membership() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-space-membership-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let first_space = workspace.spaces()[0].id();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "UPDATE workspace_bindings SET backend = 'native' WHERE space_id = ?1",
            [first_space.persistence_value()],
        )
        .expect("make first Space native");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Second Space"],
        )
        .expect("insert second space");
        let second_space = SpaceId::from_persistence(conn.last_insert_rowid());
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                second_space.persistence_value(),
                "Second Space Binding",
                "native",
                0_i64
            ],
        )
        .expect("insert second Space binding");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config.clone(), repaint.clone(), None, None).expect("state");

        let shared_cwd = config_dir.join("shared");
        std::fs::create_dir_all(&shared_cwd).expect("create shared session directory");
        state.create_project_session_for_cwd(shared_cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        state.sync_session_order();
        let first_name = state.binding.mux.sessions()[0].name.clone();

        assert!(state.activate_space_from_ui(second_space));
        state.create_project_session_for_cwd(shared_cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        state.create_project_session_for_cwd(shared_cwd.to_string_lossy().into_owned());
        await_authoritative_commands(&mut state);
        state.sync_session_order();
        let second_names = state
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.name.clone())
            .collect::<Vec<_>>();

        assert_eq!(second_names.len(), 2);
        assert_ne!(second_names[0], second_names[1]);
        assert!(second_names.iter().all(|name| name != &first_name));

        drop(state);
        let mut reopened = AppState::new(config, repaint, None, None).expect("reopened state");
        for _ in 0..100 {
            reopened.update_frame(test_frame_inputs(Vec::new(), None));
            let visible_names = reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.clone())
                .collect::<Vec<_>>();
            if visible_names == second_names {
                break;
            }
        }
        assert_eq!(
            reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.clone())
                .collect::<Vec<_>>(),
            second_names
        );

        assert!(reopened.activate_space_from_ui(first_space));
        for _ in 0..100 {
            reopened.update_frame(test_frame_inputs(Vec::new(), None));
            if reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .eq([first_name.as_str()])
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            reopened
                .binding
                .mux
                .sessions()
                .iter()
                .map(|session| session.name.as_str())
                .collect::<Vec<_>>(),
            vec![first_name.as_str()]
        );
    }

    #[test]
    fn native_terminal_owner_survives_space_switches_through_non_native_backend() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-native-space-owner-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let first_space = workspace.spaces()[0].id();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "UPDATE workspace_bindings SET backend = 'native' WHERE space_id = ?1",
            [first_space.persistence_value()],
        )
        .expect("make first space native");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 1)",
            ["Remote Space"],
        )
        .expect("insert non-native space");
        let remote_space = SpaceId::from_persistence(conn.last_insert_rowid());
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                remote_space.persistence_value(),
                "Remote Binding",
                "rmux",
                0_i64
            ],
        )
        .expect("insert non-native binding");
        conn.execute(
            "INSERT INTO workspace_spaces (name, position) VALUES (?1, 2)",
            ["Second Native Space"],
        )
        .expect("insert second native space");
        let second_native_space = SpaceId::from_persistence(conn.last_insert_rowid());
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                second_native_space.persistence_value(),
                "Second Native Binding",
                "native",
                0_i64
            ],
        )
        .expect("insert second native binding");
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");
        let native_terminal = std::ptr::from_ref(state.binding.terminal.as_ref());
        let native_side_effect_tx = state.binding.terminal_side_effect_tx.clone();
        let first_scope = state.binding.scope;
        let first_config = state.binding.multiplexer.clone();
        state
            .binding
            .session_order
            .add_session("$1")
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &first_config,
        );
        let first_anchor = state
            .binding
            .mux
            .selected_session_anchor()
            .expect("first native Space anchor")
            .clone();
        let repaint = state.repaint.clone();
        let (second_scope, second_anchor) = {
            let second_runtime = state
                .inactive_spaces
                .iter_mut()
                .find(|space| space.id == second_native_space)
                .expect("second native Space runtime");
            let second_config = second_runtime.binding.multiplexer.clone();
            second_runtime
                .binding
                .session_order
                .add_session("$1")
                .expect("persist session order");
            second_runtime.binding.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: "$1".to_owned(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &repaint,
                &second_config,
            );
            (
                second_runtime.binding.scope,
                second_runtime
                    .binding
                    .mux
                    .selected_session_anchor()
                    .expect("second native Space anchor")
                    .clone(),
            )
        };
        assert_eq!(first_anchor.session_id, second_anchor.session_id);
        assert_eq!(first_anchor.pane_id, second_anchor.pane_id);
        state
            .sync_terminal_panes()
            .expect("sync first native Space terminal");
        assert_eq!(state.binding.terminal.active_mux_scope(), Some(first_scope));

        assert!(state.activate_space_from_ui(remote_space));
        native_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some("%1".to_owned()),
                TerminalSideEffect::WindowTitle("inactive native owner".to_owned()),
            ))
            .expect("send inactive native side effect");
        state.update_frame(test_frame_inputs(Vec::new(), None));
        assert!(state.activate_space_from_ui(second_native_space));
        assert_eq!(
            state.binding.terminal.active_mux_scope(),
            Some(second_scope),
            "colliding native IDs must retarget to the selected Space scope"
        );
        native_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some(crate::mux::terminal::encode_scoped_pane_id(
                    first_scope,
                    "%1",
                )),
                TerminalSideEffect::Bell,
            ))
            .expect("send inactive scoped side effect");
        native_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some(crate::mux::terminal::encode_scoped_pane_id(
                    second_scope,
                    "%1",
                )),
                TerminalSideEffect::Bell,
            ))
            .expect("send active scoped side effect");
        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| matches!(effect, AppEffect::Bell))
                .count(),
            1,
            "only side effects from the selected native Space may reach the host"
        );
        assert!(
            state.binding.terminal_side_effect_rx.try_recv().is_err(),
            "inactive native side effects must not leak into the newly active Space"
        );

        assert_eq!(
            std::ptr::from_ref(state.binding.terminal.as_ref()),
            native_terminal,
            "the single native terminal must follow the active native Space"
        );

        assert!(state.activate_space_from_ui(first_space));
        assert_eq!(state.binding.terminal.active_mux_scope(), Some(first_scope));
        assert_eq!(
            std::ptr::from_ref(state.binding.terminal.as_ref()),
            native_terminal,
            "direct native Space switches must retain the same terminal owner"
        );
        native_side_effect_tx
            .send(TerminalSideEffectEvent::new(
                Some("%1".to_owned()),
                TerminalSideEffect::WindowTitle("native owner".to_owned()),
            ))
            .expect("send native side effect after Space switches");
        let mut title_delivered = false;
        for _ in 0..64 {
            match state.binding.terminal_side_effect_rx.try_recv() {
                Ok(TerminalSideEffectEvent {
                    effect: TerminalSideEffect::WindowTitle(title),
                    ..
                }) if title == "native owner" => {
                    title_delivered = true;
                    break;
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => {
                    panic!("native title side effect was not delivered");
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("native title side-effect channel disconnected");
                }
            }
        }
        assert!(
            title_delivered,
            "native title side effect was not delivered before the bounded drain was exhausted"
        );
    }

    #[test]
    fn native_terminal_owner_survives_binding_switch_within_space() {
        let unique = unique_test_id();
        let config_dir = std::env::temp_dir().join(format!("bootty-native-binding-owner-{unique}"));
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        let config = BoottyConfig {
            config_path: config_dir.join("config.toml"),
            ..BoottyConfig::default()
        };
        let workspace = WorkspaceStore::for_config_path(&config.config_path);
        let space_id = workspace.spaces()[0].id();
        let conn = crate::workspace::open_db(workspace.path()).expect("open workspace database");
        conn.execute(
            "UPDATE workspace_bindings SET backend = 'native' WHERE space_id = ?1",
            [space_id.persistence_value()],
        )
        .expect("make first binding native");
        conn.execute(
            "INSERT INTO workspace_bindings (space_id, name, backend, hide_tmux_status)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                space_id.persistence_value(),
                "Other Native",
                "native",
                0_i64
            ],
        )
        .expect("insert second native binding");
        let other_binding = BindingId::from_persistence(conn.last_insert_rowid());
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");
        let native_terminal = std::ptr::from_ref(state.binding.terminal.as_ref());
        let first_scope = state.binding.scope;
        let first_config = state.binding.multiplexer.clone();
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "$1".to_owned(),
                cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            },
            &state.repaint,
            &first_config,
        );
        let first_anchor = state
            .binding
            .mux
            .selected_session_anchor()
            .expect("first native binding anchor")
            .clone();
        let repaint = state.repaint.clone();
        let (second_scope, second_anchor) = {
            let second = state
                .inactive_bindings
                .iter_mut()
                .find(|binding| binding.scope.binding_id() == other_binding)
                .expect("second native binding runtime");
            let second_config = second.multiplexer.clone();
            second.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: "$1".to_owned(),
                    cwd: std::env::temp_dir().to_string_lossy().into_owned(),
                },
                &repaint,
                &second_config,
            );
            (
                second.scope,
                second
                    .mux
                    .selected_session_anchor()
                    .expect("second native binding anchor")
                    .clone(),
            )
        };
        assert_eq!(first_anchor.session_id, second_anchor.session_id);
        assert_eq!(first_anchor.pane_id, second_anchor.pane_id);
        state
            .sync_terminal_panes()
            .expect("sync first native binding terminal");
        assert_eq!(state.binding.terminal.active_mux_scope(), Some(first_scope));
        let target = ScopedSessionTarget::new(second_scope, "$1");

        assert!(state.activate_scoped_session_from_ui(&target));
        assert_eq!(
            state.binding.terminal.active_mux_scope(),
            Some(second_scope)
        );
        assert_eq!(
            std::ptr::from_ref(state.binding.terminal.as_ref()),
            native_terminal,
            "native bindings in one Space must share the terminal owner"
        );
    }

    #[test]
    fn native_startup_waits_for_user_to_open_first_session() {
        let state = test_state();

        assert!(
            state.binding.mux.sessions().is_empty(),
            "startup must not open a session before the user asks for one"
        );
        assert_eq!(state.binding.mux.selected_session(), None);
    }

    fn sync_initial_native_terminal(state: &mut AppState) {
        let mux_config = state.config_state.current().multiplexer.clone();
        if let Some(error) = state
            .binding
            .mux
            .refresh_sessions(&state.repaint, &mux_config)
        {
            panic!("initial native mux refresh failed: {error}");
        }
        state
            .sync_terminal_panes()
            .expect("initial native terminal sync");
    }

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn sidebar_keybinds_produce_registry_backed_invocations() {
        let bindings =
            SidebarKeyBindings::from_keybinds(&BoottyConfig::default().input.sidebar_keybind)
                .expect("default sidebar keybinds");
        let cases = [
            (
                egui::Key::J,
                egui::Modifiers::NONE,
                SidebarAction::NextSession,
            ),
            (
                egui::Key::ArrowUp,
                egui::Modifiers::NONE,
                SidebarAction::PreviousSession,
            ),
            (
                egui::Key::N,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                },
                SidebarAction::NextSession,
            ),
            (
                egui::Key::Enter,
                egui::Modifiers::NONE,
                SidebarAction::ActivateSession,
            ),
        ];

        for (key, modifiers, action) in cases {
            let invocation = bindings
                .invocation_for_key(key, modifiers)
                .expect("configured sidebar binding");
            assert_eq!(invocation.caller, Caller::Keybinding);
            assert_eq!(
                CommandRegistry::core()
                    .resolve(invocation)
                    .unwrap()
                    .executor,
                CoreCommandExecutor::Sidebar(action)
            );
        }
        assert!(
            bindings
                .invocation_for_key(egui::Key::Escape, egui::Modifiers::NONE)
                .is_none()
        );
    }

    #[test]
    fn pane_widget_key_namespaces_same_pane_id_by_session_and_window() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_a = format!("widget-a-{}", unique_test_id());
        let session_b = format!("widget-b-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_a.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let key_a = state.pane_widget_key("pane-1");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_b,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let key_b = state.pane_widget_key("pane-1");

        assert_ne!(key_a, key_b);
        assert!(key_a.contains(&session_a));
    }

    #[test]
    fn sidebar_focus_consumes_keys_and_enter_returns_terminal_focus() {
        let mut state = test_state();
        state.input_focus = InputFocus::Sidebar;
        let viewport = ViewportSnapshot::default();
        let mut effects = Vec::new();

        assert_eq!(
            state.handle_sidebar_input(
                vec![
                    key_event(egui::Key::J, egui::Modifiers::NONE),
                    egui::Event::Text("j".to_owned()),
                ],
                viewport,
                &mut effects,
            ),
            2
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(
                vec![key_event(egui::Key::Escape, egui::Modifiers::NONE)],
                viewport,
                &mut effects,
            ),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Sidebar);

        assert_eq!(
            state.handle_sidebar_input(
                vec![key_event(egui::Key::Enter, egui::Modifiers::NONE)],
                viewport,
                &mut effects,
            ),
            1
        );
        assert_eq!(state.input_focus, InputFocus::Terminal);
        assert!(effects.is_empty());
    }

    #[test]
    fn prune_pane_layouts_drops_dead_sessions_but_keeps_the_active_window() {
        let mut state = test_state();
        let current = state.current_window_key();
        let ghost = state
            .binding
            .window_id("ghost-session".to_owned(), "@9".to_owned());
        state
            .binding
            .pane_layouts
            .insert(current.clone(), PaneLayout::single("p1".to_owned()));
        state
            .binding
            .pane_layouts
            .insert(ghost.clone(), PaneLayout::single("p2".to_owned()));

        state.prune_pane_layouts();

        assert!(
            state.binding.pane_layouts.contains_key(&current),
            "active window's layout must survive pruning"
        );
        assert!(
            !state.binding.pane_layouts.contains_key(&ghost),
            "layout for a session that no longer exists must be reclaimed"
        );
    }

    #[test]
    fn native_layout_reconcile_keeps_local_focus_when_server_anchor_is_stale() {
        assert_eq!(
            focus_after_native_layout_reconcile(false, &[], Some("%1")),
            None,
            "refreshes must not let a stale rmux active-pane anchor overwrite Bootty focus"
        );
    }

    #[test]
    fn native_layout_reconcile_focuses_new_or_restored_server_pane() {
        assert_eq!(
            focus_after_native_layout_reconcile(true, &[], Some("%2")),
            Some("%2".to_owned())
        );
        assert_eq!(
            focus_after_native_layout_reconcile(false, &["%2".to_owned()], Some("%2")),
            Some("%2".to_owned())
        );
        assert_eq!(
            focus_after_native_layout_reconcile(false, &["%2".to_owned()], Some("%1")),
            Some("%2".to_owned())
        );
    }

    #[test]
    fn native_new_tab_command_syncs_terminal_before_next_frame() {
        let mut state = test_state_with_config(|config| {
            config.session.shell = Some("/usr/bin/true".to_owned());
        });
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let previous = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("first native tab focused pane");

        state.apply_mux_key_action(MuxKeyAction::NewTab);

        let selected = state
            .binding
            .mux
            .selected_session_anchor()
            .and_then(|anchor| anchor.pane_id.as_deref())
            .map(str::to_owned)
            .expect("new tab selected pane");
        assert_eq!(
            state.binding.terminal.focused_pane_id(),
            Some(selected.as_str())
        );
        assert_ne!(selected, previous);
    }

    #[test]
    fn native_session_activation_syncs_terminal_before_next_frame() {
        let mut state = test_state_with_config(|config| {
            config.session.shell = Some("/usr/bin/true".to_owned());
        });
        sync_initial_native_terminal(&mut state);
        let mux_config = state.config().multiplexer.clone();
        let session_a = format!("native-a-{}", unique_test_id());
        let session_b = format!("native-b-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_a.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.sync_native_layout_terminal_now();
        let first_pane = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("first focused pane");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_b,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.sync_native_layout_terminal_now();
        let second_pane = state
            .binding
            .terminal
            .focused_pane_id()
            .map(str::to_owned)
            .expect("second focused pane");
        assert_ne!(second_pane, first_pane);

        state.activate_session_from_ui(&session_a);

        assert_eq!(
            state.binding.terminal.focused_pane_id(),
            Some(first_pane.as_str())
        );
    }

    #[test]
    #[ignore = "requires an isolated RMUX_TMPDIR"]
    fn rmux_live_app_state_session_and_tab_activation_stay_interactive() -> Result<()> {
        std::env::var_os("RMUX_TMPDIR").context("set isolated RMUX_TMPDIR")?;
        bootty_mux::start_embedded_rmux_daemon_for_tests()?;
        use crate::mux::rmux::{RmuxSessionClient, SdkRmuxClient};

        let client = SdkRmuxClient::new();
        let cwd = std::env::current_dir()?.to_string_lossy().into_owned();
        let session_a = format!("bootty-app-perf-a-{}", std::process::id());
        let session_b = format!("bootty-app-perf-b-{}", std::process::id());
        client.ensure_session(&session_a, &cwd)?;
        client.ensure_session(&session_b, &cwd)?;
        client.new_window(&session_a, Some(&cwd))?;
        client.new_window(&session_a, Some(&cwd))?;
        client.new_window(&session_b, Some(&cwd))?;

        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Rmux;
        });
        let refresh_start = Instant::now();
        let deadline = refresh_start + Duration::from_secs(5);
        loop {
            let mux_config = state.config_state.current().multiplexer.clone();
            if let Some(error) = state
                .binding
                .mux
                .refresh_sessions(&state.repaint, &mux_config)
            {
                anyhow::bail!(error);
            }
            if state
                .binding
                .mux
                .sessions()
                .iter()
                .any(|session| session.id == session_a)
                && state
                    .binding
                    .mux
                    .sessions()
                    .iter()
                    .any(|session| session.id == session_b)
            {
                break;
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for rmux app-state snapshot");
            }
            thread::sleep(Duration::from_millis(10));
        }
        let refresh_elapsed = refresh_start.elapsed();

        let session_start = Instant::now();
        state.activate_session_from_ui(&session_b);
        state.sync_terminal_panes()?;
        let session_elapsed = session_start.elapsed();

        let window_id = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_a)
            .and_then(|session| session.windows.get(1))
            .map(|window| window.id.clone())
            .context("app perf target tab should exist")?;
        let tab_start = Instant::now();
        state.activate_window_from_ui(&session_a, &window_id);
        state.sync_terminal_panes()?;
        let tab_elapsed = tab_start.elapsed();

        eprintln!(
            "rmux app-state perf probe: refresh={refresh_elapsed:?} session={session_elapsed:?} tab={tab_elapsed:?}"
        );

        client.kill_session(&session_a)?;
        client.kill_session(&session_b)?;

        assert!(
            session_elapsed < Duration::from_millis(100),
            "app-state rmux session activation should not block: {session_elapsed:?}"
        );
        assert!(
            tab_elapsed < Duration::from_millis(100),
            "app-state rmux tab activation should not block: {tab_elapsed:?}"
        );
        Ok(())
    }

    #[test]
    fn pending_pane_split_direction_survives_window_id_materialization() {
        let mut state = test_state();
        let pending = state
            .binding
            .window_id("rmux-session".to_owned(), String::new());
        state
            .binding
            .pending_pane_split_directions
            .insert(pending, SplitDirection::Down);
        let materialized = state
            .binding
            .window_id("rmux-session".to_owned(), "@1".to_owned());

        let direction = state.take_pending_pane_split_direction(&materialized);

        assert_eq!(direction, Some(SplitDirection::Down));
        assert!(state.binding.pending_pane_split_directions.is_empty());
    }

    #[test]
    fn rmux_split_layout_defers_when_selected_anchor_is_still_old_pane() {
        let mut state = test_state();
        let key = state
            .binding
            .window_id("rmux-session".to_owned(), "@1".to_owned());
        state
            .binding
            .pane_layouts
            .insert(key.clone(), PaneLayout::single("%1".to_owned()));

        state.apply_split_layout_after_command(
            key.clone(),
            Some("%1".to_owned()),
            SplitDirection::Down,
            MultiplexerBackendConfig::Rmux,
        );

        assert_eq!(
            state.take_pending_pane_split_direction(&key),
            Some(SplitDirection::Down)
        );
        assert_eq!(
            state.binding.pane_layouts.get(&key).map(PaneLayout::panes),
            Some(vec!["%1".to_owned()])
        );
    }

    #[test]
    fn direct_input_suppression_tracks_terminal_ownership() {
        let mut state = test_state();

        assert!(state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarFocus),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());

        state.apply_keybind_action(
            KeybindAction::App(AppAction::SessionPicker),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(!state.direct_input_suppresses_egui_events());
    }

    #[test]
    fn last_session_toggles_bootty_selected_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "local".to_owned(),
                cwd: ".".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: "project".to_owned(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        state.activate_session_from_ui("local");
        state.activate_session_from_ui("project");
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.binding.mux.selected_session(), Some("local"));

        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(state.binding.mux.selected_session(), Some("project"));
    }

    #[test]
    fn last_session_without_a_prior_session_is_a_no_op_not_a_panic() {
        // A fresh state has only the initial session and no previous selection; last_session must be
        // consumed silently instead of falling through to the command builder's `unreachable!`.
        let mut state = test_state();
        let before = state.binding.mux.selected_session().map(str::to_owned);
        state.apply_mux_key_action(MuxKeyAction::LastSession);
        assert_eq!(
            state.binding.mux.selected_session().map(str::to_owned),
            before
        );
    }

    #[test]
    fn context_session_commands_open_their_picker_or_navigate_the_active_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-session-command-first-{}", unique_test_id());
        let second = format!("context-session-command-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.activate_session_from_ui(&second);

        assert!(state.open_new_session_dialog_from_ui());
        assert!(state.take_dialog().is_some());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.open_session_picker_dialog_from_ui());
        assert!(state.take_session_picker_dialog().is_some());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.activate_relative_session_from_ui(&second, -1));
        assert_ne!(state.binding.mux.selected_session(), Some(second.as_str()));

        assert!(state.activate_last_session_from_ui());
        assert_eq!(state.binding.mux.selected_session(), Some(second.as_str()));
    }

    #[test]
    fn context_session_navigation_anchors_to_the_clicked_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = unique_test_id();
        let first = format!("context-session-first-{unique}");
        let clicked = format!("context-session-clicked-{unique}");
        let next = format!("context-session-next-{unique}");
        for session_id in [&first, &clicked, &next] {
            state.binding.mux.create_project_session(
                crate::mux::controller::NewMuxSessionRequest {
                    session_id: (*session_id).clone(),
                    cwd: "/tmp".to_owned(),
                },
                &state.repaint,
                &mux_config,
            );
        }
        state.activate_session_from_ui(&first);
        let sessions = state.binding.mux.sessions();
        let clicked_index = sessions
            .iter()
            .position(|session| session.id == clicked)
            .expect("clicked session is present");
        let selected_index = sessions
            .iter()
            .position(|session| session.id == first)
            .expect("selected session is present");
        let expected_clicked_next = sessions[(clicked_index + 1) % sessions.len()].id.clone();
        let selected_next = sessions[(selected_index + 1) % sessions.len()].id.clone();
        assert_ne!(expected_clicked_next, selected_next);

        assert!(state.activate_relative_session_from_ui(&clicked, 1));

        assert_eq!(
            state.binding.mux.selected_session(),
            Some(expected_clicked_next.as_str())
        );
    }

    #[test]
    fn move_session_reorders_bootty_owned_session_order() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let unique = unique_test_id();
        let alpha = format!("alpha-{unique}");
        let beta = format!("beta-{unique}");
        state
            .binding
            .session_order
            .add_session(&alpha)
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: alpha.clone(),
                cwd: "repo/a".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state
            .binding
            .session_order
            .add_session(&beta)
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: beta.clone(),
                cwd: "repo/b".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        assert!(
            state
                .binding
                .session_order
                .move_session(&beta, -1, [alpha.as_str(), beta.as_str()])
                .expect("move session")
        );
        let ordered = state
            .binding
            .session_order
            .sync_sessions([alpha.as_str(), beta.as_str()])
            .expect("sync session order");

        assert_eq!(ordered, vec![beta, alpha]);
    }

    #[test]
    fn context_rename_session_targets_the_clicked_inactive_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-session-first-{}", unique_test_id());
        let second = format!("context-session-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );

        assert_eq!(state.mux().selected_session(), Some(second.as_str()));

        state.open_rename_session_dialog_for(&first);

        let dialog = state
            .take_rename_session_dialog()
            .expect("clicked session should open its rename dialog");
        assert_eq!(
            dialog,
            RenameSessionDialog::open(first.clone(), first.clone())
        );
        state.apply_rename_session_event(
            dialog,
            RenameSessionEvent::Rename {
                session_id: first.clone(),
                name: "renamed-from-context".to_owned(),
            },
        );

        assert_eq!(state.mux().selected_session(), Some(second.as_str()));
        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .find(|session| session.id == first)
                .map(|session| session.name.as_str()),
            Some("renamed-from-context")
        );
    }

    #[test]
    fn context_ditch_keeps_the_other_session_selected() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-ditch-first-{}", unique_test_id());
        let second = format!("context-ditch-second-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        persist_session_launch_plan(
            &state.config().config_path,
            state.binding.scope.binding_id().persistence_value(),
            &simple_session_launch_plan(&first, "/tmp"),
        )
        .expect("persist launch plan");

        state.apply_ditch_session_event(
            DitchSessionDialog::open(first.clone(), None),
            DitchSessionEvent::Ditch {
                session_id: first.clone(),
                cwd: None,
                action: DitchAction::KillOnly,
                confirmation: None,
            },
        );

        assert_eq!(state.mux().selected_session(), Some(second.as_str()));
        assert!(
            state
                .mux()
                .sessions()
                .iter()
                .all(|session| session.id != first)
        );
        let plans = load_session_launch_plans(
            &state.config().config_path,
            state.binding.scope.binding_id().persistence_value(),
        )
        .expect("load launch plans");
        assert!(
            plans.iter().all(|(_, plan)| plan.session_id != first),
            "successful ditch must not resurrect persisted launch intent"
        );
    }

    #[test]
    fn failed_ditch_preserves_persisted_launch_plan() {
        let mut state = test_state();
        let session_id = format!("failed-ditch-{}", unique_test_id());
        let mux_config = state.config().multiplexer.clone();
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        persist_session_launch_plan(
            &state.config().config_path,
            state.binding.scope.binding_id().persistence_value(),
            &simple_session_launch_plan(&session_id, "/tmp"),
        )
        .expect("persist launch plan");

        state.apply_ditch_session_event(
            DitchSessionDialog::open(session_id.clone(), Some("/not/a/worktree".to_owned())),
            DitchSessionEvent::Ditch {
                session_id: session_id.clone(),
                cwd: Some("/not/a/worktree".to_owned()),
                action: DitchAction::DetachWorktree,
                confirmation: None,
            },
        );

        assert!(state.ditch_session_dialog.is_some());
        let plans = load_session_launch_plans(
            &state.config().config_path,
            state.binding.scope.binding_id().persistence_value(),
        )
        .expect("load launch plans");
        assert!(
            plans.iter().any(|(_, plan)| plan.session_id == session_id),
            "failed ditch must retain restore intent"
        );
    }

    #[test]
    fn context_move_session_reorders_the_clicked_inactive_session() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let first = format!("context-move-session-first-{}", unique_test_id());
        let second = format!("context-move-session-second-{}", unique_test_id());
        state
            .binding
            .session_order
            .add_session(&first)
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: first.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state
            .binding
            .session_order
            .add_session(&second)
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: second.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state
            .binding
            .sync_session_order()
            .expect("sync session order");
        let before = state
            .binding
            .mux
            .sessions()
            .iter()
            .map(|session| session.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|session_id| session_id == &first)
            .expect("clicked session should be present");
        assert!(
            before_index + 1 < before.len(),
            "clicked session should have a following session: {before:?}"
        );

        assert!(state.move_session_from_ui(&first, 1));

        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .position(|session| session.id == first),
            Some(before_index + 1)
        );
    }

    #[test]
    fn close_action_emits_close_window_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::Close),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::CloseWindow]);
    }

    #[test]
    fn quit_action_emits_instance_quit_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::Quit),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::QuitApplication]);
    }

    #[test]
    fn new_tab_action_adds_a_window() {
        let mut state = test_state();
        let before = state.binding.mux.selected_session_windows().len();
        let selected = state.binding.mux.selected_session().map(str::to_owned);

        state.apply_mux_key_action(MuxKeyAction::NewTab);

        let after = state.binding.mux.selected_session_windows().len();
        assert!(
            after > before,
            "before={before} after={after} selected={selected:?}"
        );
    }

    #[test]
    fn move_tab_action_reorders_selected_session_windows() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let moved = state
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .expect("new tab selected");
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|id| id == &moved)
            .expect("selected tab is in window list");
        assert!(
            before_index > 0,
            "new tab should be movable left: {before:?}"
        );

        state.apply_mux_key_action(MuxKeyAction::MoveTab(-1));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after[before_index - 1], moved);
    }

    #[test]
    fn context_rename_tab_targets_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let windows = state.binding.mux.selected_session_windows();
        let clicked = windows[0].clone();
        let selected = windows[1].id.clone();

        state.open_rename_tab_dialog_for(&session_id, &clicked.id);

        assert_eq!(
            state.take_rename_tab_dialog(),
            Some(RenameTabDialog::open(session_id, clicked.id, clicked.name,))
        );
        assert_eq!(state.mux().selected_window(), Some(selected.as_str()));
    }

    #[test]
    fn context_new_tab_for_an_inactive_tab_uses_its_anchor_cwd() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-tab-cwd-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/context/tab-one".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.execute_command(
            &state.repaint,
            &mux_config,
            MuxCommand::NewWindow {
                session_id: session_id.clone(),
                cwd: Some("/context/tab-two".to_owned()),
            },
        );
        let clicked = state.binding.mux.selected_session_windows()[0].id.clone();

        assert!(state.new_tab_for_window_from_ui(&session_id, &clicked));

        assert_eq!(
            state
                .mux()
                .sessions()
                .iter()
                .find(|session| session.id == session_id)
                .and_then(|session| session.windows.last())
                .and_then(|window| window.anchor.cwd.as_deref()),
            Some("/context/tab-one")
        );
    }

    #[test]
    fn context_close_pane_closes_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-close-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.binding.mux.execute_command(
            &state.repaint,
            &mux_config,
            MuxCommand::NewWindow {
                session_id: session_id.clone(),
                cwd: None,
            },
        );
        let clicked = state.binding.mux.selected_session_windows()[0].id.clone();
        let selected = state.binding.mux.selected_session_windows()[1].id.clone();

        assert!(state.close_pane_for_window_from_ui(&session_id, &clicked));

        let remaining = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("target session should stay open")
            .windows
            .iter()
            .map(|window| window.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![selected.as_str()]);
        assert_eq!(state.mux().selected_window(), Some(selected.as_str()));
    }

    #[test]
    fn context_move_tab_reorders_the_clicked_inactive_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let clicked = before[0].clone();
        let active = before[2].clone();

        assert!(state.move_window_from_ui(&session_id, &clicked, 1));

        assert_eq!(
            state
                .mux()
                .selected_session_windows()
                .iter()
                .map(|window| window.id.clone())
                .collect::<Vec<_>>(),
            vec![before[1].clone(), before[0].clone(), before[2].clone()]
        );
        assert_eq!(state.mux().selected_window(), Some(active.as_str()));
    }

    #[test]
    fn context_tab_navigation_anchors_to_the_clicked_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("context-navigate-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let tabs = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let clicked = tabs[1].clone();
        state.activate_window_from_ui(&session_id, &tabs[0]);

        assert!(state.activate_relative_window_from_ui(&session_id, &clicked, 1));

        assert_eq!(state.mux().selected_window(), Some(tabs[2].as_str()));
    }

    #[test]
    fn window_reorder_from_ui_moves_non_active_tab_to_end() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("drag-move-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let moved = before[0].clone();

        assert!(state.reorder_window_before_from_ui(&moved, None));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            after,
            vec![before[1].clone(), before[2].clone(), before[0].clone()]
        );
    }

    #[test]
    fn window_reorder_from_ui_ignores_self_drop() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("self-drop-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let moved = before[0].clone();

        assert!(!state.reorder_window_before_from_ui(&moved, Some(&moved)));

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after, before);
    }

    #[test]
    fn command_palette_move_tab_action_reorders_selected_session_windows() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("palette-move-tab-{}", unique_test_id());
        state
            .binding
            .session_order
            .add_session(&session_id)
            .expect("persist session order");
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "/tmp".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let moved = state
            .binding
            .mux
            .selected_window()
            .map(str::to_owned)
            .expect("new tab selected");
        let before = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        let before_index = before
            .iter()
            .position(|id| id == &moved)
            .expect("selected tab is in window list");
        assert!(
            before_index > 0,
            "new tab should be movable left: {before:?}"
        );

        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run(crate::action_catalog::Command::MoveTabLeft),
        );
        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert!(
            effects.contains(&AppEffect::RequestRepaint),
            "palette move-tab must schedule an immediate repaint so status tabs re-render"
        );

        let after = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .map(|window| window.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(after[before_index - 1], moved);
    }

    #[test]
    fn copy_mode_leaves_global_shortcuts_for_app_keybindings() {
        let alt_shift = egui::Modifiers {
            alt: true,
            shift: true,
            ..Default::default()
        };
        assert!(copy_mode_egui_key_should_pass_to_app(
            egui::Key::Comma,
            alt_shift
        ));
        assert!(copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::Comma,
            mods: crate::terminal::KeyMods {
                alt: true,
                shift: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("<"),
            unshifted: Some(','),
        }));

        assert!(!copy_mode_egui_key_should_pass_to_app(
            egui::Key::J,
            egui::Modifiers::default()
        ));
        assert!(!copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::J,
            mods: crate::terminal::KeyMods::default(),
            repeat: false,
            utf8: Some("j"),
            unshifted: Some('j'),
        }));

        let command = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        assert!(!copy_mode_egui_key_should_pass_to_app(
            egui::Key::C,
            command
        ));
        assert!(copy_mode_egui_key_should_pass_to_app(egui::Key::F, command));
        assert!(!copy_mode_input_should_pass_to_app(KeyInput {
            key: TerminalKey::C,
            mods: crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: Some('c'),
        }));
    }

    #[test]
    fn rename_tab_action_opens_dialog_and_renames_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("rename-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::RenameTab),
            ViewportSnapshot::default(),
            &mut effects,
        );
        let dialog = state
            .take_rename_tab_dialog()
            .expect("rename tab action should open the dialog");

        state.apply_rename_tab_event(
            dialog,
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("renamed tab should remain present");
        assert_eq!(window.name, "build");
        assert_eq!(effects, vec![AppEffect::RequestRepaint]);
    }

    #[test]
    fn unscoped_window_title_side_effect_renames_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id,
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle(
                "⠼ agents".to_owned(),
            )),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "⠼ agents");
        assert_eq!(
            effects,
            vec![AppEffect::SetWindowTitle("⠼ agents".to_owned())]
        );
    }

    #[test]
    fn scoped_window_title_side_effect_renames_source_tab_not_selected_tab() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("scoped-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let first_window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        let first_original_name = state.binding.mux.selected_session_windows()[0].name.clone();
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let second_window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id != first_window_id)
            .expect("second tab should be present")
            .clone();
        let second_pane_id = second_window
            .anchor
            .pane_id
            .clone()
            .expect("native tab should have a source pane id");
        state.activate_window_from_ui(&session_id, &first_window_id);
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id),
                TerminalSideEffect::WindowTitle("⠼ agents".to_owned()),
            ),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let windows = state.binding.mux.selected_session_windows();
        let first_window = windows
            .iter()
            .find(|window| window.id == first_window_id)
            .expect("selected tab should remain present");
        let second_window = windows
            .iter()
            .find(|window| window.id == second_window.id)
            .expect("source tab should remain present");
        assert_eq!(first_window.name, first_original_name);
        assert_eq!(second_window.name, "⠼ agents");
        assert_eq!(
            state.binding.mux.selected_window(),
            Some(first_window_id.as_str())
        );
        assert_eq!(effects, Vec::<AppEffect>::new());
    }

    #[test]
    fn scoped_terminal_progress_updates_and_clears_its_inactive_window_indicator() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("progress-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let first_window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_mux_key_action(MuxKeyAction::NewTab);
        let second_window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id != first_window_id)
            .expect("second tab should be present")
            .clone();
        let second_pane_id = second_window
            .anchor
            .pane_id
            .clone()
            .expect("native tab should have a source pane id");
        state.activate_window_from_ui(&session_id, &first_window_id);

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id.clone()),
                TerminalSideEffect::ConEmuProgress {
                    state: "normal".to_owned(),
                    value: Some(42),
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert_eq!(state.window_progress(&second_window), Some(42));

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id.clone()),
                TerminalSideEffect::ConEmuProgress {
                    state: "indeterminate".to_owned(),
                    value: None,
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert!(state.has_indeterminate_terminal_progress());
        assert_eq!(state.window_progress(&second_window), Some(50));
        assert!(state.window_has_indeterminate_progress(&second_window));

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(second_pane_id),
                TerminalSideEffect::ConEmuProgress {
                    state: "inactive".to_owned(),
                    value: Some(0),
                },
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert_eq!(state.window_progress(&second_window), None);
        assert!(!state.has_indeterminate_terminal_progress());
        assert!(!state.window_has_indeterminate_progress(&second_window));
    }

    #[test]
    fn scoped_terminal_ports_ignore_other_bindings_and_stay_with_the_source_pane() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("ports-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let pane_id = state.binding.mux.selected_session_windows()[0]
            .anchor
            .pane_id
            .clone()
            .expect("native tab should have a source pane id");
        let other_scope = MuxScope::new(
            SpaceId::from_persistence(99),
            BindingId::from_persistence(99),
        );

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(crate::mux::terminal::encode_scoped_pane_id(
                    other_scope,
                    &pane_id,
                )),
                TerminalSideEffect::Iterm2UserVarPorts(vec![3000]),
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );
        assert_eq!(state.pane_ports(&pane_id), None);

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::new(
                Some(crate::mux::terminal::encode_scoped_pane_id(
                    state.binding.scope,
                    &pane_id,
                )),
                TerminalSideEffect::Iterm2UserVarPorts(vec![8080, 3000]),
            ),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );

        assert_eq!(state.pane_ports(&pane_id), Some([8080, 3000].as_slice()));
        let session = state
            .binding
            .mux
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .expect("created session")
            .clone();
        assert_eq!(state.session_ports(&session), vec![8080, 3000]);
    }

    #[test]
    fn manually_renamed_tab_ignores_terminal_title_renames() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("manual-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "tab-1".to_owned()),
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );
        let mut effects = Vec::new();

        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("editor".to_owned())),
            &mut effects,
            8.0,
            16.0,
            1.0,
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "build");
        assert_eq!(
            effects,
            vec![AppEffect::SetWindowTitle("editor".to_owned())]
        );
    }

    #[test]
    fn blank_tab_rename_restores_terminal_title_following() {
        let mut state = test_state();
        let mux_config = state.config().multiplexer.clone();
        let session_id = format!("blank-title-tab-{}", unique_test_id());
        state.binding.mux.create_project_session(
            crate::mux::controller::NewMuxSessionRequest {
                session_id: session_id.clone(),
                cwd: "repo".to_owned(),
            },
            &state.repaint,
            &mux_config,
        );
        let window_id = state.binding.mux.selected_session_windows()[0].id.clone();
        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("editor".to_owned())),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "tab-1".to_owned()),
            RenameTabEvent::Rename {
                session_id: session_id.clone(),
                window_id: window_id.clone(),
                name: "build".to_owned(),
            },
        );
        state.apply_terminal_side_effect_event(
            TerminalSideEffectEvent::unscoped(TerminalSideEffect::WindowTitle("server".to_owned())),
            &mut Vec::new(),
            8.0,
            16.0,
            1.0,
        );
        state.apply_rename_tab_event(
            RenameTabDialog::open(session_id.clone(), window_id.clone(), "build".to_owned()),
            RenameTabEvent::Rename {
                session_id,
                window_id: window_id.clone(),
                name: String::new(),
            },
        );

        let window = state
            .binding
            .mux
            .selected_session_windows()
            .iter()
            .find(|window| window.id == window_id)
            .expect("selected window should remain present");
        assert_eq!(window.name, "server");
    }

    #[test]
    fn copy_action_emits_request_copy_effect() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::CopyToClipboard(CopyToClipboard::Mixed),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(effects, vec![AppEffect::RequestCopy]);
    }

    #[test]
    fn toggle_sidebar_visibility_flips_config_and_requests_repaint() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::App(AppAction::ToggleSidebarVisibility),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().chrome.sidebar, !before);
        assert_eq!(effects, vec![AppEffect::RequestRepaint]);
    }

    #[test]
    fn command_palette_toggle_sidebar_visibility_runs_on_next_frame() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run(crate::action_catalog::Command::ToggleSidebar),
        );

        assert_eq!(state.config().chrome.sidebar, before);
        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(state.config().chrome.sidebar, !before);
        assert!(effects.contains(&AppEffect::RequestRepaint));
    }

    #[test]
    fn command_palette_rejects_a_missing_target_before_queueing() {
        let mut state = test_state();
        assert!(state.current_command_target(ResourceKind::Pane).is_none());

        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run(crate::action_catalog::Command::KillPane),
        );

        assert!(state.pending_command.is_none());
        assert!(
            state
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("no current Pane target"))
        );
    }

    #[test]
    fn command_palette_pins_the_binding_selected_by_the_user() {
        let mut state = test_state();
        let home = state.active_space_id();
        assert!(state.create_space_from_ui(
            "Work",
            "folder",
            crate::workspace::DEFAULT_SPACE_COLOR,
            false,
        ));
        let space_count = state.space_summaries().len();

        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run(crate::action_catalog::Command::CloseSpace),
        );
        assert_eq!(
            state
                .pending_command
                .as_ref()
                .and_then(|invocation| invocation.target.as_ref())
                .map(|target| target.kind),
            Some(ResourceKind::Binding)
        );

        assert!(state.activate_space_from_ui(home));
        let invocation = state.pending_command.take().unwrap();
        let outcome =
            state.dispatch_command(invocation, ViewportSnapshot::default(), &mut Vec::new());

        assert_eq!(outcome, CommandOutcome::success());
        assert_eq!(state.active_space_id(), home);
        assert_eq!(state.space_summaries().len(), space_count - 1);
    }

    #[test]
    fn egui_keybinding_dispatches_sidebar_command() {
        let mut state = test_state();
        state.app_key_bindings =
            AppKeyBindings::from_keybinds(&["ctrl+b=toggle_sidebar_visibility".to_owned()])
                .unwrap();
        let before = state.config().chrome.sidebar;
        let event = egui::Event::Key {
            key: egui::Key::B,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        };

        let effects = state.update_frame(test_frame_inputs(vec![event], None));

        assert_eq!(state.config().chrome.sidebar, !before);
        assert!(effects.contains(&AppEffect::RequestRepaint));
    }
    #[test]
    fn dispatcher_validation_and_outcomes_are_equivalent_for_every_caller() {
        let callers = [
            Caller::CommandPalette,
            Caller::Keybinding,
            Caller::BuiltinKeybinding,
            Caller::Cli,
            Caller::Socket,
            Caller::Luau,
            Caller::Internal,
        ];

        for caller in callers {
            let mut state = test_state();
            let mut effects = Vec::new();
            let invalid = state.dispatch_command(
                CommandInvocation::from_action("select_tab:nope", caller),
                ViewportSnapshot::default(),
                &mut effects,
            );
            assert!(matches!(
                invalid,
                CommandOutcome::Failed { code, .. } if code == "invalid_arguments"
            ));
            assert!(effects.is_empty());

            let before = state.config().chrome.sidebar;
            assert_eq!(
                state.dispatch_command(
                    CommandInvocation::from_action("toggle_sidebar_visibility", caller),
                    ViewportSnapshot::default(),
                    &mut effects,
                ),
                CommandOutcome::success()
            );
            assert_eq!(state.config().chrome.sidebar, !before);
            assert!(effects.contains(&AppEffect::RequestRepaint));
        }
    }

    #[test]
    fn direct_keybinding_dispatches_sidebar_command() {
        let mut state = test_state();
        state.app_key_bindings =
            AppKeyBindings::from_keybinds(&["cmd+b=toggle_sidebar_visibility".to_owned()]).unwrap();
        state.pending_direct_input.push(
            crate::direct_input::direct_key_input_from_winit_code(
                winit::keyboard::KeyCode::KeyB,
                winit::keyboard::ModifiersState::SUPER,
                ModifierSideState::default(),
                false,
            )
            .expect("direct key input"),
        );
        let before = state.config().chrome.sidebar;

        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));

        assert_eq!(state.config().chrome.sidebar, !before);
        assert!(effects.contains(&AppEffect::RequestRepaint));
    }

    #[test]
    fn app_command_channel_runs_on_the_ui_thread() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let sender = state.app_command_sender(Caller::Socket);
        let (response, response_rx) = mpsc::channel();
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action(
                    "toggle_sidebar_visibility",
                    Caller::Socket,
                ),
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: crate::commands::CommandCancellation::new(),
                response,
                completion: None,
            })
            .unwrap();

        let effects = state.update_frame(test_frame_inputs(Vec::new(), None));

        assert_eq!(state.config().chrome.sidebar, !before);
        assert!(effects.contains(&AppEffect::RequestRepaint));
        assert_eq!(response_rx.recv().unwrap(), CommandOutcome::success());
    }

    #[test]
    fn mux_command_waits_for_backend_failure_before_replying() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        ScriptedBackend::failing(
            vec![session_with_window_and_pane("s1", "project", "/repo")],
            "backend failed",
        )
        .install(&mut state.binding);
        let config = state.active_multiplexer().clone();
        state.binding.mux.refresh_on_next_frame();
        for _ in 0..100 {
            state.binding.mux.refresh_sessions(&state.repaint, &config);
            if state.binding.mux.selected_session().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(state.binding.mux.selected_session(), Some("s1"));

        let sender = state.app_command_sender(Caller::Socket);
        let (response, response_rx) = mpsc::channel();
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action("next_tab", Caller::Socket),
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: crate::commands::CommandCancellation::new(),
                response,
                completion: None,
            })
            .unwrap();

        state.update_frame(test_frame_inputs(Vec::new(), None));
        assert_eq!(response_rx.try_recv(), Err(mpsc::TryRecvError::Empty));

        let outcome = loop {
            state.update_frame(test_frame_inputs(Vec::new(), None));
            match response_rx.try_recv() {
                Ok(outcome) => break outcome,
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                Err(mpsc::TryRecvError::Disconnected) => panic!("command response disconnected"),
            }
        };
        assert!(
            matches!(
                &outcome,
                CommandOutcome::Failed { code, message }
                    if code == "execution_failed" && message == "backend failed"
            ),
            "unexpected command outcome: {outcome:?}"
        );
    }

    #[test]
    fn mux_command_returns_focused_window_after_backend_confirmation() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        ScriptedBackend::with(vec![session_with_window_and_pane("s1", "project", "/repo")])
            .install(&mut state.binding);
        let config = state.active_multiplexer().clone();
        state.binding.mux.refresh_on_next_frame();
        for _ in 0..100 {
            state.binding.mux.refresh_sessions(&state.repaint, &config);
            if state.binding.mux.selected_session().is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let sender = state.app_command_sender(Caller::Socket);
        let (response, response_rx) = mpsc::channel();
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action("next_tab", Caller::Socket),
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: crate::commands::CommandCancellation::new(),
                response,
                completion: None,
            })
            .unwrap();
        state.update_frame(test_frame_inputs(Vec::new(), None));

        let outcome = loop {
            state.update_frame(test_frame_inputs(Vec::new(), None));
            match response_rx.try_recv() {
                Ok(outcome) => break outcome,
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                Err(mpsc::TryRecvError::Disconnected) => panic!("command response disconnected"),
            }
        };
        let CommandOutcome::Success { value, .. } = &outcome else {
            panic!("mux command failed: {outcome:?}");
        };
        assert_eq!(value["focused"]["kind"], "mux_window");
        assert_eq!(value["focused"]["generation"], "1");
    }

    #[test]
    fn pane_mux_commands_reach_typed_commands_and_preflight_backend_gaps() {
        fn invoke(state: &mut AppState, invocation: CommandInvocation) -> CommandOutcome {
            let sender = state.app_command_sender(Caller::Cli);
            let (response, response_rx) = mpsc::channel();
            sender
                .try_send(crate::commands::AppCommandRequest {
                    invocation,
                    deadline: Instant::now() + Duration::from_secs(1),
                    cancellation: crate::commands::CommandCancellation::new(),
                    response,
                    completion: None,
                })
                .expect("queue command");
            for _ in 0..100 {
                state.update_frame(test_frame_inputs(Vec::new(), None));
                match response_rx.try_recv() {
                    Ok(outcome) => return outcome,
                    Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        panic!("command response disconnected")
                    }
                }
            }
            panic!("mux command did not complete");
        }

        fn explicit_pane_target(state: &AppState) -> CommandTarget {
            let window_target = state
                .current_command_target(ResourceKind::MuxWindow)
                .expect("current mux window target");
            let mut path =
                serde_json::from_str::<Vec<String>>(&window_target.handle).expect("mux path");
            let session_id = path.get(1).expect("session id").clone();
            let window_id = path.get(2).expect("window id").clone();
            let pane_id = "s1-pane".to_owned();
            let generation = state
                .binding
                .mux
                .pane_generation(&session_id, &window_id, &pane_id)
                .expect("pane generation");
            path.push(pane_id);
            CommandTarget {
                kind: ResourceKind::Pane,
                handle: serde_json::to_string(&path).expect("serialize pane target"),
                generation,
            }
        }

        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        let backend = ScriptedBackend::with_operations(
            vec![session_with_window_and_pane("s1", "project", "/repo")],
            [
                BindingOperation::NavigatePane,
                BindingOperation::LastPane,
                BindingOperation::ResizePane,
            ],
        )
        .install(&mut state.binding);
        let config = state.active_multiplexer().clone();
        state.binding.mux.refresh_on_next_frame();
        for _ in 0..100 {
            state.binding.mux.refresh_sessions(&state.repaint, &config);
            if state.current_command_target(ResourceKind::Pane).is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        let mut expected_commands = Vec::new();
        for (command, arguments, kind, expected) in [
            (
                "pane.select",
                vec!["right".to_owned()],
                ResourceKind::Pane,
                MuxCommand::SelectPane {
                    session_id: "s1".to_owned(),
                    window_id: Some("s1-window".to_owned()),
                    direction: MuxDirection::Right,
                },
            ),
            (
                "last-pane",
                Vec::new(),
                ResourceKind::MuxWindow,
                MuxCommand::SelectLastPane {
                    session_id: "s1".to_owned(),
                    window_id: Some("s1-window".to_owned()),
                },
            ),
            (
                "resize-pane",
                vec![r#"{"kind":"directional","direction":"down","cells":3}"#.to_owned()],
                ResourceKind::Pane,
                MuxCommand::ResizePane {
                    session_id: "s1".to_owned(),
                    pane_id: Some("s1-pane".to_owned()),
                    adjustment: MuxPaneResize::Directional {
                        direction: MuxDirection::Down,
                        cells: 3,
                    },
                },
            ),
        ] {
            let target = state
                .current_command_target(kind)
                .expect("current mux target");
            let outcome = invoke(
                &mut state,
                CommandInvocation {
                    command: command.to_owned(),
                    arguments,
                    caller: Caller::Cli,
                    target: Some(target),
                    confirmation: None,
                },
            );
            assert!(matches!(outcome, CommandOutcome::Success { .. }),);
            expected_commands.push(expected);
        }
        assert_eq!(backend.executed_commands(), expected_commands);

        let mut native = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Native;
        });
        let native_backend =
            ScriptedBackend::with(vec![session_with_window_and_pane("s1", "project", "/repo")])
                .install(&mut native.binding);
        let config = native.active_multiplexer().clone();
        native.binding.mux.refresh_on_next_frame();
        for _ in 0..100 {
            native
                .binding
                .mux
                .refresh_sessions(&native.repaint, &config);
            if native
                .current_command_target(ResourceKind::MuxWindow)
                .is_some()
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        for (command, arguments, kind) in [
            ("pane.last", Vec::new(), ResourceKind::MuxWindow),
            (
                "pane.resize",
                vec![r#"{"kind":"absolute","columns":120}"#.to_owned()],
                ResourceKind::Pane,
            ),
        ] {
            let target = if kind == ResourceKind::Pane {
                explicit_pane_target(&native)
            } else {
                native
                    .current_command_target(kind)
                    .expect("current mux target")
            };
            let outcome = native.dispatch_command(
                CommandInvocation {
                    command: command.to_owned(),
                    arguments,
                    caller: Caller::Cli,
                    target: Some(target),
                    confirmation: None,
                },
                ViewportSnapshot::default(),
                &mut Vec::new(),
            );
            assert!(
                matches!(outcome, CommandOutcome::Unsupported { .. }),
                "{command} must be preflighted as unsupported"
            );
        }
        assert!(native_backend.executed_commands().is_empty());
    }

    #[test]
    fn app_command_channel_rejects_cancelled_and_expired_requests() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let sender = state.app_command_sender(Caller::Socket);
        let cancellation = crate::commands::CommandCancellation::new();
        cancellation.cancel();

        for (deadline, cancellation, expected_code) in [
            (
                Instant::now() + Duration::from_secs(1),
                cancellation,
                "cancelled",
            ),
            (
                Instant::now() - Duration::from_secs(1),
                crate::commands::CommandCancellation::new(),
                "deadline_exceeded",
            ),
        ] {
            let (response, response_rx) = mpsc::channel();
            sender
                .try_send(crate::commands::AppCommandRequest {
                    invocation: CommandInvocation::from_action(
                        "toggle_sidebar_visibility",
                        Caller::Socket,
                    ),
                    deadline,
                    cancellation,
                    response,
                    completion: None,
                })
                .unwrap();
            state.update_frame(test_frame_inputs(Vec::new(), None));
            assert!(matches!(
                response_rx.recv().unwrap(),
                CommandOutcome::Failed { code, .. } if code == expected_code
            ));
        }

        assert_eq!(state.config().chrome.sidebar, before);
    }

    #[test]
    fn expired_started_request_reports_indeterminate_completion() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let sender = state.app_command_sender(Caller::Socket);
        let cancellation = crate::commands::CommandCancellation::new();
        assert!(cancellation.try_start());
        let (response, response_rx) = mpsc::channel();
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action(
                    "toggle_sidebar_visibility",
                    Caller::Socket,
                ),
                deadline: Instant::now() - Duration::from_secs(1),
                cancellation,
                response,
                completion: None,
            })
            .unwrap();

        state.update_frame(test_frame_inputs(Vec::new(), None));

        assert!(matches!(
            response_rx.recv().unwrap(),
            CommandOutcome::Failed { code, .. } if code == "completion_indeterminate"
        ));
        assert_eq!(state.config().chrome.sidebar, before);
    }

    #[test]
    fn started_completion_is_reconciled_when_caller_drops_response() {
        let automation = AutomationHub::new();
        let mut state = test_state_with_config_and_automation(|_| {}, automation.clone());
        let scope = format!("instance:{}", state.command_instance_handle);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["command.completed".to_owned()].into_iter().collect(),
                scope,
            )
            .unwrap()
            .subscription;
        let sender = state.app_command_sender(Caller::Socket);
        let cancellation = crate::commands::CommandCancellation::new();
        assert!(cancellation.try_start());
        let (response, response_rx) = mpsc::channel();
        drop(response_rx);
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action(
                    "toggle_sidebar_visibility",
                    Caller::Socket,
                ),
                deadline: Instant::now() - Duration::from_secs(1),
                cancellation,
                response,
                completion: Some(CommandCompletionContext {
                    caller: Caller::Socket,
                    owner_pid: owner.pid(),
                    owner_generation: owner.generation(),
                    target: None,
                }),
            })
            .unwrap();

        state.update_frame(test_frame_inputs(Vec::new(), None));

        let events = (0..100)
            .find_map(|_| {
                let delivery = automation
                    .events()
                    .poll(&subscription, &owner, 0)
                    .expect("reconciled completion event");
                if delivery.events.is_empty() {
                    std::thread::sleep(Duration::from_millis(1));
                    None
                } else {
                    Some(delivery)
                }
            })
            .expect("reconciled completion event");
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].provenance["caller"], "socket");
        assert_eq!(
            events.events[0].provenance["owner_pid"],
            serde_json::json!(owner.pid())
        );
        assert_eq!(
            events.events[0].provenance["owner_generation"],
            serde_json::json!(owner.generation())
        );
        assert_eq!(events.events[0].payload["reconciled"], true);
        assert!(events.events[0].payload["request_id"].is_u64());
        assert_eq!(events.events[0].payload["outcome"]["status"], "failed");
    }

    #[test]
    fn reconciliation_worker_fairly_publishes_over_capacity_started_completions() {
        const TOTAL: usize = 80;
        let automation = AutomationHub::new();
        let owner = crate::automation::OwnerIdentity::new(91, 7);
        let scope = "instance:reconciliation-worker-test".to_owned();
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["command.completed".to_owned()].into_iter().collect(),
                scope.clone(),
            )
            .expect("subscribe to completion events")
            .subscription;
        let mut cursor = 0;
        let mut events = Vec::new();
        let mut stalled_sender: Option<mpsc::Sender<MuxCommandResult>> = None;
        let (reconciliation_tx, reconciliation_rx) =
            mpsc::channel::<ShutdownReconciliationCompletion>();
        drop(reconciliation_rx);
        let origin = MuxScope::new(
            SpaceId::from_persistence(91),
            BindingId::from_persistence(91),
        );
        let binding_identity = BindingRef {
            window: WindowRef {
                instance: InstanceRef {
                    instance_id: "reconciliation-worker-test".to_owned(),
                    generation: owner.generation(),
                },
                window_id: "reconciliation-worker-test".to_owned(),
            },
            space_id: origin.space_id().persistence_value().to_string(),
            binding_id: origin.binding_id().persistence_value().to_string(),
            generation: 1,
        };
        let namespace = BackendConnectionNamespace::new(MultiplexerBackendConfig::Native, None);

        for index in 0..TOTAL {
            let (result_tx, result_rx) = mpsc::channel::<MuxCommandResult>();
            let cancellation = CommandCancellation::new();
            assert!(cancellation.try_start());
            let completion = Some(CommandCompletionContext {
                caller: Caller::Socket,
                owner_pid: owner.pid(),
                owner_generation: owner.generation(),
                target: None,
            });
            enqueue_shutdown_reconciliation(ShutdownReconciliationJob::Mux(
                ShutdownMuxReconciliation {
                    request_id: index as u64 + 1,
                    command_id: "test.reconcile".to_owned(),
                    command: MuxCommand::ActivateNextWindow {
                        session_id: "session".to_owned(),
                    },
                    result: result_rx,
                    origin,
                    binding_identity: binding_identity.clone(),
                    binding_generation: 1,
                    namespace: namespace.clone(),
                    deadline: Instant::now() + SHUTDOWN_RECONCILIATION_GRACE,
                    reconciliation: reconciliation_tx.clone(),
                    cancellation,
                    target: None,
                    completion,
                    automation: automation.clone(),
                    scope: scope.clone(),
                    fallback_scope: scope.clone(),
                },
            ));
            if index == 0 {
                stalled_sender = Some(result_tx);
            } else {
                result_tx
                    .send(Ok(MuxCommandCompletion::default()))
                    .expect("send actual completion");
            }
            let delivery = automation
                .events()
                .poll(&subscription, &owner, cursor)
                .expect("poll completion events");
            cursor = delivery.cursor;
            events.extend(delivery.events);
        }

        for _ in 0..1000 {
            if events.len() == TOTAL - 1 {
                break;
            }
            let delivery = automation
                .events()
                .poll(&subscription, &owner, cursor)
                .expect("poll completion events");
            cursor = delivery.cursor;
            events.extend(delivery.events);
            if events.len() != TOTAL - 1 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        assert_eq!(events.len(), TOTAL - 1);
        assert!(
            events
                .iter()
                .all(|event| event.payload["outcome"]["status"] == "success")
        );
        drop(stalled_sender);
    }

    #[test]
    fn reconciled_completion_preserves_target_and_owner_provenance() {
        let automation = AutomationHub::new();
        let mut state = test_state_with_config_and_automation(|_| {}, automation.clone());
        let scope = format!("instance:{}", state.command_instance_handle);
        let owner = crate::automation::OwnerIdentity::new(2, 3);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                ["command.completed".to_owned()].into_iter().collect(),
                scope,
            )
            .unwrap()
            .subscription;
        let target = CommandTarget {
            kind: ResourceKind::Pane,
            handle: "resolved-pane".to_owned(),
            generation: 7,
        };
        let completion = CommandCompletionContext {
            caller: Caller::Socket,
            owner_pid: owner.pid(),
            owner_generation: owner.generation(),
            target: Some(target.clone()),
        };

        state.publish_reconciled_command_completion(
            17,
            "pane.select",
            None,
            Some(&completion),
            &CommandOutcome::success(),
        );

        let events = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("reconciled completion event");
        assert_eq!(events.events.len(), 1);
        assert_eq!(events.events[0].target.as_ref(), Some(&target));
        assert_eq!(
            events.events[0].payload["target"],
            serde_json::to_value(&target).unwrap()
        );
        assert_eq!(events.events[0].provenance["caller"], "socket");
        assert_eq!(
            events.events[0].provenance["owner_pid"],
            serde_json::json!(owner.pid())
        );
        assert_eq!(
            events.events[0].provenance["owner_generation"],
            serde_json::json!(owner.generation())
        );
    }
    #[test]
    fn synchronous_command_rechecks_deadline_before_starting() {
        let mut state = test_state();
        let before = state.config().chrome.sidebar;
        let outcome = state.dispatch_command_with_execution(
            CommandInvocation::from_action("toggle_sidebar_visibility", Caller::Socket),
            ViewportSnapshot::default(),
            &mut Vec::new(),
            Some((
                Instant::now() - Duration::from_millis(1),
                CommandCancellation::new(),
            )),
        );

        assert!(matches!(
            outcome,
            CommandDispatch::Complete(CommandOutcome::Failed { code, .. })
                if code == "deadline_exceeded"
        ));
        assert_eq!(state.config().chrome.sidebar, before);
    }

    #[test]
    fn destructive_command_confirmation_binds_the_current_window_generation() {
        let mut state = test_state();
        let viewport = ViewportSnapshot::default();
        let mut effects = Vec::new();
        let invocation = CommandInvocation::from_action("close_window", Caller::Socket);

        let confirmation = match state.dispatch_command_with_execution(
            invocation.clone(),
            viewport,
            &mut effects,
            None,
        ) {
            CommandDispatch::Complete(CommandOutcome::ConfirmationRequired { confirmation }) => {
                *confirmation
            }
            CommandDispatch::Pending { .. } => panic!("expected confirmation"),
            CommandDispatch::ExtensionPending { .. } => {
                panic!("expected confirmation, got extension pending")
            }
            CommandDispatch::Complete(outcome) => {
                panic!("expected confirmation, got {outcome:?}")
            }
        };
        assert_eq!(
            confirmation.target.as_ref().map(|target| target.kind),
            Some(ResourceKind::ApplicationWindow)
        );

        let mut confirmed = invocation;
        confirmed.target = confirmation.target.clone();
        confirmed.confirmation = Some(confirmation);
        assert!(matches!(
            state.dispatch_command_with_execution(confirmed, viewport, &mut effects, None),
            CommandDispatch::Complete(CommandOutcome::Success { .. })
        ));
    }

    #[test]
    fn command_rejects_a_stale_target_before_confirmation() {
        let mut state = test_state();
        let viewport = ViewportSnapshot::default();
        let mut effects = Vec::new();
        let mut invocation = CommandInvocation::from_action("close_window", Caller::Socket);
        let mut target = state
            .current_command_target(ResourceKind::ApplicationWindow)
            .unwrap();
        target.generation += 1;
        invocation.target = Some(target);
        invocation.confirmation = Some(invocation.confirmation());

        assert!(matches!(
            state.dispatch_command_with_execution(invocation, viewport, &mut effects, None),
            CommandDispatch::Complete(CommandOutcome::StaleTarget { .. })
        ));
        assert!(effects.is_empty());
    }

    #[test]
    fn missing_session_target_is_unavailable() {
        let mut state = test_state();
        assert_eq!(state.current_command_target(ResourceKind::Session), None);

        let mut effects = Vec::new();
        assert!(matches!(
            state.dispatch_command(
                CommandInvocation::from_action("rename_session", Caller::Keybinding),
                ViewportSnapshot::default(),
                &mut effects,
            ),
            CommandOutcome::Unavailable { .. }
        ));
        assert!(effects.is_empty());
    }

    #[test]
    fn unobserved_mux_resources_do_not_fabricate_completion_targets() {
        let state = test_state();
        let session_id = "unobserved-session";
        let window_id = "unobserved-window";
        let pane_id = "unobserved-pane";

        assert_eq!(
            state.mux_resource_target_for_scope(
                state.binding.scope,
                ResourceKind::Session,
                session_id,
                None,
            ),
            None
        );
        assert_eq!(
            state.mux_resource_target_for_scope(
                state.binding.scope,
                ResourceKind::MuxWindow,
                session_id,
                Some(window_id),
            ),
            None
        );
        assert_eq!(
            state.mux_pane_resource_target_for_scope(
                state.binding.scope,
                session_id,
                window_id,
                pane_id,
            ),
            None
        );

        let mut completion = MuxCommandCompletion::default();
        completion.selected_session = Some(session_id.to_owned());
        let value = state.mux_command_completion_value(
            state.binding.scope,
            &MuxCommand::NewWindow {
                session_id: session_id.to_owned(),
                cwd: None,
            },
            &completion,
        );
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn flat_tmux_recursive_completion_uses_authoritative_created_session_reference() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        FlatRecursiveAllocationBackend {
            sessions: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
        .install(&mut state.binding);
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        let pane = || MuxPaneLaunch {
            cwd: cwd.clone(),
            command: None,
            argv: None,
            environment: std::collections::BTreeMap::new(),
            title: None,
        };
        let command = MuxCommand::CreateSession {
            plan: MuxSessionLaunchPlan {
                session_id: "requested".to_owned(),
                focus: false,
                default_cwd: cwd.clone(),
                environment: std::collections::BTreeMap::new(),
                windows: vec![MuxWindowLaunchPlan {
                    name: None,
                    focus: true,
                    layout: MuxPaneLaunchPlan::Split(MuxSplitLaunch {
                        direction: MuxSplitDirection::Right,
                        ratio_millis: 600,
                        first: Box::new(MuxPaneLaunchPlan::Pane(pane())),
                        second: Box::new(MuxPaneLaunchPlan::Pane(pane())),
                    }),
                }],
                focused_window: 0,
            },
        };
        let config = state.active_multiplexer().clone();
        let repaint = state.repaint.clone();
        let scope = state.binding.scope;
        let result = state
            .binding
            .mux
            .execute_command_authoritatively(
                &repaint,
                &config,
                command.clone(),
                Instant::now() + Duration::from_secs(1),
                CommandCancellation::new(),
            )
            .recv_timeout(Duration::from_secs(1))
            .expect("recursive command completion");
        let completion = state
            .binding
            .mux
            .complete_authoritative_command(result, &config)
            .expect("accept authoritative recursive completion");
        let value = state.mux_command_completion_value(scope, &command, &completion);

        let allocated = &value["allocated"];
        let created = value.get("created").expect("created session reference");
        assert_eq!(
            created, &allocated["session"],
            "created must exactly match the authoritative allocated session reference"
        );
        let session_path = serde_json::from_str::<Vec<String>>(
            allocated["session"]["handle"]
                .as_str()
                .expect("session handle"),
        )
        .expect("decode exact session path");
        assert_eq!(session_path.last().map(String::as_str), Some("$recursive"));
        assert_eq!(allocated["session"]["kind"], "session");
        assert_eq!(allocated["session"]["generation"], "1");
        let windows = allocated["windows"].as_array().expect("allocated windows");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["window"]["kind"], "mux_window");
        let window_path = serde_json::from_str::<Vec<String>>(
            windows[0]["window"]["handle"]
                .as_str()
                .expect("window handle"),
        )
        .expect("decode exact window path");
        assert_eq!(window_path.last().map(String::as_str), Some("@recursive"));
        assert_eq!(windows[0]["window"]["generation"], "1");
        let panes = windows[0]["panes"].as_array().expect("allocated panes");
        assert_eq!(panes.len(), 2);
        assert_eq!(
            panes
                .iter()
                .map(|pane| {
                    let path = serde_json::from_str::<Vec<String>>(
                        pane["handle"].as_str().expect("pane handle"),
                    )
                    .expect("decode exact pane path");
                    path.last().cloned().expect("pane id")
                })
                .collect::<Vec<_>>(),
            vec!["%first", "%second"]
        );
        assert!(
            panes
                .iter()
                .all(|pane| pane["generation"] == serde_json::json!("1"))
        );
    }

    fn refresh_selector_test_sessions(
        binding: &mut BindingRuntime,
        repaint: &RepaintHandle,
        expected_sessions: usize,
    ) {
        let config = binding.multiplexer.clone();
        binding.mux.refresh_on_next_frame();
        for _ in 0..100 {
            binding.mux.refresh_sessions(repaint, &config);
            if binding.mux.all_sessions().len() == expected_sessions {
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("scripted selector sessions did not refresh");
    }

    #[test]
    fn canonical_session_select_returns_scoped_candidates_without_selecting_a_collision() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        ScriptedBackend::with(vec![
            session_with("first-id", "shared", "/first"),
            session_with("shared", "second", "/second"),
        ])
        .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 2);
        let selected_before = state.binding.mux.selected_session().map(str::to_owned);
        let binding_handle = state
            .current_command_target(ResourceKind::Binding)
            .expect("current binding target")
            .handle;

        let outcome = state.dispatch_command(
            CommandInvocation {
                command: "session.select".to_owned(),
                arguments: vec!["shared".to_owned()],
                caller: Caller::Socket,
                target: None,
                confirmation: None,
            },
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );

        let CommandOutcome::Ambiguous {
            candidates,
            message,
        } = outcome
        else {
            panic!("expected an ambiguity outcome");
        };
        assert!(message.contains("shared"));
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| {
                    assert_eq!(candidate.kind, ResourceKind::Session);
                    assert!(candidate.generation > 0);
                    let path = serde_json::from_str::<Vec<String>>(&candidate.handle)
                        .expect("session resource path");
                    assert_eq!(path[0], binding_handle);
                    path[1].clone()
                })
                .collect::<Vec<_>>(),
            vec!["first-id", "shared"]
        );
        assert_eq!(
            state.binding.mux.selected_session(),
            selected_before.as_deref(),
            "a collision must not silently change selection"
        );
    }

    #[test]
    fn canonical_session_select_stays_in_the_target_binding_and_rejects_stale_generations() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        ScriptedBackend::with(vec![session_with("local-id", "remote-two", "/local")])
            .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 1);
        let local_scope = state.binding.scope;
        let remote_scope = MuxScope::new(
            local_scope.space_id(),
            BindingId::from_persistence(
                local_scope
                    .binding_id()
                    .persistence_value()
                    .saturating_add(1_000),
            ),
        );
        ensure_test_binding(
            &state.config().config_path,
            remote_scope,
            selected_backend(&state.config().multiplexer),
        );

        let mut remote = BindingRuntime::new(
            remote_scope,
            state.config(),
            state.active_appearance_variant,
            state.repaint.clone(),
        )
        .expect("create remote binding");
        remote.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        ScriptedBackend::with(vec![
            session_with("remote-id", "remote-one", "/remote-one"),
            session_with("remote-second", "remote-two", "/remote-two"),
        ])
        .install(&mut remote);
        refresh_selector_test_sessions(&mut remote, &state.repaint, 2);
        state.inactive_bindings.push(remote);

        let binding_target = {
            let binding = state
                .binding_runtime(remote_scope)
                .expect("remote binding runtime");
            let space = remote_scope.space_id().persistence_value().to_string();
            let binding_id = remote_scope.binding_id().persistence_value().to_string();
            CommandTarget {
                kind: ResourceKind::Binding,
                handle: serde_json::to_string(&(
                    &state.command_instance_handle,
                    &state.window_state_key,
                    state.command_window_generation,
                    &space,
                    &binding_id,
                    binding.mux.binding_generation(),
                ))
                .expect("serialize remote binding target"),
                generation: binding.mux.binding_generation(),
            }
        };
        let outcome = state.dispatch_command(
            CommandInvocation {
                command: "session.select".to_owned(),
                arguments: vec!["remote-two".to_owned()],
                caller: Caller::Socket,
                target: Some(binding_target.clone()),
                confirmation: None,
            },
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        let CommandOutcome::Success { value, .. } = outcome else {
            panic!("remote selector did not succeed");
        };
        let focused = serde_json::from_value::<CommandTarget>(value["focused"].clone())
            .expect("focused session target");
        let focused_path =
            serde_json::from_str::<Vec<String>>(&focused.handle).expect("focused session path");
        assert_eq!(focused.kind, ResourceKind::Session);
        assert_eq!(focused_path[0], binding_target.handle.as_str());
        assert_eq!(focused_path[1], "remote-second");
        assert_eq!(state.binding.scope, local_scope);
        assert_eq!(state.binding.mux.selected_session(), Some("local-id"));
        assert_eq!(
            state
                .binding_runtime(remote_scope)
                .expect("remote binding")
                .mux
                .selected_session(),
            Some("remote-second")
        );

        let mut stale_target = binding_target;
        stale_target.generation = stale_target.generation.saturating_add(1);
        let outcome = state.dispatch_command(
            CommandInvocation {
                command: "session.select".to_owned(),
                arguments: vec!["remote-one".to_owned()],
                caller: Caller::Socket,
                target: Some(stale_target),
                confirmation: None,
            },
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        assert!(matches!(outcome, CommandOutcome::StaleTarget { .. }));
        assert_eq!(
            state
                .binding_runtime(remote_scope)
                .expect("remote binding")
                .mux
                .selected_session(),
            Some("remote-second"),
            "a stale binding target must not retarget its current selection"
        );
    }

    #[test]
    fn new_tab_command_creates_a_session_when_none_is_selected() {
        let mut state = test_state();
        assert!(state.binding.mux.selected_session().is_none());
        let empty_session_target = state
            .current_command_target_for("window.create", ResourceKind::Session)
            .expect("new tab must bind the empty session slot");

        let outcome = state.dispatch_command(
            CommandInvocation::from_action("new_tab", Caller::Keybinding),
            ViewportSnapshot::default(),
            &mut Vec::new(),
        );

        assert!(matches!(outcome, CommandOutcome::Pending { .. }));
        await_authoritative_commands(&mut state);
        assert!(state.binding.mux.selected_session().is_some());

        let mut delayed = CommandInvocation::from_action("new_tab", Caller::CommandPalette);
        delayed.target = Some(empty_session_target);
        assert!(matches!(
            state.dispatch_command(delayed, ViewportSnapshot::default(), &mut Vec::new()),
            CommandOutcome::StaleTarget { .. }
        ));
    }

    #[test]
    fn copy_command_remains_available_before_mux_selection() {
        let mut state = test_state();
        assert!(state.binding.mux.selected_session().is_none());
        let mut effects = Vec::new();

        let outcome = state.dispatch_command(
            CommandInvocation::from_action("copy_to_clipboard", Caller::Keybinding),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(outcome, CommandOutcome::success());
        assert_eq!(effects, [AppEffect::RequestCopy]);
    }

    #[test]
    fn font_size_decrease_clamps_at_one_and_emits_text_config() {
        let mut state = test_state();
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::Font(FontSizeAction::Decrease(10_000.0)),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().font.size, 1.0);
        assert!(matches!(
            effects.as_slice(),
            [AppEffect::SetTerminalTextConfig(_)]
        ));
    }
    #[test]
    fn repeated_font_size_steps_coalesce_renderer_reconfiguration() {
        let mut state = test_state();
        let initial_size = state.config().font.size;
        let mut effects = Vec::new();

        state.apply_keybind_action(
            KeybindAction::Font(FontSizeAction::Increase(0.25)),
            ViewportSnapshot::default(),
            &mut effects,
        );
        state.apply_keybind_action(
            KeybindAction::Font(FontSizeAction::Increase(0.25)),
            ViewportSnapshot::default(),
            &mut effects,
        );

        assert_eq!(state.config().font.size, initial_size + 0.5);
        assert!(matches!(
            effects.as_slice(),
            [AppEffect::SetTerminalTextConfig(config)]
                if config.font_size == initial_size + 0.5
        ));
    }

    #[test]
    fn local_file_handoff_is_typed_and_non_mutating_on_rejection() {
        assert_eq!(
            local_file_handoff(&[]),
            LocalFileHandoff::Rejected("file handoff ignored: no local files")
        );
        assert_eq!(
            local_file_handoff(&[PathBuf::from("/definitely/missing/bootty-handoff")]),
            LocalFileHandoff::Rejected("file handoff rejected: local path is unavailable")
        );

        let file = tempfile::NamedTempFile::new().expect("temp file");
        assert!(matches!(
            local_file_handoff(&[file.path().to_path_buf()]),
            LocalFileHandoff::Ready(_)
        ));

        let mut state = test_state();
        state.last_error = None;
        assert_eq!(state.handle_dropped_file_paths(Vec::new()), 0);
        assert_eq!(state.last_error(), None);

        state.binding.multiplexer.remote = Some(crate::config::SshRemoteConfig::for_host("remote"));
        state.last_error = None;
        assert_eq!(state.handle_dropped_file_paths(Vec::new()), 0);
        assert_eq!(state.last_error(), None);
        assert_eq!(
            state.handle_dropped_file_paths(vec![file.path().to_path_buf()]),
            0
        );
        assert_eq!(
            state.last_error(),
            Some("File handoff to remote Spaces is not supported.")
        );
    }

    #[test]
    fn reload_with_unreadable_config_rejects_and_keeps_previous_config() {
        let mut state = test_state();
        let previous_title = state.config().window.title.clone();
        let mut effects = Vec::new();

        // Default config_path points at a location the test never writes, so
        // the reload must take the rejection path.
        let reloaded = state.reload_config(&mut effects);

        if reloaded {
            // A real user config exists on this machine; the reload accepting
            // it is correct behavior, nothing to assert against.
            return;
        }
        assert!(state.last_error().is_some());
        assert_eq!(state.config().window.title, previous_title);
        assert!(effects.is_empty());
    }

    #[test]
    fn app_command_channel_reads_the_active_terminal() {
        let mut state = test_state();
        let sender = state.app_command_sender(Caller::Socket);
        let (response, response_rx) = mpsc::channel();
        sender
            .try_send(crate::commands::AppCommandRequest {
                invocation: CommandInvocation::from_action("terminal.read", Caller::Socket),
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: crate::commands::CommandCancellation::new(),
                response,
                completion: None,
            })
            .unwrap();

        state.update_frame(test_frame_inputs(Vec::new(), None));

        let CommandOutcome::Success { value, .. } = response_rx.recv().unwrap() else {
            panic!("terminal.read failed");
        };
        assert!(value["cols"].is_number());
        assert!(value["rows"].is_number());
        assert!(value["text"].is_string());
    }

    fn scripted_two_pane_state() -> AppState {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        state.binding.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        ScriptedBackend::with(vec![
            session_with_window_and_pane("current", "current", "/current"),
            session_with_window_and_pane("other", "other", "/other"),
        ])
        .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 2);
        state
    }

    #[test]
    fn non_current_terminal_and_pane_targets_are_rejected_without_retargeting() {
        let mut state = scripted_two_pane_state();
        let scope = state.binding.scope;
        let terminal = state
            .mux_terminal_resource_target_for_scope(
                scope,
                "other",
                "other-window",
                "other-pane",
                "other-terminal",
            )
            .expect("other terminal target");
        let pane = state
            .mux_pane_resource_target_for_scope(scope, "other", "other-window", "other-pane")
            .expect("other pane target");
        state.input_focus = InputFocus::Sidebar;

        for (command, target) in [("terminal.read", terminal), ("pane.focus", pane)] {
            let mut invocation = CommandInvocation::from_action(command, Caller::Socket);
            invocation.target = Some(target);
            assert!(
                matches!(
                    state.dispatch_command(
                        invocation,
                        ViewportSnapshot::default(),
                        &mut Vec::new()
                    ),
                    CommandOutcome::Unsupported { .. }
                ),
                "{command} must not silently act on the current terminal or pane"
            );
        }
        assert_eq!(state.input_focus, InputFocus::Sidebar);
    }

    #[test]
    fn targeted_window_create_uses_the_target_session_anchor_cwd() {
        let mut state = scripted_two_pane_state();
        let scope = state.binding.scope;
        let target = state
            .mux_resource_target_for_scope(scope, ResourceKind::Session, "other", None)
            .expect("other session target");

        assert_eq!(
            state
                .mux_command_for_command(MuxKeyAction::NewTab, Some(&target), scope)
                .expect("targeted tab command"),
            Some(MuxCommand::NewWindow {
                session_id: "other".to_owned(),
                cwd: Some("/other".to_owned()),
            })
        );
    }

    #[test]
    fn targeted_pane_navigation_stays_in_the_target_window() {
        let mut state = scripted_two_pane_state();
        let scope = state.binding.scope;
        let target = state
            .mux_pane_resource_target_for_scope(scope, "other", "other-window", "other-pane")
            .expect("other pane target");

        assert_eq!(
            state
                .mux_command_for_command(MuxKeyAction::NextPane, Some(&target), scope)
                .expect("targeted pane command"),
            Some(MuxCommand::SelectNextPane {
                session_id: "other".to_owned(),
                window_id: Some("other-window".to_owned()),
            })
        );
    }

    #[test]
    fn new_window_palette_action_stays_local_to_the_new_session_picker() {
        let mut state = test_state();

        state.apply_command_palette_event(
            CommandPaletteDialog::open(&[]),
            CommandPaletteEvent::Run(crate::action_catalog::Command::NewWindow),
        );

        assert!(state.take_dialog().is_some());
        assert!(state.pending_command.is_none());

        assert_eq!(
            state.dispatch_command(
                CommandInvocation::from_action("new_window", Caller::Keybinding),
                ViewportSnapshot::default(),
                &mut Vec::new(),
            ),
            CommandOutcome::success()
        );
        assert!(state.take_dialog().is_some());
    }

    #[test]
    fn reload_applies_window_title_change_as_effect() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").expect("write empty config");

        let config = BoottyConfig {
            config_path: path.clone(),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");

        std::fs::write(&path, "[window]\ntitle = \"renamed\"\n").expect("write config");
        let mut effects = Vec::new();
        let reloaded = state.reload_config(&mut effects);

        assert!(reloaded);
        assert!(
            effects.contains(&AppEffect::SetWindowTitle("renamed".to_owned())),
            "{effects:?}"
        );
        assert_eq!(state.config().window.title, "renamed");
    }

    #[test]
    fn reload_applies_valid_font_with_ignored_ghostty_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "").expect("write empty config");
        let config = BoottyConfig {
            config_path: path.clone(),
            ..BoottyConfig::default()
        };
        let repaint: RepaintHandle = std::sync::Arc::new(|| {});
        let mut state = AppState::new(config, repaint, None, None).expect("state");

        std::fs::write(&path, "background-opacity = 0.9\n[font]\nsize = 17.0\n")
            .expect("write config");
        let mut effects = Vec::new();

        assert!(state.reload_config(&mut effects));
        assert_eq!(state.config().font.size, 17.0);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect, AppEffect::SetTerminalTextConfig(_)))
        );
        assert!(
            state
                .last_error()
                .is_some_and(|error| error.contains("background-opacity"))
        );
    }

    #[test]
    fn automation_state_rebase_keeps_latest_state_per_target_after_gap() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(201),
            BindingId::from_persistence(201),
        );
        let mut binding = test_binding_runtime(scope);
        let context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let automation = AutomationHub::new();
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let event_scope = automation_event_scope(scope);
        automation
            .events()
            .replace_live_binding_scopes([event_scope.clone()]);
        let topics = ["terminal.title_changed".to_owned()]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let subscription = automation
            .events()
            .subscribe(owner.clone(), topics, event_scope)
            .expect("subscribe to title changes")
            .subscription;
        let targets = [
            MuxEventTarget::pane("$1", "@1", "%1", "t1", None),
            MuxEventTarget::pane("$2", "@2", "%2", "t2", None),
        ];
        for revision in 0..70 {
            let target = targets[revision % targets.len()].clone();
            let observation = observed_mux_event(
                scope,
                revision as u64 + 1,
                MuxEventTopic::PaneTitleChanged,
                Some(target),
                MuxEventPayload::Title {
                    old_title: None,
                    new_title: Some(format!("title-{revision}")),
                },
                ObservationGenerations {
                    binding: binding.mux.binding_generation(),
                    target: Some(1),
                    retired_target: None,
                },
            );
            publish_mux_event(&automation, &mut binding, &context, &observation)
                .expect("publish title change");
        }
        assert!(
            automation.events().poll(&subscription, &owner, 0).is_err(),
            "the bounded subscription must report a gap"
        );
        let rebase = automation
            .events()
            .rebase(&subscription, &owner)
            .expect("rebase title subscription");
        let targets = rebase.snapshot.snapshots["terminal.title_changed"]["targets"]
            .as_array()
            .expect("title targets");
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0]["target"]["terminal_id"], json!("t1"));
        assert_eq!(targets[0]["value"]["new_title"], json!("title-68"));
        assert_eq!(targets[1]["target"]["terminal_id"], json!("t2"));
        assert_eq!(targets[1]["value"]["new_title"], json!("title-69"));
    }

    #[test]
    fn automation_state_close_retires_only_the_exact_generation() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(202),
            BindingId::from_persistence(202),
        );
        let mut binding = test_binding_runtime(scope);
        let target = MuxEventTarget::pane("$1", "@1", "%1", "t1", None);
        for generation in [1, 2] {
            let binding_generation = binding.mux.binding_generation();
            let observation = observed_mux_event(
                scope,
                generation,
                MuxEventTopic::PaneTitleChanged,
                Some(target.clone()),
                MuxEventPayload::Title {
                    old_title: None,
                    new_title: Some(format!("title-{generation}")),
                },
                ObservationGenerations {
                    binding: binding_generation,
                    target: Some(generation),
                    retired_target: None,
                },
            );
            apply_automation_terminal_event_state(&mut binding, &observation);
        }
        let binding_generation = binding.mux.binding_generation();
        let observation = observed_mux_event(
            scope,
            3,
            MuxEventTopic::PaneClosed,
            Some(target),
            MuxEventPayload::Closed {
                reason: "test".to_owned(),
            },
            ObservationGenerations {
                binding: binding_generation,
                target: Some(2),
                retired_target: Some(1),
            },
        );
        apply_automation_terminal_event_state(&mut binding, &observation);
        let snapshot = automation_binding_state_snapshot(
            &binding,
            &AutomationTargetContext {
                process: state.command_instance_handle.clone(),
                window_state_key: state.window_state_key.clone(),
                window_generation: state.command_window_generation,
            },
            "terminal.title_changed",
        );
        let targets = snapshot["targets"].as_array().expect("title targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0]["generation"], json!(2));
        assert_eq!(targets[0]["value"]["new_title"], json!("title-2"));
    }

    #[test]
    fn automation_state_rebase_marks_unavailable_backend_fields_explicitly() {
        let mut state = test_state_with_config(|config| {
            config.multiplexer.backend = MultiplexerBackendConfig::Tmux;
        });
        ScriptedBackend::with(vec![session_with_window_and_pane("s1", "session", "/repo")])
            .install(&mut state.binding);
        refresh_selector_test_sessions(&mut state.binding, &state.repaint, 1);
        let _ = state.binding.mux.take_refresh_completed();
        rebase_automation_terminal_states(&mut state.binding);
        let context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let title =
            automation_binding_state_snapshot(&state.binding, &context, "terminal.title_changed");
        let title_target = &title["targets"][0];
        assert_eq!(title_target["availability"], json!("unknown"));
        assert_eq!(title_target["value"], Value::Null);
        assert_eq!(
            title_target["reason"],
            json!(AUTOMATION_UNKNOWN_STATE_REASON)
        );
        let cwd =
            automation_binding_state_snapshot(&state.binding, &context, "terminal.cwd_changed");
        assert_eq!(cwd["targets"][0]["availability"], json!("available"));
        assert_eq!(cwd["targets"][0]["value"]["new_cwd"], json!("/repo"));
    }

    #[test]
    fn automation_state_topics_never_reuse_another_topic_snapshot() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(203),
            BindingId::from_persistence(203),
        );
        let mut binding = test_binding_runtime(scope);
        let context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let target = MuxEventTarget::pane("$1", "@1", "%1", "t1", None);
        let observation = observed_mux_event(
            scope,
            1,
            MuxEventTopic::PaneTitleChanged,
            Some(target),
            MuxEventPayload::Title {
                old_title: None,
                new_title: Some("title".to_owned()),
            },
            ObservationGenerations {
                binding: binding.mux.binding_generation(),
                target: Some(1),
                retired_target: None,
            },
        );
        apply_automation_terminal_event_state(&mut binding, &observation);
        let title = automation_binding_state_snapshot(&binding, &context, "terminal.title_changed");
        let options =
            automation_binding_state_snapshot(&binding, &context, "terminal.options_changed");
        assert_eq!(title["targets"][0]["value"]["kind"], json!("title"));
        assert_eq!(options["targets"][0]["availability"], json!("unknown"));
        assert_eq!(options["targets"][0]["value"], Value::Null);
    }
    #[test]
    fn automation_event_queue_rebases_after_lifecycle_overflow() {
        let scope = MuxScope::new(
            SpaceId::from_persistence(204),
            BindingId::from_persistence(204),
        );
        let mut binding = test_binding_runtime(scope);
        let binding_generation = binding.mux.binding_generation();
        let target = MuxEventTarget::pane("$1", "@1", "%1", "t1", None);
        for revision in 1..=257 {
            enqueue_automation_event(
                &mut binding,
                observed_mux_event(
                    scope,
                    revision,
                    MuxEventTopic::PaneClosed,
                    Some(target.clone()),
                    MuxEventPayload::Closed {
                        reason: "overflow".to_owned(),
                    },
                    ObservationGenerations {
                        binding: binding_generation,
                        target: Some(1),
                        retired_target: Some(1),
                    },
                ),
            );
        }
        assert_eq!(
            binding.pending_automation_events.len(),
            AUTOMATION_PENDING_EVENT_LIMIT
        );
        assert!(binding.automation_event_refresh_pending);
    }
    #[test]
    fn automation_stale_terminal_events_publish_without_repopulating_rebased_cache() {
        let state = test_state_with_config(|_| {});
        let scope = MuxScope::new(
            SpaceId::from_persistence(205),
            BindingId::from_persistence(205),
        );
        let mut binding = test_binding_runtime(scope);
        let context = AutomationTargetContext {
            process: state.command_instance_handle.clone(),
            window_state_key: state.window_state_key.clone(),
            window_generation: state.command_window_generation,
        };
        let automation = AutomationHub::new();
        automation
            .events()
            .replace_live_binding_scopes([automation_event_scope(scope)]);
        let owner = crate::automation::OwnerIdentity::new(1, 1);
        let subscription = automation
            .events()
            .subscribe(
                owner.clone(),
                [
                    "terminal.process_changed".to_owned(),
                    "terminal.title_changed".to_owned(),
                    "backend.rebased".to_owned(),
                ]
                .into_iter()
                .collect(),
                automation_event_scope(scope),
            )
            .expect("subscribe to stale event topics")
            .subscription;
        let binding_generation = binding.mux.binding_generation();
        let new_target = MuxEventTarget::pane("$new", "@new", "%new", "t-new", None);
        let new_observation = observed_mux_event(
            scope,
            1,
            MuxEventTopic::PaneStateChanged,
            Some(new_target),
            MuxEventPayload::PaneState {
                state: MuxPaneState {
                    title: Some("new-title".to_owned()),
                    options: Vec::new(),
                    foreground: Some(MuxForegroundState {
                        pid: Some(2),
                        command: Some("new-process".to_owned()),
                        cwd: Some("/new".to_owned()),
                        executable: None,
                    }),
                },
            },
            ObservationGenerations {
                binding: binding_generation,
                target: Some(2),
                retired_target: None,
            },
        );
        apply_automation_terminal_event_state(&mut binding, &new_observation);

        let stale_binding_generation = binding_generation
            .checked_sub(1)
            .unwrap_or_else(|| binding_generation.saturating_add(1));
        let old_target = MuxEventTarget::pane("$old", "@old", "%old", "t-old", None);
        let old_state = observed_mux_event(
            scope,
            2,
            MuxEventTopic::PaneStateChanged,
            Some(old_target.clone()),
            MuxEventPayload::PaneState {
                state: MuxPaneState {
                    title: Some("old-title".to_owned()),
                    options: Vec::new(),
                    foreground: Some(MuxForegroundState {
                        pid: Some(1),
                        command: Some("old-process".to_owned()),
                        cwd: Some("/old".to_owned()),
                        executable: None,
                    }),
                },
            },
            ObservationGenerations {
                binding: stale_binding_generation,
                target: Some(1),
                retired_target: None,
            },
        );
        let old_title = observed_mux_event(
            scope,
            3,
            MuxEventTopic::PaneTitleChanged,
            Some(old_target),
            MuxEventPayload::Title {
                old_title: None,
                new_title: Some("old-title-delta".to_owned()),
            },
            ObservationGenerations {
                binding: stale_binding_generation,
                target: Some(1),
                retired_target: None,
            },
        );
        publish_mux_event(&automation, &mut binding, &context, &old_state)
            .expect("publish stale pane state");
        publish_mux_event(&automation, &mut binding, &context, &old_title)
            .expect("publish stale title");
        let rebase = observed_mux_event(
            scope,
            4,
            MuxEventTopic::SnapshotRebased,
            None,
            MuxEventPayload::Rebase {
                reason: MuxRebaseReason::Reconnect,
            },
            ObservationGenerations {
                binding: binding_generation,
                target: None,
                retired_target: None,
            },
        );
        publish_mux_event(&automation, &mut binding, &context, &rebase)
            .expect("publish reconnect rebase");

        let snapshot =
            automation_binding_state_snapshot(&binding, &context, "terminal.title_changed");
        let targets = snapshot["targets"].as_array().expect("title targets");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0]["target"]["terminal_id"], json!("t-new"));
        assert_eq!(targets[0]["generation"], json!(2));
        assert_eq!(targets[0]["value"]["new_title"], json!("new-title"));
        let delivery = automation
            .events()
            .poll(&subscription, &owner, 0)
            .expect("poll stale event delivery");
        assert!(
            delivery
                .events
                .iter()
                .any(|event| event.topic == "terminal.process_changed")
        );
        assert!(
            delivery
                .events
                .iter()
                .any(|event| event.topic == "terminal.title_changed")
        );
        assert!(
            delivery
                .events
                .iter()
                .any(|event| event.topic == "backend.rebased")
        );
    }
}
