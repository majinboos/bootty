use std::{
    io::{Read, Write},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bootty_surface::geometry::TerminalGeometry;
use bootty_terminal::{
    terminal_engine::TerminalColorConfig,
    terminal_frame::{CellStyle, CursorSnapshot, FrameColors, FrameStats, RenderCell, RenderFrame},
    terminal_input_model::{KeyInput, MouseAction, MouseButton, MouseInput},
};
use rmux_ipc::{LocalEndpoint, connect_blocking};
use rmux_proto::{
    AttachFrameDecoder, AttachMessage, AttachSessionExt2Request, ClientTerminalContext,
    FrameDecoder, Request, Response, TerminalGeometry as RmuxTerminalGeometry, TerminalPixels,
    TerminalSize, encode_attach_message, encode_frame,
};
use rmux_sdk::{
    Pane, PaneAttributes, PaneColor, PaneId, PaneRef, PaneSnapshot, Rmux, RmuxEndpoint,
    SessionName, TerminalSizeSpec,
};
use tokio::runtime::{Builder, Runtime};

use crate::{renderer::TerminalRenderSource, terminal::DrainStats};

use super::pane::{MuxPaneTarget, TerminalRuntime};

pub(super) struct RmuxNativeTerminal {
    runtime: Runtime,
    pane: Pane,
    attach: RmuxAttachedClient,
    geometry: TerminalGeometry,
    colors: TerminalColorConfig,
    latest_frame: Arc<RenderFrame>,
}

impl RmuxNativeTerminal {
    pub(super) fn new(
        target: MuxPaneTarget,
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
    ) -> Result<Self> {
        let runtime = Builder::new_current_thread().enable_all().build()?;
        let session_name = SessionName::new(target.session_id())
            .context("invalid rmux session name for native render")?;
        let pane_id = target
            .input_selector()
            .strip_prefix('%')
            .and_then(|value| value.parse::<u32>().ok())
            .map(PaneId::from);
        let pane = runtime.block_on(async {
            let endpoint = rmux_ipc::default_endpoint()
                .map_err(rmux_sdk::RmuxError::from)?
                .into_path();
            let rmux = Rmux::connect_or_start_at(RmuxEndpoint::UnixSocket(endpoint)).await?;
            if let Some(pane_id) = pane_id {
                return rmux.pane_by_id(session_name, pane_id).await;
            }
            Ok(rmux.session(session_name).await?.pane(0, 0))
        })?;
        let attach = RmuxAttachedClient::connect(pane.endpoint(), pane.target(), geometry)?;
        let mut terminal = Self {
            runtime,
            pane,
            attach,
            geometry,
            colors,
            latest_frame: Arc::new(RenderFrame::default()),
        };
        terminal.resize(geometry)?;
        Ok(terminal)
    }

    fn refresh_frame(&mut self) -> Result<()> {
        let snapshot = self.runtime.block_on(self.pane.snapshot())?;
        self.latest_frame = Arc::new(render_frame_from_snapshot(&snapshot, &self.colors));
        Ok(())
    }
}

impl TerminalRenderSource for RmuxNativeTerminal {
    fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if self.geometry != geometry {
            self.geometry = geometry;
            self.attach.resize(geometry)?;
            self.runtime.block_on(
                self.pane
                    .resize(TerminalSizeSpec::new(geometry.cols, geometry.rows)),
            )?;
        }
        self.refresh_frame()
    }

    fn extract_frame(&mut self) -> Result<Arc<RenderFrame>> {
        self.refresh_frame()?;
        Ok(Arc::clone(&self.latest_frame))
    }
}

impl TerminalRuntime for RmuxNativeTerminal {
    fn drain_pty(&mut self) -> DrainStats {
        DrainStats::default()
    }

    fn pending_pty_len(&self) -> usize {
        0
    }

    fn child_exited(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        self.colors = colors;
        self.refresh_frame()
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.attach.write_input(bytes)
    }

