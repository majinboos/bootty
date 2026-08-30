use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use bootty_mux::terminal::TerminalRuntime;
use bootty_mux::{
    MuxBackendKind, MuxBindingConfig,
    backend::MuxBackend,
    capability::BindingCapabilityDescriptor,
    command::MuxCommand,
    controller::SpaceId,
    provider::{
        GeneratedSessionNamePolicy, MuxAppBackendPolicy, MuxAppBackendProvider, MuxBackendProvider,
        MuxBackendRegistry, MuxCommandDispatch, PaneBehavior, PaneTopology, PersistedSessionPolicy,
        SelectionPublicationPolicy, TerminalProgressPolicy, TerminalResidency,
    },
    snapshot::{MuxPaneAnchor, MuxSnapshot},
    terminal::{BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest},
};
use bootty_runtime::{frame_source::TerminalFrameSource, terminal_session::DrainStats};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;

struct CachedPaneRuntime {
    fail_live_config: bool,
    exit_signal: Arc<AtomicBool>,
    tracks_exit: bool,
}

macro_rules! terminal_runtime_stubs {
    () => {
        fn drain_pty(&mut self) -> DrainStats {
            DrainStats::default()
        }
        fn pending_pty_len(&self) -> usize {
            0
        }
        fn child_exited(&mut self) -> Result<bool> {
            Ok(self.tracks_exit && self.exit_signal.load(Ordering::SeqCst))
        }
        fn tty_name(&self) -> Option<&str> {
            None
        }
        fn discard_pending_output(&mut self) -> Result<()> {
            Ok(())
        }
        fn force_resize(&mut self) -> Result<()> {
            Ok(())
        }
        fn format_selection(&mut self, _: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn current_working_directory(&mut self) -> Result<Option<String>> {
            Ok(None)
        }
        fn is_mouse_tracking(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn scroll_viewport_delta(&mut self, _: isize) -> Result<()> {
            Ok(())
        }
        fn enter_copy_mode(&mut self) -> Result<()> {
            Ok(())
        }
        fn copy_mode_active(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn handle_copy_mode_action(
            &mut self,
            _: TerminalCopyModeAction,
        ) -> Result<TerminalCopyModeOutcome> {
            Ok(TerminalCopyModeOutcome::default())
        }
        fn search_viewport(&mut self, _: &str, _: TerminalSearchDirection) -> Result<bool> {
            Ok(false)
        }
        fn begin_selection(&mut self, _: TerminalSelectionEvent) -> Result<()> {
            Ok(())
        }
        fn update_selection(&mut self, _: TerminalSelectionEvent) -> Result<()> {
            Ok(())
        }
        fn end_selection(&mut self, _: Option<TerminalSelectionEvent>) -> Result<()> {
            Ok(())
        }
        fn write_input(&mut self, _: &[u8]) -> Result<()> {
            Ok(())
        }
        fn write_paste(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn encode_key(&mut self, _: KeyInput) -> Result<()> {
            Ok(())
        }
        fn encode_focus(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn encode_mouse(&mut self, _: MouseInput) -> Result<()> {
            Ok(())
        }
        fn handle_mouse_wheel(&mut self, _: MouseInput, _: isize) -> Result<()> {
            Ok(())
        }
    };
}

impl TerminalFrameSource for CachedPaneRuntime {
    fn set_display_scale(&mut self, _: f32) -> Result<()> {
        Ok(())
    }
    fn set_render_cell_metrics(&mut self, _: CellMetrics) -> Result<()> {
        Ok(())
    }
    fn resize(&mut self, _: TerminalGeometry) -> Result<()> {
        Ok(())
    }
    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::default())
    }
}

impl TerminalRuntime for CachedPaneRuntime {
    terminal_runtime_stubs!();
    fn apply_live_config(&mut self, _config: TerminalLiveConfig) -> Result<()> {
        if self.fail_live_config {
            anyhow::bail!("cached pane is gone")
        }
        Ok(())
    }
}

#[derive(Arbitrary, Debug)]
struct ScopedPaneInput {
    first_scope: i64,
    second_scope: i64,
    pane_id: String,
}

proptest! {
    /// Property: scoped pane encoding is injective, and decoding is its left inverse.
    #[test]
    fn scoped_pane_ids_round_trip_without_cross_space_collisions(input in any::<ScopedPaneInput>()) {
    use bootty_mux::{
        controller::SpaceId,
        terminal::{decode_scoped_pane_id, encode_scoped_pane_id},
    };

    prop_assume!(input.first_scope != input.second_scope);
    let first = SpaceId::from_persistence(input.first_scope);
    let second = SpaceId::from_persistence(input.second_scope);

    let first_id = encode_scoped_pane_id(first, &input.pane_id);
    let second_id = encode_scoped_pane_id(second, &input.pane_id);

    prop_assert_ne!(&first_id, &second_id);
    prop_assert_eq!(
        decode_scoped_pane_id(&first_id),
        Some((first, input.pane_id.clone()))
    );
    }
}

struct EmptyBackend;

impl MuxBackend for EmptyBackend {
    fn snapshot(&self) -> Result<MuxSnapshot> {
        Ok(MuxSnapshot::default())
    }
    fn execute(&mut self, _: MuxCommand) -> Result<()> {
        Ok(())
    }
}

struct StaleCacheProvider {
    starts: Arc<AtomicUsize>,
    exit_signal: Arc<AtomicBool>,
}

struct StaleCachePolicy {
    starts: Arc<AtomicUsize>,
    exit_signal: Arc<AtomicBool>,
}

impl MuxBackendProvider for StaleCacheProvider {
    fn kind(&self) -> MuxBackendKind {
        MuxBackendKind::Native
    }
    fn command_dispatch(&self) -> MuxCommandDispatch {
        MuxCommandDispatch::CallerThread
    }
    fn build_backend(&self, _: &MuxBindingConfig, _: Option<&Path>) -> Box<dyn MuxBackend> {
        Box::new(EmptyBackend)
    }
}

impl MuxAppBackendProvider for StaleCacheProvider {
    fn build_pane_policy(&self, _config: &MuxBindingConfig) -> Box<dyn BackendPanePolicy> {
        Box::new(StaleCachePolicy {
            starts: Arc::clone(&self.starts),
            exit_signal: Arc::clone(&self.exit_signal),
        })
    }

    fn app_policy(&self) -> MuxAppBackendPolicy {
        MuxAppBackendPolicy {
            panes: PaneBehavior {
                topology: PaneTopology::BackendReconciled,
                cache_terminals: true,
                resize_cached_terminals: false,
            },
            progress: TerminalProgressPolicy::TerminalOsc,
            persisted_sessions: PersistedSessionPolicy::Never,
            generated_session_names: GeneratedSessionNamePolicy::Reconcile,
            terminal_residency: TerminalResidency::BindingScoped,
            selection_publication: SelectionPublicationPolicy::Direct,
        }
    }

    fn capabilities(&self, scope: SpaceId) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [])
    }
}

impl BackendPanePolicy for StaleCachePolicy {
    fn remote_target(&self) -> Option<&bootty_mux::SshTarget> {
        None
    }
    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Some(Box::new(CachedPaneRuntime {
            fail_live_config: request.target.pane_id() == Some("%2"),
            exit_signal: Arc::clone(&self.exit_signal),
            tracks_exit: request.target.pane_id() == Some("%2"),
        })))
    }

    fn sync_target(&mut self, _: Option<&bootty_mux::terminal::ScopedMuxPaneTarget>, _: bool) {}
    fn set_layout_window(&mut self, _: Option<&str>) {}
    fn resize_layout_window(&mut self, _: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }
    fn deactivate(&mut self) {}
}

