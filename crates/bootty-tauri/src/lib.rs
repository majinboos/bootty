use std::sync::Mutex;

use bootty_runtime::terminal::{
    CellStyle, CursorSnapshot, FrameColors, RenderCell, RenderFrame, TerminalSession,
};
use bootty_surface::geometry::{CellMetrics, SurfaceRect, TerminalGeometry};
use bootty_terminal::terminal_image::{KittyImageLayer, KittyImagePlacement};
use serde::Serialize;
use tauri::State;

const DEFAULT_COLS: u16 = 96;
const DEFAULT_ROWS: u16 = 32;
const DEFAULT_CELL_WIDTH: u32 = 10;
const DEFAULT_CELL_HEIGHT: u32 = 20;

struct AppState {
    terminal: Mutex<Option<TerminalSession>>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResizeRequest {
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebTerminalFrame {
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
    colors: WebFrameColors,
    cursor: Option<WebCursor>,
    cells: Vec<WebCell>,
    images: Vec<WebImage>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebFrameColors {
    background: WebColor,
    foreground: WebColor,
    cursor: Option<WebColor>,
    cursor_text: Option<WebColor>,
    selection_background: Option<WebColor>,
    selection_foreground: Option<WebColor>,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebColor {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCell {
    x: u16,
    y: u16,
    text: String,
    fg: Option<WebColor>,
    bg: Option<WebColor>,
    osc8: Option<String>,
    style: WebCellStyle,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCellStyle {
    bold: bool,
    italic: bool,
    faint: bool,
    blink: bool,
    inverse: bool,
    invisible: bool,
    strikethrough: bool,
    overline: bool,
    underline: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebImage {
    key: String,
    layer: WebImageLayer,
    image_width: u32,
    image_height: u32,
    source: WebRect,
    destination: WebRect,
    rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum WebImageLayer {
    BelowBackground,
    BelowText,
    AboveText,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRect {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCursor {
    x: u16,
    y: u16,
    at_wide_tail: bool,
    blinking: bool,
    color: Option<WebColor>,
}

#[tauri::command]
fn start_terminal(state: State<'_, AppState>) -> Result<WebTerminalFrame, String> {
    let mut terminal = state
        .terminal
        .lock()
        .map_err(|_| "terminal state lock poisoned".to_owned())?;
    if terminal.is_none() {
        *terminal =
            Some(TerminalSession::new(default_geometry()).map_err(|error| error.to_string())?);
    }
    terminal_frame_from_state(&mut terminal)
}

#[tauri::command]
fn resize_terminal(
    request: ResizeRequest,
    state: State<'_, AppState>,
) -> Result<WebTerminalFrame, String> {
    let mut terminal = state
        .terminal
        .lock()
        .map_err(|_| "terminal state lock poisoned".to_owned())?;
    let terminal = terminal
        .as_mut()
        .ok_or_else(|| "terminal has not been started".to_owned())?;
    terminal
        .resize(TerminalGeometry {
            cols: request.cols.max(1),
            rows: request.rows.max(1),
            cell_width: request.cell_width.max(1),
            cell_height: request.cell_height.max(1),
        })
        .map_err(|error| error.to_string())?;
    let frame = terminal
        .extract_frame()
        .map_err(|error| error.to_string())?;
    Ok(web_frame(
        &frame,
        terminal.grid_size(),
        CellMetrics::new(request.cell_width as f32, request.cell_height as f32),
    ))
}

#[tauri::command]
fn write_terminal(input: String, state: State<'_, AppState>) -> Result<(), String> {
    let terminal = state
        .terminal
        .lock()
        .map_err(|_| "terminal state lock poisoned".to_owned())?;
    let terminal = terminal
        .as_ref()
        .ok_or_else(|| "terminal has not been started".to_owned())?;
    terminal
        .write_input(input.as_bytes())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn terminal_frame(state: State<'_, AppState>) -> Result<WebTerminalFrame, String> {
    let mut terminal = state
        .terminal
        .lock()
        .map_err(|_| "terminal state lock poisoned".to_owned())?;
    terminal_frame_from_state(&mut terminal)
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState {
            terminal: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            start_terminal,
            resize_terminal,
            write_terminal,
            terminal_frame
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Bootty Tauri app");
}

fn terminal_frame_from_state(
    terminal: &mut Option<TerminalSession>,
) -> Result<WebTerminalFrame, String> {
    let terminal = terminal
        .as_mut()
        .ok_or_else(|| "terminal has not been started".to_owned())?;
    let frame = terminal
        .extract_frame()
        .map_err(|error| error.to_string())?;
    let (cols, rows) = terminal.grid_size();
    Ok(web_frame(
        &frame,
        (cols, rows),
        CellMetrics::new(DEFAULT_CELL_WIDTH as f32, DEFAULT_CELL_HEIGHT as f32),
    ))
}

fn default_geometry() -> TerminalGeometry {
    TerminalGeometry {
        cols: DEFAULT_COLS,
        rows: DEFAULT_ROWS,
        cell_width: DEFAULT_CELL_WIDTH,
        cell_height: DEFAULT_CELL_HEIGHT,
    }
}

fn web_frame(
    frame: &RenderFrame,
    fallback_grid: (u16, u16),
    cell: CellMetrics,
) -> WebTerminalFrame {
    WebTerminalFrame {
        cols: if frame.cols == 0 {
            fallback_grid.0
        } else {
            frame.cols
        },
        rows: if frame.rows == 0 {
            fallback_grid.1
        } else {
            frame.rows
        },
        cell_width: cell.width.ceil().max(1.0) as u32,
        cell_height: cell.height.ceil().max(1.0) as u32,
        colors: web_colors(frame.colors),
        cursor: frame.cursor.map(web_cursor),
        images: frame
            .images
            .placements
            .iter()
            .filter_map(web_image)
            .collect(),
        cells: frame
            .cells
            .iter()
            .map(|cell| web_cell(frame, cell))
            .collect(),
    }
}

fn web_cell(frame: &RenderFrame, cell: &RenderCell) -> WebCell {
    WebCell {
        x: cell.x,
        y: cell.y,
        text: frame.cell_text(cell).iter().collect(),
        fg: cell.fg.map(web_color),
        bg: cell.bg.map(web_color),
        osc8: None,
        style: web_style(cell.style),
    }
}

fn web_colors(colors: FrameColors) -> WebFrameColors {
    WebFrameColors {
        background: web_color(colors.background),
        foreground: web_color(colors.foreground),
        cursor: colors.cursor.map(web_color),
        cursor_text: colors.cursor_text.map(web_color),
        selection_background: colors.selection_background.map(web_color),
        selection_foreground: colors.selection_foreground.map(web_color),
    }
}

fn web_image(image: &KittyImagePlacement) -> Option<WebImage> {
    let rgba = rgba_image_pixels(image)?;
    Some(WebImage {
        key: format!("{}:{}", image.image_id, image.placement_id),
        layer: web_image_layer(image.layer),
        image_width: image.image_width,
        image_height: image.image_height,
        source: WebRect {
            min_x: image.source.x as f32,
            min_y: image.source.y as f32,
            max_x: image.source.x.checked_add(image.source.width)? as f32,
            max_y: image.source.y.checked_add(image.source.height)? as f32,
        },
        destination: web_rect(image.destination),
        rgba,
    })
}

fn rgba_image_pixels(image: &KittyImagePlacement) -> Option<Vec<u8>> {
    let pixels = image.image_width.checked_mul(image.image_height)? as usize;
    match image.image_format {
        libghostty_vt::kitty::graphics::ImageFormat::Rgba => {
            let expected = pixels.checked_mul(4)?;
            (image.data.len() >= expected).then(|| image.data[..expected].to_vec())
        }
        libghostty_vt::kitty::graphics::ImageFormat::Rgb => {
            let expected = pixels.checked_mul(3)?;
            if image.data.len() < expected {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for rgb in image.data[..expected].chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Some(rgba)
        }
        libghostty_vt::kitty::graphics::ImageFormat::GrayAlpha => {
            let expected = pixels.checked_mul(2)?;
            if image.data.len() < expected {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for gray_alpha in image.data[..expected].chunks_exact(2) {
                rgba.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[1],
                ]);
            }
            Some(rgba)
        }
        libghostty_vt::kitty::graphics::ImageFormat::Gray => {
            if image.data.len() < pixels {
                return None;
            }
            let mut rgba = Vec::with_capacity(pixels * 4);
            for gray in &image.data[..pixels] {
                rgba.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            Some(rgba)
        }
        libghostty_vt::kitty::graphics::ImageFormat::Png => decode_png_rgba(image),
        _ => None,
    }
}

fn decode_png_rgba(image: &KittyImagePlacement) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(image.data.as_slice()));
    decoder.set_transformations(png::Transformations::ALPHA | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;
    if info.width != image.image_width || info.height != image.image_height {
        return None;
    }
    let data = &buffer[..info.buffer_size()];
    match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => Some(data.to_vec()),
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() / 3 * 4);
            for rgb in data.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
            Some(rgba)
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() / 2 * 4);
            for gray_alpha in data.chunks_exact(2) {
                rgba.extend_from_slice(&[
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[0],
                    gray_alpha[1],
                ]);
            }
            Some(rgba)
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            let mut rgba = Vec::with_capacity(data.len() * 4);
            for gray in data {
                rgba.extend_from_slice(&[*gray, *gray, *gray, 255]);
            }
            Some(rgba)
        }
        _ => None,
    }
}

fn web_image_layer(layer: KittyImageLayer) -> WebImageLayer {
    match layer {
        KittyImageLayer::BelowBackground => WebImageLayer::BelowBackground,
        KittyImageLayer::BelowText => WebImageLayer::BelowText,
        KittyImageLayer::AboveText => WebImageLayer::AboveText,
    }
}

fn web_rect(rect: SurfaceRect) -> WebRect {
    WebRect {
        min_x: rect.min_x,
        min_y: rect.min_y,
        max_x: rect.max_x,
        max_y: rect.max_y,
    }
}

fn web_style(style: CellStyle) -> WebCellStyle {
    WebCellStyle {
        bold: style.bold,
        italic: style.italic,
        faint: style.faint,
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
        overline: style.overline,
        underline: !matches!(style.underline, libghostty_vt::style::Underline::None),
    }
}

fn web_cursor(cursor: CursorSnapshot) -> WebCursor {
    WebCursor {
        x: cursor.x,
        y: cursor.y,
        at_wide_tail: cursor.at_wide_tail,
        blinking: cursor.blinking,
        color: cursor.color.map(web_color),
    }
}

fn web_color(color: libghostty_vt::style::RgbColor) -> WebColor {
    WebColor {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

#[cfg(test)]
mod tests {

    use std::sync::Arc;

    use bootty_runtime::terminal::{CellStyle, FrameColors, FrameStats, RenderCell, RenderFrame};
    use bootty_surface::geometry::SurfaceRect;
    use bootty_terminal::terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement};
    use libghostty_vt::{
        kitty::graphics::{ImageFormat, SourceRect},
        render::Dirty,
        style::{RgbColor, Underline},
    };

    use super::{CellMetrics, web_frame};
    #[test]
    fn web_frame_preserves_cells_text_colors_and_metrics() {
        let style = CellStyle {
            bold: true,
            underline: Underline::Single,
            ..Default::default()
        };
        let frame = RenderFrame {
            cols: 2,
            rows: 1,
            dirty: Dirty::Full,
            colors: FrameColors {
                background: rgb(0x10, 0x11, 0x12),
                foreground: rgb(0xa0, 0xa1, 0xa2),
                cursor: None,
                cursor_text: None,
                selection_background: None,
                selection_foreground: None,
            },
            cursor: None,
            row_dirty: vec![true],
            cells: vec![RenderCell {
                x: 0,
                y: 0,
                text_start: 0,
                text_len: 2,
                fg: Some(rgb(0xff, 0xee, 0xdd)),
                bg: None,
                style,
            }],
            text: vec!['h', 'i'],
            images: KittyImageFrame {
                placements: vec![KittyImagePlacement {
                    image_id: 7,
                    placement_id: 9,
                    layer: KittyImageLayer::AboveText,
                    image_width: 1,
                    image_height: 1,
                    image_format: ImageFormat::Rgba,
                    source: SourceRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    },
                    destination: SurfaceRect::from_min_size(2.0, 3.0, 4.0, 5.0),
                    data: Arc::new(vec![1, 2, 3, 4]),
                }],
                ..KittyImageFrame::default()
            },
            scrollbar: None,
            stats: FrameStats::default(),
        };

        let web = web_frame(&frame, (80, 24), CellMetrics::new(9.2, 18.1));

        assert_eq!(web.cols, 2);
        assert_eq!(web.rows, 1);
        assert_eq!(web.cell_width, 10);
        assert_eq!(web.cell_height, 19);
        assert_eq!(web.cells[0].text, "hi");
        assert!(web.cells[0].style.bold);
        assert!(web.cells[0].style.underline);
        assert_eq!(web.cells[0].fg.map(|color| color.r), Some(0xff));
        assert_eq!(web.images.len(), 1);
        assert_eq!(web.images[0].key, "7:9");
        assert_eq!(web.images[0].layer, super::WebImageLayer::AboveText);
        assert_eq!(web.images[0].rgba, [1, 2, 3, 4]);
        assert_eq!(web.images[0].destination.min_x, 2.0);
    }

    fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }
}