    fn write_paste(&mut self, text: &str) -> Result<()> {
        self.runtime.block_on(self.pane.send_text(text))?;
        Ok(())
    }

    fn encode_key(&mut self, input: KeyInput) -> Result<()> {
        if let Some(token) = rmux_key_token(input) {
            self.runtime.block_on(self.pane.send_key(token))?;
        }
        Ok(())
    }

    fn encode_focus(&mut self, _gained: bool) -> Result<()> {
        Ok(())
    }

    fn encode_mouse(&mut self, input: MouseInput) -> Result<()> {
        self.runtime
            .block_on(self.pane.send_text(rmux_mouse_input(input)))?;
        Ok(())
    }

    fn handle_mouse_wheel(&mut self, input: MouseInput, _scroll_delta: isize) -> Result<()> {
        self.encode_mouse(input)
    }
}

pub(super) fn render_frame_from_snapshot(
    snapshot: &PaneSnapshot,
    colors: &TerminalColorConfig,
) -> RenderFrame {
    let start = Instant::now();
    let mut frame = RenderFrame {
        cols: snapshot.cols,
        rows: snapshot.rows,
        colors: FrameColors {
            background: colors.background,
            foreground: colors.foreground,
            cursor: colors.cursor,
            cursor_text: colors.cursor_text,
            selection_background: colors.selection_background,
            selection_foreground: colors.selection_foreground,
        },
        row_dirty: vec![true; usize::from(snapshot.rows)],
        ..RenderFrame::default()
    };

    for (index, cell) in snapshot.cells.iter().enumerate() {
        if cell.glyph.padding
            || cell.glyph.text.is_empty()
            || is_kitty_virtual_placeholder(&cell.glyph.text)
        {
            continue;
        }
        let y = index / usize::from(snapshot.cols.max(1));
        let x = index % usize::from(snapshot.cols.max(1));
        let text_start = frame.text.len();
        frame.text.extend(cell.glyph.text.chars());
        let text_len = frame.text.len() - text_start;
        if text_len == 0 {
            continue;
        }
        frame.cells.push(RenderCell {
            x: x as u16,
            y: y as u16,
            text_start,
            text_len,
            fg: pane_color(cell.foreground, colors, true),
            bg: pane_color(cell.background, colors, false),
            style: cell_style(cell.attributes),
        });
    }

    if snapshot.cursor.visible {
        frame.cursor = Some(CursorSnapshot {
            x: snapshot.cursor.col,
            y: snapshot.cursor.row,
            at_wide_tail: false,
            style: libghostty_vt::render::CursorVisualStyle::Block,
            blinking: false,
            color: colors.cursor,
        });
    }
    frame.stats = FrameStats {
        extraction_us: start.elapsed().as_micros() as u64,
        cells: frame.cells.len(),
        chars: frame.text.len(),
        dirty_rows: usize::from(snapshot.rows),
        ..FrameStats::default()
    };
    frame
}

fn is_kitty_virtual_placeholder(text: &str) -> bool {
    text.starts_with('\u{10eeee}')
}

fn pane_color(
    color: PaneColor,
    colors: &TerminalColorConfig,
    foreground: bool,
) -> Option<libghostty_vt::style::RgbColor> {
    match color {
        PaneColor::Default | PaneColor::Terminal => Some(if foreground {
            colors.foreground
        } else {
            colors.background
        }),
        PaneColor::None | PaneColor::Encoded { .. } => None,
        PaneColor::Ansi { index } => colors.palette.get(usize::from(index)).copied(),
        PaneColor::BrightAnsi { index } => colors.palette.get(usize::from(index + 8)).copied(),
        PaneColor::Indexed { index } => colors.palette.get(usize::from(index)).copied(),
        PaneColor::Rgb { red, green, blue } => Some(libghostty_vt::style::RgbColor {
            r: red,
            g: green,
            b: blue,
        }),
        _ => None,
    }
}

