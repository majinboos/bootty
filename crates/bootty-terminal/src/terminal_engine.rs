use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
    time::Instant,
};

use crate::{
    geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface},
    terminal_image::{
        KittyImageDataCache, KittyImageFrame, KittyVirtualCell, append_virtual_image_placements,
        collect_kitty_image_frame,
    },
    terminal_png_decoder::BoottyPngDecoder,
};
use anyhow::Result;
use libghostty_vt::{
    Terminal, TerminalOptions, focus, key,
    kitty::graphics::set_png_decoder,
    mouse, paste,
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIterator},
    style::RgbColor,
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
        PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, SizeReportSize,
        TertiaryDeviceAttributes,
    },
};

use crate::terminal_frame::{
    CellStyle, CursorSnapshot, FrameColors, FrameScrollbar, FrameStats, RenderCell, RenderFrame,
};
use crate::terminal_input_model::{KeyInput, MouseAction, MouseEncoderSize, MouseInput};
use crate::terminal_palette::generate_256_palette;

#[cfg(test)]
use {
    crate::terminal_input_model::{KeyMods, MouseButton, TerminalKey},
    libghostty_vt::style::Underline,
};

pub const DEFAULT_MAX_SCROLLBACK: usize = 0;
pub const NATIVE_SCROLLBACK_TARGET_ROWS: usize = 1_000_000;
pub const NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE: usize = 320;
pub const NATIVE_MAX_SCROLLBACK: usize =
    NATIVE_SCROLLBACK_TARGET_ROWS * NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
pub const TERMINAL_TERM: &str = "xterm-ghostty";
pub const TERMINAL_BACKGROUND: (u8, u8, u8) = (0x1a, 0x1b, 0x25);
pub const TERMINAL_FOREGROUND: (u8, u8, u8) = (0xc0, 0xca, 0xf5);

#[derive(Clone, Debug)]
pub struct TerminalColorConfig {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub cursor: Option<RgbColor>,
    pub cursor_text: Option<RgbColor>,
    pub selection_background: Option<RgbColor>,
    pub selection_foreground: Option<RgbColor>,
    pub palette: Vec<RgbColor>,
}

impl Default for TerminalColorConfig {
    fn default() -> Self {
        Self {
            background: rgb(TERMINAL_BACKGROUND),
            foreground: rgb(TERMINAL_FOREGROUND),
            cursor: None,
            cursor_text: None,
            selection_background: None,
            selection_foreground: None,
            palette: default_palette16().into(),
        }
    }
}

pub struct TerminalEngine {
    terminal: Terminal<'static, 'static>,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    image_placements: libghostty_vt::kitty::graphics::PlacementIterator<'static>,
    image_data_cache: KittyImageDataCache,
    frame: RenderFrame,
    grapheme_scratch: Vec<char>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    mouse_any_button_pressed: bool,
    mouse_encoder_options_dirty: bool,
    mouse_encoder_size: Option<MouseEncoderSize>,
    geometry: TerminalGeometry,
    size_report_geometry: Arc<Mutex<TerminalGeometry>>,
    osc_pwd_pending: Vec<u8>,
    current_working_directory: String,
    colors: TerminalColorConfig,
    content_epoch: u64,
    extracted_content_epoch: u64,
    kitty_graphics_touched: bool,
}

fn configure_default_colors(
    terminal: &mut Terminal<'static, 'static>,
    config: &TerminalColorConfig,
) -> Result<()> {
    let mut base = terminal.default_color_palette()?;
    for (index, color) in config.palette.iter().take(256).copied().enumerate() {
        base[index] = color;
    }
    let palette = generate_256_palette(
        &base,
        &[false; 256],
        config.background,
        config.foreground,
        false,
    );
    terminal.set_default_color_palette(Some(palette))?;
    terminal
        .set_default_bg_color(Some(config.background))?
        .set_default_fg_color(Some(config.foreground))?
        .set_default_cursor_color(config.cursor)?;
    Ok(())
}

fn rgb((r, g, b): (u8, u8, u8)) -> RgbColor {
    RgbColor { r, g, b }
}

fn rgb_hex(value: u32) -> RgbColor {
    RgbColor {
        r: ((value >> 16) & 0xff) as u8,
        g: ((value >> 8) & 0xff) as u8,
        b: (value & 0xff) as u8,
    }
}

