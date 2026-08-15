use std::{
    borrow::Cow,
    cell::RefCell,
    io::Write as _,
    rc::Rc,
    sync::{Arc, Mutex, Once},
    time::{Duration, Instant},
};

use crate::{
    geometry::{
        CellMetrics, GridPoint, SurfacePoint, TerminalGeometry, TerminalPadding, TerminalSurface,
    },
    terminal_image::{
        KittyImageDataCache, KittyImageFrame, KittyImagePlacement, KittyVirtualCell,
        append_virtual_image_placements, collect_kitty_image_frame,
    },
    terminal_png_decoder::BoottyPngDecoder,
};

mod logical_search;
pub(crate) mod write_ingress;

use logical_search::{
    CopyModeSearchMatch, copy_mode_logical_search_matches, frame_search_matches,
    normalize_search_char, normalized_search_query,
};
use write_ingress::{
    CURSOR_HOME, SanitizedKittyGraphics, SgrOptimizer, complete_streaming_control_prefix_len,
    contains_tracked_streaming_control, find_osc_terminator, find_subslice,
    repeated_cursor_home_prefix_len, sanitize_kitty_graphics_commands, split_osc_payload,
    terminal_write_features, unwrap_tmux_passthrough_commands,
};

use crate::terminal_frame::{
    CellStyle, CursorSnapshot, FrameColors, FrameCopyMode, FrameScrollbar, FrameSelection,
    FrameStats, RenderCell, RenderFrame,
};
use crate::terminal_input_model::{
    KeyInput, MacosOptionAsAlt, MouseAction, MouseEncoderSize, MouseInput,
};
use crate::terminal_palette::generate_256_palette;
use crate::terminal_side_effect::{TerminalHostAction, TerminalSideEffectCollector};
pub use crate::terminal_side_effect::{TerminalSideEffect, TerminalSideEffectEvent};
use anyhow::Result;
use base64::{Engine as _, engine::general_purpose};
use libghostty_vt::{
    Terminal, TerminalOptions,
    fmt::Format,
    focus, key,
    kitty::graphics::set_png_decoder,
    mouse, paste,
    render::{CellIterator, CursorVisualStyle, Dirty, RenderState, RowIteration, RowIterator},
    selection::{FormatOptions as SelectionFormatOptions, Selection, gesture},
    style::{RgbColor, StyleColor},
    terminal::{
        ColorScheme, ConformanceLevel, CursorStyle, DeviceAttributeFeature, DeviceAttributes,
        DeviceType, Mode, Point, PointCoordinate, PrimaryDeviceAttributes, ScrollViewport,
        SecondaryDeviceAttributes, SizeReportSize, TertiaryDeviceAttributes,
    },
};

pub const DEFAULT_MAX_SCROLLBACK: usize = 0;
const SELECTION_REPEAT_INTERVAL: Duration = Duration::from_millis(500);
/// Ceiling on button reports emitted for one wheel event. Well above a full-screen page scroll.
const MAX_WHEEL_REPORTS: usize = 1024;
const NATIVE_SCROLLBACK_TARGET_ROWS: usize = 1_000_000;
pub const NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE: usize = 320;
pub const NATIVE_MAX_SCROLLBACK: usize =
    NATIVE_SCROLLBACK_TARGET_ROWS * NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE;
