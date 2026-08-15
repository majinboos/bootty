use std::{cell::RefCell, rc::Rc, sync::mpsc};

use anyhow::{Context, Result};
use eframe::UserEvent;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, KeyEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::ModifiersState,
    window::WindowId,
};

use crate::{
    app::BoottyApp,
    application_identity::ApplicationIdentity,
    config::BoottyConfig,
    control::{ControlPlane, ControlServer},
    direct_input::{DirectKeyInput, ModifierSideState, direct_key_input_from_winit_event},
    platform::disable_automatic_window_tabbing,
};

pub fn run(
    options: eframe::NativeOptions,
    config: BoottyConfig,
    window_state_key: String,
) -> Result<()> {
    // Must run before any window is created (the flag is read at window-creation time), otherwise
    // macOS automatic window tabbing keeps the Cmd+T key equivalent and the keypress never reaches
    // the app.
    disable_automatic_window_tabbing();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("create bootty event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (direct_input_tx, direct_input_rx) = mpsc::channel();
    let (modifier_side_tx, modifier_side_rx) = mpsc::channel();
    let control_server = Rc::new(RefCell::new(None));
    let app_control_server = Rc::clone(&control_server);
    let app_creator = Box::new(move |cc: &eframe::CreationContext<'_>| {
        let control_plane = ControlPlane::default();
        let app = BoottyApp::new_for_native_host(
            cc,
            config,
            window_state_key.clone(),
            direct_input_rx,
            modifier_side_rx,
            control_plane.clone(),
        )?;
        let (commands, catalog) = app.control_binding();
        app_control_server.replace(Some(ControlServer::spawn(
            window_state_key,
            commands,
            catalog,
            control_plane,
        )?));
        Ok(Box::new(app) as Box<dyn eframe::App>)
    });
    let inner = eframe::create_native(
        ApplicationIdentity::current().display_name(),
        options,
        app_creator,
        &event_loop,
    );
    let mut app = BoottyNativeHost {
        inner,
        _control_server: control_server,
        direct_input_tx,
        modifier_side_tx,
        input_state: NativeInputState::default(),
        cursor_needs_reapply: false,
    };

    event_loop.run_app(&mut app).context("run bootty")
}

struct BoottyNativeHost<'app> {
    inner: eframe::EframeWinitApplication<'app>,
    _control_server: Rc<RefCell<Option<ControlServer>>>,
    direct_input_tx: mpsc::Sender<DirectKeyInput>,
    modifier_side_tx: mpsc::Sender<ModifierSideState>,
    input_state: NativeInputState,
    cursor_needs_reapply: bool,
}

#[derive(Default)]
struct NativeInputState {
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    // winit can report the pre-focus modifier state after focus changes; ignore only that exact echo.
    stale_modifiers_after_focus: Option<ModifiersState>,
}

impl NativeInputState {
    fn handle_modifiers_changed(&mut self, next: ModifiersState) -> Option<ModifierSideState> {
        if self
            .stale_modifiers_after_focus
            .take()
            .is_some_and(|stale| stale == next && next != ModifiersState::empty())
        {
            return None;
        }
        self.modifiers = next;
        self.side_state.retain_active_modifiers(self.modifiers);
        Some(self.side_state)
    }

    fn handle_focus_changed(&mut self) -> ModifierSideState {
        if self.modifiers != ModifiersState::empty() {
            self.stale_modifiers_after_focus = Some(self.modifiers);
        }
        self.modifiers = ModifiersState::empty();
        self.side_state.clear();
        self.side_state
    }

    fn handle_keyboard_input(
        &mut self,
        event: &KeyEvent,
    ) -> (ModifierSideState, Option<DirectKeyInput>) {
        self.stale_modifiers_after_focus = None;
        if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
            self.side_state.update_key(code, event.state);
        }
        let input = direct_key_input_from_winit_event(event, self.modifiers, self.side_state);
        (self.side_state, input)
    }
}

impl ApplicationHandler<UserEvent> for BoottyNativeHost<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.resumed(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.cursor_needs_reapply |= matches!(event, WindowEvent::CursorMoved { .. });
        match &event {
            WindowEvent::ModifiersChanged(modifiers) => {
                if let Some(side_state) =
                    self.input_state.handle_modifiers_changed(modifiers.state())
                {
                    let _ = self.modifier_side_tx.send(side_state);
                }
            }
            WindowEvent::Focused(_) => {
                let side_state = self.input_state.handle_focus_changed();
                let _ = self.modifier_side_tx.send(side_state);
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                let (side_state, input) = self.input_state.handle_keyboard_input(event);
                let _ = self.modifier_side_tx.send(side_state);
                if let Some(input) = input {
                    let _ = self.direct_input_tx.send(input);
                }
            }
            _ => {}
        }
        self.inner.window_event(event_loop, window_id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        self.inner.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        self.inner.user_event(event_loop, event);
    }

    fn device_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        device_id: winit::event::DeviceId,
        event: DeviceEvent,
    ) {
        self.inner.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
        if std::mem::take(&mut self.cursor_needs_reapply) {
            crate::platform::reapply_macos_cursor_icon();
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.memory_warning(event_loop);
    }
}
