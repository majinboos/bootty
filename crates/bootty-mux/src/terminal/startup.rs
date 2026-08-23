use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    thread,
};

use anyhow::Result;
use bootty_runtime::{
    DrainStats, TerminalSession, TerminalSessionConfig, frame_source::TerminalFrameSource,
};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
use bootty_terminal::{
    terminal_engine::{
        TerminalCopyModeAction, TerminalCopyModeOutcome, TerminalLiveConfig,
        TerminalSearchDirection, TerminalSelectionEvent, TerminalSelectionFormat,
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

pub struct StartingNativeTerminal {
    rx: mpsc::Receiver<std::result::Result<TerminalSession, String>>,
    terminal: Option<TerminalSession>,
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
    pending_live_config: Option<TerminalLiveConfig>,
    pending_commands: VecDeque<QueuedStartupCommand>,
    startup_error: Option<String>,
}

impl StartingNativeTerminal {
    pub fn spawn(
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
            pending_live_config: None,
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
        if let Some(config) = self.pending_live_config.take() {
            terminal.apply_live_config(config)?;
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

    fn with_terminal<T>(
        &mut self,
        pending: T,
        ready: impl FnOnce(&mut TerminalSession) -> Result<T>,
    ) -> Result<T> {
        match self.ready_terminal()? {
            Some(terminal) => ready(terminal),
            None => Ok(pending),
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

impl TerminalFrameSource for StartingNativeTerminal {
    fn set_display_scale(&mut self, display_scale: f32) -> Result<()> {
        self.display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        let display_scale = self.display_scale;
        self.with_terminal((), |terminal| terminal.set_display_scale(display_scale))
    }

    fn set_render_cell_metrics(&mut self, cell: CellMetrics) -> Result<()> {
        self.render_cell = cell;
        self.with_terminal((), |terminal| terminal.set_render_cell_metrics(cell))
    }

    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        self.geometry = geometry;
        self.with_terminal((), |terminal| terminal.resize(geometry))
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.extract_frame()
        } else {
            Ok(startup_placeholder_frame(self.geometry))
        }
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
            .map_or(0, TerminalSession::pending_pty_len)
    }

    fn child_exited(&mut self) -> Result<bool> {
        self.with_terminal(false, TerminalSession::child_exited)
    }

    fn tty_name(&self) -> Option<&str> {
        self.terminal.as_ref().and_then(TerminalSession::tty_name)
    }

    fn discard_pending_output(&mut self) -> Result<()> {
        self.pending_commands.clear();
        self.with_terminal((), TerminalSession::discard_pending_output)
    }

    fn force_resize(&mut self) -> Result<()> {
        Ok(())
    }

    fn format_selection(&mut self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        self.with_terminal(None, |terminal| terminal.format_selection(format))
    }

    fn current_working_directory(&mut self) -> Result<Option<String>> {
        self.with_terminal(None, |terminal| {
            Ok(TerminalSession::current_working_directory(&*terminal))
        })
    }

    fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        if let Some(terminal) = self.ready_terminal()? {
            terminal.apply_live_config(config)?;
        } else {
            // Keep the latest aggregate. Startup applies it once after the worker is ready.
            self.pending_live_config = Some(config);
        }
        Ok(())
    }

    fn is_mouse_tracking(&mut self) -> Result<bool> {
        self.with_terminal(false, TerminalSession::is_mouse_tracking)
    }

    fn scroll_viewport_delta(&mut self, delta: isize) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::ScrollViewport(delta))
    }

    fn enter_copy_mode(&mut self) -> Result<()> {
        self.queue_or_apply(QueuedStartupCommand::EnterCopyMode)
    }

    fn copy_mode_active(&mut self) -> Result<bool> {
        self.with_terminal(false, TerminalSession::copy_mode_active)
    }

    fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        self.with_terminal(TerminalCopyModeOutcome::default(), |terminal| {
            terminal.handle_copy_mode_action(action)
        })
    }

    fn search_viewport(&mut self, query: &str, direction: TerminalSearchDirection) -> Result<bool> {
        self.with_terminal(false, |terminal| terminal.search_viewport(query, direction))
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