fn default_palette16() -> [RgbColor; 16] {
    [
        0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868,
        0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
    ]
    .map(rgb_hex)
}

fn default_device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            [DeviceAttributeFeature::ANSI_COLOR],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT220,
            firmware_version: 0,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let first = *needle.first()?;
    if needle.len() == 1 {
        return haystack.iter().position(|byte| *byte == first);
    }

    let mut offset = 0;
    while let Some(relative_start) = haystack[offset..].iter().position(|byte| *byte == first) {
        let start = offset + relative_start;
        if haystack[start..].starts_with(needle) {
            return Some(start);
        }
        offset = start + 1;
    }
    None
}

fn find_osc_terminator(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x07 => return Some((index, 1)),
            0x1b if bytes.get(index + 1) == Some(&b'\\') => return Some((index, 2)),
            _ => index += 1,
        }
    }
    None
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TerminalWriteFeatures {
    tmux_passthrough: bool,
    kitty_graphics: bool,
    osc_pwd: bool,
}

impl TerminalWriteFeatures {
    fn needs_sanitizing(self) -> bool {
        self.tmux_passthrough || self.kitty_graphics || self.osc_pwd
    }
}

fn terminal_write_features(data: &[u8]) -> TerminalWriteFeatures {
    let mut features = TerminalWriteFeatures::default();
    let mut index = 0;
    while let Some(relative_start) = data[index..].iter().position(|byte| *byte == 0x1b) {
        let start = index + relative_start;
        match data.get(start + 1).copied() {
            Some(b'P') if data.get(start + 2..start + 7) == Some(b"tmux;") => {
                features.tmux_passthrough = true;
            }
            Some(b'_') if data.get(start + 2) == Some(&b'G') => {
                features.kitty_graphics = true;
            }
            Some(b']') if data.get(start + 2..start + 4) == Some(b"7;") => {
                features.osc_pwd = true;
            }
            _ => {}
        }
        if features.tmux_passthrough && features.kitty_graphics && features.osc_pwd {
            break;
        }
        index = start + 1;
    }
    features
}

fn unwrap_tmux_passthrough_commands(data: &[u8]) -> Cow<'_, [u8]> {
    let mut out: Option<Vec<u8>> = None;
    let mut read_start = 0;
    while let Some(relative_start) = find_subslice(&data[read_start..], b"\x1bPtmux;") {
        let start = read_start + relative_start;
        let payload_start = start + 7;
        let mut cursor = payload_start;
        let mut payload = Vec::new();
        let mut end = None;

        while cursor < data.len() {
            if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&0x1b) {
                payload.push(0x1b);
                cursor += 2;
                continue;
            }
            if data[cursor] == 0x1b && data.get(cursor + 1) == Some(&b'\\') {
                end = Some(cursor + 2);
                break;
            }
            payload.push(data[cursor]);
            cursor += 1;
        }

        let Some(end) = end else {
            read_start = payload_start;
            continue;
        };

        let out = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        out.extend_from_slice(&data[read_start..start]);
        out.extend_from_slice(&payload);
        read_start = end;
    }

    match out {
        Some(mut out) => {
            out.extend_from_slice(&data[read_start..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(data),
    }
}

struct SanitizedKittyGraphics<'a> {
    bytes: Cow<'a, [u8]>,
    touched: bool,
}

fn sanitize_kitty_graphics_commands(data: &[u8]) -> SanitizedKittyGraphics<'_> {
    let mut out: Option<Vec<u8>> = None;
    let mut read_start = 0;
    let mut touched = false;
    while let Some(relative_start) = find_subslice(&data[read_start..], b"\x1b_G") {
        touched = true;
        let start = read_start + relative_start;
        let payload_start = start + 3;
        let Some((payload_len, terminator_len)) = find_osc_terminator(&data[payload_start..])
        else {
            read_start = payload_start;
            continue;
        };
        let payload_end = payload_start + payload_len;
        let payload = &data[payload_start..payload_end];
        let control_end = payload
            .iter()
            .position(|byte| *byte == b';')
            .unwrap_or(payload.len());
        let control = &payload[..control_end];
        let Some(sanitized_control) = sanitize_kitty_graphics_control(control) else {
            read_start = payload_end + terminator_len;
            continue;
        };

        let out = out.get_or_insert_with(|| Vec::with_capacity(data.len()));
        out.extend_from_slice(&data[read_start..payload_start]);
        out.extend_from_slice(&sanitized_control);
        out.extend_from_slice(&payload[control_end..payload.len()]);
        out.extend_from_slice(&data[payload_end..payload_end + terminator_len]);
        read_start = payload_end + terminator_len;
    }

    match out {
        Some(mut out) => {
            out.extend_from_slice(&data[read_start..]);
            SanitizedKittyGraphics {
                bytes: Cow::Owned(out),
                touched,
            }
        }
        None => SanitizedKittyGraphics {
            bytes: Cow::Borrowed(data),
            touched,
        },
    }
}

