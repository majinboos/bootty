use std::sync::Arc;

use anyhow::{Context, Result};
use eframe::egui::Pos2;
use libghostty_vt::{
    render::{CursorVisualStyle, Dirty},
    style::{RgbColor, Underline},
};
use wgpu::CurrentSurfaceTexture;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{
        ElementState, KeyEvent, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent,
    },
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{Window, WindowId},
};

use crate::{
    direct_input::ModifierSideState,
    geometry::{
        CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding, TerminalSurface,
        geometry_for_pixels,
    },
    input_keymap::{
        key_mods_from_winit_modifiers, key_unshifted, mouse_input_from_surface,
        mouse_wheel_button_from_delta_y, physical_key_utf8,
    },
    modifier_remap::ModifierRemapSet,
    renderer_frame::RendererFrame,
    terminal::{
        CellStyle, CursorSnapshot, FrameColors, FrameStats, KeyInput, MouseAction, MouseButton,
        MouseInput, RenderCell, RenderFrame, TerminalKey, TerminalSession,
    },
    terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
    terminal_render::TerminalRenderFrame,
    terminal_text::TerminalTextConfig,
    terminal_wgpu::{TerminalWgpuRenderer, terminal_text_cell_metrics},
};

const INITIAL_WIDTH: u32 = 1220;
const INITIAL_HEIGHT: u32 = 760;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BareTerminalViewport {
    width: f32,
    height: f32,
    cell: CellMetrics,
    padding: TerminalPadding,
}

impl BareTerminalViewport {
    pub fn new(width: u32, height: u32, cell: CellMetrics, padding: TerminalPadding) -> Self {
        Self::from_logical_size(width as f32, height as f32, cell, padding)
    }

    pub fn from_logical_size(
        width: f32,
        height: f32,
        cell: CellMetrics,
        padding: TerminalPadding,
    ) -> Self {
        Self {
            width: width.max(0.0),
            height: height.max(0.0),
            cell,
            padding,
        }
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.resize_logical(width as f32, height as f32);
    }

    pub fn resize_logical(&mut self, width: f32, height: f32) {
        self.width = width.max(0.0);
        self.height = height.max(0.0);
    }

    pub fn is_drawable(self) -> bool {
        self.width > 0.0 && self.height > 0.0
    }

    pub fn geometry(self) -> TerminalGeometry {
        geometry_for_pixels(self.width, self.height, self.cell, self.padding)
    }

    pub fn surface_rect(self) -> SurfaceRect {
        SurfaceRect::from_min_size(0.0, 0.0, self.width, self.height)
    }

    fn terminal_surface(self) -> TerminalSurface {
        TerminalSurface::for_logical_size(self.width, self.height, self.cell, self.padding)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BareRendererSurfaceConfig {
    pub width: u32,
    pub height: u32,
    pub format: wgpu::TextureFormat,
}

impl BareRendererSurfaceConfig {
    pub fn new(width: u32, height: u32, format: wgpu::TextureFormat) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            format,
        }
    }

