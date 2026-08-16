//! Conversion from ratatui buffers into Bootty web terminal frames and egui chrome.

use std::io::Cursor;
use std::sync::LazyLock;

use bootty_surface::selection::{SelectionPoint, TerminalSelection};
use egui::epaint::{ImageData, Primitive};
use egui::{
    Color32, Context as EguiContext, LayerId, Order, Pos2, RawInput, Rect as EguiRect, Stroke,
    StrokeKind, TextureId, Vec2,
};
use serde::Serialize;
use tuirealm::ratatui::buffer::{Buffer, Cell};
use tuirealm::ratatui::layout::Rect;
use tuirealm::ratatui::style::{Color, Modifier};

use crate::constants::{
    CELL_HEIGHT, CELL_WIDTH, EGUI_SIDEBAR_ROW_HEIGHT_PX, EGUI_SIDEBAR_TOP_PX, ICON_PNG,
    ICON_RENDER_SIZE, ICON_TEXTURE_SIZE,
};
use crate::content::sections;
use crate::input::Focus;
use crate::layout::site_layout;

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

#[derive(Clone, Copy)]
struct EguiShellRects {
    shell: WebRect,
    header: WebRect,
    sidebar: WebRect,
    footer: WebRect,
}

const HTML_SHELL: bool = true;

pub(crate) fn web_frame(
    egui: &EguiContext,
    buffer: &Buffer,
    state: WebFrameState,
) -> WebTerminalFrame {
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
        egui: Some(if HTML_SHELL {
            empty_egui_frame()
        } else {
            egui_shell_frame(egui, buffer.area.width, buffer.area.height, state)
        }),
    }
}

fn empty_egui_frame() -> WebEguiFrame {
    WebEguiFrame {
        labels: Vec::new(),
        links: Vec::new(),
        textures: Vec::new(),
        meshes: Vec::new(),
    }
}

fn egui_shell_frame(
    egui: &EguiContext,
    cols: u16,
    rows: u16,
    state: WebFrameState,
) -> WebEguiFrame {
    let layout = site_layout(cols, rows);
    let rects = EguiShellRects {
        shell: WebRect {
            min_x: 0.0,
            min_y: 0.0,
            max_x: cols as f32 * CELL_WIDTH as f32,
            max_y: rows as f32 * CELL_HEIGHT as f32,
        },
        header: web_rect(layout.header),
        sidebar: web_rect(layout.menu),
        footer: web_rect(layout.footer),
    };
    let shell = rects.shell;
    let raw_input = RawInput {
        screen_rect: Some(EguiRect::from_min_size(
            Pos2::ZERO,
            Vec2::new(shell.max_x, shell.max_y),
        )),
        max_texture_side: Some(4096),
        time: Some(state.tick as f64 / 60.0),
        ..Default::default()
    };
    let labels = egui_shell_labels(rects, state.selected, state.hovered_menu, state.fps);
    let links = egui_shell_links(rects);
    let output = egui.run_ui(raw_input, |ui| {
        paint_egui_shell(ui.ctx(), rects, state.selected, state.hovered_menu);
    });
    let primitives = egui.tessellate(output.shapes, output.pixels_per_point);
    let mut textures_delta = output.textures_delta;
    let mut textures = std::mem::take(&mut textures_delta.set)
        .into_iter()
        .flat_map(|(id, deltas)| {
            deltas
                .into_iter()
                .map(move |delta| egui_texture(id, delta.image))
        })
        .collect::<Vec<_>>();
    textures_delta.clear();
    let mut meshes = primitives
        .into_iter()
        .filter_map(|primitive| match primitive.primitive {
            Primitive::Mesh(mesh) => Some(WebEguiMesh {
                texture_id: texture_id(mesh.texture_id),
                clip: WebRect {
                    min_x: primitive.clip_rect.min.x,
                    min_y: primitive.clip_rect.min.y,
                    max_x: primitive.clip_rect.max.x,
                    max_y: primitive.clip_rect.max.y,
                },
                vertices: mesh
                    .vertices
                    .into_iter()
                    .flat_map(|vertex| {
                        let color = vertex.color;
                        [
                            vertex.pos.x,
                            vertex.pos.y,
                            vertex.uv.x,
                            vertex.uv.y,
                            f32::from(color.r()) / 255.0,
                            f32::from(color.g()) / 255.0,
                            f32::from(color.b()) / 255.0,
                            f32::from(color.a()) / 255.0,
                        ]
                    })
                    .collect(),
                indices: mesh.indices,
            }),
            Primitive::Callback(_) => None,
        })
        .collect::<Vec<_>>();
    push_egui_icon(&mut textures, &mut meshes, rects.header);
    WebEguiFrame {
        textures,
        meshes,
        labels,
        links,
    }
}

