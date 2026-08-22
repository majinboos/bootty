//! Conversion from ratatui buffers into Bootty web terminal frames and egui chrome.

use bootty_surface::selection::{SelectionPoint, TerminalSelection};
use egui::Context as EguiContext;
use serde::Serialize;
use tuirealm::ratatui::buffer::{Buffer, Cell};
use tuirealm::ratatui::style::{Color, Modifier};

use crate::constants::{CELL_HEIGHT, CELL_WIDTH};
use crate::input::Focus;

pub(crate) fn new_egui_context() -> EguiContext {
    EguiContext::default()
}

#[derive(Clone, Copy)]
pub(crate) struct WebFrameState {
    pub(crate) selected: usize,
    pub(crate) hovered_menu: Option<usize>,
    pub(crate) tick: u64,
    pub(crate) focus: Focus,
    pub(crate) fps: f64,
    pub(crate) selection: Option<TerminalSelection>,
}

pub(crate) fn web_frame(
    _egui: &EguiContext,
    buffer: &Buffer,
    state: WebFrameState,
) -> WebTerminalFrame {
    let _ = (state.hovered_menu, state.tick, state.fps);
    let mut cells = Vec::with_capacity(buffer.content.len());
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            cells.push(web_cell(x, y, cell, None));
        }
    }
    WebTerminalFrame {
        selected: state.selected,
        focus: match state.focus {
            Focus::Menu => "menu",
            Focus::Detail => "detail",
        },
        cols: buffer.area.width,
        rows: buffer.area.height,
        cell_width: CELL_WIDTH,
        cell_height: CELL_HEIGHT,
        colors: WebFrameColors {
            background: web_color(Color::Rgb(17, 18, 26)),
            foreground: web_color(Color::Rgb(192, 202, 245)),
            cursor: Some(web_color(Color::Magenta)),
        },
        cursor: None,
        selection: state.selection.map(web_selection),
        cells,
        images: Vec::new(),
        egui: Some(WebEguiFrame::default()),
    }
}

pub(crate) fn web_cell(x: u16, y: u16, cell: &Cell, osc8: Option<&str>) -> WebCell {
    WebCell {
        x,
        y,
        text: cell.symbol().to_owned(),
        fg: web_optional_color(cell.fg),
        bg: web_optional_color(cell.bg),
        osc8: osc8.map(str::to_owned),
        style: WebCellStyle {
            bold: cell.modifier.contains(Modifier::BOLD),
            italic: cell.modifier.contains(Modifier::ITALIC),
            faint: cell.modifier.contains(Modifier::DIM),
            blink: cell.modifier.contains(Modifier::SLOW_BLINK)
                || cell.modifier.contains(Modifier::RAPID_BLINK),
            inverse: cell.modifier.contains(Modifier::REVERSED),
            invisible: cell.modifier.contains(Modifier::HIDDEN),
            strikethrough: cell.modifier.contains(Modifier::CROSSED_OUT),
            overline: false,
            underline: cell.modifier.contains(Modifier::UNDERLINED),
        },
    }
}

fn web_optional_color(color: Color) -> Option<WebColor> {
    match color {
        Color::Reset => None,
        _ => Some(web_color(color)),
    }
}

fn web_color(color: Color) -> WebColor {
    match color {
        Color::Black | Color::Reset => WebColor {
            r: 17,
            g: 18,
            b: 26,
        },
        Color::Red => WebColor {
            r: 247,
            g: 118,
            b: 142,
        },
        Color::Green => WebColor {
            r: 158,
            g: 206,
            b: 106,
        },
        Color::Yellow => WebColor {
            r: 224,
            g: 175,
            b: 104,
        },
        Color::Blue => WebColor {
            r: 122,
            g: 162,
            b: 247,
        },
        Color::Magenta => WebColor {
            r: 255,
            g: 79,
            b: 176,
        },
        Color::Cyan => WebColor {
            r: 125,
            g: 207,
            b: 255,
        },
        Color::Gray | Color::DarkGray => WebColor {
            r: 169,
            g: 177,
            b: 214,
        },
        Color::White => WebColor {
            r: 192,
            g: 202,
            b: 245,
        },
        Color::Rgb(r, g, b) => WebColor { r, g, b },
        Color::Indexed(_)
        | Color::LightRed
        | Color::LightGreen
        | Color::LightYellow
        | Color::LightBlue
        | Color::LightMagenta
        | Color::LightCyan => WebColor {
            r: 192,
            g: 202,
            b: 245,
        },
    }
}

fn web_selection(selection: TerminalSelection) -> WebSelection {
    WebSelection {
        anchor: web_selection_point(selection.anchor),
        focus: web_selection_point(selection.focus),
    }
}

fn web_selection_point(point: SelectionPoint) -> WebSelectionPoint {
    WebSelectionPoint {
        x: point.x,
        y: point.y,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebTerminalFrame {
    pub selected: usize,
    pub focus: &'static str,
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u32,
    pub cell_height: u32,
    pub colors: WebFrameColors,
    pub cursor: Option<WebCursor>,
    pub selection: Option<WebSelection>,
    pub cells: Vec<WebCell>,
    pub images: Vec<WebImage>,
    pub egui: Option<WebEguiFrame>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebFrameColors {
    pub background: WebColor,
    pub foreground: WebColor,
    pub cursor: Option<WebColor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebCell {
    pub x: u16,
    pub y: u16,
    pub text: String,
    pub fg: Option<WebColor>,
    pub bg: Option<WebColor>,
    pub osc8: Option<String>,
    pub style: WebCellStyle,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebCellStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
    pub overline: bool,
    pub underline: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSelection {
    pub anchor: WebSelectionPoint,
    pub focus: WebSelectionPoint,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSelectionPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebCursor {
    pub x: u16,
    pub y: u16,
    pub color: Option<WebColor>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebImage {
    pub key: String,
    pub layer: String,
    pub image_width: u32,
    pub image_height: u32,
    pub source: WebRect,
    pub destination: WebRect,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebRect {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebEguiFrame {
    pub textures: Vec<WebEguiTexture>,
    pub meshes: Vec<WebEguiMesh>,
    pub labels: Vec<WebEguiLabel>,
    pub links: Vec<WebEguiLink>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebEguiLabel {
    pub x: f32,
    pub y: f32,
    pub text: String,
    pub size: f32,
    pub color: WebColor,
    pub align: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebEguiLink {
    pub rect: WebRect,
    pub url: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebEguiTexture {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebEguiMesh {
    pub texture_id: String,
    pub clip: WebRect,
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
}