    fn to_wgpu(
        self,
        present_mode: wgpu::PresentMode,
        alpha_mode: wgpu::CompositeAlphaMode,
    ) -> wgpu::SurfaceConfiguration {
        wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: self.format,
            width: self.width,
            height: self.height,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode,
            view_formats: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BareTerminalInput {
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    modifier_remaps: ModifierRemapSet,
    cursor_pos: Option<Pos2>,
    pressed_mouse_button: Option<MouseButton>,
}

impl BareTerminalInput {
    pub fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.modifiers = modifiers;
    }

    pub fn set_cursor_position(&mut self, x: f32, y: f32) {
        self.cursor_pos = Some(Pos2::new(x, y));
    }

    pub fn set_mouse_button_state(&mut self, button: MouseButton, state: ElementState) {
        match state {
            ElementState::Pressed => self.pressed_mouse_button = Some(button),
            ElementState::Released if self.pressed_mouse_button == Some(button) => {
                self.pressed_mouse_button = None;
            }
            ElementState::Released => {}
        }
    }

    pub fn set_modifier_remaps(&mut self, modifier_remaps: ModifierRemapSet) {
        self.modifier_remaps = modifier_remaps;
    }

    pub fn key_input(&mut self, event: &KeyEvent) -> Option<KeyInput> {
        self.update_key_side_state(event);
        self.key_input_after_side_state(event)
    }

    pub fn paste_shortcut(&mut self, event: &KeyEvent) -> bool {
        self.update_key_side_state(event);
        self.paste_shortcut_after_side_state(event)
    }

    fn update_key_side_state(&mut self, event: &KeyEvent) {
        if let PhysicalKey::Code(code) = event.physical_key {
            self.side_state.update_key(code, event.state);
        }
    }

    fn key_input_after_side_state(&self, event: &KeyEvent) -> Option<KeyInput> {
        if event.state != ElementState::Pressed {
            return None;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return None;
        };
        bare_terminal_key_input_with_sides_and_remaps(
            code,
            self.modifiers,
            self.side_state,
            event.repeat,
            &self.modifier_remaps,
        )
    }

    fn paste_shortcut_after_side_state(&self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed || event.repeat {
            return false;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return false;
        };
        bare_terminal_paste_shortcut(code, self.modifiers)
    }

    pub fn mouse_input(
        &self,
        action: MouseAction,
        button: Option<MouseButton>,
        viewport: BareTerminalViewport,
    ) -> Option<MouseInput> {
        bare_terminal_mouse_input(self.cursor_pos?, action, button, self.modifiers, viewport)
    }

    pub fn mouse_wheel(
        &self,
        delta: MouseScrollDelta,
        viewport: BareTerminalViewport,
    ) -> Option<MouseInput> {
        let button = bare_terminal_mouse_wheel_button(delta)?;
        self.mouse_input(MouseAction::Press, Some(button), viewport)
    }

    pub fn mouse_motion(&self, viewport: BareTerminalViewport) -> Option<MouseInput> {
        self.mouse_input(MouseAction::Motion, self.pressed_mouse_button, viewport)
    }
}

pub fn bare_terminal_key_input(
    code: KeyCode,
    modifiers: ModifiersState,
    repeat: bool,
) -> Option<KeyInput> {
    bare_terminal_key_input_with_remaps(code, modifiers, repeat, &ModifierRemapSet::default())
}

pub fn bare_terminal_key_input_with_remaps(
    code: KeyCode,
    modifiers: ModifiersState,
    repeat: bool,
    modifier_remaps: &ModifierRemapSet,
) -> Option<KeyInput> {
    bare_terminal_key_input_with_sides_and_remaps(
        code,
        modifiers,
        ModifierSideState::default(),
        repeat,
        modifier_remaps,
    )
}

pub fn bare_terminal_key_input_with_sides_and_remaps(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
    modifier_remaps: &ModifierRemapSet,
) -> Option<KeyInput> {
    let mut input = bare_terminal_key_input_with_sides(code, modifiers, side_state, repeat)?;
    input.mods = modifier_remaps.apply(input.mods);
    Some(input)
}

pub fn bare_terminal_paste_shortcut(code: KeyCode, modifiers: ModifiersState) -> bool {
    if code != KeyCode::KeyV {
        return false;
    }
    let platform_paste = if cfg!(target_os = "macos") {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    };
    platform_paste && !modifiers.alt_key()
}

pub fn bare_terminal_key_input_with_sides(
    code: KeyCode,
    modifiers: ModifiersState,
    side_state: ModifierSideState,
    repeat: bool,
) -> Option<KeyInput> {
    let key = terminal_key_from_winit_code(code)?;
    let mut input = KeyInput {
        key,
        mods: key_mods_from_winit_modifiers(modifiers),
        repeat,
        utf8: physical_key_utf8(key, modifiers.shift_key()),
        unshifted: key_unshifted(key),
    };
    side_state.apply_to_key_input(&mut input);
    Some(input)
}

pub fn bare_terminal_mouse_input(
    pos: Pos2,
    action: MouseAction,
    button: Option<MouseButton>,
    modifiers: ModifiersState,
    viewport: BareTerminalViewport,
) -> Option<MouseInput> {
    let surface = viewport.terminal_surface();
    mouse_input_from_surface(
        pos,
        action,
        button,
        key_mods_from_winit_modifiers(modifiers),
        surface,
    )
}

fn bare_terminal_mouse_button(button: WinitMouseButton) -> Option<MouseButton> {
    match button {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Right => Some(MouseButton::Right),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

fn bare_terminal_mouse_wheel_button(delta: MouseScrollDelta) -> Option<MouseButton> {
    let y = match delta {
        MouseScrollDelta::LineDelta(_, y) => y,
        MouseScrollDelta::PixelDelta(pos) => pos.y as f32,
    };
    mouse_wheel_button_from_delta_y(y)
}

fn terminal_key_from_winit_code(code: KeyCode) -> Option<TerminalKey> {
    Some(match code {
        KeyCode::Backquote => TerminalKey::Backquote,
        KeyCode::Backslash => TerminalKey::Backslash,
        KeyCode::BracketLeft => TerminalKey::BracketLeft,
        KeyCode::BracketRight => TerminalKey::BracketRight,
        KeyCode::Comma => TerminalKey::Comma,
        KeyCode::Digit0 => TerminalKey::Digit0,
        KeyCode::Digit1 => TerminalKey::Digit1,
        KeyCode::Digit2 => TerminalKey::Digit2,
        KeyCode::Digit3 => TerminalKey::Digit3,
        KeyCode::Digit4 => TerminalKey::Digit4,
        KeyCode::Digit5 => TerminalKey::Digit5,
        KeyCode::Digit6 => TerminalKey::Digit6,
        KeyCode::Digit7 => TerminalKey::Digit7,
        KeyCode::Digit8 => TerminalKey::Digit8,
        KeyCode::Digit9 => TerminalKey::Digit9,
        KeyCode::Equal => TerminalKey::Equal,
        KeyCode::KeyA => TerminalKey::A,
        KeyCode::KeyB => TerminalKey::B,
        KeyCode::KeyC => TerminalKey::C,
        KeyCode::KeyD => TerminalKey::D,
        KeyCode::KeyE => TerminalKey::E,
        KeyCode::KeyF => TerminalKey::F,
        KeyCode::KeyG => TerminalKey::G,
        KeyCode::KeyH => TerminalKey::H,
        KeyCode::KeyI => TerminalKey::I,
        KeyCode::KeyJ => TerminalKey::J,
        KeyCode::KeyK => TerminalKey::K,
        KeyCode::KeyL => TerminalKey::L,
        KeyCode::KeyM => TerminalKey::M,
        KeyCode::KeyN => TerminalKey::N,
        KeyCode::KeyO => TerminalKey::O,
        KeyCode::KeyP => TerminalKey::P,
        KeyCode::KeyQ => TerminalKey::Q,
        KeyCode::KeyR => TerminalKey::R,
        KeyCode::KeyS => TerminalKey::S,
        KeyCode::KeyT => TerminalKey::T,
        KeyCode::KeyU => TerminalKey::U,
        KeyCode::KeyV => TerminalKey::V,
        KeyCode::KeyW => TerminalKey::W,
        KeyCode::KeyX => TerminalKey::X,
        KeyCode::KeyY => TerminalKey::Y,
        KeyCode::KeyZ => TerminalKey::Z,
        KeyCode::Minus => TerminalKey::Minus,
        KeyCode::Period => TerminalKey::Period,
        KeyCode::Quote => TerminalKey::Quote,
        KeyCode::Semicolon => TerminalKey::Semicolon,
        KeyCode::Slash => TerminalKey::Slash,
        KeyCode::Enter => TerminalKey::Enter,
        KeyCode::NumpadEnter => TerminalKey::NumpadEnter,
        KeyCode::Tab => TerminalKey::Tab,
        KeyCode::Backspace => TerminalKey::Backspace,
        KeyCode::Escape => TerminalKey::Escape,
        KeyCode::ArrowUp => TerminalKey::ArrowUp,
        KeyCode::ArrowDown => TerminalKey::ArrowDown,
        KeyCode::ArrowRight => TerminalKey::ArrowRight,
        KeyCode::ArrowLeft => TerminalKey::ArrowLeft,
        KeyCode::Delete => TerminalKey::Delete,
        KeyCode::Home => TerminalKey::Home,
        KeyCode::End => TerminalKey::End,
        KeyCode::PageUp => TerminalKey::PageUp,
        KeyCode::PageDown => TerminalKey::PageDown,
        KeyCode::Space => TerminalKey::Space,
        KeyCode::Insert => TerminalKey::Insert,
        KeyCode::F1 => TerminalKey::F1,
        KeyCode::F2 => TerminalKey::F2,
        KeyCode::F3 => TerminalKey::F3,
        KeyCode::F4 => TerminalKey::F4,
        KeyCode::F5 => TerminalKey::F5,
        KeyCode::F6 => TerminalKey::F6,
        KeyCode::F7 => TerminalKey::F7,
        KeyCode::F8 => TerminalKey::F8,
        KeyCode::F9 => TerminalKey::F9,
        KeyCode::F10 => TerminalKey::F10,
        KeyCode::F11 => TerminalKey::F11,
        KeyCode::F12 => TerminalKey::F12,
        KeyCode::Numpad0 => TerminalKey::Numpad0,
        KeyCode::Numpad1 => TerminalKey::Numpad1,
        KeyCode::Numpad2 => TerminalKey::Numpad2,
        KeyCode::Numpad3 => TerminalKey::Numpad3,
        KeyCode::Numpad4 => TerminalKey::Numpad4,
        KeyCode::Numpad5 => TerminalKey::Numpad5,
        KeyCode::Numpad6 => TerminalKey::Numpad6,
        KeyCode::Numpad7 => TerminalKey::Numpad7,
        KeyCode::Numpad8 => TerminalKey::Numpad8,
        KeyCode::Numpad9 => TerminalKey::Numpad9,
        KeyCode::NumpadAdd => TerminalKey::NumpadAdd,
        KeyCode::NumpadDecimal => TerminalKey::NumpadDecimal,
        KeyCode::NumpadDivide => TerminalKey::NumpadDivide,
        KeyCode::NumpadEqual => TerminalKey::NumpadEqual,
        KeyCode::NumpadMultiply => TerminalKey::NumpadMultiply,
        KeyCode::NumpadSubtract => TerminalKey::NumpadSubtract,
        _ => return None,
    })
}

pub fn terminal_render_frame_for_bare_host(
    frame: &RenderFrame,
    viewport: BareTerminalViewport,
    text_config: &TerminalTextConfig,
) -> TerminalRenderFrame {
    renderer_frame_for_bare_host(frame, viewport, text_config).to_terminal_render_frame(text_config)
}

pub fn renderer_frame_for_bare_host(
    frame: &RenderFrame,
    viewport: BareTerminalViewport,
    text_config: &TerminalTextConfig,
) -> RendererFrame {
    RendererFrame::from_terminal(frame, viewport.terminal_surface(), text_config)
}

pub fn renderer_parity_gallery_frame() -> RendererFrame {
    let cell = CellMetrics::new(10.0, 20.0);
    let viewport = BareTerminalViewport::new(60, 20, cell, TerminalPadding::default());
    let text_config = TerminalTextConfig::with_cell_metrics(cell);
    let frame = RenderFrame {
        cols: 6,
        rows: 1,
        dirty: Dirty::Full,
        colors: FrameColors {
            background: rgb(1, 2, 3),
            foreground: rgb(220, 221, 222),
            cursor: Some(rgb(255, 255, 255)),
            ..Default::default()
        },
        cursor: Some(CursorSnapshot {
            x: 4,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: true,
            color: None,
        }),
        row_dirty: vec![true],
        cells: vec![
            gallery_cell(0, 0, 0, 1, CellStyle::default()),
            gallery_cell(1, 0, 1, 1, CellStyle::default()),
            gallery_cell(
                2,
                0,
                2,
                1,
                CellStyle {
                    underline: Underline::Double,
                    ..Default::default()
                },
            ),
            gallery_cell(
                3,
                0,
                3,
                1,
                CellStyle {
                    strikethrough: true,
                    ..Default::default()
                },
            ),
            gallery_cell(
                4,
                0,
                4,
                1,
                CellStyle {
                    overline: true,
                    ..Default::default()
                },
            ),
            RenderCell {
                x: 5,
                y: 0,
                text_start: 5,
                text_len: 1,
                fg: Some(rgb(10, 10, 10)),
                bg: Some(rgb(12, 12, 12)),
                style: CellStyle::default(),
            },
        ],
        text: vec!['A', '█', 'B', 'C', 'D', 'E'],
        images: gallery_images(),
        scrollbar: None,
        stats: FrameStats {
            cells: 6,
            chars: 6,
            dirty_rows: 1,
            ..Default::default()
        },
    };
    let mut renderer_frame = renderer_frame_for_bare_host(&frame, viewport, &text_config);
    renderer_frame.select_cells(0, 0..1);
    renderer_frame
}

pub fn run() -> Result<()> {
    let event_loop = EventLoop::new().context("create bare terminal event loop")?;
    let mut app = BareTerminalApp::default();
    let result = event_loop
        .run_app(&mut app)
        .context("run bare terminal host");
    if let Some(error) = app.error {
        return Err(error);
    }
    result
}

#[derive(Default)]
struct BareTerminalApp {
    state: Option<BareTerminalState>,
    error: Option<anyhow::Error>,
}

impl BareTerminalApp {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler for BareTerminalApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let attributes = Window::default_attributes()
            .with_title("Bootty bare terminal")
            .with_inner_size(LogicalSize::new(
                f64::from(INITIAL_WIDTH),
                f64::from(INITIAL_HEIGHT),
            ));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.fail(event_loop, error.into());
                return;
            }
        };

        match pollster::block_on(BareTerminalState::new(window)) {
            Ok(state) => {
                state.window.request_redraw();
                self.state = Some(state);
            }
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.window.id() != window_id {
            return;
        }

        let result = match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                Ok(())
            }
            WindowEvent::Resized(size) => state.resize(size),
            WindowEvent::ModifiersChanged(modifiers) => {
                state.set_modifiers(modifiers.state());
                Ok(())
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => state.handle_key_input(&event),
            WindowEvent::CursorMoved { position, .. } => {
                let pos = position.to_logical::<f32>(state.window.scale_factor());
                state.input.set_cursor_position(pos.x, pos.y);
                state.handle_mouse_motion()
            }
            WindowEvent::MouseInput {
                state: button_state,
                button,
                ..
            } => state.handle_mouse_button(button_state, button),
            WindowEvent::MouseWheel { delta, .. } => state.handle_mouse_wheel(delta),
            WindowEvent::RedrawRequested => state.redraw(),
            WindowEvent::ScaleFactorChanged { .. } => state.resize(state.window.inner_size()),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.fail(event_loop, error);
        }
    }
}