fn web_rect(rect: Rect) -> WebRect {
    WebRect {
        min_x: f32::from(rect.x) * CELL_WIDTH as f32,
        min_y: f32::from(rect.y) * CELL_HEIGHT as f32,
        max_x: f32::from(rect.x.saturating_add(rect.width)) * CELL_WIDTH as f32,
        max_y: f32::from(rect.y.saturating_add(rect.height)) * CELL_HEIGHT as f32,
    }
}

fn paint_egui_shell(
    ctx: &EguiContext,
    rects: EguiShellRects,
    selected: usize,
    hovered_menu: Option<usize>,
) {
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, egui::Id::new("site-shell")));
    let base = Color32::from_rgb(15, 16, 24);
    let border = Color32::from_rgb(34, 38, 52);
    let hover = Color32::from_rgb(24, 27, 38);
    let current = Color32::from_rgb(30, 34, 48);
    let header = egui_rect(rects.header);
    let sidebar = egui_rect(rects.sidebar);
    let footer = egui_rect(rects.footer);

    painter.line_segment(
        [
            Pos2::new(header.left(), header.bottom() - 0.5),
            Pos2::new(header.right(), header.bottom() - 0.5),
        ],
        Stroke::new(1.0, border),
    );
    painter.line_segment(
        [
            Pos2::new(footer.left(), footer.top() + 0.5),
            Pos2::new(footer.right(), footer.top() + 0.5),
        ],
        Stroke::new(1.0, border),
    );
    painter.rect_filled(sidebar, 0.0, base);
    painter.rect_stroke(sidebar, 0.0, Stroke::new(1.0, border), StrokeKind::Inside);

    let mut row_y = sidebar.top() + EGUI_SIDEBAR_TOP_PX;
    for (index, section) in sections().iter().enumerate() {
        let row = EguiRect::from_min_size(
            Pos2::new(sidebar.left(), row_y),
            Vec2::new(sidebar.width(), EGUI_SIDEBAR_ROW_HEIGHT_PX),
        );
        let row_frame = row.shrink2(Vec2::new(8.0, 3.0));
        let active = index == selected;
        let hovered = hovered_menu == Some(index);
        painter.rect_filled(
            row_frame.translate(Vec2::new(1.0, 1.0)),
            5.0,
            Color32::from_rgba_unmultiplied(0, 0, 0, if active { 58 } else { 30 }),
        );
        painter.rect_filled(
            row_frame,
            5.0,
            if active {
                current
            } else if hovered {
                hover
            } else {
                Color32::from_rgb(18, 20, 30)
            },
        );
        painter.line_segment(
            [
                Pos2::new(row_frame.left() + 6.0, row_frame.top() + 0.5),
                Pos2::new(row_frame.right() - 6.0, row_frame.top() + 0.5),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 10)),
        );
        painter.line_segment(
            [
                Pos2::new(row_frame.left() + 6.0, row_frame.bottom() - 0.5),
                Pos2::new(row_frame.right() - 6.0, row_frame.bottom() - 0.5),
            ],
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(0, 0, 0, 56)),
        );
        if active {
            painter.rect_filled(
                EguiRect::from_min_size(
                    Pos2::new(row.left() + 8.0, row.top() + 6.0),
                    Vec2::new(3.0, row.height() - 12.0),
                ),
                2.0,
                egui_accent(section.accent),
            );
        }
        row_y += EGUI_SIDEBAR_ROW_HEIGHT_PX;
    }

    painter.line_segment(
        [
            Pos2::new(sidebar.left(), sidebar.bottom() - 54.0),
            Pos2::new(sidebar.right(), sidebar.bottom() - 54.0),
        ],
        Stroke::new(1.0, border),
    );
}

