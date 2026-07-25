use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::Result;
use bootty_runtime::{
    DrainStats, TerminalSession, TerminalSessionConfig, render_source::TerminalRenderSource,
};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TerminalColorConfig, TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalCursorConfig,
        TerminalFeatureConfig, TerminalSelectionEvent, TerminalSelectionFormat,
    },
    terminal_frame::RenderFrame,
    terminal_input_model::{KeyInput, MouseInput},
};

use super::pane::TerminalRuntime;

enum QueuedStartupCommand {
    RawInput(Vec<u8>),
    Paste(String),
    Key(KeyInput),
    Focus(bool),
    Mouse(MouseInput),
    MouseWheel {
        input: MouseInput,
        scroll_delta: isize,
    },
    ScrollViewport(isize),
    EnterCopyMode,
    SelectionBegin(TerminalSelectionEvent),
    SelectionUpdate(TerminalSelectionEvent),
    SelectionEnd(Option<TerminalSelectionEvent>),
}

pub(super) struct StartingNativeTerminal {
    rx: mpsc::Receiver<std::result::Result<TerminalSession, String>>,
    terminal: Option<TerminalSession>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    pending_colors: Option<TerminalColorConfig>,
    pending_cursor: Option<TerminalCursorConfig>,
    pending_features: Option<TerminalFeatureConfig>,
    pending_commands: VecDeque<QueuedStartupCommand>,
    startup_error: Option<String>,
}

impl StartingNativeTerminal {
    pub(super) fn spawn(
        geometry: TerminalGeometry,
        config: TerminalSessionConfig,
        repaint_wakeup: Arc<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let thread_repaint = Arc::clone(&repaint_wakeup);
        thread::spawn(move || {
            let result =
                TerminalSession::new_with_config(geometry, config, Arc::clone(&thread_repaint))
                    .map_err(|error| error.to_string());
            let _ = tx.send(result);
            thread_repaint();
        });

        Self {
            rx,
            terminal: None,
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
            pending_colors: None,
            pending_cursor: None,
            pending_features: None,
            pending_commands: VecDeque::new(),
            startup_error: None,
        }
    }

    fn ready_terminal(&mut self) -> Result<Option<&mut TerminalSession>> {
        self.poll_startup()?;
        Ok(self.terminal.as_mut())
    }

    fn poll_startup(&mut self) -> Result<()> {
        if self.terminal.is_some() {
            return Ok(());
        }
        if let Some(error) = &self.startup_error {
            anyhow::bail!(error.clone());
        }

        let mut terminal = match self.rx.try_recv() {
            Ok(Ok(terminal)) => terminal,
            Ok(Err(error)) => {
                self.startup_error = Some(error.clone());
                anyhow::bail!(error);
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                let error = "native terminal startup worker stopped".to_owned();
                self.startup_error = Some(error.clone());
                anyhow::bail!(error);
            }
        };

        terminal.resize(self.geometry)?;
        terminal.set_display_scale(self.display_scale)?;
        terminal.set_render_cell_metrics(self.render_cell)?;
        if let Some(colors) = self.pending_colors.clone() {
            terminal.set_colors(colors)?;
        }
        if let Some(cursor) = self.pending_cursor {
            terminal.set_cursor_config(cursor)?;
        }
        if let Some(features) = self.pending_features {
            terminal.set_feature_config(features)?;
        }
        while let Some(command) = self.pending_commands.pop_front() {
            apply_queued_startup_command(&mut terminal, command)?;
        }
        self.terminal = Some(terminal);
        Ok(())
    }

    fn queue_or_apply(&mut self, command: QueuedStartupCommand) -> Result<()> {
        if let Some(terminal) = self.ready_terminal()? {
            apply_queued_startup_command(terminal, command)
        } else {
            self.pending_commands.push_back(command);
            Ok(())
        }
    }
}

fn startup_placeholder_frame(geometry: TerminalGeometry) -> Arc<RenderFrame> {
    let mut frame = RenderFrame {
        cols: geometry.cols,
        rows: geometry.rows,
        row_dirty: vec![true; geometry.rows as usize],
        row_wraps: vec![false; geometry.rows as usize],
        ..RenderFrame::default()
    };
    frame.stats.dirty_rows = geometry.rows as usize;
    Arc::new(frame)
}