struct BareTerminalState {
    window: Arc<Window>,
    terminal: TerminalSession,
    viewport: BareTerminalViewport,
    input: BareTerminalInput,
    text_config: TerminalTextConfig,
    gpu: BareTerminalGpu,
}

impl BareTerminalState {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let text_config = TerminalTextConfig::default();
        let cell = terminal_text_cell_metrics(&text_config);
        let viewport = viewport_for_window(size, window.scale_factor(), cell);
        let redraw_window = window.clone();
        let terminal = TerminalSession::new_with_repaint_wakeup(
            viewport.geometry(),
            Arc::new(move || redraw_window.request_redraw()),
        )?;
        let gpu = BareTerminalGpu::new(window.clone(), size).await?;

        Ok(Self {
            window,
            terminal,
            viewport,
            input: BareTerminalInput::default(),
            text_config,
            gpu,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) -> Result<()> {
        self.viewport = viewport_for_window(
            size,
            self.window.scale_factor(),
            terminal_text_cell_metrics(&self.text_config),
        );
        self.gpu.resize(size);
        if self.viewport.is_drawable() {
            self.terminal.resize(self.viewport.geometry())?;
            self.window.request_redraw();
        }
        Ok(())
    }

    fn set_modifiers(&mut self, modifiers: ModifiersState) {
        self.input.set_modifiers(modifiers);
    }

    fn handle_key_input(&mut self, event: &KeyEvent) -> Result<()> {
        self.input.update_key_side_state(event);
        if self.input.paste_shortcut_after_side_state(event) {
            if let Some(text) = read_clipboard_text()? {
                self.terminal.write_paste(&text)?;
                self.window.request_redraw();
            }
            return Ok(());
        }
        if let Some(input) = self.input.key_input_after_side_state(event) {
            self.terminal.encode_key(input)?;
            self.window.request_redraw();
        }
        Ok(())
    }

    fn handle_mouse_button(&mut self, state: ElementState, button: WinitMouseButton) -> Result<()> {
        let Some(button) = bare_terminal_mouse_button(button) else {
            return Ok(());
        };
        let action = if state == ElementState::Pressed {
            MouseAction::Press
        } else {
            MouseAction::Release
        };
        if let Some(input) = self.input.mouse_input(action, Some(button), self.viewport) {
            self.input.set_mouse_button_state(button, state);
            self.terminal.encode_mouse(input)?;
            self.window.request_redraw();
        } else {
            self.input.set_mouse_button_state(button, state);
        }
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) -> Result<()> {
        if let Some(input) = self.input.mouse_wheel(delta, self.viewport) {
            self.terminal.encode_mouse(input)?;
            self.window.request_redraw();
        }
        Ok(())
    }

    fn handle_mouse_motion(&mut self) -> Result<()> {
        if let Some(input) = self.input.mouse_motion(self.viewport) {
            self.terminal.encode_mouse(input)?;
            self.window.request_redraw();
        }
        Ok(())
    }

    fn redraw(&mut self) -> Result<()> {
        if !self.viewport.is_drawable() {
            return Ok(());
        }

        let frame = self.terminal.extract_frame()?;
        let render_frame =
            terminal_render_frame_for_bare_host(&frame, self.viewport, &self.text_config);
        self.gpu.render(&render_frame, clear_color(frame.colors))?;
        Ok(())
    }
}

fn viewport_for_window(
    size: PhysicalSize<u32>,
    scale_factor: f64,
    cell: CellMetrics,
) -> BareTerminalViewport {
    let logical = size.to_logical::<f32>(scale_factor);
    BareTerminalViewport::from_logical_size(
        logical.width,
        logical.height,
        cell,
        TerminalPadding::default(),
    )
}

fn read_clipboard_text() -> Result<Option<String>> {
    let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Ok(Some(text)),
        Ok(_) => Ok(None),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(error) => Err(error).context("read system clipboard text"),
    }
}