fn egui_shell_labels(
    rects: EguiShellRects,
    selected: usize,
    hovered_menu: Option<usize>,
    fps: f64,
) -> Vec<WebEguiLabel> {
    let header = egui_rect(rects.header);
    let sidebar = egui_rect(rects.sidebar);
    let footer = egui_rect(rects.footer);
    let mut labels = Vec::with_capacity(24);
    let text = web_color(Color::Rgb(192, 202, 245));
    let muted = web_color(Color::Rgb(116, 125, 156));
    let magenta = web_color(Color::Rgb(255, 79, 176));

    labels.push(WebEguiLabel::left(
        header.left() + 72.0,
        header.center().y,
        "BOOTTY".to_owned(),
        18.0,
        magenta,
    ));
    labels.push(WebEguiLabel::left(
        header.left() + 142.0,
        header.center().y,
        "bootty.org".to_owned(),
        14.0,
        text,
    ));
    labels.push(WebEguiLabel::right(
        header.right() - 18.0,
        header.center().y,
        format!("{fps:05.1} fps"),
        14.0,
        web_color(Color::Rgb(158, 206, 106)),
    ));

    let mut row_y = sidebar.top() + EGUI_SIDEBAR_TOP_PX;
    for (index, section) in sections().iter().enumerate() {
        let row = EguiRect::from_min_size(
            Pos2::new(sidebar.left(), row_y),
            Vec2::new(sidebar.width(), EGUI_SIDEBAR_ROW_HEIGHT_PX),
        );
        let color = if index == selected {
            web_color(section.accent)
        } else if hovered_menu == Some(index) {
            text
        } else {
            web_color(Color::Rgb(154, 163, 197))
        };
        labels.push(WebEguiLabel::left(
            row.left() + 22.0,
            row.center().y + 3.0,
            section.label.to_owned(),
            15.0,
            color,
        ));
        row_y += EGUI_SIDEBAR_ROW_HEIGHT_PX;
    }

    labels.push(WebEguiLabel::left(
        sidebar.left() + 14.0,
        sidebar.bottom() - 28.0,
        "Open source".to_owned(),
        12.5,
        muted,
    ));
    labels.push(WebEguiLabel::left(
        sidebar.left() + 14.0,
        sidebar.bottom() - 13.0,
        "github.com/majinboos/bootty".to_owned(),
        12.5,
        text,
    ));
    labels.push(WebEguiLabel::left(
        footer.left() + 2.0,
        footer.center().y,
        "Bootty".to_owned(),
        13.5,
        magenta,
    ));
    labels.push(WebEguiLabel::left(
        footer.left() + 72.0,
        footer.center().y,
        "native terminal UI for Rust apps".to_owned(),
        13.5,
        muted,
    ));
    labels
}

fn egui_shell_links(_rects: EguiShellRects) -> Vec<WebEguiLink> {
    Vec::new()
}

fn egui_rect(rect: WebRect) -> EguiRect {
    EguiRect::from_min_max(
        Pos2::new(rect.min_x, rect.min_y),
        Pos2::new(rect.max_x, rect.max_y),
    )
}

fn push_egui_icon(
    textures: &mut Vec<WebEguiTexture>,
    meshes: &mut Vec<WebEguiMesh>,
    header: WebRect,
) {
    let icon = site_icon();
    let id = "user:bootty-icon".to_owned();
    textures.push(WebEguiTexture {
        id: id.clone(),
        width: ICON_TEXTURE_SIZE,
        height: ICON_TEXTURE_SIZE,
        rgba: premultiplied_rgba(&icon.rgba),
    });

    let size = ICON_RENDER_SIZE as f32;
    let min_x = header.min_x + 16.0;
    let min_y = header.min_y + ((header.max_y - header.min_y - size) / 2.0).max(0.0);
    let max_x = min_x + size;
    let max_y = min_y + size;
    meshes.push(WebEguiMesh {
        texture_id: id,
        clip: header,
        vertices: vec![
            min_x, min_y, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, min_x, max_y, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0,
            max_x, max_y, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, max_x, min_y, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0,
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
    });
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn premultiplied_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|pixel| {
            let alpha = u16::from(pixel[3]);
            [
                ((u16::from(pixel[0]) * alpha) / 255) as u8,
                ((u16::from(pixel[1]) * alpha) / 255) as u8,
                ((u16::from(pixel[2]) * alpha) / 255) as u8,
                pixel[3],
            ]
        })
        .collect()
}