#[test]
fn failed_cached_runtime_is_retired_without_blocking_live_config_publication() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let exit_signal = Arc::new(AtomicBool::new(true));
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider {
            starts: Arc::clone(&starts),
            exit_signal,
        })],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let pane = |id: &str| MuxPaneAnchor {
        session_id: "session".into(),
        pane_id: Some(id.into()),
        ..MuxPaneAnchor::default()
    };
    let (first, stale) = (pane("%1"), pane("%2"));
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        bootty_runtime::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    );

    terminal.sync_native_window(
        &[first.clone(), stale.clone()],
        Some(&first),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    terminal.apply_live_config(TerminalLiveConfig::default())?;
    assert_eq!(starts.load(Ordering::SeqCst), 2);

    terminal.sync_native_window(
        &[first.clone(), stale.clone()],
        Some(&first),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);
    Ok(())
}

#[test]
fn exited_backend_controller_restarts_with_exponential_cooldown() -> Result<()> {
    let starts = Arc::new(AtomicUsize::new(0));
    let exit_signal = Arc::new(AtomicBool::new(true));
    let registry = Arc::new(MuxBackendRegistry::from_app_providers(
        [Arc::new(StaleCacheProvider {
            starts: Arc::clone(&starts),
            exit_signal: Arc::clone(&exit_signal),
        })],
        [MuxBackendKind::Native],
    )?);
    let config = MuxBindingConfig {
        backend: MuxBackendKind::Native,
        ..MuxBindingConfig::default()
    };
    let pane = |id: &str| MuxPaneAnchor {
        session_id: "session".into(),
        pane_id: Some(id.into()),
        ..MuxPaneAnchor::default()
    };
    let (focused, exited) = (pane("%1"), pane("%2"));
    let panes = [focused.clone(), exited.clone()];
    let mut terminal = bootty_mux::terminal::ActiveTerminal::new(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 10,
            cell_height: 20,
        },
        registry,
        &config,
        bootty_runtime::TerminalSessionConfig::default(),
        Arc::new(|| {}),
    );

    terminal.sync_native_window(
        &panes,
        Some(&focused),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 2);
    let now = Instant::now();
    assert_eq!(
        terminal.recover_exited_native_runtimes(now),
        (Vec::new(), Some(Duration::from_millis(250)))
    );
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(249)),
        (Vec::new(), Some(Duration::from_millis(1)))
    );
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(250)),
        (Vec::new(), None)
    );

    // The backend snapshot still contains the pane, so reconciliation starts a new controller.
    terminal.sync_native_window(
        &panes,
        Some(&focused),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 3);

    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(250)),
        (Vec::new(), Some(Duration::from_millis(500)))
    );
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(750)),
        (Vec::new(), None)
    );
    terminal.sync_native_window(
        &panes,
        Some(&focused),
        Some("window"),
        MuxBackendKind::Native,
        false,
    )?;
    assert_eq!(starts.load(Ordering::SeqCst), 4);

    exit_signal.store(false, Ordering::SeqCst);
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(750)),
        (Vec::new(), None)
    );
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(10_750)),
        (Vec::new(), None)
    );
    exit_signal.store(true, Ordering::SeqCst);
    assert_eq!(
        terminal.recover_exited_native_runtimes(now + Duration::from_millis(10_750)),
        (Vec::new(), Some(Duration::from_millis(250)))
    );
    Ok(())
}