fn gallery_cell(
    x: u16,
    y: u16,
    text_start: usize,
    text_len: usize,
    style: CellStyle,
) -> RenderCell {
    RenderCell {
        x,
        y,
        text_start,
        text_len,
        fg: None,
        bg: None,
        style,
    }
}

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

fn gallery_images() -> KittyImageFrame {
    KittyImageFrame {
        placements: vec![
            gallery_image(1, KittyImageLayer::BelowBackground, 0.0),
            gallery_image(2, KittyImageLayer::BelowText, 10.0),
            gallery_image(3, KittyImageLayer::AboveText, 20.0),
        ],
        ..Default::default()
    }
}

fn gallery_image(image_id: u32, layer: KittyImageLayer, x: f32) -> KittyImagePlacement {
    KittyImagePlacement {
        image_id,
        placement_id: image_id,
        layer,
        image_width: 1,
        image_height: 1,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination: SurfaceRect::from_min_size(x, 0.0, 10.0, 20.0),
        data: Arc::new(vec![255, 0, 255, 96]),
    }
}

struct BareTerminalGpu {
    window: Arc<Window>,
    instance: wgpu::Instance,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: TerminalWgpuRenderer,
}

impl BareTerminalGpu {
    async fn new(window: Arc<Window>, size: PhysicalSize<u32>) -> Result<Self> {
        let instance = wgpu::Instance::default();
        let surface = create_surface(&instance, window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("request bare terminal WGPU adapter")?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("bootty bare terminal device"),
                ..Default::default()
            })
            .await
            .context("request bare terminal WGPU device")?;
        let capabilities = surface.get_capabilities(&adapter);
        let format = preferred_bare_surface_format(&capabilities.formats)
            .context("bare terminal surface reports no WGPU formats")?;
        let present_mode = capabilities
            .present_modes
            .first()
            .copied()
            .unwrap_or(wgpu::PresentMode::Fifo);
        let alpha_mode = capabilities
            .alpha_modes
            .first()
            .copied()
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        let config = BareRendererSurfaceConfig::new(size.width, size.height, format)
            .to_wgpu(present_mode, alpha_mode);
        surface.configure(&device, &config);
        let renderer = TerminalWgpuRenderer::new(&device, config.format);