fn cell_style(attrs: PaneAttributes) -> CellStyle {
    CellStyle {
        bold: attrs.contains(PaneAttributes::BOLD),
        italic: attrs.contains(PaneAttributes::ITALIC),
        faint: attrs.contains(PaneAttributes::DIM),
        blink: attrs.contains(PaneAttributes::BLINK),
        inverse: attrs.contains(PaneAttributes::REVERSE),
        invisible: attrs.contains(PaneAttributes::HIDDEN),
        strikethrough: attrs.contains(PaneAttributes::STRIKETHROUGH),
        overline: attrs.contains(PaneAttributes::OVERLINE),
        underline: libghostty_vt::style::Underline::None,
    }
}

fn rmux_key_token(input: KeyInput) -> Option<String> {
    if input.mods.command {
        return None;
    }

    if let Some(utf8) = input.utf8
        && !has_terminal_modifier(input.mods)
    {
        return Some(utf8.to_owned());
    }

    let key = rmux_key_base(input)?;
    let mut modifiers = Vec::new();
    if input.mods.ctrl {
        modifiers.push("C");
    }
    if input.mods.alt {
        modifiers.push("M");
    }
    if should_encode_shift_modifier(input) {
        modifiers.push("S");
    }

    if modifiers.is_empty() {
        Some(key)
    } else {
        Some(format!("{}-{key}", modifiers.join("-")))
    }
}

fn has_terminal_modifier(mods: crate::terminal::KeyMods) -> bool {
    mods.ctrl || mods.alt
}

fn should_encode_shift_modifier(input: KeyInput) -> bool {
    input.mods.shift && input.utf8.is_none()
}

fn rmux_key_base(input: KeyInput) -> Option<String> {
    if input.mods.ctrl && input.unshifted.is_some() {
        return input.unshifted.map(|unshifted| unshifted.to_string());
    }
    if let Some(utf8) = input.utf8 {
        return Some(utf8.to_owned());
    }
    rmux_named_key(input.key).map(str::to_owned)
}

fn rmux_named_key(key: crate::terminal::TerminalKey) -> Option<&'static str> {
    use crate::terminal::TerminalKey;

    Some(match key {
        TerminalKey::Enter => "Enter",
        TerminalKey::Tab => "Tab",
        TerminalKey::Backspace => "BSpace",
        TerminalKey::Escape => "Escape",
        TerminalKey::ArrowLeft => "Left",
        TerminalKey::ArrowRight => "Right",
        TerminalKey::ArrowUp => "Up",
        TerminalKey::ArrowDown => "Down",
        TerminalKey::Home => "Home",
        TerminalKey::End => "End",
        TerminalKey::PageUp => "PageUp",
        TerminalKey::PageDown => "PageDown",
        TerminalKey::Delete => "Delete",
        TerminalKey::Insert => "Insert",
        TerminalKey::Space => "Space",
        TerminalKey::F1 => "F1",
        TerminalKey::F2 => "F2",
        TerminalKey::F3 => "F3",
        TerminalKey::F4 => "F4",
        TerminalKey::F5 => "F5",
        TerminalKey::F6 => "F6",
        TerminalKey::F7 => "F7",
        TerminalKey::F8 => "F8",
        TerminalKey::F9 => "F9",
        TerminalKey::F10 => "F10",
        TerminalKey::F11 => "F11",
        TerminalKey::F12 => "F12",
        TerminalKey::A => "a",
        TerminalKey::B => "b",
        TerminalKey::C => "c",
        TerminalKey::D => "d",
        TerminalKey::E => "e",
        TerminalKey::F => "f",
        TerminalKey::G => "g",
        TerminalKey::H => "h",
        TerminalKey::I => "i",
        TerminalKey::J => "j",
        TerminalKey::K => "k",
        TerminalKey::L => "l",
        TerminalKey::M => "m",
        TerminalKey::N => "n",
        TerminalKey::O => "o",
        TerminalKey::P => "p",
        TerminalKey::Q => "q",
        TerminalKey::R => "r",
        TerminalKey::S => "s",
        TerminalKey::T => "t",
        TerminalKey::U => "u",
        TerminalKey::V => "v",
        TerminalKey::W => "w",
        TerminalKey::X => "x",
        TerminalKey::Y => "y",
        TerminalKey::Z => "z",
        _ => return None,
    })
}