pub const TERMINAL_TERM: &str = "xterm-bootty";
pub const TERMINAL_PROGRAM: &str = "ghostty";
pub const TERMINAL_PROGRAM_VERSION: &str = concat!("Bootty ", env!("CARGO_PKG_VERSION"));
const TERMINAL_XTVERSION: &str = concat!("ghostty (Bootty ", env!("CARGO_PKG_VERSION"), ")");
pub const TERMINAL_BACKGROUND: (u8, u8, u8) = (0x1a, 0x1b, 0x25);
pub const TERMINAL_FOREGROUND: (u8, u8, u8) = (0xc0, 0xca, 0xf5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalColorConfig {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub cursor: Option<RgbColor>,
    pub cursor_text: Option<RgbColor>,
    pub pointer_foreground: Option<RgbColor>,
    pub pointer_background: Option<RgbColor>,
    pub tektronix_foreground: Option<RgbColor>,
    pub tektronix_background: Option<RgbColor>,
    pub highlight_background: Option<RgbColor>,
    pub tektronix_cursor: Option<RgbColor>,
    pub highlight_foreground: Option<RgbColor>,
    pub selection_background: Option<RgbColor>,
    pub selection_foreground: Option<RgbColor>,
    pub palette: Vec<RgbColor>,
    pub palette_generate: bool,
    pub palette_harmonious: bool,
}

impl Default for TerminalColorConfig {
    fn default() -> Self {
        Self {
            background: rgb(TERMINAL_BACKGROUND),
            foreground: rgb(TERMINAL_FOREGROUND),
            cursor: None,
            cursor_text: None,
            pointer_foreground: None,
            pointer_background: None,
            tektronix_foreground: None,
            tektronix_background: None,
            highlight_background: None,
            tektronix_cursor: None,
            highlight_foreground: None,
            selection_background: None,
            selection_foreground: None,
            palette: default_palette16().into(),
            palette_generate: false,
            palette_harmonious: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalCursorConfig {
    pub style: Option<TerminalCursorStyle>,
    pub blink: Option<bool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCursorStyle {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

impl TerminalCursorStyle {
    fn into_ghostty(self) -> CursorStyle {
        match self {
            Self::Bar => CursorStyle::Bar,
            Self::Block => CursorStyle::Block,
            Self::Underline => CursorStyle::Underline,
            Self::HollowBlock => CursorStyle::BlockHollow,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalFeatureConfig {
    pub glyph_protocol: bool,
}

impl Default for TerminalFeatureConfig {
    fn default() -> Self {
        Self {
            glyph_protocol: true,
        }
    }
}

/// Live terminal settings that can change without restarting the terminal session.
///
/// Callers apply this value as one unit. The terminal engine applies colors first, then the
/// cursor, then terminal features.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalLiveConfig {
    pub colors: TerminalColorConfig,
    pub cursor: TerminalCursorConfig,
    pub features: TerminalFeatureConfig,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalSelectionEvent {
    pub surface: TerminalSurface,
    pub position: SurfacePoint,
    pub rectangle: bool,
}

impl TerminalSelectionEvent {
    fn grid_point(self) -> GridPoint {
        let x = ((self.position.x - self.surface.padding.left).max(0.0) / self.surface.cell.width)
            .floor();
        let y = ((self.position.y - self.surface.padding.top).max(0.0) / self.surface.cell.height)
            .floor();
        GridPoint {
            x: x as u16,
            y: y as u16,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSelectionFormat {
    PlainText,
    Vt,
    Html,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalSearchDirection {
    Previous,
    Current,
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalCopyModeAction {
    Cancel,
    CancelOrClearSelection,
    ClearSelection,
    BeginSelection,
    ToggleSelection,
    SelectLine,
    ToggleSelectionEnd,
    ToggleRectangle,
    CopySelectionAndCancel,
    CopyEndOfLineAndCancel,
    Search {
        query: String,
        direction: TerminalSearchDirection,
    },
    SearchWord(TerminalSearchDirection),
    Move(TerminalCopyModeMotion),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalCopyModeMotion {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    HalfPageUp,
    HalfPageDown,
    HistoryTop,
    HistoryBottom,
    StartOfLine,
    EndOfLine,
    BackToIndentation,
    TopLine,
    MiddleLine,
    BottomLine,
    NextWord,
    PreviousWord,
    NextWordEnd,
    ScrollUp,
    ScrollDown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalCopyModeOutcome {
    pub copied: Option<Vec<u8>>,
    pub search: Option<TerminalCopyModeSearchOutcome>,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCopyModeSearchOutcome {
    pub query: String,
    pub found: bool,
}

#[derive(Debug)]
struct CopyModeState {
    cursor: PointCoordinate,
    anchor: Option<PointCoordinate>,
    rectangle: bool,
    linewise: bool,
    desired_col: u16,
}

impl CopyModeState {
    fn selecting(&self) -> bool {
        self.anchor.is_some()
    }
}

impl TerminalSelectionFormat {
    fn emit_format(self) -> Format {
        match self {
            Self::PlainText => Format::Plain,
            Self::Vt => Format::Vt,
            Self::Html => Format::Html,
        }
    }
}

fn selection_geometry(surface: TerminalSurface) -> gesture::Geometry {
    let metrics = surface.mouse_metrics();
    let grid = surface.raw_grid_size();
    gesture::Geometry {
        columns: u32::from(grid.cols),
        cell_width: metrics.cell_width,
        padding_left: metrics.padding.left,
        screen_height: metrics.screen_height,
    }
}

fn selection_point(
    event: TerminalSelectionEvent,
    geometry: TerminalGeometry,
) -> Option<PointCoordinate> {
    let grid = event.grid_point();
    (grid.x < geometry.cols && grid.y < geometry.rows).then_some(PointCoordinate {
        x: grid.x,
        y: u32::from(grid.y),
    })
}

#[derive(Default)]
struct CachedRenderRow {
    cells: Vec<RenderCell>,
    text: Vec<char>,
    virtual_cells: Vec<KittyVirtualCell>,
    selection: Option<FrameSelection>,
    wrapped: bool,
    wrap_continuation: bool,
}

impl CachedRenderRow {
    fn clear(&mut self) {
        self.cells.clear();
        self.text.clear();
        self.virtual_cells.clear();
        self.selection = None;
        self.wrapped = false;
        self.wrap_continuation = false;
    }
}

#[derive(Clone, Copy)]
struct SizeReportState {
    geometry: TerminalGeometry,
    display_scale: f32,
    render_cell: CellMetrics,
}

impl SizeReportState {
    fn size(self) -> SizeReportSize {
        let cell_width = scaled_metric_size(self.render_cell.width, self.display_scale);
        let cell_height = scaled_metric_size(self.render_cell.height, self.display_scale);
        SizeReportSize {
            rows: self.geometry.rows,
            columns: self.geometry.cols,
            cell_width,
            cell_height,
        }
    }
}

fn scaled_metric_size(cell_size: f32, display_scale: f32) -> u32 {
    let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
        display_scale
    } else {
        1.0
    };
    (cell_size * display_scale).round().max(1.0) as u32
}

fn scaled_mouse_encoder_size(size: MouseEncoderSize, display_scale: f32) -> MouseEncoderSize {
    MouseEncoderSize {
        screen_width: scaled_mouse_extent(size.screen_width, display_scale).max(1),
        screen_height: scaled_mouse_extent(size.screen_height, display_scale).max(1),
        cell_width: scaled_mouse_extent(size.cell_width, display_scale).max(1),
        cell_height: scaled_mouse_extent(size.cell_height, display_scale).max(1),
        padding_top: scaled_mouse_extent(size.padding_top, display_scale),
        padding_bottom: scaled_mouse_extent(size.padding_bottom, display_scale),
        padding_right: scaled_mouse_extent(size.padding_right, display_scale),
        padding_left: scaled_mouse_extent(size.padding_left, display_scale),
    }
}

fn scaled_mouse_extent(value: u32, display_scale: f32) -> u32 {
    ((value as f32) * display_scale).round() as u32
}

fn scaled_mouse_position(value: f32, display_scale: f32) -> f32 {
    value * display_scale
}

fn extract_render_row(
    terminal: &Terminal<'static, 'static>,
    cell_iterator: &mut CellIterator<'static>,
    grapheme_scratch: &mut Vec<char>,
    hyperlink_scratch: &mut Vec<u8>,
    row: &RowIteration<'static, '_>,
    row_index: u16,
    out: &mut CachedRenderRow,
) -> Result<()> {
    out.clear();
    let raw_row = row.raw_row()?;
    out.wrapped = raw_row.is_wrapped().unwrap_or(false);
    out.wrap_continuation = raw_row.is_wrap_continuation().unwrap_or(false);
    let row_has_hyperlink = raw_row.has_hyperlink().unwrap_or(false);
    out.selection = row.selection()?.map(|selection| FrameSelection {
        row: row_index,
        start_col: selection.start_x,
        end_col: selection.end_x,
    });

    let mut cell_iter = cell_iterator.update(row)?;
    let mut col_index = 0_u16;
    while let Some(cell) = cell_iter.next() {
        // Every field read here is its own call across the FFI boundary, and a terminal is mostly
        // unstyled text. A cell that reports no styling has a default style and no foreground of
        // its own, so asking for either only repeats what this one answer already said. Its
        // background still has to be read: a blank painted with a background colour carries that
        // colour and nothing else.
        let style = cell.has_styling()?.then(|| cell.style()).transpose()?;
        let grapheme_len = cell.graphemes_len()?;
        grapheme_scratch.resize(grapheme_len, '\0');
        if grapheme_len > 0 {
            cell.graphemes_buf(grapheme_scratch)?;
        }

        let is_virtual_placeholder = grapheme_scratch.first() == Some(&'\u{10EEEE}');
        if is_virtual_placeholder {
            out.virtual_cells.push(KittyVirtualCell {
                x: col_index,
                y: row_index,
                grapheme: grapheme_scratch[..grapheme_len].to_vec(),
                foreground: style.map_or(StyleColor::None, |style| style.fg_color),
                underline_color: style.map_or(StyleColor::None, |style| style.underline_color),
            });
        }

        let text_start = out.text.len();
        let text_len = if is_virtual_placeholder {
            0
        } else {
            out.text
                .extend_from_slice(&grapheme_scratch[..grapheme_len]);
            grapheme_len
        };

        let hyperlink = if row_has_hyperlink {
            hyperlink_uri_at(terminal, col_index, row_index, hyperlink_scratch)
        } else {
            None
        };

        out.cells.push(RenderCell {
            x: col_index,
            y: row_index,
            text_start,
            text_len,
            fg: match style {
                Some(_) => cell.fg_color()?,
                None => None,
            },
            bg: cell.bg_color()?,
            style: style.map_or_else(CellStyle::default, |style| CellStyle {
                bold: style.bold,
                italic: style.italic,
                faint: style.faint,
                blink: style.blink,
                inverse: style.inverse,
                invisible: style.invisible,
                strikethrough: style.strikethrough,
                overline: style.overline,
                underline: style.underline,
            }),
            hyperlink,
        });

        col_index += 1;
    }

    Ok(())
}

type PtyWriteCallback =
    Rc<RefCell<Option<Box<dyn libghostty_vt::terminal::PtyWriteFn<'static, 'static>>>>>;

#[derive(Clone, Debug, Default)]
struct XtermColorOverrides {
    pointer_foreground: Option<RgbColor>,
    pointer_background: Option<RgbColor>,
    tektronix_foreground: Option<RgbColor>,
    tektronix_background: Option<RgbColor>,
    highlight_background: Option<RgbColor>,
    tektronix_cursor: Option<RgbColor>,
    highlight_foreground: Option<RgbColor>,
}

impl XtermColorOverrides {
    fn set(&mut self, code: u8, color: RgbColor) -> bool {
        let slot = match code {
            13 => &mut self.pointer_foreground,
            14 => &mut self.pointer_background,
            15 => &mut self.tektronix_foreground,
            16 => &mut self.tektronix_background,
            17 => &mut self.highlight_background,
            18 => &mut self.tektronix_cursor,
            19 => &mut self.highlight_foreground,
            _ => return false,
        };
        if *slot == Some(color) {
            false
        } else {
            *slot = Some(color);
            true
        }
    }

    fn reset(&mut self, code: u8) -> bool {
        let slot = match code {
            13 => &mut self.pointer_foreground,
            14 => &mut self.pointer_background,
            15 => &mut self.tektronix_foreground,
            16 => &mut self.tektronix_background,
            17 => &mut self.highlight_background,
            18 => &mut self.tektronix_cursor,
            19 => &mut self.highlight_foreground,
            _ => return false,
        };
        slot.take().is_some()
    }
}
pub struct TerminalEngine {
    terminal: Terminal<'static, 'static>,
    base_color_palette: crate::terminal_palette::Palette,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    image_placements: libghostty_vt::kitty::graphics::PlacementIterator<'static>,
    image_data_cache: KittyImageDataCache,
    frame: RenderFrame,
    row_cache: Vec<CachedRenderRow>,
    grapheme_scratch: Vec<char>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    macos_option_as_alt: MacosOptionAsAlt,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    selection_gesture: gesture::Gesture<'static>,
    selection_press_event: gesture::PressEvent<'static>,
    selection_drag_event: gesture::DragEvent<'static>,
    selection_release_event: gesture::ReleaseEvent<'static>,
    selection_clock_started: Instant,
    mouse_any_button_pressed: bool,
    mouse_encoder_options_dirty: bool,
    mouse_encoder_size: Option<MouseEncoderSize>,
    geometry: TerminalGeometry,
    size_report_state: Arc<Mutex<SizeReportState>>,
    current_working_directory_state: Arc<Mutex<String>>,
    side_effects: TerminalSideEffectCollector,
    terminal_write_pending: Vec<u8>,
    cursor_home_pending_len: usize,
    sgr_optimizer: SgrOptimizer,
    pty_write_callback: PtyWriteCallback,
    current_working_directory: String,
    colors: TerminalColorConfig,
    xterm_color_overrides: XtermColorOverrides,
    color_scheme: Arc<Mutex<ColorScheme>>,
    search_query: String,
    search_active_index: usize,
    search_pulse: u64,
    copy_mode: Option<CopyModeState>,
    content_epoch: u64,
    extracted_content_epoch: u64,
    kitty_graphics_touched: bool,
    display_scale: f32,
    render_cell: CellMetrics,
    render_cell_explicit: bool,
}

fn configure_default_colors(
    terminal: &mut Terminal<'static, 'static>,
    base_color_palette: &crate::terminal_palette::Palette,
    config: &TerminalColorConfig,
) -> Result<()> {
    let mut palette = *base_color_palette;
    let mut explicit = [false; 256];
    for (index, color) in config.palette.iter().take(256).copied().enumerate() {
        palette[index] = color;
        explicit[index] = true;
    }
    if config.palette_generate {
        palette = generate_256_palette(
            &palette,
            &explicit,
            config.background,
            config.foreground,
            config.palette_harmonious,
        );
    }
    terminal.set_default_color_palette(Some(palette))?;
    terminal
        .set_default_bg_color(Some(config.background))?
        .set_default_fg_color(Some(config.foreground))?
        .set_default_cursor_color(config.cursor)?;
    Ok(())
}

fn configure_default_cursor(
    terminal: &mut Terminal<'static, 'static>,
    config: TerminalCursorConfig,
) -> Result<()> {
    terminal
        .set_default_cursor_style(config.style.map(TerminalCursorStyle::into_ghostty))?
        .set_default_cursor_blink(config.blink)?;
    Ok(())
}

fn configure_terminal_features(
    terminal: &mut Terminal<'static, 'static>,
    config: TerminalFeatureConfig,
) -> Result<()> {
    terminal.set_glyph_protocol_enabled(config.glyph_protocol)?;
    Ok(())
}

struct BoottyGhosttyLogger;

impl libghostty_vt::log::Logger for BoottyGhosttyLogger {
    fn log(&self, level: libghostty_vt::log::Level, scope: &str, message: &str) {
        if !libghostty_log_enabled(level) {
            return;
        }
        let scope = if scope.is_empty() {
            "libghostty-vt"
        } else {
            scope
        };
        eprintln!("[libghostty-vt {level:?}] {scope}: {message}");
    }
}

fn install_libghostty_logger() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let _ = libghostty_vt::set_logger(Some(Box::new(BoottyGhosttyLogger)));
    });
}

fn libghostty_log_enabled(level: libghostty_vt::log::Level) -> bool {
    let minimum = std::env::var("BOOTTY_LIBGHOSTTY_LOG")
        .ok()
        .and_then(|value| parse_libghostty_log_level(&value))
        .unwrap_or(Some(libghostty_vt::log::Level::Warning));
    minimum.is_some_and(|minimum| log_level_rank(level) <= log_level_rank(minimum))
}

fn parse_libghostty_log_level(value: &str) -> Option<Option<libghostty_vt::log::Level>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "false" | "0" => Some(None),
        "error" => Some(Some(libghostty_vt::log::Level::Error)),
        "warn" | "warning" | "true" | "1" => Some(Some(libghostty_vt::log::Level::Warning)),
        "info" => Some(Some(libghostty_vt::log::Level::Info)),
        "debug" | "trace" | "all" => Some(Some(libghostty_vt::log::Level::Debug)),
        _ => None,
    }
}

fn log_level_rank(level: libghostty_vt::log::Level) -> u8 {
    match level {
        libghostty_vt::log::Level::Error => 0,
        libghostty_vt::log::Level::Warning => 1,
        libghostty_vt::log::Level::Info => 2,
        libghostty_vt::log::Level::Debug => 3,
        _ => 3,
    }
}

fn rgb((r, g, b): (u8, u8, u8)) -> RgbColor {
    RgbColor { r, g, b }
}
fn color_scheme_for_background(color: RgbColor) -> ColorScheme {
    let luma = u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114;
    if luma < 128_000 {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    }
}

fn default_palette16() -> [RgbColor; 16] {
    crate::terminal_palette::default_base16()
}

fn default_device_attributes() -> DeviceAttributes {
    DeviceAttributes {
        primary: PrimaryDeviceAttributes::new(
            ConformanceLevel::VT220,
            &[
                DeviceAttributeFeature::ANSI_COLOR,
                DeviceAttributeFeature::CLIPBOARD,
            ],
        ),
        secondary: SecondaryDeviceAttributes {
            device_type: DeviceType::VT220,
            firmware_version: 0,
            rom_cartridge: 0,
        },
        tertiary: TertiaryDeviceAttributes { unit_id: 0 },
    }
}

fn parse_palette_index(bytes: &[u8]) -> Option<u8> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.parse().ok()
}

fn parse_osc_number(bytes: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(bytes).ok()?;
    text.parse().ok()
}

fn parse_color_channel(s: &str) -> Option<u8> {
    let value = u16::from_str_radix(s, 16).ok()?;
    Some(match s.len() {
        1 => (value as u8) * 0x11,
        2 => value as u8,
        _ => (value >> 8) as u8,
    })
}

fn parse_rgb_color_spec(bytes: &[u8]) -> Option<RgbColor> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if let Some(rgb) = text.strip_prefix("rgb:") {
        let mut parts = rgb.split('/');
        let r = parse_color_channel(parts.next()?)?;
        let g = parse_color_channel(parts.next()?)?;
        let b = parse_color_channel(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        return Some(RgbColor { r, g, b });
    }
    let hex = text.strip_prefix('#')?;
    if hex.len() % 3 != 0 {
        return None;
    }
    let channel_len = hex.len() / 3;
    if !(1..=4).contains(&channel_len) {
        return None;
    }
    let channel =
        |index: usize| parse_color_channel(&hex[index * channel_len..(index + 1) * channel_len]);
    Some(RgbColor {
        r: channel(0)?,
        g: channel(1)?,
        b: channel(2)?,
    })
}

fn rgb_spec(color: RgbColor) -> String {
    format!(
        "rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}",
        color.r, color.r, color.g, color.g, color.b, color.b
    )
}

pub fn encode_iterm2_report_cell_size(cell_width: f32, cell_height: f32, scale: f32) -> Vec<u8> {
    format!(
        "\x1b]1337;ReportCellSize={:.4};{:.4};{:.4}\x1b\\",
        cell_height.max(1.0),
        cell_width.max(1.0),
        scale.max(1.0)
    )
    .into_bytes()
}

pub fn encode_iterm2_report_variable(value: &str) -> Vec<u8> {
    format!(
        "\x1b]1337;ReportVariable={}\x1b\\",
        general_purpose::STANDARD.encode(value.as_bytes())
    )
    .into_bytes()
}

pub fn encode_osc52_response(selection: &str, text: &str) -> Vec<u8> {
    format!(
        "\x1b]52;{};{}\x1b\\",
        selection,
        general_purpose::STANDARD.encode(text.as_bytes())
    )
    .into_bytes()
}

fn hyperlink_uri_at(
    terminal: &Terminal<'static, 'static>,
    x: u16,
    y: u16,
    scratch: &mut Vec<u8>,
) -> Option<String> {
    let grid_ref = terminal
        .grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }))
        .ok()?;
    scratch.resize(256, 0);
    loop {
        match grid_ref.hyperlink_uri(scratch) {
            Ok(0) => return None,
            Ok(len) => return String::from_utf8(scratch[..len].to_vec()).ok(),
            Err(libghostty_vt::Error::OutOfSpace { required }) => scratch.resize(required, 0),
            Err(_) => return None,
        }
    }
}

fn placement_rows_overlap_content(
    placement: &KittyImagePlacement,
    surface: TerminalSurface,
    rows: &[CachedRenderRow],
) -> bool {
    let origin = surface.content_origin();
    let min_y = placement.destination.min_y - origin.y;
    let max_y = placement.destination.max_y - origin.y;
    let min_x = placement.destination.min_x - origin.x;
    let max_x = placement.destination.max_x - origin.x;
    if max_y <= 0.0
        || min_y >= rows.len() as f32 * surface.cell.height
        || max_x <= 0.0
        || surface.cell.width <= 0.0
        || surface.cell.height <= 0.0
    {
        return false;
    }

    let start = (min_y.max(0.0) / surface.cell.height).floor() as usize;
    let end = (max_y.max(0.0) / surface.cell.height).ceil() as usize;
    let end = end.saturating_sub(1).min(rows.len().saturating_sub(1));
    let start_col = (min_x.max(0.0) / surface.cell.width).floor() as u16;
    let end_col = (max_x.max(0.0) / surface.cell.width).ceil().max(1.0) as u16;
    let end_col = end_col.saturating_sub(1);
    (start..=end).any(|index| {
        rows.get(index).is_some_and(|row| {
            row.cells.iter().any(|cell| {
                cell.x >= start_col
                    && cell.x <= end_col
                    && row
                        .text
                        .get(cell.text_start..cell.text_start + cell.text_len)
                        .is_some_and(|text| text.iter().any(|ch| !ch.is_whitespace()))
            })
        })
    })
}

impl TerminalEngine {
    pub fn new(geometry: TerminalGeometry) -> Result<Self> {
        Self::new_with_scrollback(
            geometry,
            TerminalColorConfig::default(),
            DEFAULT_MAX_SCROLLBACK,
        )
    }

    pub fn new_with_terminal_options(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        cursor: TerminalCursorConfig,
        features: TerminalFeatureConfig,
        max_scrollback: usize,
        macos_option_as_alt: MacosOptionAsAlt,
    ) -> Result<Self> {
        Self::new_inner(
            geometry,
            colors,
            cursor,
            features,
            max_scrollback,
            macos_option_as_alt,
        )
    }

    pub fn new_with_scrollback(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        max_scrollback: usize,
    ) -> Result<Self> {
        Self::new_inner(
            geometry,
            colors,
            TerminalCursorConfig::default(),
            TerminalFeatureConfig::default(),
            max_scrollback,
            MacosOptionAsAlt::default(),
        )
    }

    fn new_inner(
        geometry: TerminalGeometry,
        colors: TerminalColorConfig,
        cursor: TerminalCursorConfig,
        features: TerminalFeatureConfig,
        max_scrollback: usize,
        macos_option_as_alt: MacosOptionAsAlt,
    ) -> Result<Self> {
        install_libghostty_logger();
        let mut terminal = Terminal::new(TerminalOptions {
            cols: geometry.cols,
            rows: geometry.rows,
            max_scrollback,
        })?;
        let base_color_palette = terminal.default_color_palette()?;
        configure_default_colors(&mut terminal, &base_color_palette, &colors)?;
        configure_default_cursor(&mut terminal, cursor)?;
        configure_terminal_features(&mut terminal, features)?;
        // Measure grapheme clusters by their display width (DEC mode 2027), matching Ghostty's
        // default. Without it the grid uses legacy per-codepoint wcwidth, so a VS16 emoji
        // presentation sequence (⚠️ ❤️) lands in a single cell — too narrow to render the color
        // glyph and inconsistent with how bootty measures the run.
        terminal.set_mode(Mode::GRAPHEME_CLUSTER, true)?;
        let size_report_state = Arc::new(Mutex::new(SizeReportState {
            geometry,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
        }));
        let report_state = size_report_state.clone();
        terminal.on_size(move |_terminal| {
            let state = *report_state.lock().ok()?;
            Some(state.size())
        })?;
        terminal.on_device_attributes(|_terminal| Some(default_device_attributes()))?;
        let color_scheme = Arc::new(Mutex::new(color_scheme_for_background(colors.background)));
        let report_color_scheme = color_scheme.clone();
        terminal.on_color_scheme(move |_terminal| report_color_scheme.lock().ok().map(|s| *s))?;
        terminal.on_xtversion(|_terminal| Some(TERMINAL_XTVERSION))?;
        let side_effects = TerminalSideEffectCollector::new();
        let callback_side_effects = side_effects.callback_effects();
        let title_side_effects = callback_side_effects.clone();
        terminal.on_title_changed(move |terminal| {
            if let Ok(title) = terminal.title()
                && let Ok(mut effects) = title_side_effects.lock()
            {
                effects.push(TerminalSideEffect::WindowTitle(title.to_owned()));
            }
        })?;
        let pwd_state = Arc::new(Mutex::new(String::new()));
        let callback_pwd_state = pwd_state.clone();
        terminal.on_pwd_changed(move |terminal| {
            if let Ok(pwd) = terminal.pwd()
                && let Ok(mut current) = callback_pwd_state.lock()
            {
                current.clear();
                current.push_str(pwd);
            }
        })?;
        let bell_side_effects = callback_side_effects.clone();
        terminal.on_bell(move |_terminal| {
            if let Ok(mut effects) = bell_side_effects.lock() {
                effects.push(TerminalSideEffect::Bell);
            }
        })?;
        let pty_write_callback: PtyWriteCallback = Rc::new(RefCell::new(None));
        let terminal_pty_write_callback = pty_write_callback.clone();
        terminal.on_pty_write(move |terminal, bytes| {
            if let Some(callback) = terminal_pty_write_callback.borrow_mut().as_deref_mut() {
                callback(terminal, bytes);
            }
        })?;
        terminal.resize(
            geometry.cols,
            geometry.rows,
            geometry.cell_width,
            geometry.cell_height,
        )?;
        terminal.set_kitty_image_from_file_allowed(true)?;
        terminal.set_kitty_image_from_temp_file_allowed(true)?;
        terminal.set_kitty_image_from_shared_mem_allowed(true)?;
        set_png_decoder(Some(Box::new(BoottyPngDecoder)))?;

        let selection_gesture = gesture::Gesture::new()?;
        let selection_press_event = gesture::PressEvent::new()?;
        let selection_drag_event = gesture::DragEvent::new()?;
        let selection_release_event = gesture::ReleaseEvent::new()?;
        let mut engine = Self {
            terminal,
            base_color_palette,
            render_state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            image_placements: libghostty_vt::kitty::graphics::PlacementIterator::new()?,
            image_data_cache: KittyImageDataCache::default(),
            frame: RenderFrame::default(),
            row_cache: Vec::new(),
            grapheme_scratch: Vec::new(),
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            macos_option_as_alt,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            mouse_any_button_pressed: false,
            selection_gesture,
            selection_press_event,
            selection_drag_event,
            selection_release_event,
            selection_clock_started: Instant::now(),
            mouse_encoder_options_dirty: true,
            mouse_encoder_size: None,
            geometry,
            size_report_state,
            side_effects,
            terminal_write_pending: Vec::new(),
            cursor_home_pending_len: 0,
            sgr_optimizer: SgrOptimizer::default(),
            pty_write_callback,
            current_working_directory: String::new(),
            current_working_directory_state: pwd_state,
            color_scheme,
            colors,
            xterm_color_overrides: XtermColorOverrides::default(),
            search_query: String::new(),
            search_active_index: 0,
            search_pulse: 0,
            copy_mode: None,
            content_epoch: 0,
            extracted_content_epoch: u64::MAX,
            kitty_graphics_touched: false,
            display_scale: 1.0,
            render_cell: CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32),
            render_cell_explicit: false,
        };
        engine.set_kitty_image_storage_limit(64 * 1024 * 1024)?;
        Ok(engine)
    }

    fn mark_content_changed(&mut self) {
        self.content_epoch = self.content_epoch.wrapping_add(1);
    }

    pub fn begin_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        let Some(point) = selection_point(event, self.geometry) else {
            self.clear_selection()?;
            return Ok(());
        };

        {
            let terminal = &self.terminal;
            let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
            self.selection_press_event
                .set_position(f64::from(event.position.x), f64::from(event.position.y))?
                .set_repeat_distance(4.0)?
                .set_repeat_interval(SELECTION_REPEAT_INTERVAL)?
                .set_time(self.selection_clock_started.elapsed())?;
            let selection = self.selection_press_event.apply(
                &mut self.selection_gesture,
                terminal,
                grid_ref,
            )?;
            terminal.set_selection(selection.as_ref())?;
        }
        self.mark_content_changed();
        Ok(())
    }

    pub fn update_selection(&mut self, event: TerminalSelectionEvent) -> Result<()> {
        let Some(point) = selection_point(event, self.geometry) else {
            return Ok(());
        };

        {
            let terminal = &self.terminal;
            let grid_ref = terminal.grid_ref(Point::Viewport(point))?;
            self.selection_drag_event
                .set_position(f64::from(event.position.x), f64::from(event.position.y))?
                .set_rectangle(event.rectangle)?;
            let selection = self.selection_drag_event.apply(
                &mut self.selection_gesture,
                terminal,
                grid_ref,
                selection_geometry(event.surface),
            )?;
            terminal.set_selection(selection.as_ref())?;
        }
        self.mark_content_changed();
        Ok(())
    }

    pub fn end_selection(&mut self, event: Option<TerminalSelectionEvent>) -> Result<()> {
        let was_dragged = self
            .selection_gesture
            .dragged(&self.terminal)
            .unwrap_or(false);
        let behavior = self
            .selection_gesture
            .behavior(&self.terminal)
            .unwrap_or(gesture::Behavior::Cell);
        let point = event.and_then(|event| selection_point(event, self.geometry));

        {
            let terminal = &self.terminal;
            let grid_ref = point
                .map(|point| terminal.grid_ref(Point::Viewport(point)))
                .transpose()?;
            self.selection_release_event
                .apply(&mut self.selection_gesture, terminal, grid_ref)?;
        }

        if !was_dragged && behavior == gesture::Behavior::Cell {
            self.terminal.set_selection(None)?;
        }
        self.mark_content_changed();
        Ok(())
    }

    fn clear_selection(&mut self) -> Result<()> {
        self.terminal.set_selection(None)?;
        self.selection_gesture.reset(&self.terminal);
        self.mark_content_changed();
        Ok(())
    }

    pub fn format_selection(&self, format: TerminalSelectionFormat) -> Result<Option<Vec<u8>>> {
        let options = SelectionFormatOptions::new()
            .with_emit_format(format.emit_format())
            .with_unwrap(true)
            .with_trim(true);
        Ok(self
            .terminal
            .format_selection_alloc(None, options)?
            .map(|bytes| bytes.as_ref().to_vec()))
    }

    fn set_cursor_config(&mut self, cursor: TerminalCursorConfig) -> Result<()> {
        configure_default_cursor(&mut self.terminal, cursor)?;
        self.mark_content_changed();
        Ok(())
    }

    fn set_feature_config(&mut self, features: TerminalFeatureConfig) -> Result<()> {
        configure_terminal_features(&mut self.terminal, features)?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn apply_live_config(&mut self, config: TerminalLiveConfig) -> Result<()> {
        self.set_colors(config.colors)?;
        self.set_cursor_config(config.cursor)?;
        self.set_feature_config(config.features)
    }

    fn set_kitty_image_storage_limit(&mut self, limit: u64) -> Result<()> {
        self.terminal.set_kitty_image_storage_limit(limit)?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn on_pty_write(
        &mut self,
        f: impl libghostty_vt::terminal::PtyWriteFn<'static, 'static>,
    ) -> Result<()> {
        *self.pty_write_callback.borrow_mut() = Some(Box::new(f));
        Ok(())
    }

    fn write_pty_response(&self, bytes: &[u8]) {
        if let Some(callback) = self.pty_write_callback.borrow_mut().as_deref_mut() {
            callback(&self.terminal, bytes);
        }
    }

    fn write_vt_with_ordered_osc_color_responses(&mut self, data: &[u8]) {
        let mut search_start = 0;
        while let Some(relative_start) = find_subslice(&data[search_start..], b"\x1b]") {
            let start = search_start + relative_start;
            let payload_start = start + 2;
            let (payload_len, terminator_len) = match find_osc_terminator(&data[payload_start..]) {
                Some(found) => found,
                None => {
                    self.terminal.vt_write(&data[search_start..]);
                    return;
                }
            };
            let payload_end = payload_start + payload_len;
            let terminator_end = payload_end + terminator_len;

            self.terminal.vt_write(&data[search_start..terminator_end]);
            self.apply_osc_color_state(&data[payload_start..payload_end]);
            if let Some(response) = self.osc_color_query_response(
                &data[payload_start..payload_end],
                &data[payload_end..terminator_end],
            ) {
                self.write_pty_response(&response);
            }
            search_start = terminator_end;
        }
        if search_start < data.len() {
            self.terminal.vt_write(&data[search_start..]);
        }
    }

    fn apply_osc_color_state(&mut self, payload: &[u8]) {
        let Some((command, rest)) = split_osc_payload(payload) else {
            if let Some(reset_code) = parse_osc_number(payload)
                && (113..=119).contains(&reset_code)
                && self.xterm_color_overrides.reset((reset_code - 100) as u8)
            {
                self.mark_content_changed();
            }
            return;
        };
        let Some(start_code) = parse_osc_number(command) else {
            return;
        };
        if !(13..=19).contains(&start_code) {
            return;
        }
        let mut changed = false;
        for (offset, spec) in rest.split(|byte| *byte == b';').enumerate() {
            let code = start_code + offset as u16;
            if code > 19 || spec == b"?" {
                break;
            }
            if let Some(color) = parse_rgb_color_spec(spec) {
                changed |= self.xterm_color_overrides.set(code as u8, color);
            }
        }
        if changed {
            self.mark_content_changed();
        }
    }

    fn xterm_dynamic_color(&self, code: u8) -> Option<RgbColor> {
        match code {
            10 => self.terminal.fg_color().ok().flatten(),
            11 => self.terminal.bg_color().ok().flatten(),
            12 => self
                .terminal
                .cursor_color()
                .ok()
                .flatten()
                .or(self.colors.cursor)
                .or(Some(self.colors.foreground)),
            13 => self
                .xterm_color_overrides
                .pointer_foreground
                .or(self.colors.pointer_foreground)
                .or(Some(self.colors.foreground)),
            14 => self
                .xterm_color_overrides
                .pointer_background
                .or(self.colors.pointer_background)
                .or(Some(self.colors.background)),
            15 => self
                .xterm_color_overrides
                .tektronix_foreground
                .or(self.colors.tektronix_foreground)
                .or(Some(self.colors.foreground)),
            16 => self
                .xterm_color_overrides
                .tektronix_background
                .or(self.colors.tektronix_background)
                .or(Some(self.colors.background)),
            17 => self
                .xterm_color_overrides
                .highlight_background
                .or(self.colors.highlight_background)
                .or(self.colors.selection_background)
                .or(Some(self.colors.foreground)),
            18 => self
                .xterm_color_overrides
                .tektronix_cursor
                .or(self.colors.tektronix_cursor)
                .or(self.colors.cursor)
                .or(Some(self.colors.foreground)),
            19 => self
                .xterm_color_overrides
                .highlight_foreground
                .or(self.colors.highlight_foreground)
                .or(self.colors.selection_foreground)
                .or(Some(self.colors.background)),
            _ => None,
        }
    }

    fn osc_color_query_response(&self, payload: &[u8], terminator: &[u8]) -> Option<Vec<u8>> {
        let (command, rest) = split_osc_payload(payload)?;
        let mut response = Vec::new();
        match command {
            b"4" => {
                let palette = self.terminal.color_palette().ok()?;
                let mut parts = rest.split(|byte| *byte == b';');
                while let Some(index_bytes) = parts.next() {
                    let operation = parts.next()?;
                    if operation != b"?" {
                        return None;
                    }
                    let index = parse_palette_index(index_bytes)?;
                    write!(
                        response,
                        "\x1b]4;{};{}",
                        index,
                        rgb_spec(palette[usize::from(index)])
                    )
                    .ok()?;
                    response.extend_from_slice(terminator);
                }
            }
            b"10" | b"11" | b"12" | b"13" | b"14" | b"15" | b"16" | b"17" | b"18" | b"19" => {
                let start_code = parse_osc_number(command)? as u8;
                for (code, operation) in (start_code..=19).zip(rest.split(|byte| *byte == b';')) {
                    if operation != b"?" {
                        return None;
                    }
                    let color = self.xterm_dynamic_color(code)?;
                    write!(response, "\x1b]{};{}", code, rgb_spec(color)).ok()?;
                    response.extend_from_slice(terminator);
                }
            }
            _ => return None,
        }
        (!response.is_empty()).then_some(response)
    }

    pub fn grid_size(&self) -> (u16, u16) {
        (self.geometry.cols, self.geometry.rows)
    }

    pub fn geometry(&self) -> TerminalGeometry {
        self.geometry
    }

    fn set_colors(&mut self, colors: TerminalColorConfig) -> Result<()> {
        configure_default_colors(&mut self.terminal, &self.base_color_palette, &colors)?;
        *self.color_scheme.lock().expect("color scheme lock") =
            color_scheme_for_background(colors.background);
        self.colors = colors;
        self.mark_content_changed();
        Ok(())
    }

    pub fn resize(&mut self, geometry: TerminalGeometry) -> Result<()> {
        if geometry == self.geometry {
            return Ok(());
        }

        let previous = self.geometry;
        self.geometry = geometry;
        if let Ok(mut report_state) = self.size_report_state.lock() {
            report_state.geometry = geometry;
        }
        if !self.render_cell_explicit {
            self.render_cell =
                CellMetrics::new(geometry.cell_width as f32, geometry.cell_height as f32);
            if let Ok(mut report_state) = self.size_report_state.lock() {
                report_state.render_cell = self.render_cell;
            }
        }

        if geometry.cols != previous.cols && geometry.rows < previous.rows {
            self.terminal.resize(
                geometry.cols,
                previous.rows,
                geometry.cell_width,
                geometry.cell_height,
            )?;
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

    pub fn set_display_scale(&mut self, display_scale: f32) {
        let display_scale = if display_scale.is_finite() && display_scale > 0.0 {
            display_scale
        } else {
            1.0
        };
        if (self.display_scale - display_scale).abs() > f32::EPSILON {
            self.display_scale = display_scale;
            if let Ok(mut report_state) = self.size_report_state.lock() {
                report_state.display_scale = display_scale;
            }
            if self.kitty_graphics_touched {
                self.mark_content_changed();
            }
        }
    }

    pub fn set_render_cell_metrics(&mut self, cell: CellMetrics) {
        let width = if cell.width.is_finite() && cell.width > 0.0 {
            cell.width
        } else {
            self.geometry.cell_width as f32
        };
        let height = if cell.height.is_finite() && cell.height > 0.0 {
            cell.height
        } else {
            self.geometry.cell_height as f32
        };
        let cell = CellMetrics::new(width, height);
        if self.render_cell != cell {
            self.render_cell = cell;
            if let Ok(mut report_state) = self.size_report_state.lock() {
                report_state.render_cell = cell;
            }
            if self.kitty_graphics_touched {
                self.mark_content_changed();
            }
        }
        self.render_cell_explicit = true;
    }

    fn complete_streaming_terminal_write<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.terminal_write_pending.is_empty() {
            let complete_len = complete_streaming_control_prefix_len(bytes);
            if complete_len == bytes.len() {
                return Cow::Borrowed(bytes);
            }
            self.terminal_write_pending
                .extend_from_slice(&bytes[complete_len..]);
            return Cow::Borrowed(&bytes[..complete_len]);
        }

        let mut joined = Vec::with_capacity(self.terminal_write_pending.len() + bytes.len());
        joined.extend_from_slice(&self.terminal_write_pending);
        joined.extend_from_slice(bytes);
        self.terminal_write_pending.clear();

        let complete_len = complete_streaming_control_prefix_len(&joined);
        if complete_len < joined.len() {
            self.terminal_write_pending
                .extend_from_slice(&joined[complete_len..]);
            joined.truncate(complete_len);
        }
        Cow::Owned(joined)
    }

    fn try_write_repeated_cursor_home(&mut self, bytes: &[u8]) -> bool {
        let Some((complete, pending_len)) =
            repeated_cursor_home_prefix_len(bytes, self.cursor_home_pending_len)
        else {
            if self.cursor_home_pending_len > 0 {
                self.terminal
                    .vt_write(&CURSOR_HOME[..self.cursor_home_pending_len]);
                self.cursor_home_pending_len = 0;
            }
            return false;
        };

        if complete > 0 {
            self.terminal.vt_write(CURSOR_HOME);
        }
        self.cursor_home_pending_len = pending_len;
        true
    }

    pub fn write_vt(&mut self, bytes: &[u8]) {
        let can_fast_write = self.terminal_write_pending.is_empty()
            && !self.side_effects.has_pending_osc()
            && !contains_tracked_streaming_control(bytes);

        if can_fast_write && self.try_write_repeated_cursor_home(bytes) {
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        if self.cursor_home_pending_len > 0 {
            self.terminal
                .vt_write(&CURSOR_HOME[..self.cursor_home_pending_len]);
            self.cursor_home_pending_len = 0;
        }

        if can_fast_write {
            let optimized = self.sgr_optimizer.optimize(bytes);
            self.terminal.vt_write(optimized);
            self.sync_current_working_directory();
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        let write_bytes = self.complete_streaming_terminal_write(bytes);
        if write_bytes.is_empty() {
            return;
        }

        let mut features = terminal_write_features(write_bytes.as_ref());
        if self.side_effects.has_pending_osc() {
            features.osc_side_effect = true;
        }
        if !features.needs_sanitizing() {
            self.sgr_optimizer.reset();
            self.terminal.vt_write(write_bytes.as_ref());
            self.sync_current_working_directory();
            self.mouse_encoder_options_dirty = true;
            self.mark_content_changed();
            return;
        }

        let bytes = if features.tmux_passthrough {
            let unwrapped = unwrap_tmux_passthrough_commands(write_bytes.as_ref());
            features = terminal_write_features(unwrapped.as_ref());
            if self.side_effects.has_pending_osc() {
                features.osc_side_effect = true;
            }
            unwrapped
        } else {
            write_bytes
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
        self.sgr_optimizer.reset();
        if features.osc_color {
            self.write_vt_with_ordered_osc_color_responses(sanitized.bytes.as_ref());
        } else {
            self.terminal.vt_write(sanitized.bytes.as_ref());
        }
        self.sync_current_working_directory();
        if features.osc_side_effect {
            for action in self.side_effects.collect(sanitized.bytes.as_ref()) {
                match action {
                    TerminalHostAction::WriteVt(bytes) => {
                        self.terminal.vt_write(bytes);
                        self.mark_content_changed();
                    }
                }
            }
        }
        self.mouse_encoder_options_dirty = true;
        self.mark_content_changed();
    }

    fn sync_current_working_directory(&mut self) {
        if let Ok(current) = self.current_working_directory_state.lock()
            && self.current_working_directory.as_str() != current.as_str()
        {
            self.current_working_directory.clear();
            self.current_working_directory.push_str(&current);
        }
    }

    pub fn current_working_directory(&self) -> &str {
        &self.current_working_directory
    }

    pub fn drain_side_effects(&mut self) -> Vec<TerminalSideEffect> {
        self.side_effects.drain()
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
            .set_macos_option_as_alt(self.macos_option_as_alt.into());
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
        let display_scale = self.display_scale;
        let encoder_size = scaled_mouse_encoder_size(input.size, display_scale);
        if self.mouse_encoder_size != Some(encoder_size) {
            self.mouse_encoder.set_size(encoder_size.into());
            self.mouse_encoder_size = Some(encoder_size);
        }
        self.mouse_encoder
            .set_any_button_pressed(self.mouse_any_button_pressed);
        self.mouse_event
            .set_action(input.action.into())
            .set_button(input.button.map(Into::into))
            .set_mods(input.mods.into())
            .set_position(mouse::Position {
                x: scaled_mouse_position(input.x, display_scale),
                y: scaled_mouse_position(input.y, display_scale),
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

    /// Encode a wheel scroll of `notches` rows for a mouse-tracking application, which expects one
    /// button report per row. A wheel event carries however many rows the scroll accumulator
    /// resolved, so reporting it once makes fast scrolling crawl a line at a time.
    pub fn encode_mouse_wheel_to_vec(
        &mut self,
        input: MouseInput,
        notches: usize,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        self.encode_mouse_to_vec(input, out)?;
        let notch = out.len();
        if notch == 0 {
            return Ok(());
        }
        // The row count reaches us from a saturating f32 cast of an OS scroll delta, so a
        // non-finite delta arrives as `isize::MAX`. Nothing observed produces one, but the cost of
        // being wrong is an unbounded write to the PTY, and no real scroll exceeds a screenful.
        let notches = notches.min(MAX_WHEEL_REPORTS);
        out.reserve(notch * notches.saturating_sub(1));
        for _ in 1..notches {
            out.extend_from_within(..notch);
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

    pub fn search_viewport(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        let query = query.trim();
        if query.is_empty() {
            self.search_query.clear();
            self.search_active_index = 0;
            self.mark_content_changed();
            let _ = self.extract_frame()?;
            return Ok(false);
        }

        if self.search_query != query {
            self.search_query = query.to_owned();
            self.search_active_index = 0;
            self.mark_content_changed();
        }

        let initial_offset = self.current_scroll_offset()?;
        let visible_count = frame_search_matches(self.extract_frame()?, query).len();
        if visible_count > 0 {
            match direction {
                TerminalSearchDirection::Current => return Ok(true),
                TerminalSearchDirection::Next if self.search_active_index + 1 < visible_count => {
                    self.search_active_index += 1;
                    self.bump_search_pulse();
                    let _ = self.extract_frame()?;
                    return Ok(true);
                }
                TerminalSearchDirection::Previous if self.search_active_index > 0 => {
                    self.search_active_index -= 1;
                    self.bump_search_pulse();
                    let _ = self.extract_frame()?;
                    return Ok(true);
                }
                TerminalSearchDirection::Next | TerminalSearchDirection::Previous => {}
            }
        }

        let delta = match direction {
            TerminalSearchDirection::Current | TerminalSearchDirection::Previous => {
                -(self.geometry.rows.max(1) as isize)
            }
            TerminalSearchDirection::Next => self.geometry.rows.max(1) as isize,
        };

        loop {
            let before = self.current_scroll_offset()?;
            self.scroll_viewport_delta(delta);
            let frame = self.extract_frame()?;
            let after = frame.scrollbar.map_or(before, |scrollbar| scrollbar.offset);
            if after == before {
                break;
            }
            let found_count = frame_search_matches(frame, query).len();
            if found_count > 0 {
                self.search_active_index = match direction {
                    TerminalSearchDirection::Previous => found_count - 1,
                    TerminalSearchDirection::Current | TerminalSearchDirection::Next => 0,
                };
                if direction != TerminalSearchDirection::Current {
                    self.bump_search_pulse();
                }
                let _ = self.extract_frame()?;
                return Ok(true);
            }
        }

        let current_offset = self.current_scroll_offset()?;
        let restore_delta = initial_offset as i128 - current_offset as i128;
        if restore_delta != 0 {
            self.scroll_viewport_delta(
                restore_delta.clamp(isize::MIN as i128, isize::MAX as i128) as isize
            );
            let _ = self.extract_frame()?;
        }
        let found = frame_search_matches(self.extract_frame()?, query).len();
        if found > 0 {
            self.search_active_index = self.search_active_index.min(found - 1);
            if direction != TerminalSearchDirection::Current {
                self.bump_search_pulse();
                let _ = self.extract_frame()?;
            }
            return Ok(true);
        }
        Ok(false)
    }

    fn bump_search_pulse(&mut self) {
        self.search_pulse = self.search_pulse.wrapping_add(1);
        self.mark_content_changed();
    }

    fn current_scroll_offset(&mut self) -> Result<u64> {
        Ok(self
            .extract_frame()?
            .scrollbar
            .map_or(0, |scrollbar| scrollbar.offset))
    }

    pub fn enter_copy_mode(&mut self) -> Result<()> {
        let cursor = self.copy_mode_entry_point()?;
        self.copy_mode = Some(CopyModeState {
            cursor,
            anchor: None,
            rectangle: false,
            linewise: false,
            desired_col: cursor.x,
        });
        self.terminal.set_selection(None)?;
        self.ensure_copy_mode_cursor_visible()?;
        self.mark_content_changed();
        Ok(())
    }

    pub fn copy_mode_active(&self) -> bool {
        self.copy_mode.is_some()
    }

    pub fn handle_copy_mode_action(
        &mut self,
        action: TerminalCopyModeAction,
    ) -> Result<TerminalCopyModeOutcome> {
        if self.copy_mode.is_none() {
            return Ok(TerminalCopyModeOutcome::default());
        }

        match action {
            TerminalCopyModeAction::Cancel => self.cancel_copy_mode()?,
            TerminalCopyModeAction::CancelOrClearSelection => {
                if self
                    .copy_mode
                    .as_ref()
                    .is_some_and(CopyModeState::selecting)
                {
                    self.clear_copy_mode_selection()?;
                } else {
                    self.cancel_copy_mode()?;
                }
            }
            TerminalCopyModeAction::ClearSelection => self.clear_copy_mode_selection()?,
            TerminalCopyModeAction::BeginSelection => self.begin_copy_mode_selection(false)?,
            TerminalCopyModeAction::ToggleSelection => self.toggle_copy_mode_selection()?,
            TerminalCopyModeAction::SelectLine => self.select_copy_mode_line()?,
            TerminalCopyModeAction::ToggleSelectionEnd => self.toggle_copy_mode_selection_end()?,
            TerminalCopyModeAction::ToggleRectangle => self.toggle_copy_mode_rectangle()?,
            TerminalCopyModeAction::Search { query, direction } => {
                let found = self.search_copy_mode_query(&query, direction)?;
                return Ok(TerminalCopyModeOutcome {
                    copied: None,
                    search: Some(TerminalCopyModeSearchOutcome { query, found }),
                    active: self.copy_mode.is_some(),
                });
            }
            TerminalCopyModeAction::SearchWord(direction) => {
                let Some(query) = self.copy_mode_word_under_cursor()? else {
                    return Ok(TerminalCopyModeOutcome {
                        copied: None,
                        search: None,
                        active: self.copy_mode.is_some(),
                    });
                };
                let found = self.search_copy_mode_query(&query, direction)?;
                return Ok(TerminalCopyModeOutcome {
                    copied: None,
                    search: Some(TerminalCopyModeSearchOutcome { query, found }),
                    active: self.copy_mode.is_some(),
                });
            }
            TerminalCopyModeAction::CopySelectionAndCancel => {
                let copied = self.copy_mode_selection_and_cancel()?;
                return Ok(TerminalCopyModeOutcome {
                    copied,
                    search: None,
                    active: false,
                });
            }
            TerminalCopyModeAction::CopyEndOfLineAndCancel => {
                if !self
                    .copy_mode
                    .as_ref()
                    .is_some_and(CopyModeState::selecting)
                {
                    self.begin_copy_mode_selection(false)?;
                }
                self.move_copy_mode_cursor(TerminalCopyModeMotion::EndOfLine)?;
                let copied = self.copy_mode_selection_and_cancel()?;
                return Ok(TerminalCopyModeOutcome {
                    copied,
                    search: None,
                    active: false,
                });
            }
            TerminalCopyModeAction::Move(motion) => self.move_copy_mode_cursor(motion)?,
        }

        Ok(TerminalCopyModeOutcome {
            copied: None,
            search: None,
            active: self.copy_mode.is_some(),
        })
    }

    fn copy_mode_entry_point(&mut self) -> Result<PointCoordinate> {
        let viewport_top = self.viewport_top_screen_row()?;
        let max_y = (self.terminal.total_rows()?.max(1) as u32).saturating_sub(1);
        let max_x = self.geometry.cols.saturating_sub(1);
        let last_row = u32::from(self.geometry.rows.max(1).saturating_sub(1));
        let cursor = self.extract_frame()?.cursor;
        if let Some(cursor) = cursor {
            return Ok(PointCoordinate {
                x: cursor.x.min(max_x),
                y: viewport_top
                    .saturating_add(u32::from(
                        cursor.y.min(self.geometry.rows.saturating_sub(1)),
                    ))
                    .min(max_y),
            });
        }

        Ok(PointCoordinate {
            x: 0,
            y: viewport_top.saturating_add(last_row).min(max_y),
        })
    }

    fn copy_mode_screen_point(&self) -> Option<PointCoordinate> {
        self.copy_mode.as_ref().map(|state| state.cursor)
    }

    fn begin_copy_mode_selection(&mut self, rectangle: bool) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        if let Some(state) = &mut self.copy_mode {
            state.anchor = Some(point);
            state.rectangle = rectangle;
            state.linewise = false;
        }
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_selection(&mut self) -> Result<()> {
        if self
            .copy_mode
            .as_ref()
            .is_some_and(CopyModeState::selecting)
        {
            self.clear_copy_mode_selection()
        } else {
            self.begin_copy_mode_selection(false)
        }
    }

    fn clear_copy_mode_selection(&mut self) -> Result<()> {
        if let Some(state) = &mut self.copy_mode {
            state.anchor = None;
            state.rectangle = false;
            state.linewise = false;
        }
        self.terminal.set_selection(None)?;
        self.mark_content_changed();
        Ok(())
    }

    fn cancel_copy_mode(&mut self) -> Result<()> {
        self.copy_mode = None;
        self.terminal.set_selection(None)?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_rectangle(&mut self) -> Result<()> {
        if !self
            .copy_mode
            .as_ref()
            .is_some_and(CopyModeState::selecting)
        {
            self.begin_copy_mode_selection(true)?;
            return Ok(());
        }
        if let Some(state) = &mut self.copy_mode {
            state.rectangle = !state.rectangle;
            state.linewise = false;
        }
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn select_copy_mode_line(&mut self) -> Result<()> {
        let Some(mut point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let start = PointCoordinate { x: 0, y: point.y };
        point.x = self.screen_row_end_col(point.y)?;
        if let Some(state) = &mut self.copy_mode {
            state.anchor = Some(start);
            state.rectangle = false;
            state.linewise = true;
            state.cursor = point;
            state.desired_col = point.x;
        }
        self.ensure_copy_mode_cursor_visible()?;
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn toggle_copy_mode_selection_end(&mut self) -> Result<()> {
        if let Some(state) = &mut self.copy_mode
            && let Some(anchor) = &mut state.anchor
        {
            std::mem::swap(anchor, &mut state.cursor);
            state.desired_col = state.cursor.x;
            self.ensure_copy_mode_cursor_visible()?;
            self.sync_copy_mode_selection()?;
            self.mark_content_changed();
        }
        Ok(())
    }

    fn copy_mode_selection_and_cancel(&mut self) -> Result<Option<Vec<u8>>> {
        self.sync_copy_mode_selection()?;
        let copied = self.format_selection(TerminalSelectionFormat::PlainText)?;
        self.copy_mode = None;
        self.terminal.set_selection(None)?;
        self.mark_content_changed();
        Ok(copied)
    }

    fn move_copy_mode_cursor(&mut self, motion: TerminalCopyModeMotion) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let desired_col = self
            .copy_mode
            .as_ref()
            .map_or(point.x, |state| state.desired_col);
        let (next, update_desired_col) =
            self.copy_mode_motion_target(point, desired_col, motion)?;
        self.set_copy_mode_cursor(next, update_desired_col)
    }

    fn copy_mode_motion_target(
        &mut self,
        point: PointCoordinate,
        desired_col: u16,
        motion: TerminalCopyModeMotion,
    ) -> Result<(PointCoordinate, bool)> {
        let total_rows = self.terminal.total_rows()?.max(1) as u32;
        let max_y = total_rows.saturating_sub(1);
        let max_x = self.geometry.cols.saturating_sub(1);
        let page = u32::from(self.geometry.rows.max(1));
        let half_page = (page / 2).max(1);
        let mut next = point;
        let mut update_desired_col = true;

        match motion {
            TerminalCopyModeMotion::Left => {
                if next.x > 0 {
                    next.x -= 1;
                } else if next.y > 0 {
                    next.y -= 1;
                    next.x = self.screen_row_end_col(next.y)?;
                }
            }
            TerminalCopyModeMotion::Right => {
                if next.x < max_x {
                    next.x += 1;
                } else if next.y < max_y {
                    next.y += 1;
                    next.x = 0;
                }
            }
            TerminalCopyModeMotion::Up => {
                next.y = next.y.saturating_sub(1);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::Down => {
                next.y = next.y.saturating_add(1).min(max_y);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::PageUp => {
                next.y = next.y.saturating_sub(page);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::PageDown => {
                next.y = next.y.saturating_add(page).min(max_y);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::HalfPageUp => {
                next.y = next.y.saturating_sub(half_page);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::HalfPageDown => {
                next.y = next.y.saturating_add(half_page).min(max_y);
                next.x = desired_col.min(max_x);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::HistoryTop => {
                next.y = 0;
                next.x = 0;
            }
            TerminalCopyModeMotion::HistoryBottom => {
                next.y = max_y;
                next.x = self.screen_row_end_col(next.y)?;
            }
            TerminalCopyModeMotion::StartOfLine => next.x = 0,
            TerminalCopyModeMotion::EndOfLine => next.x = self.screen_row_end_col(next.y)?,
            TerminalCopyModeMotion::BackToIndentation => {
                next.x = self.screen_row_first_nonblank_col(next.y)?;
            }
            TerminalCopyModeMotion::TopLine => next.y = self.viewport_top_screen_row()?,
            TerminalCopyModeMotion::MiddleLine => {
                next.y = self
                    .viewport_top_screen_row()?
                    .saturating_add(page / 2)
                    .min(max_y);
            }
            TerminalCopyModeMotion::BottomLine => {
                next.y = self
                    .viewport_top_screen_row()?
                    .saturating_add(page.saturating_sub(1))
                    .min(max_y);
            }
            TerminalCopyModeMotion::NextWord => next = self.next_word_start(point)?,
            TerminalCopyModeMotion::PreviousWord => next = self.previous_word_start(point)?,
            TerminalCopyModeMotion::NextWordEnd => next = self.next_word_end(point)?,
            TerminalCopyModeMotion::ScrollUp => {
                let before_top = self.viewport_top_screen_row()?;
                self.scroll_viewport_delta(-1);
                let after_top = self.viewport_top_screen_row()?;
                next.y = Self::shifted_screen_row(next.y, before_top, after_top, max_y);
                update_desired_col = false;
            }
            TerminalCopyModeMotion::ScrollDown => {
                let before_top = self.viewport_top_screen_row()?;
                self.scroll_viewport_delta(1);
                let after_top = self.viewport_top_screen_row()?;
                next.y = Self::shifted_screen_row(next.y, before_top, after_top, max_y);
                update_desired_col = false;
            }
        }

        next.x = next.x.min(max_x);
        next.y = next.y.min(max_y);
        Ok((next, update_desired_col))
    }
    fn shifted_screen_row(row: u32, before_top: u32, after_top: u32, max_y: u32) -> u32 {
        let delta = i128::from(after_top) - i128::from(before_top);
        (i128::from(row) + delta).clamp(0, i128::from(max_y)) as u32
    }

    fn set_copy_mode_cursor(
        &mut self,
        point: PointCoordinate,
        update_desired_col: bool,
    ) -> Result<()> {
        if let Some(state) = &mut self.copy_mode {
            state.cursor = point;
            if update_desired_col {
                state.desired_col = point.x;
            }
        }
        self.ensure_copy_mode_cursor_visible()?;
        self.sync_copy_mode_selection()?;
        self.mark_content_changed();
        Ok(())
    }

    fn search_copy_mode_query(
        &mut self,
        query: &str,
        direction: TerminalSearchDirection,
    ) -> Result<bool> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(false);
        }
        if self.search_query != query {
            self.search_query = query.to_owned();
            self.search_active_index = 0;
            self.mark_content_changed();
        }

        let Some(cursor) = self.copy_mode_screen_point() else {
            return Ok(false);
        };
        let matches = self.copy_mode_search_matches(query)?;
        let Some(found) = Self::copy_mode_search_target(&matches, cursor, direction) else {
            self.bump_search_pulse();
            let _ = self.extract_frame()?;
            return Ok(false);
        };

        self.set_copy_mode_cursor(found.start, true)?;
        self.align_copy_mode_active_search_match()?;
        self.bump_search_pulse();
        Ok(true)
    }

    fn copy_mode_word_under_cursor(&self) -> Result<Option<String>> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(None);
        };
        let chars = self.screen_row_chars(point.y)?;
        let Some(ch) = chars.get(usize::from(point.x)).copied() else {
            return Ok(None);
        };
        if ch.is_whitespace() {
            return Ok(None);
        }

        let mut start = usize::from(point.x);
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let mut end = usize::from(point.x);
        while end + 1 < chars.len() && !chars[end + 1].is_whitespace() {
            end += 1;
        }

        let word: String = chars[start..=end].iter().collect();
        Ok((!word.is_empty()).then_some(word))
    }

    fn copy_mode_search_matches(&self, query: &str) -> Result<Vec<CopyModeSearchMatch>> {
        let query = normalized_search_query(query);
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let total_rows = self.terminal.total_rows()?.max(1) as u32;
        let mut matches = Vec::new();
        let mut logical = Vec::new();
        let mut positions = Vec::new();
        for y in 0..total_rows {
            let chars = self.screen_row_chars(y)?;
            for (x, ch) in chars.into_iter().enumerate() {
                logical.push(normalize_search_char(ch));
                positions.push(PointCoordinate { x: x as u16, y });
            }
            if !self.screen_row_wrapped(y)? {
                matches.extend(copy_mode_logical_search_matches(
                    &logical, &positions, &query,
                ));
                logical.clear();
                positions.clear();
            }
        }
        matches.extend(copy_mode_logical_search_matches(
            &logical, &positions, &query,
        ));
        Ok(matches)
    }

    fn copy_mode_search_target(
        matches: &[CopyModeSearchMatch],
        cursor: PointCoordinate,
        direction: TerminalSearchDirection,
    ) -> Option<CopyModeSearchMatch> {
        match direction {
            TerminalSearchDirection::Current => matches
                .iter()
                .copied()
                .find(|candidate| Self::copy_mode_search_match_contains(*candidate, cursor))
                .or_else(|| {
                    matches
                        .iter()
                        .copied()
                        .find(|candidate| Self::point_after(candidate.start, cursor))
                })
                .or_else(|| matches.first().copied()),
            TerminalSearchDirection::Next => matches
                .iter()
                .copied()
                .find(|candidate| {
                    !Self::copy_mode_search_match_contains(*candidate, cursor)
                        && Self::point_after(candidate.start, cursor)
                })
                .or_else(|| {
                    matches.iter().copied().find(|candidate| {
                        !Self::copy_mode_search_match_contains(*candidate, cursor)
                    })
                })
                .or_else(|| matches.first().copied()),
            TerminalSearchDirection::Previous => matches
                .iter()
                .rev()
                .copied()
                .find(|candidate| {
                    !Self::copy_mode_search_match_contains(*candidate, cursor)
                        && Self::point_before(candidate.start, cursor)
                })
                .or_else(|| {
                    matches.iter().rev().copied().find(|candidate| {
                        !Self::copy_mode_search_match_contains(*candidate, cursor)
                    })
                })
                .or_else(|| matches.last().copied()),
        }
    }

    fn align_copy_mode_active_search_match(&mut self) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let viewport_top = self.viewport_top_screen_row()?;
        let Some(row) = point.y.checked_sub(viewport_top) else {
            return Ok(());
        };
        let Ok(row) = u16::try_from(row) else {
            return Ok(());
        };
        let index = {
            let frame = self.extract_frame()?;
            frame.search_matches.iter().position(|selection| {
                selection.row == row
                    && selection.start_col <= point.x
                    && point.x <= selection.end_col
            })
        };
        if let Some(index) = index {
            self.search_active_index = index;
            self.mark_content_changed();
            let _ = self.extract_frame()?;
        }
        Ok(())
    }

    fn copy_mode_search_match_contains(
        candidate: CopyModeSearchMatch,
        point: PointCoordinate,
    ) -> bool {
        !Self::point_before(point, candidate.start) && !Self::point_after(point, candidate.end)
    }

    fn point_before(point: PointCoordinate, cursor: PointCoordinate) -> bool {
        point.y < cursor.y || point.y == cursor.y && point.x < cursor.x
    }

    fn point_after(point: PointCoordinate, cursor: PointCoordinate) -> bool {
        point.y > cursor.y || point.y == cursor.y && point.x > cursor.x
    }

    fn sync_copy_mode_selection(&mut self) -> Result<()> {
        let Some(state) = &self.copy_mode else {
            self.terminal.set_selection(None)?;
            return Ok(());
        };
        let Some(anchor) = state.anchor else {
            self.terminal.set_selection(None)?;
            return Ok(());
        };
        let (start_point, end_point) = if state.linewise {
            let start_y = anchor.y.min(state.cursor.y);
            let end_y = anchor.y.max(state.cursor.y);
            (
                PointCoordinate { x: 0, y: start_y },
                PointCoordinate {
                    x: self.screen_row_end_col(end_y)?,
                    y: end_y,
                },
            )
        } else {
            (anchor, state.cursor)
        };
        let start = self.terminal.grid_ref(Point::Screen(start_point))?;
        let end = self.terminal.grid_ref(Point::Screen(end_point))?;
        let selection = Selection::new(start, end, state.rectangle);
        self.terminal.set_selection(Some(&selection))?;
        Ok(())
    }

    fn ensure_copy_mode_cursor_visible(&mut self) -> Result<()> {
        let Some(point) = self.copy_mode_screen_point() else {
            return Ok(());
        };
        let top = self.viewport_top_screen_row()?;
        let bottom = top.saturating_add(u32::from(self.geometry.rows.max(1)).saturating_sub(1));
        let delta = if point.y < top {
            i128::from(point.y) - i128::from(top)
        } else if point.y > bottom {
            i128::from(point.y) - i128::from(bottom)
        } else {
            0
        };
        if delta != 0 {
            self.scroll_viewport_delta(
                delta.clamp(isize::MIN as i128, isize::MAX as i128) as isize,
            );
        }
        Ok(())
    }

    fn viewport_top_screen_row(&self) -> Result<u32> {
        Ok(self.terminal.scrollbar()?.offset as u32)
    }

    fn screen_row_chars(&self, row: u32) -> Result<Vec<char>> {
        let mut chars = Vec::with_capacity(usize::from(self.geometry.cols));
        for x in 0..self.geometry.cols {
            let cell = self
                .terminal
                .grid_ref(Point::Screen(PointCoordinate { x, y: row }))?
                .cell()?;
            let ch = if cell.has_text()? {
                char::from_u32(cell.codepoint()?).unwrap_or(' ')
            } else {
                ' '
            };
            chars.push(ch);
        }
        Ok(chars)
    }

    fn screen_row_end_col(&self, row: u32) -> Result<u16> {
        let chars = self.screen_row_chars(row)?;
        Ok(chars
            .iter()
            .rposition(|ch| !ch.is_whitespace())
            .map_or(0, |index| index as u16))
    }

    fn screen_row_first_nonblank_col(&self, row: u32) -> Result<u16> {
        let chars = self.screen_row_chars(row)?;
        Ok(chars
            .iter()
            .position(|ch| !ch.is_whitespace())
            .map_or(0, |index| index as u16))
    }

    fn screen_char(&self, point: PointCoordinate) -> Result<char> {
        let cell = self.terminal.grid_ref(Point::Screen(point))?.cell()?;
        if cell.has_text()? {
            Ok(char::from_u32(cell.codepoint()?).unwrap_or(' '))
        } else {
            Ok(' ')
        }
    }

    fn screen_row_wrapped(&self, row: u32) -> Result<bool> {
        Ok(self
            .terminal
            .grid_ref(Point::Screen(PointCoordinate { x: 0, y: row }))?
            .row()?
            .is_wrapped()
            .unwrap_or(false))
    }

    fn next_screen_point(&self, point: PointCoordinate) -> Result<Option<PointCoordinate>> {
        let max_x = self.geometry.cols.saturating_sub(1);
        let max_y = (self.terminal.total_rows()?.max(1) as u32).saturating_sub(1);
        if point.x < max_x {
            Ok(Some(PointCoordinate {
                x: point.x + 1,
                y: point.y,
            }))
        } else if point.y < max_y {
            Ok(Some(PointCoordinate {
                x: 0,
                y: point.y + 1,
            }))
        } else {
            Ok(None)
        }
    }

    fn previous_screen_point(&self, point: PointCoordinate) -> Result<Option<PointCoordinate>> {
        if point.x > 0 {
            Ok(Some(PointCoordinate {
                x: point.x - 1,
                y: point.y,
            }))
        } else if point.y > 0 {
            Ok(Some(PointCoordinate {
                x: self.geometry.cols.saturating_sub(1),
                y: point.y - 1,
            }))
        } else {
            Ok(None)
        }
    }

    fn next_word_start(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        let mut left_word = self.screen_char(current)?.is_whitespace();
        while let Some(next) = self.next_screen_point(current)? {
            current = next;
            let is_word = !self.screen_char(current)?.is_whitespace();
            if left_word && is_word {
                return Ok(current);
            }
            if !is_word {
                left_word = true;
            }
        }
        Ok(current)
    }

    fn previous_word_start(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        while let Some(previous) = self.previous_screen_point(current)? {
            current = previous;
            if !self.screen_char(current)?.is_whitespace() {
                break;
            }
        }
        while let Some(previous) = self.previous_screen_point(current)? {
            if self.screen_char(previous)?.is_whitespace() {
                break;
            }
            current = previous;
        }
        Ok(current)
    }

    fn next_word_end(&self, point: PointCoordinate) -> Result<PointCoordinate> {
        let mut current = point;
        while let Some(next) = self.next_screen_point(current)? {
            current = next;
            if !self.screen_char(current)?.is_whitespace() {
                break;
            }
        }
        while let Some(next) = self.next_screen_point(current)? {
            if self.screen_char(next)?.is_whitespace() {
                break;
            }
            current = next;
        }
        Ok(current)
    }

    fn copy_mode_frame_state(copy_mode: Option<&CopyModeState>) -> Option<FrameCopyMode> {
        copy_mode.map(|state| FrameCopyMode {
            selecting: state.selecting(),
            rectangle: state.rectangle,
        })
    }

    fn apply_copy_mode_frame_cursor(
        frame: &mut RenderFrame,
        copy_mode: Option<&CopyModeState>,
        viewport_top: u32,
    ) {
        let Some(point) = copy_mode.map(|state| state.cursor) else {
            return;
        };
        let Some(row) = point.y.checked_sub(viewport_top) else {
            return;
        };
        let Ok(y) = u16::try_from(row) else {
            return;
        };
        if point.x >= frame.cols || y >= frame.rows {
            return;
        }
        frame.cursor = Some(CursorSnapshot {
            x: point.x,
            y,
            at_wide_tail: false,
            style: CursorVisualStyle::BlockHollow,
            blinking: false,
            color: frame.colors.cursor.or(Some(frame.colors.foreground)),
        });
    }

    pub fn is_mouse_tracking(&self) -> Result<bool> {
        self.terminal.is_mouse_tracking().map_err(Into::into)
    }

    pub fn is_synchronized_output(&self) -> Result<bool> {
        self.terminal.mode(Mode::SYNC_OUTPUT).map_err(Into::into)
    }

    fn assemble_cached_frame(
        &mut self,
        extract_start: Instant,
        render_state_update_us: u64,
        row_dirty: Vec<bool>,
    ) -> Result<&RenderFrame> {
        self.frame.row_dirty = row_dirty;
        self.frame.row_wraps.clear();
        self.frame.cells.clear();
        self.frame.text.clear();
        self.frame.images = KittyImageFrame::default();
        self.frame.selections.clear();
        self.frame.search_matches.clear();
        self.frame.active_search_match = None;
        self.frame.active_search_match_index = None;
        self.frame.search_match_count = 0;
        self.frame.search_pulse = self.search_pulse;
        self.frame.copy_mode = Self::copy_mode_frame_state(self.copy_mode.as_ref());
        self.frame.stats = FrameStats {
            render_state_update_us,
            ..FrameStats::default()
        };

        let mut virtual_cells = Vec::new();
        for row in &self.row_cache {
            if let Some(selection) = row.selection {
                self.frame.selections.push(selection);
            }
            self.frame.row_wraps.push(row.wrapped);
            virtual_cells.extend(row.virtual_cells.iter().cloned());
            let text_offset = self.frame.text.len();
            self.frame.text.extend_from_slice(&row.text);
            self.frame.stats.chars += row.text.len();
            self.frame.stats.cells += row.cells.len();
            self.frame
                .cells
                .extend(row.cells.iter().cloned().map(|mut cell| {
                    cell.text_start += text_offset;
                    cell
                }));
        }
        if !self.search_query.is_empty() {
            self.frame.search_matches = frame_search_matches(&self.frame, &self.search_query);
            self.frame.search_match_count = self.frame.search_matches.len();
            if self.frame.search_match_count > 0 {
                self.search_active_index = self
                    .search_active_index
                    .min(self.frame.search_match_count - 1);
                self.frame.active_search_match_index = Some(self.search_active_index + 1);
                self.frame.active_search_match = self
                    .frame
                    .search_matches
                    .get(self.search_active_index)
                    .copied();
            }
        }
        self.frame.stats.dirty_rows = self.frame.row_dirty.iter().filter(|dirty| **dirty).count();

        if self.kitty_graphics_touched || !virtual_cells.is_empty() {
            let surface = TerminalSurface::for_logical_size(
                f32::from(self.geometry.cols) * self.render_cell.width,
                f32::from(self.geometry.rows) * self.render_cell.height,
                self.render_cell,
                TerminalPadding::default(),
            );
            let mut images = collect_kitty_image_frame(
                &self.terminal,
                surface,
                self.display_scale,
                &mut self.image_placements,
                &mut self.image_data_cache,
            )
            .unwrap_or_default();
            images.placements.retain(|placement| {
                !placement_rows_overlap_content(placement, surface, &self.row_cache)
            });
            images.virtual_placeholder_rows = append_virtual_image_placements(
                &self.terminal,
                surface,
                self.display_scale,
                &mut images,
                &virtual_cells,
                &mut self.image_data_cache,
            )?;
            self.image_data_cache.retain_frame(&images);
            self.frame.images = images;
        }

        let viewport_top = self.viewport_top_screen_row()?;
        Self::apply_copy_mode_frame_cursor(&mut self.frame, self.copy_mode.as_ref(), viewport_top);
        self.frame.stats.extraction_us = extract_start.elapsed().as_micros() as u64;
        self.extracted_content_epoch = self.content_epoch;
        Ok(&self.frame)
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
        let cache_matches_frame = self.frame.cols == cols
            && self.frame.rows == rows
            && self.row_cache.len() == usize::from(rows);

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
            selection_background: self
                .xterm_color_overrides
                .highlight_background
                .or(self.colors.highlight_background)
                .or(self.colors.selection_background),
            selection_foreground: self
                .xterm_color_overrides
                .highlight_foreground
                .or(self.colors.highlight_foreground)
                .or(self.colors.selection_foreground),
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
        let viewport_top = scrollbar.offset as u32;
        self.frame.copy_mode = Self::copy_mode_frame_state(self.copy_mode.as_ref());
        Self::apply_copy_mode_frame_cursor(&mut self.frame, self.copy_mode.as_ref(), viewport_top);
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

        // Extract through the row cache for any non-clean frame. A cold cache (first
        // frame, resize, or a full redraw) re-extracts every row once and leaves the
        // cache warm, so the *next* localized edit extracts incrementally instead of
        // paying a full re-extraction (the §5.4 cold-cache cliff). A warm cache touches
        // only the rows that changed. `extract_render_row` is the row-decomposed form of
        // the former inline full-frame loop, reassembled by `assemble_cached_frame`.
        let full = dirty == Dirty::Full;
        let mut row_dirty = Vec::with_capacity(usize::from(rows));
        {
            let mut row_iter = self.rows.update(&snapshot)?;
            while let Some(row) = row_iter.next() {
                row_dirty.push(full || row.dirty()?);
            }
        }

        self.row_cache
            .resize_with(usize::from(rows), CachedRenderRow::default);
        // A cold cache can't trust per-row dirty flags against stale/absent rows, so
        // re-extract everything; a warm cache extracts only the rows reported dirty.
        let update_all_rows = full || !cache_matches_frame;
        let mut row_iter = self.rows.update(&snapshot)?;
        let mut row_index = 0_u16;
        let mut hyperlink_scratch = Vec::new();
        while let Some(row) = row_iter.next() {
            let index = usize::from(row_index);
            if update_all_rows || row_dirty.get(index).copied().unwrap_or(false) {
                extract_render_row(
                    &self.terminal,
                    &mut self.cells,
                    &mut self.grapheme_scratch,
                    &mut hyperlink_scratch,
                    row,
                    row_index,
                    &mut self.row_cache[index],
                )?;
            }
            // Clear the render-state row dirty flag so the next update reports only
            // newly-changed rows. libghostty's update does not unset dirty state.
            row.set_dirty(false)?;
            row_index += 1;
        }
        snapshot.set_dirty(Dirty::Clean)?;
        self.assemble_cached_frame(extract_start, render_state_update_us, row_dirty)
    }
}