        Ok(Self {
            window,
            instance,
            surface,
            device,
            queue,
            config,
            renderer,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }

        let surface_config =
            BareRendererSurfaceConfig::new(size.width, size.height, self.config.format);
        self.config.width = surface_config.width;
        self.config.height = surface_config.height;
        self.surface.configure(&self.device, &self.config);
    }

    fn render(&mut self, frame: &TerminalRenderFrame, clear: wgpu::Color) -> Result<()> {
        let texture = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(texture) => texture,
            CurrentSurfaceTexture::Suboptimal(texture) => {
                drop(texture);
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            CurrentSurfaceTexture::Lost => {
                self.recreate_surface()?;
                return Ok(());
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => return Ok(()),
            CurrentSurfaceTexture::Validation => {
                anyhow::bail!("acquire bare terminal WGPU frame failed validation");
            }
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.renderer.prepare_terminal_frame(
            &self.device,
            &self.queue,
            frame,
            self.window.scale_factor() as f32,
        );
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bootty bare terminal encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bootty bare terminal render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer.paint(&mut pass);
        }
        self.queue.submit([encoder.finish()]);
        texture.present();

        Ok(())
    }

    fn recreate_surface(&mut self) -> Result<()> {
        self.surface = create_surface(&self.instance, self.window.clone())?;
        self.surface.configure(&self.device, &self.config);
        Ok(())
    }
}

fn create_surface(
    instance: &wgpu::Instance,
    window: Arc<Window>,
) -> Result<wgpu::Surface<'static>> {
    instance
        .create_surface(window)
        .context("create bare terminal WGPU surface")
}

