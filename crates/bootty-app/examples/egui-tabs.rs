use bootty_app::{
    geometry::TerminalSurface,
    input::{
        InputSnapshot, TerminalInputCommand, pressed_mouse_button_from_egui,
        terminal_input_commands,
    },
    renderer::TerminalWidget,
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal::TerminalSession,
};
use eframe::{
    egui::{self, RichText},
    wgpu,
};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Bootty Tabs")
            .with_inner_size([980.0, 640.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Bootty Tabs",
        options,
        Box::new(|cc| Ok(Box::new(TabsExample::new(cc)))),
    )
}

struct TabsExample {
    tabs: Vec<TerminalTab>,
    active: usize,
    next_id: usize,
    target_format: Option<wgpu::TextureFormat>,
    repaint_scheduler: RepaintScheduler,
    last_error: Option<String>,
}

impl TabsExample {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let target_format = cc
            .wgpu_render_state
            .as_ref()
            .map(|render_state| render_state.target_format);
        let mut app = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            target_format,
            repaint_scheduler: RepaintScheduler::default(),
            last_error: None,
        };
        app.open_tab(cc.egui_ctx.clone());
        app
    }

    fn open_tab(&mut self, repaint: egui::Context) {
        let id = self.next_id;
        self.next_id += 1;
        match TerminalTab::new(id, self.target_format, repaint) {
            Ok(tab) => {
                self.tabs.push(tab);
                self.active = self.tabs.len().saturating_sub(1);
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn active_tab_mut(&mut self) -> Option<&mut TerminalTab> {
        self.tabs.get_mut(self.active)
    }

    fn drain_terminals(&mut self) -> (usize, u64, usize) {
        let mut close_active = false;
        let mut drained_bytes: usize = 0;
        let mut drain_elapsed_us: u64 = 0;
        let mut pending_bytes: usize = 0;
        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let drain = tab.terminal.drain_pty();
            drained_bytes = drained_bytes.saturating_add(drain.bytes);
            drain_elapsed_us = drain_elapsed_us.saturating_add(drain.elapsed_us);
            pending_bytes = pending_bytes.saturating_add(tab.terminal.pending_pty_len());
            match tab.terminal.child_exited() {
                Ok(true) if index == self.active => close_active = true,
                Ok(true) => {}
                Ok(false) => {}
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }

        if close_active && !self.tabs.is_empty() {
            self.tabs.remove(self.active);
            self.active = self
                .active
                .saturating_sub(1)
                .min(self.tabs.len().saturating_sub(1));
        }
        (drained_bytes, drain_elapsed_us, pending_bytes)
    }

    fn send_active_input(&mut self, ctx: &egui::Context) -> usize {
        let Some(tab) = self.active_tab_mut() else {
            return 0;
        };
        let snapshot = ctx.input(|input| InputSnapshot {
            events: input.events.clone(),
            modifiers: input.modifiers,
            hover_pos: input.pointer.hover_pos(),
            pressed_mouse_button: pressed_mouse_button_from_egui(&input.pointer),
            surface: tab.surface,
            mouse_exclusion: None,
        });
        let mut last_error = None;
        let commands = terminal_input_commands(snapshot);
        let input_commands = commands.len();
        for command in commands {
            let result = match command {
                TerminalInputCommand::Text(text) => tab.terminal.write_input(text.as_bytes()),
                TerminalInputCommand::Paste(text) => tab.terminal.write_paste(&text),
                TerminalInputCommand::Focus(focused) => tab.terminal.encode_focus(focused),
                TerminalInputCommand::Key(key) => tab.terminal.encode_key(key),
                TerminalInputCommand::Mouse(mouse) => tab.terminal.encode_mouse(mouse),
                TerminalInputCommand::MouseWheel {
                    input,
                    scroll_delta,
                } => tab.terminal.handle_mouse_wheel(input, scroll_delta),
            };
            if let Err(error) = result {
                last_error = Some(error.to_string());
            }
        }
        if last_error.is_some() {
            self.last_error = last_error;
        }
        input_commands
    }

    fn schedule_repaint(
        &self,
        ctx: &egui::Context,
        drained_bytes: usize,
        drain_elapsed_us: u64,
        pending_bytes: usize,
        input_commands: usize,
    ) {
        let metrics = self
            .tabs
            .get(self.active)
            .map(|tab| tab.widget.metrics())
            .unwrap_or_default();
        let repaint = self.repaint_scheduler.recommend(RepaintSignal {
            drained_bytes,
            drain_elapsed_us,
            pending_bytes,
            dirty_rows: metrics.dirty_rows,
            cursor_blinking: metrics.cursor_blinking,
            input_commands,
        });
        ctx.request_repaint_after(repaint.after);
    }

    fn show_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (index, tab) in self.tabs.iter().enumerate() {
                let selected = index == self.active;
                if ui
                    .selectable_label(selected, RichText::new(&tab.title).monospace())
                    .clicked()
                {
                    self.active = index;
                }
            }
            if ui.button("+").clicked() {
                self.open_tab(ui.ctx().clone());
            }
        });
    }
}

impl eframe::App for TabsExample {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (drained_bytes, drain_elapsed_us, pending_bytes) = self.drain_terminals();
        let input_commands = self.send_active_input(ctx);
        self.schedule_repaint(
            ctx,
            drained_bytes,
            drain_elapsed_us,
            pending_bytes,
            input_commands,
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Frame::NONE.fill(egui::Color32::BLACK).show(ui, |ui| {
            self.show_tabs(ui);
            if let Some(error) = &self.last_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
            ui.separator();
            if let Some(tab) = self.active_tab_mut() {
                match tab.widget.show(ui, &mut tab.terminal) {
                    Ok(surface) => tab.surface = Some(surface),
                    Err(error) => self.last_error = Some(error.to_string()),
                }
            }
        });
    }
}

struct TerminalTab {
    title: String,
    terminal: TerminalSession,
    widget: TerminalWidget,
    surface: Option<TerminalSurface>,
}

impl TerminalTab {
    fn new(
        id: usize,
        target_format: Option<wgpu::TextureFormat>,
        repaint: egui::Context,
    ) -> anyhow::Result<Self> {
        let terminal = TerminalSession::new_with_repaint_wakeup(
            TerminalWidget::initial_geometry(),
            std::sync::Arc::new(move || repaint.request_repaint()),
        )?;
        Ok(Self {
            title: format!("shell {id}"),
            terminal,
            widget: TerminalWidget::new(target_format),
            surface: None,
        })
    }
}
