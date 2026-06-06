use std::sync::Mutex;

use bootty_runtime::terminal::{
    CellStyle, CursorSnapshot, FrameColors, RenderCell, RenderFrame, TerminalSession,
};
use bootty_surface::geometry::{CellMetrics, TerminalGeometry};
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
    use bootty_runtime::terminal::{CellStyle, FrameColors, FrameStats, RenderCell, RenderFrame};
    use bootty_terminal::terminal_image::KittyImageFrame;
    use libghostty_vt::{
        render::Dirty,
        style::{RgbColor, Underline},
    };

    use super::{CellMetrics, web_frame};

    #[test]
    fn web_frame_preserves_cells_text_colors_and_metrics() {
        let mut style = CellStyle::default();
        style.bold = true;
        style.underline = Underline::Single;
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
            images: KittyImageFrame::default(),
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
    }

    fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }
}
