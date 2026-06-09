use std::sync::mpsc;

use anyhow::{Context, Result};
use eframe::UserEvent;
use winit::{
    application::ApplicationHandler,
    event::{DeviceEvent, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::ModifiersState,
    window::WindowId,
};

use crate::{
    app::BoottyApp,
    config::BoottyConfig,
    direct_input::{DirectKeyInput, ModifierSideState, direct_key_input_from_winit_event},
};

pub fn run(options: eframe::NativeOptions, config: BoottyConfig) -> Result<()> {
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .context("create bootty event loop")?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let (direct_input_tx, direct_input_rx) = mpsc::channel();
    let app_creator = Box::new(move |cc: &eframe::CreationContext<'_>| {
        Ok(Box::new(BoottyApp::new_with_direct_input(
            cc,
            config,
            direct_input_rx,
        )?) as Box<dyn eframe::App>)
    });
    let inner = eframe::create_native("Bootty", options, app_creator, &event_loop);
    let mut app = BoottyNativeHost {
        inner,
        direct_input_tx,
        modifiers: ModifiersState::empty(),
        side_state: ModifierSideState::default(),
    };

    event_loop.run_app(&mut app).context("run bootty")
}

struct BoottyNativeHost<'app> {
    inner: eframe::EframeWinitApplication<'app>,
    direct_input_tx: mpsc::Sender<DirectKeyInput>,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
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
        match &event {
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => {
                if let winit::keyboard::PhysicalKey::Code(code) = event.physical_key {
                    self.side_state.update_key(code, event.state);
                }
                if let Some(input) =
                    direct_key_input_from_winit_event(event, self.modifiers, self.side_state)
                {
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