fn sanitize_kitty_graphics_control(control: &[u8]) -> Option<Vec<u8>> {
    let mut changed = false;
    let mut sanitized = Vec::with_capacity(control.len());
    for field in control.split(|byte| *byte == b',') {
        let Some(separator) = field.iter().position(|byte| *byte == b'=') else {
            append_kitty_graphics_field(&mut sanitized, field);
            continue;
        };
        let key = &field[..separator];
        let value = &field[separator + 1..];
        if key.len() != 1 || value.len() > 11 {
            changed = true;
            continue;
        }
        append_kitty_graphics_field(&mut sanitized, field);
    }

    changed.then_some(sanitized)
}

fn append_kitty_graphics_field(out: &mut Vec<u8>, field: &[u8]) {
    if !out.is_empty() {
        out.push(b',');
    }
    out.extend_from_slice(field);
}

impl TerminalEngine {
    pub fn new(geometry: TerminalGeometry) -> Result<Self> {
        Self::new_with_colors(geometry, TerminalColorConfig::default())
    }

    pub fn new_with_colors(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
    ) -> Result<Self> {
        Self::new_with_scrollback(geometry, colors, DEFAULT_MAX_SCROLLBACK)
    }

    pub fn new_with_scrollback(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        max_scrollback: usize,
    ) -> Result<Self> {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: geometry.cols,
            rows: geometry.rows,
            max_scrollback,
        })?;
        configure_default_colors(&mut terminal, &colors)?;
        let size_report_geometry = Arc::new(Mutex::new(geometry));
        let report_geometry = size_report_geometry.clone();
        terminal.on_size(move |_terminal| {
            let geometry = *report_geometry.lock().ok()?;
            Some(SizeReportSize {
                rows: geometry.rows,
                columns: geometry.cols,
                cell_width: geometry.cell_width,
                cell_height: geometry.cell_height,
            })
        })?;
        terminal.on_device_attributes(|_terminal| Some(default_device_attributes()))?;
        terminal.resize(
            geometry.cols,
            geometry.rows,
            geometry.cell_width,
            geometry.cell_height,
        )?;
        terminal.set_kitty_image_from_file_allowed(true)?;
        terminal.set_kitty_image_from_temp_file_allowed(true)?;
        terminal.set_kitty_image_from_shared_mem_allowed(false)?;
        set_png_decoder(Some(Box::new(BoottyPngDecoder)))?;

        let mut engine = Self {
            terminal,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            image_placements: libghostty_vt::kitty::graphics::PlacementIterator::new()?,
            image_data_cache: KittyImageDataCache::default(),
            frame: RenderFrame::default(),
            grapheme_scratch: Vec::new(),
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            mouse_any_button_pressed: false,
            mouse_encoder_options_dirty: true,
            mouse_encoder_size: None,
            geometry,
            size_report_geometry,
            osc_pwd_pending: Vec::new(),
            current_working_directory: String::new(),
            colors,
            content_epoch: 0,
            extracted_content_epoch: u64::MAX,
            kitty_graphics_touched: false,
        };
        engine.set_kitty_image_storage_limit(64 * 1024 * 1024)?;
        Ok(engine)
    }

    fn mark_content_changed(&mut self) {
        self.content_epoch = self.content_epoch.wrapping_add(1);
    }

    pub fn set_kitty_image_storage_limit(&mut self, limit: u64) -> Result<()> {
        self.terminal.set_kitty_image_storage_limit(limit)?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn on_pty_write(
        &mut self,
        f: impl libghostty_vt::terminal::PtyWriteFn<'static, 'static>,
    ) -> Result<()> {
        self.terminal.on_pty_write(f)?;
        Ok(())
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn geometry(&self) -> TerminalGeometry {
        self.geometry
    }

    pub fn default_color_palette(&self) -> Result<[RgbColor; 256]> {
        self.terminal.default_color_palette().map_err(Into::into)
    }

    pub fn set_default_color_palette(&mut self, palette: [RgbColor; 256]) -> Result<()> {
        self.terminal.set_default_color_palette(Some(palette))?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        configure_default_colors(&mut self.terminal, &colors)?;
        self.colors = colors;
        self.mark_content_changed();
        Ok(())
    }

    pub fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }

        self.geometry = geometry;
        if let Ok(mut report_geometry) = self.size_report_geometry.lock() {
            *report_geometry = geometry;
        }
        self.terminal.resize(
            geometry.cols,
            geometry.rows,
            geometry.cell_width,
            geometry.cell_height,
        )?;
        self.mark_content_changed();

        Ok(())
    }

    pub fn write_vt(&mut self, bytes: &[u8]) {
        let mut features = terminal_write_features(bytes);
        if !self.osc_pwd_pending.is_empty() {
            features.osc_pwd = true;
        }
        if !features.needs_sanitizing() {
            self.terminal.vt_write(bytes);
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        let bytes = if features.tmux_passthrough {
            let unwrapped = unwrap_tmux_passthrough_commands(bytes);
            features = terminal_write_features(unwrapped.as_ref());
            if !self.osc_pwd_pending.is_empty() {
                features.osc_pwd = true;
            }
            unwrapped
        } else {
            Cow::Borrowed(bytes)
        };

        let sanitized = if features.kitty_graphics {
            sanitize_kitty_graphics_commands(bytes.as_ref())
        } else {
            SanitizedKittyGraphics {
                bytes,
                touched: false,
            }
        };
        if sanitized.touched {
            self.kitty_graphics_touched = true;
        }
        self.terminal.vt_write(sanitized.bytes.as_ref());
        if features.osc_pwd {
            self.apply_osc_pwd_updates(sanitized.bytes.as_ref());
        }
        self.mouse_encoder_options_dirty = true;
        self.mark_content_changed();
    }

    pub fn current_working_directory(&self) -> &str {
        &self.current_working_directory
    }

    fn apply_osc_pwd_updates(&mut self, data: &[u8]) {
        let mut bytes = Vec::with_capacity(self.osc_pwd_pending.len() + data.len());
        bytes.extend_from_slice(&self.osc_pwd_pending);
        bytes.extend_from_slice(data);
        self.osc_pwd_pending.clear();

        let mut search_start = 0;
        while let Some(relative_start) = find_subslice(&bytes[search_start..], b"\x1b]7;") {
            let start = search_start + relative_start;
            let payload_start = start + 4;
            match find_osc_terminator(&bytes[payload_start..]) {
                Some((payload_len, terminator_len)) => {
                    let payload = &bytes[payload_start..payload_start + payload_len];
                    if let Ok(pwd) = std::str::from_utf8(payload) {
                        self.current_working_directory.clear();
                        self.current_working_directory.push_str(pwd);
                    }
                    search_start = payload_start + payload_len + terminator_len;
                }
                None => {
                    self.osc_pwd_pending.extend_from_slice(&bytes[start..]);
                    break;
                }
            }
        }
    }

    pub fn encode_paste_to_vec(&mut self, text: &str, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut data = text.as_bytes().to_vec();
        let mut capacity = data.len().saturating_add(64).max(64);

        loop {
            out.resize(capacity, 0);
            match paste::encode(&mut data, bracketed, out) {
                Ok(written) => {
                    out.truncate(written);
                    return Ok(());
                }
                Err(libghostty_vt::Error::OutOfSpace { required }) => {
                    capacity = required.max(capacity.saturating_mul(2));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn encode_key_to_vec(&mut self, input: KeyInput, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .set_alt_esc_prefix(true)
            .set_macos_option_as_alt(key::OptionAsAlt::True);
        self.key_event
            .set_action(if input.repeat {
                key::Action::Repeat
            } else {
                key::Action::Press
            })
            .set_key(input.key.into())
            .set_mods(input.mods.into())
            .set_utf8(input.utf8);
        self.key_event
            .set_unshifted_codepoint(input.unshifted.unwrap_or('\0'));
        self.key_encoder.encode_to_vec(&self.key_event, out)?;
        Ok(())
    }

    pub fn encode_focus_to_vec(&mut self, gained: bool, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        if !self.terminal.mode(Mode::FOCUS_EVENT)? {
            return Ok(());
        }

        let event = if gained {
            focus::Event::Gained
        } else {
            focus::Event::Lost
        };
        out.resize(16, 0);
        let written = event.encode(out)?;
        out.truncate(written);
        Ok(())
    }

    pub fn encode_mouse_to_vec(&mut self, input: MouseInput, out: &mut Vec<u8>) -> Result<()> {
        out.clear();
        if !self.terminal.is_mouse_tracking()? {
            return Ok(());
        }

        if self.mouse_encoder_options_dirty {
            self.mouse_encoder
                .set_options_from_terminal(&self.terminal)
                .set_track_last_cell(true);
            self.mouse_encoder_options_dirty = false;
            self.mouse_encoder_size = None;
        }
        if self.mouse_encoder_size != Some(input.size) {
            self.mouse_encoder.set_size(input.size.into());
            self.mouse_encoder_size = Some(input.size);
        }
        self.mouse_encoder
            .set_any_button_pressed(self.mouse_any_button_pressed);
        self.mouse_event
            .set_action(input.action.into())
            .set_button(input.button.map(Into::into))
            .set_mods(input.mods.into())
            .set_position(mouse::Position {
                x: input.x,
                y: input.y,
            });
        if out.capacity() < 64 {
            out.reserve(64 - out.capacity());
        }
        if let Err(error) = self.mouse_encoder.encode_to_vec(&self.mouse_event, out) {
            match error {
                libghostty_vt::Error::OutOfSpace { required } if required > out.capacity() => {
                    out.clear();
                    out.reserve(required - out.capacity());
                    self.mouse_encoder.encode_to_vec(&self.mouse_event, out)?;
                }
                error => return Err(error.into()),
            }
        }

        match input.action {
            MouseAction::Press => self.mouse_any_button_pressed = true,
            MouseAction::Release => self.mouse_any_button_pressed = false,
            MouseAction::Motion => {}
        }

        Ok(())
    }

    pub fn scroll_viewport_delta(&mut self, delta: isize) {
        self.terminal.scroll_viewport(ScrollViewport::Delta(delta));
        self.mark_content_changed();
    }

    pub fn scroll_viewport_bottom(&mut self) {
        self.terminal.scroll_viewport(ScrollViewport::Bottom);
        self.mark_content_changed();
    }

    pub fn is_mouse_tracking(&self) -> Result<bool> {
        self.terminal.is_mouse_tracking().map_err(Into::into)
    }

    pub fn extract_frame(&mut self) -> Result<&RenderFrame> {
        let extract_start = Instant::now();
        let update_start = Instant::now();
        let snapshot = self.render_state.update(&self.terminal)?;
        let render_state_update_us = update_start.elapsed().as_micros() as u64;
        let colors = snapshot.colors()?;
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        let dirty = snapshot.dirty()?;
        let can_reuse_clean_frame = self.content_epoch == self.extracted_content_epoch
            && self.frame.cols == cols
            && self.frame.rows == rows
            && !self.frame.cells.is_empty();

        self.frame.cols = cols;
        self.frame.rows = rows;
        self.frame.dirty = if can_reuse_clean_frame {
            Dirty::Clean
        } else {
            dirty
        };
        self.frame.colors = FrameColors {
            background: colors.background,
            foreground: colors.foreground,
            cursor: colors.cursor,
            cursor_text: self.colors.cursor_text,
            selection_background: self.colors.selection_background,
            selection_foreground: self.colors.selection_foreground,
        };
        self.frame.cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|cursor| CursorSnapshot {
                x: cursor.x,
                y: cursor.y,
                at_wide_tail: cursor.at_wide_tail,
                style: snapshot
                    .cursor_visual_style()
                    .unwrap_or(CursorVisualStyle::Block),
                blinking: snapshot.cursor_blinking().unwrap_or(false),
                color: snapshot.cursor_color().ok().flatten().or(colors.cursor),
            })
        } else {
            None
        };
        let scrollbar = self.terminal.scrollbar()?;
        self.frame.scrollbar = Some(FrameScrollbar {
            total: scrollbar.total,
            offset: scrollbar.offset,
            len: scrollbar.len,
        });
        if can_reuse_clean_frame {
            self.frame.row_dirty.clear();
            self.frame
                .row_dirty
                .resize(usize::from(self.frame.rows), false);
            self.frame.stats = FrameStats {
                render_state_update_us,
                extraction_us: extract_start.elapsed().as_micros() as u64,
                cells: self.frame.cells.len(),
                chars: self.frame.text.len(),
                dirty_rows: 0,
            };
            return Ok(&self.frame);
        }

        self.frame.row_dirty.clear();
        self.frame.cells.clear();
        self.frame.text.clear();
        self.frame.images = KittyImageFrame::default();
        self.frame.stats = FrameStats::default();

        let mut row_iter = self.rows.update(&snapshot)?;
        let mut row_index = 0_u16;
        let mut virtual_placeholder_rows = Vec::new();
        let mut virtual_cells = Vec::new();

        while let Some(row) = row_iter.next() {
            let row_dirty = row.dirty()?;
            if row_dirty {
                self.frame.stats.dirty_rows += 1;
            }
            self.frame.row_dirty.push(row_dirty);
            if row.raw_row()?.has_kitty_virtual_placeholder()? {
                virtual_placeholder_rows.push(row_index);
            }
            let mut cell_iter = self.cells.update(row)?;
            let mut col_index = 0_u16;

            while let Some(cell) = cell_iter.next() {
                let style = cell.style()?;
                let grapheme_len = cell.graphemes_len()?;
                self.grapheme_scratch.resize(grapheme_len, '\0');
                if grapheme_len > 0 {
                    cell.graphemes_buf(&mut self.grapheme_scratch)?;
                }

                let is_virtual_placeholder = self.grapheme_scratch.first() == Some(&'\u{10EEEE}');
                if is_virtual_placeholder {
                    virtual_cells.push(KittyVirtualCell {
                        x: col_index,
                        y: row_index,
                        grapheme: self.grapheme_scratch[..grapheme_len].to_vec(),
                        foreground: style.fg_color,
                        underline_color: style.underline_color,
                    });
                }

                let text_start = self.frame.text.len();
                let text_len = if is_virtual_placeholder {
                    0
                } else {
                    self.frame
                        .text
                        .extend_from_slice(&self.grapheme_scratch[..grapheme_len]);
                    self.frame.stats.chars += grapheme_len;
                    grapheme_len
                };

                self.frame.stats.cells += 1;
                self.frame.cells.push(RenderCell {
                    x: col_index,
                    y: row_index,
                    text_start,
                    text_len,
                    fg: cell.fg_color()?,
                    bg: cell.bg_color()?,
                    style: CellStyle {
                        bold: style.bold,
                        italic: style.italic,
                        faint: style.faint,
                        blink: style.blink,
                        inverse: style.inverse,
                        invisible: style.invisible,
                        strikethrough: style.strikethrough,
                        overline: style.overline,
                        underline: style.underline,
                    },
                });

                col_index += 1;
            }

            row_index += 1;
        }

        self.frame.stats.render_state_update_us = render_state_update_us;
        if self.kitty_graphics_touched || !virtual_cells.is_empty() {
            let surface = TerminalSurface::for_logical_size(
                f32::from(self.geometry.pixel_width()),
                f32::from(self.geometry.pixel_height()),
                CellMetrics::new(
                    self.geometry.cell_width as f32,
                    self.geometry.cell_height as f32,
                ),
                TerminalPadding::default(),
            );
            let mut images = collect_kitty_image_frame(
                &self.terminal,
                surface,
                &mut self.image_placements,
                &mut self.image_data_cache,
            )
            .unwrap_or_default();
            append_virtual_image_placements(&self.terminal, surface, &mut images, &virtual_cells)?;
            images.virtual_placeholder_rows = virtual_placeholder_rows;
            self.frame.images = images;
        }
        self.frame.stats.extraction_us = extract_start.elapsed().as_micros() as u64;
        self.extracted_content_epoch = self.content_epoch;
        Ok(&self.frame)
    }
}

#[cfg(test)]
#[path = "terminal_engine/tests/mod.rs"]
mod tests;
