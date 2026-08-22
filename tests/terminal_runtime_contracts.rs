use std::sync::Arc;

use anyhow::Result;
use bootty_app::{
    geometry::{CellMetrics, SurfacePoint, SurfaceRect, TerminalGeometry, TerminalSurface},
    mux::terminal::TerminalRuntime,
    renderer::TerminalFrameSource,
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalCursorConfig,
        TerminalFeatureConfig, TerminalLiveConfig, TerminalSearchDirection, TerminalSelectionEvent,
        TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
    terminal_session::DrainStats,
};

#[derive(Debug, PartialEq)]
enum Interaction {
    ApplyLiveConfig(TerminalLiveConfig),
    FormatSelection(TerminalSelectionFormat),
    Scroll(isize),
    EnterCopyMode,
    CopyMode(TerminalCopyModeAction),
    Search(String, TerminalSearchDirection),
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
}

#[derive(Default)]
struct ScriptedPaneRuntime {
    interactions: Vec<Interaction>,
}

impl TerminalFrameSource for ScriptedPaneRuntime {
    fn set_display_scale(&mut self, _display_scale: f32) -> Result<()> {
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, _cell: CellMetrics) -> Result<()> {
        Ok(())
    }

    fn resize(&mut self, _geometry: TerminalGeometry) -> Result<()> {
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        Ok(Arc::new(RenderFrame::default()))
    }
}

impl TerminalRuntime for ScriptedPaneRuntime {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
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

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.interactions.push(Interaction::FormatSelection(format));
        Ok(Some(b"selected".to_vec()))
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(None)
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.interactions.push(Interaction::ApplyLiveConfig(config));
        Ok(())
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.interactions.push(Interaction::Scroll(delta));
        Ok(())
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.interactions.push(Interaction::EnterCopyMode);
        Ok(())
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.interactions.push(Interaction::CopyMode(action));
        Ok(TerminalCopyModeOutcome::default())
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.interactions
            .push(Interaction::Search(query.to_owned(), direction));
        Ok(true)
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.interactions.push(Interaction::SelectionBegin(event));
        Ok(())
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.interactions.push(Interaction::SelectionUpdate(event));
        Ok(())
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.interactions.push(Interaction::SelectionEnd(event));
        Ok(())
    }

    fn write_input(&mut self, _bytes: &[u8]) -> Result<()> {
        Ok(())
    }

    fn write_paste(&mut self, _text: &str) -> Result<()> {
        Ok(())
    }

    fn encode_key(&mut self, _input: KeyInput) -> Result<()> {
        Ok(())
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, _input: MouseInput) -> Result<()> {
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, _input: MouseInput, _scroll_delta: isize) -> Result<()> {
        Ok(())
    }
}

fn selection_event(x: f32) -> TerminalSelectionEvent {
    TerminalSelectionEvent {
        surface: TerminalSurface::for_rect(
            SurfaceRect::from_min_size(0.0, 0.0, 100.0, 40.0),
            CellMetrics::new(10.0, 20.0),
        ),
        position: SurfacePoint { x, y: 10.0 },
        rectangle: false,
    }
}

#[test]
fn pane_interactions_reach_the_mux_runtime_boundary() -> Result<()> {
    let mut runtime = ScriptedPaneRuntime::default();
    let begin = selection_event(10.0);
    let update = selection_event(20.0);
    let live_config = TerminalLiveConfig {
        cursor: TerminalCursorConfig {
            style: None,
            blink: Some(false),
        },
        features: TerminalFeatureConfig {
            glyph_protocol: false,
        },
        ..TerminalLiveConfig::default()
    };

    runtime.apply_live_config(live_config.clone())?;
    assert_eq!(
        runtime.format_selection(TerminalSelectionFormat::PlainText)?,
        Some(b"selected".to_vec())
    );
    assert!(runtime.is_mouse_tracking()?);
    runtime.scroll_viewport_delta(-3)?;
    runtime.enter_copy_mode()?;
    assert!(runtime.copy_mode_active()?);
    runtime.handle_copy_mode_action(TerminalCopyModeAction::SelectLine)?;
    assert!(runtime.search_viewport("needle", TerminalSearchDirection::Next)?);
    runtime.begin_selection(begin)?;
    runtime.update_selection(update)?;
    runtime.end_selection(Some(update))?;

    assert_eq!(
        runtime.interactions,
        vec![
            Interaction::ApplyLiveConfig(live_config),
            Interaction::FormatSelection(TerminalSelectionFormat::PlainText),
            Interaction::Scroll(-3),
            Interaction::EnterCopyMode,
            Interaction::CopyMode(TerminalCopyModeAction::SelectLine),
            Interaction::Search("needle".to_owned(), TerminalSearchDirection::Next),
            Interaction::SelectionBegin(begin),
            Interaction::SelectionUpdate(update),
            Interaction::SelectionEnd(Some(update)),
        ]
    );
    Ok(())
}