fn rmux_mouse_input(input: MouseInput) -> String {
    let (col, row) = rmux_mouse_position(input);
    let mut button = match input.action {
        MouseAction::Motion => input
            .button
            .map_or(35, |button| 32 + rmux_mouse_button(button)),
        MouseAction::Press | MouseAction::Release => input.button.map_or(3, rmux_mouse_button),
    };
    if input.mods.shift {
        button += 4;
    }
    if input.mods.alt {
        button += 8;
    }
    if input.mods.ctrl {
        button += 16;
    }
    let suffix = if input.action == MouseAction::Release {
        'm'
    } else {
        'M'
    };
    format!("\x1b[<{button};{col};{row}{suffix}")
}

fn rmux_mouse_button(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::Four => 64,
        MouseButton::Five => 65,
        MouseButton::Six => 66,
        MouseButton::Seven => 67,
        MouseButton::Eight => 68,
        MouseButton::Nine => 69,
        MouseButton::Ten => 70,
        MouseButton::Eleven => 71,
    }
}

fn rmux_mouse_position(input: MouseInput) -> (u16, u16) {
    let cell_width = input.size.cell_width.max(1) as f32;
    let cell_height = input.size.cell_height.max(1) as f32;
    let x = (input.x - input.size.padding_left as f32).max(0.0);
    let y = (input.y - input.size.padding_top as f32).max(0.0);
    let col = (x / cell_width).floor() as u32 + 1;
    let row = (y / cell_height).floor() as u32 + 1;
    (
        col.min(u32::from(u16::MAX)) as u16,
        row.min(u32::from(u16::MAX)) as u16,
    )
}

struct RmuxAttachedClient {
    command_tx: mpsc::Sender<AttachIoCommand>,
    worker: Option<thread::JoinHandle<()>>,
}

enum AttachIoCommand {
    Message(AttachMessage),
    Resize(TerminalGeometry),
    Shutdown,
}

impl RmuxAttachedClient {
    fn connect(
        endpoint: &RmuxEndpoint,
        target: &PaneRef,
        geometry: TerminalGeometry,
    ) -> Result<Self> {
        let (stream, initial_bytes) = open_attach_stream(endpoint, target, geometry)?;
        let (command_tx, command_rx) = mpsc::channel();
        let worker = thread::spawn(move || run_attach_stream(stream, initial_bytes, command_rx));
        let attach = Self {
            command_tx,
            worker: Some(worker),
        };
        attach.resize(geometry)?;
        Ok(attach)
    }

    fn write_input(&self, bytes: &[u8]) -> Result<()> {
        self.send_message(AttachMessage::Data(bytes.to_vec()))
    }

    fn resize(&self, geometry: TerminalGeometry) -> Result<()> {
        self.command_tx
            .send(AttachIoCommand::Resize(geometry))
            .context("queue rmux attach resize")
    }

    fn send_message(&self, message: AttachMessage) -> Result<()> {
        self.command_tx
            .send(AttachIoCommand::Message(message))
            .context("queue rmux attach message")
    }
}

