use std::sync::mpsc;

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
    config::BoottyConfig,
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
    let app_creator = Box::new(move |cc: &eframe::CreationContext<'_>| {
        Ok(Box::new(BoottyApp::new_with_direct_input(
            cc,
            config,
            window_state_key,
            direct_input_rx,
            modifier_side_rx,
        )?) as Box<dyn eframe::App>)
    });
    let inner = eframe::create_native("Bootty", options, app_creator, &event_loop);
    let mut app = BoottyNativeHost {
        inner,
        direct_input_tx,
        modifier_side_tx,
        input_state: NativeInputState::default(),
        cursor_needs_reapply: false,
    };

    event_loop.run_app(&mut app).context("run bootty")
}

struct BoottyNativeHost<'app> {
    inner: eframe::EframeWinitApplication<'app>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        direct_input::direct_key_input_from_winit_code,
        terminal::{KeyMods, TerminalKey},
    };
    use winit::keyboard::KeyCode;

    #[test]
    fn focus_reset_ignores_same_modifier_state_known_before_focus_changed() {
        let mut state = NativeInputState::default();

        state.handle_modifiers_changed(ModifiersState::SUPER);
        state.handle_focus_changed();
        state.handle_focus_changed();
        let side_state = state.handle_modifiers_changed(ModifiersState::SUPER);

        assert_eq!(side_state, None);
        assert_eq!(state.modifiers, ModifiersState::empty());
    }

    #[test]
    fn command_modifier_after_focus_reset_applies_to_next_key() {
        let mut state = NativeInputState::default();

        state.handle_focus_changed();
        state.handle_modifiers_changed(ModifiersState::SUPER);
        let direct = direct_key_input_from_winit_code(
            KeyCode::KeyV,
            state.modifiers,
            state.side_state,
            false,
        )
        .expect("command-modified V uses direct input so paste bindings can run");

        assert_eq!(direct.input.key, TerminalKey::V);
        assert_eq!(
            direct.input.mods,
            KeyMods {
                command: true,
                ..Default::default()
            }
        );
    }
}