fn apply_queued_startup_command(
    terminal: &mut TerminalSession,
    command: QueuedStartupCommand,
) -> Result<()> {
    match command {
        QueuedStartupCommand::RawInput(bytes) => terminal.write_input(&bytes),
        QueuedStartupCommand::Paste(text) => terminal.write_paste(&text),
        QueuedStartupCommand::Key(input) => terminal.encode_key(input),
        QueuedStartupCommand::Focus(gained) => terminal.encode_focus(gained),
        QueuedStartupCommand::Mouse(input) => terminal.encode_mouse(input),
        QueuedStartupCommand::MouseWheel {
            input,
            scroll_delta,
        } => terminal.handle_mouse_wheel(input, scroll_delta),
        QueuedStartupCommand::ScrollViewport(delta) => terminal.scroll_viewport_delta(delta),
        QueuedStartupCommand::EnterCopyMode => terminal.enter_copy_mode(),
        QueuedStartupCommand::SelectionBegin(event) => terminal.begin_selection(event),
        QueuedStartupCommand::SelectionUpdate(event) => terminal.update_selection(event),
        QueuedStartupCommand::SelectionEnd(event) => terminal.end_selection(event),
    }
}

impl TerminalRenderSource for StartingNativeTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        let display_scale = self.display_scale;
        if let Some(terminal) = self.ready_terminal()? {
            terminal.set_display_scale(display_scale)?;
        }
        Ok(())
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.render_cell = cell;
        if let Some(terminal) = self.ready_terminal()? {
            terminal.set_render_cell_metrics(cell)?;
        }
        Ok(())
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.geometry = geometry;
        if let Some(terminal) = self.ready_terminal()? {
            terminal.resize(geometry)?;
        }
        Ok(())
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.extract_frame()
        } else {
            Ok(startup_placeholder_frame(self.geometry))
        }
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.is_mouse_tracking()
        } else {
            Ok(false)
        }
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::ScrollViewport(delta))
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.copy_mode_active()
        } else {
            Ok(false)
        }
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.handle_copy_mode_action(action)
        } else {
            Ok(TerminalCopyModeOutcome::default())
        }
    }

    fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionBegin(event))
    }

    fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionUpdate(event))
    }

    fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::SelectionEnd(event))
    }
}

impl TerminalRuntime for StartingNativeTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        match self.ready_terminal() {
            Ok(Some(terminal)) => terminal.drain_pty(),
            Ok(None) | Err(_) => DrainStats::default(),
        }
    }

    fn pending_pty_len(&self) -> usize {
        self.terminal
            .as_ref()
            .map(TerminalSession::pending_pty_len)
            .unwrap_or_default()
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(self
            .ready_terminal()?
            .map(TerminalSession::child_exited)
            .transpose()?
            .unwrap_or(false))
    }

    fn tty_name(&self) -> Option<&str> {
        self.terminal.as_ref().and_then(TerminalSession::tty_name)
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.pending_commands.clear();
        if let Some(terminal) = self.ready_terminal()? {
            terminal.discard_pending_output()?;
        }
        Ok(())
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.format_selection(format)
        } else {
            Ok(None)
        }
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        Ok(self
            .ready_terminal()?
            .and_then(|terminal| TerminalSession::current_working_directory(&*terminal)))
    }

    fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        self.pending_cursor = Some(cursor);
        if let Some(terminal) = self.ready_terminal()? {
            terminal.set_cursor_config(cursor)?;
        }
        Ok(())
    }

    fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        self.pending_features = Some(features);
        if let Some(terminal) = self.ready_terminal()? {
            terminal.set_feature_config(features)?;
        }
        Ok(())
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.pending_colors = Some(colors.clone());
        if let Some(terminal) = self.ready_terminal()? {
            terminal.set_colors(colors)?;
        }
        Ok(())
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::RawInput(bytes.to_vec()))
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Paste(text.to_owned()))
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Key(input))
    }

    fn encode_focus(&mut self, gained: bool) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Focus(gained))
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::Mouse(input))
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, scroll_delta: isize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::MouseWheel {
            input,
            scroll_delta,
        })
    }
}