fn egui_texture(id: TextureId, image: ImageData) -> WebEguiTexture {
    let ImageData::Color(image) = image;
    WebEguiTexture {
        id: texture_id(id),
        width: image.width() as u32,
        height: image.height() as u32,
        rgba: image
            .pixels
            .iter()
            .flat_map(|pixel| [pixel.r(), pixel.g(), pixel.b(), pixel.a()])
            .collect(),
    }
}

fn texture_id(id: TextureId) -> String {
    match id {
        TextureId::Managed(id) => format!("managed:{id}"),
        TextureId::User(id) => format!("user:{id}"),
    }
}

fn egui_accent(color: Color) -> Color32 {
    let color = web_color(color);
    Color32::from_rgb(color.r, color.g, color.b)
}

fn site_icon() -> &'static IconImage {
    static ICON: LazyLock<IconImage> = LazyLock::new(decode_site_icon);
    &ICON
}

fn decode_site_icon() -> IconImage {
    let decoder = png::Decoder::new(Cursor::new(ICON_PNG));
    let mut reader = decoder.read_info().expect("bootty logo png header decodes");
    let output_size = reader
        .output_buffer_size()
        .expect("bootty logo png output size is known");
    let mut output = vec![0; output_size];
    let info = reader
        .next_frame(&mut output)
        .expect("bootty logo png frame decodes");
    let bytes = &output[..info.buffer_size()];
    let source = rgba_from_png(bytes, info.color_type);
    let mut rgba = vec![0; (ICON_TEXTURE_SIZE * ICON_TEXTURE_SIZE * 4) as usize];
    for y in 0..ICON_TEXTURE_SIZE {
        for x in 0..ICON_TEXTURE_SIZE {
            let src_x = x * info.width / ICON_TEXTURE_SIZE;
            let src_y = y * info.height / ICON_TEXTURE_SIZE;
            let src = ((src_y * info.width + src_x) * 4) as usize;
            let dst = ((y * ICON_TEXTURE_SIZE + x) * 4) as usize;
            rgba[dst..dst + 4].copy_from_slice(&source[src..src + 4]);
        }
    }
    IconImage { rgba }
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn rgba_from_png(bytes: &[u8], color_type: png::ColorType) -> Vec<u8> {
    match color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        png::ColorType::Grayscale => bytes
            .iter()
            .flat_map(|gray| [*gray, *gray, *gray, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|gray| [gray[0], gray[0], gray[0], gray[1]])
            .collect(),
        png::ColorType::Indexed => panic!("indexed bootty logo png is unsupported"),
    }
}

pub(crate) fn web_cell(x: u16, y: u16, cell: &Cell, osc8: Option<&str>) -> WebCell {
    WebCell {
        x,
        y,
        text: cell.symbol().to_owned(),
        fg: web_fg(cell.fg),
        bg: web_bg(cell.bg),
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

fn web_fg(color: Color) -> Option<WebColor> {
    match color {
        Color::Reset => None,
        _ => Some(web_color(color)),
    }
}

fn web_bg(color: Color) -> Option<WebColor> {
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

struct IconImage {
    rgba: Vec<u8>,
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

#[derive(Serialize)]
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

impl WebEguiLabel {
    fn left(x: f32, y: f32, text: String, size: f32, color: WebColor) -> Self {
        Self {
            x,
            y,
            text,
            size,
            color,
            align: "left",
        }
    }

    fn right(x: f32, y: f32, text: String, size: f32, color: WebColor) -> Self {
        Self {
            x,
            y,
            text,
            size,
            color,
            align: "right",
        }
    }
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