impl Drop for RmuxAttachedClient {
    fn drop(&mut self) {
        let _ = self.command_tx.send(AttachIoCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn open_attach_stream(
    endpoint: &RmuxEndpoint,
    target: &PaneRef,
    geometry: TerminalGeometry,
) -> Result<(rmux_ipc::BlockingLocalStream, Vec<u8>)> {
    let endpoint = local_endpoint(endpoint)?;
    let mut stream = connect_blocking(&endpoint, Duration::from_secs(2))
        .context("connect rmux IPC endpoint for attach input")?;
    let request = attach_session_request(target, geometry);
    let frame = encode_frame(&request).context("encode rmux attach input request")?;
    stream
        .write_all(&frame)
        .context("write rmux attach input request")?;
    stream.flush().context("flush rmux attach input request")?;

    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 4096];
    loop {
        if let Some(response) = decoder
            .next_frame::<Response>()
            .context("decode rmux attach input response")?
        {
            return match response {
                Response::AttachSession(_) => Ok((stream, decoder.remaining_bytes().to_vec())),
                Response::Error(error) => Err(anyhow::anyhow!(error.error.to_string()))
                    .context("rmux rejected attach input upgrade"),
                other => Err(anyhow::anyhow!(
                    "rmux attach input request received unexpected {} response",
                    other.command_name()
                )),
            };
        }

        let read = stream
            .read(&mut buffer)
            .context("read rmux attach input response")?;
        anyhow::ensure!(read > 0, "rmux closed before acknowledging attach input");
        decoder.push_bytes(&buffer[..read]);
    }
}

fn attach_session_request(target: &PaneRef, geometry: TerminalGeometry) -> Request {
    Request::AttachSessionExt2(AttachSessionExt2Request {
        target: Some(target.session_name.clone()),
        target_spec: Some(attach_target_spec(target)),
        detach_other_clients: false,
        kill_other_clients: false,
        read_only: false,
        skip_environment_update: false,
        flags: None,
        working_directory: None,
        client_terminal: ClientTerminalContext {
            terminal_features: Vec::new(),
            utf8: true,
        },
        client_size: Some(TerminalSize::new(geometry.cols, geometry.rows)),
    })
}

fn attach_target_spec(target: &PaneRef) -> String {
    format!(
        "{}:{}.{}",
        target.session_name, target.window_index, target.pane_index
    )
}

fn rmux_terminal_geometry(geometry: TerminalGeometry) -> RmuxTerminalGeometry {
    let pixels = TerminalPixels::new(geometry.pixel_width(), geometry.pixel_height());
    RmuxTerminalGeometry::new(geometry.cols, geometry.rows).with_pixels(pixels)
}

fn run_attach_stream(
    mut stream: rmux_ipc::BlockingLocalStream,
    initial_bytes: Vec<u8>,
    command_rx: mpsc::Receiver<AttachIoCommand>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&initial_bytes);
    let mut buffer = [0_u8; 4096];

    loop {
        while let Ok(command) = command_rx.try_recv() {
            match command {
                AttachIoCommand::Message(message) => {
                    let _ = write_attach_message(&mut stream, &message);
                }
                AttachIoCommand::Resize(geometry) => {
                    let _ = write_attach_message(
                        &mut stream,
                        &AttachMessage::ResizeGeometry(rmux_terminal_geometry(geometry)),
                    );
                }
                AttachIoCommand::Shutdown => return,
            }
        }

        drain_attach_messages(&mut decoder);

        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => decoder.push_bytes(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

fn drain_attach_messages(decoder: &mut AttachFrameDecoder) {
    for _ in 0..32 {
        match decoder.next_message() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return,
        }
    }
}

fn write_attach_message(
    stream: &mut rmux_ipc::BlockingLocalStream,
    message: &AttachMessage,
) -> Result<()> {
    let frame = encode_attach_message(message).context("encode rmux attach message")?;
    stream
        .write_all(&frame)
        .context("write rmux attach message")?;
    stream.flush().context("flush rmux attach message")?;
    Ok(())
}

fn local_endpoint(endpoint: &RmuxEndpoint) -> Result<LocalEndpoint> {
    match endpoint {
        RmuxEndpoint::UnixSocket(path) => Ok(LocalEndpoint::from_path(path.clone())),
        RmuxEndpoint::WindowsPipe(name) => Ok(LocalEndpoint::from_path(name.clone().into())),
        RmuxEndpoint::Default => {
            anyhow::bail!("rmux endpoint should be resolved before attach input")
        }
        _ => anyhow::bail!("unsupported rmux endpoint variant for attach input"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{KeyMods, MouseAction, MouseButton, MouseEncoderSize, TerminalKey};
    use rmux_sdk::{PaneCell, PaneCursor, PaneGlyph, SessionName};

    fn snapshot(cells: Vec<PaneCell>) -> PaneSnapshot {
        PaneSnapshot::new(3, 2, cells, PaneCursor::new(1, 2, true, 0)).unwrap()
    }

    fn cell(text: &str, attrs: PaneAttributes) -> PaneCell {
        PaneCell {
            glyph: PaneGlyph::new(text.to_owned(), 1),
            attributes: attrs,
            foreground: PaneColor::Rgb {
                red: 1,
                green: 2,
                blue: 3,
            },
            background: PaneColor::Default,
            underline: PaneColor::Default,
        }
    }

    fn key_input(
        key: TerminalKey,
        mods: KeyMods,
        utf8: Option<&'static str>,
        unshifted: Option<char>,
    ) -> KeyInput {
        KeyInput {
            key,
            mods,
            repeat: false,
            utf8,
            unshifted,
        }
    }

    #[test]
    fn rmux_key_token_preserves_readline_ctrl_chords() {
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::U,
                KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                Some("u"),
                Some('u'),
            )),
            Some("C-u".to_owned())
        );
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::C,
                KeyMods {
                    ctrl: true,
                    ..Default::default()
                },
                Some("c"),
                Some('c'),
            )),
            Some("C-c".to_owned())
        );
    }

    #[test]
    fn rmux_key_token_uses_a_general_modifier_policy() {
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::ArrowLeft,
                KeyMods {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
                None,
                None,
            )),
            Some("C-S-Left".to_owned())
        );
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::ArrowRight,
                KeyMods {
                    alt: true,
                    shift: true,
                    ..Default::default()
                },
                None,
                None,
            )),
            Some("M-S-Right".to_owned())
        );
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::Q,
                KeyMods {
                    command: true,
                    ..Default::default()
                },
                Some("q"),
                Some('q'),
            )),
            None
        );
    }

    #[test]
    fn rmux_key_token_keeps_plain_text_plain() {
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::U,
                KeyMods::default(),
                Some("u"),
                Some('u')
            )),
            Some("u".to_owned())
        );
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::U,
                KeyMods {
                    shift: true,
                    ..Default::default()
                },
                Some("U"),
                Some('u'),
            )),
            Some("U".to_owned())
        );
    }

    #[test]
    fn rmux_key_token_preserves_alt_shift_letter_chords() {
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::Q,
                KeyMods {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                Some("Q"),
                Some('q'),
            )),
            Some("M-Q".to_owned())
        );
        assert_eq!(
            rmux_key_token(key_input(
                TerminalKey::Q,
                KeyMods {
                    alt: true,
                    ..Default::default()
                },
                Some("q"),
                Some('q'),
            )),
            Some("M-q".to_owned())
        );
    }

    #[test]
    fn rmux_mouse_input_uses_sgr_reports_for_attach_input() {
        let size = MouseEncoderSize {
            screen_width: 800,
            screen_height: 480,
            cell_width: 10,
            cell_height: 20,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        };
        let mods = KeyMods {
            shift: true,
            alt: true,
            ctrl: true,
            ..Default::default()
        };

        assert_eq!(
            rmux_mouse_input(MouseInput {
                action: MouseAction::Press,
                button: Some(MouseButton::Left),
                mods,
                x: 0.0,
                y: 0.0,
                size,
            }),
            "\x1b[<28;1;1M"
        );
        assert_eq!(
            rmux_mouse_input(MouseInput {
                action: MouseAction::Release,
                button: Some(MouseButton::Left),
                mods: KeyMods::default(),
                x: 0.0,
                y: 0.0,
                size,
            }),
            "\x1b[<0;1;1m"
        );
        assert_eq!(
            rmux_mouse_input(MouseInput {
                action: MouseAction::Motion,
                button: Some(MouseButton::Left),
                mods: KeyMods::default(),
                x: 10.0,
                y: 20.0,
                size,
            }),
            "\x1b[<32;2;2M"
        );
        assert_eq!(
            rmux_mouse_input(MouseInput {
                action: MouseAction::Press,
                button: Some(MouseButton::Four),
                mods: KeyMods::default(),
                x: 0.0,
                y: 0.0,
                size,
            }),
            "\x1b[<64;1;1M"
        );
    }

    #[test]
    fn attach_session_request_targets_exact_pane_and_client_size() {
        let request = attach_session_request(
            &PaneRef::new(SessionName::new("alpha").unwrap(), 0, 0),
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 10,
                cell_height: 20,
            },
        );

        let Request::AttachSessionExt2(request) = request else {
            panic!("expected attach-session-ext2 request");
        };
        assert_eq!(request.target, Some(SessionName::new("alpha").unwrap()));
        assert_eq!(request.target_spec.as_deref(), Some("alpha:0.0"));
        assert_eq!(request.client_size, Some(TerminalSize::new(80, 24)));
        assert!(request.client_terminal.utf8);
    }

    #[test]
    fn rmux_terminal_geometry_preserves_cells_and_pixels() {
        let geometry = rmux_terminal_geometry(TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 9,
            cell_height: 18,
        });

        assert_eq!(geometry.size, TerminalSize::new(80, 24));
        assert_eq!(geometry.pixels, Some(TerminalPixels::new(720, 432)));
    }

    #[test]
    fn native_render_harness_projects_rmux_snapshot_into_bootty_frame() {
        let frame = render_frame_from_snapshot(
            &snapshot(vec![
                cell("a", PaneAttributes::BOLD),
                cell("界", PaneAttributes::ITALIC),
                cell("", PaneAttributes::EMPTY),
                cell("b", PaneAttributes::UNDERLINE),
                cell(" ", PaneAttributes::EMPTY),
                cell("c", PaneAttributes::REVERSE),
            ]),
            &TerminalColorConfig::default(),
        );

        let rendered = frame
            .cells
            .iter()
            .map(|cell| frame.cell_text(cell).iter().collect::<String>())
            .collect::<Vec<_>>();

        assert_eq!(frame.cols, 3);
        assert_eq!(frame.rows, 2);
        assert_eq!(rendered, vec!["a", "界", "b", " ", "c"]);
        assert!(frame.cells[0].style.bold);
        assert!(frame.cells[1].style.italic);
        assert!(frame.cells[4].style.inverse);
        assert_eq!(
            frame.cursor.map(|cursor| (cursor.x, cursor.y)),
            Some((2, 1))
        );
    }

    #[test]
    fn native_render_harness_skips_wide_padding_cells() {
        let mut padding = cell("", PaneAttributes::EMPTY);
        padding.glyph = PaneGlyph {
            text: String::new(),
            width: 0,
            padding: true,
        };
        let frame = render_frame_from_snapshot(
            &snapshot(vec![
                cell("界", PaneAttributes::EMPTY),
                padding,
                cell("x", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
            ]),
            &TerminalColorConfig::default(),
        );

        let positions = frame
            .cells
            .iter()
            .map(|cell| {
                (
                    cell.x,
                    cell.y,
                    frame.cell_text(cell).iter().collect::<String>(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            positions,
            vec![(0, 0, "界".to_owned()), (2, 0, "x".to_owned())]
        );
    }

    #[test]
    fn native_render_harness_skips_kitty_virtual_placeholder_cells() {
        let frame = render_frame_from_snapshot(
            &snapshot(vec![
                cell("a", PaneAttributes::EMPTY),
                cell("\u{10eeee}\u{0305}\u{0305}", PaneAttributes::EMPTY),
                cell("b", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
                cell("", PaneAttributes::EMPTY),
            ]),
            &TerminalColorConfig::default(),
        );

        assert_eq!(frame.text.iter().collect::<String>(), "ab");
        assert_eq!(frame.cells.len(), 2);
        assert_eq!(frame.cells[0].x, 0);
        assert_eq!(frame.cells[1].x, 2);
    }
}