fn preferred_bare_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats
        .iter()
        .copied()
        .find(|format| !format.is_srgb())
        .or_else(|| formats.first().copied())
}

fn clear_color(colors: FrameColors) -> wgpu::Color {
    wgpu::Color {
        r: f64::from(colors.background.r) / 255.0,
        g: f64::from(colors.background.g) / 255.0,
        b: f64::from(colors.background.b) / 255.0,
        a: 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_terminal_ignores_standalone_modifier_keys() {
        assert!(
            bare_terminal_key_input(KeyCode::ShiftLeft, ModifiersState::SHIFT, false).is_none()
        );
        assert!(
            bare_terminal_key_input(KeyCode::ControlRight, ModifiersState::CONTROL, false)
                .is_none()
        );
        assert!(bare_terminal_key_input(KeyCode::AltRight, ModifiersState::ALT, false).is_none());
    }

    #[test]
    fn bare_surface_prefers_non_srgb_format_for_terminal_palette_colors() {
        let formats = [
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
        ];

        assert_eq!(
            preferred_bare_surface_format(&formats),
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
    }

    #[test]
    fn bare_surface_falls_back_to_first_format_when_only_srgb_is_available() {
        let formats = [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ];

        assert_eq!(
            preferred_bare_surface_format(&formats),
            Some(wgpu::TextureFormat::Rgba8UnormSrgb)
        );
    }
}
