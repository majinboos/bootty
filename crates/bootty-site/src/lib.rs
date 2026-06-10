use std::io::Cursor;
use std::sync::OnceLock;

use egui::epaint::{ImageData, Primitive};
use egui::{
    Color32, Context as EguiContext, LayerId, Order, Pos2, RawInput, Rect as EguiRect, Stroke,
    StrokeKind, TextureId, Vec2,
};
use serde::Serialize;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use tui_markdown::{Options as MarkdownOptions, StyleSheet, from_str_with_options};
use tuirealm::command::{Cmd, CmdResult, Direction};
use tuirealm::component::{AppComponent, Component};
use tuirealm::event::{Event, Key, KeyEvent, KeyModifiers, NoUserEvent};
use tuirealm::props::{AttrValue, Attribute, QueryResult};
use tuirealm::ratatui::Frame;
use tuirealm::ratatui::buffer::{Buffer, Cell};
use tuirealm::ratatui::layout::{Constraint, Direction as LayoutDirection, Layout, Rect, Size};
use tuirealm::ratatui::style::{Color, Modifier, Style};
use tuirealm::ratatui::text::{Line, Span, Text};
use tuirealm::ratatui::widgets::{Paragraph, Wrap};
use tuirealm::state::State;
use tuirealm::terminal::{TerminalAdapter, TestTerminalAdapter};
use wasm_bindgen::prelude::*;

const CELL_WIDTH: u32 = 9;
const CELL_HEIGHT: u32 = 18;
const DEFAULT_COLS: u16 = 96;
const DEFAULT_ROWS: u16 = 32;
const ICON_TEXTURE_SIZE: u32 = 96;
const ICON_RENDER_SIZE: u32 = 48;
const ICON_PNG: &[u8] = include_bytes!("../../bootty-tauri/src-ui/public/bootty-mascot.png");
const SECTION_DETAIL_STATIC_ROWS: u16 = 4;
const EGUI_SIDEBAR_TOP_PX: f32 = 18.0;
const EGUI_SIDEBAR_ROW_HEIGHT_PX: f32 = 36.0;
const EGUI_SIDEBAR_WIDTH_COLS: u16 = 32;
const EGUI_HEADER_ROWS: u16 = 4;
const EGUI_FOOTER_ROWS: u16 = 2;
const GITHUB_URL: &str = "https://github.com/majinboos/bootty";
const GITHUB_LINKS: &[SectionLink] = &[SectionLink {
    text: "GitHub",
    url: GITHUB_URL,
}];
const GETTING_STARTED_MARKDOWN: &str = r#"# Getting Started

Host loop: resize the terminal, write input, extract a frame, then render it
through the surface your app owns.

## Tauri command host

```rust
tauri::generate_handler![
    start_terminal,
    resize_terminal,
    write_terminal,
    terminal_frame,
];

#[tauri::command]
fn write_terminal(input: String, state: State<'_, AppState>) -> Result<(), String> {
    terminal(&state)?.write_input(input.as_bytes())?;
    Ok(())
}
```

## Winit/WGPU host

```rust
let viewport = BareTerminalViewport::new(
    width, height, cell_metrics, padding,
);

session.resize(viewport.geometry())?;
let frame = session.extract_frame()?;
let render_frame = terminal_render_frame_for_bare_host(
    &frame, viewport, &text_config,
);

renderer.prepare_terminal_frame(
    &device, &queue, &render_frame, scale,
);
```
"#;

#[derive(Clone, Copy)]
struct BoottyMarkdownStyle;

impl StyleSheet for BoottyMarkdownStyle {
    fn heading(&self, level: u8) -> Style {
        let color = match level {
            1 => Color::Green,
            2 => Color::Cyan,
            _ => Color::Magenta,
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }

    fn code(&self) -> Style {
        Style::default().fg(Color::Yellow).bg(Color::Black)
    }

    fn link(&self) -> Style {
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::UNDERLINED)
    }

    fn blockquote(&self) -> Style {
        Style::default().fg(Color::Green)
    }

    fn heading_meta(&self) -> Style {
        Style::default().fg(Color::DarkGray)
    }

    fn metadata_block(&self) -> Style {
        Style::default().fg(Color::Yellow)
    }
}

fn getting_started_text() -> Text<'static> {
    let mut text = from_str_with_options(
        GETTING_STARTED_MARKDOWN,
        &MarkdownOptions::new(BoottyMarkdownStyle),
    );
    highlight_code_fences(&mut text.lines);
    text
}

fn highlight_code_fences(lines: &mut [Line<'static>]) {
    let mut highlighter = None;
    for line in lines {
        let content = line_text(line);
        if let Some(fence_language) = content.strip_prefix("```") {
            let next_highlighter = if highlighter.is_some() {
                None
            } else {
                code_highlighter(fence_language)
            };
            *line = Line::default();
            highlighter = next_highlighter;
        } else if let Some(highlighter) = highlighter.as_mut() {
            *line = highlighted_code_line(&content, highlighter);
        }
    }
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn code_highlighter(language: &str) -> Option<HighlightLines<'static>> {
    let syntax = syntax_set().find_syntax_by_token(language)?;
    let theme = theme_set().themes.get("base16-ocean.dark")?;
    Some(HighlightLines::new(syntax, theme))
}

fn highlighted_code_line(line: &str, highlighter: &mut HighlightLines<'_>) -> Line<'static> {
    let Ok(ranges) = highlighter.highlight_line(line, syntax_set()) else {
        return Line::from(Span::styled(
            line.to_owned(),
            Style::default().fg(Color::Yellow).bg(Color::Black),
        ));
    };
    Line::from(
        ranges
            .into_iter()
            .map(|(style, text)| Span::styled(text.to_owned(), syntect_style(style)))
            .collect::<Vec<_>>(),
    )
}

fn syntect_style(style: SyntectStyle) -> Style {
    let mut ratatui_style = Style::default()
        .fg(Color::Rgb(
            style.foreground.r,
            style.foreground.g,
            style.foreground.b,
        ))
        .bg(Color::Black);
    if style.font_style.contains(FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }
    ratatui_style
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

impl Default for SiteBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn new_egui_context() -> EguiContext {
    EguiContext::default()
}

impl SiteBackend {
    fn handle_event(&mut self, event: Event<NoUserEvent>) -> Result<(), JsValue> {
        if self.focus == Focus::Detail
            && sections()[self.selected].plain_label == "Shell"
            && self.handle_demo_event(&event)
        {
            return Ok(());
        }
        for msg in self.forward_event(&event) {
            self.update(msg)?;
        }
        Ok(())
    }

    fn handle_mouse(&mut self, kind: &str, x: u16, y: u16, button: i16) -> Result<(), JsValue> {
        if kind == "leave" {
            self.hovered_menu = None;
            return Ok(());
        }

        match hit_target(self.cols, self.rows, x, y) {
            Some(HitTarget::Detail) if kind == "wheel" => {
                self.hovered_menu = None;
                self.update(Msg::Focus(Focus::Detail))?;
                self.update(Msg::Scroll(isize::from(button)))?;
            }
            Some(HitTarget::Menu(index)) => {
                let index = index.min(sections().len() - 1);
                self.hovered_menu = Some(index);
                if kind == "down" {
                    self.selected = index;
                    self.detail_scroll = 0;
                    self.focus = Focus::Menu;
                    self.menu.attr(Attribute::Focus, AttrValue::Flag(true));
                    self.detail.attr(Attribute::Focus, AttrValue::Flag(false));
                    self.update(Msg::ToggleFocus)?;
                }
            }
            Some(HitTarget::Detail) if kind == "down" => {
                self.hovered_menu = None;
                self.update(Msg::Focus(Focus::Detail))?;
            }
            _ => {
                self.hovered_menu = None;
            }
        }
        Ok(())
    }

    fn handle_demo_event(&mut self, event: &Event<NoUserEvent>) -> bool {
        match event {
            Event::Keyboard(KeyEvent {
                code: Key::Char(ch),
                ..
            }) => {
                self.demo_input.push(*ch);
                true
            }
            Event::Keyboard(KeyEvent {
                code: Key::Backspace,
                ..
            }) => {
                self.demo_input.pop();
                true
            }
            Event::Keyboard(KeyEvent {
                code: Key::Enter, ..
            }) => {
                self.run_demo_command();
                true
            }
            Event::Keyboard(KeyEvent {
                code: Key::Esc | Key::Left | Key::BackTab,
                ..
            }) => false,
            _ => true,
        }
    }

    fn run_demo_command(&mut self) {
        let command = self.demo_input.trim().to_owned();
        self.demo_lines.push(format!("❯ {command}"));
        self.demo_input.clear();
        match command.as_str() {
            "" => {}
            "help" => self.demo_lines.extend([
                "help         show commands".to_owned(),
                "ls           list the demo workspace".to_owned(),
                "cat README.md  print the Bootty summary".to_owned(),
                "clear        reset the terminal pane".to_owned(),
            ]),
            "ls" => self.demo_lines.extend([
                "README.md".to_owned(),
                "architecture.md".to_owned(),
                "demo.txt".to_owned(),
            ]),
            "cat README.md" => self.demo_lines.extend([
                "Bootty is a native GPU-rendered terminal and reusable terminal crate set."
                    .to_owned(),
                "The desktop app, bare WGPU host, and tabs example share renderer paths."
                    .to_owned(),
            ]),
            "cat demo.txt" => self.demo_lines.extend([
                "Bootty keeps terminal input, output, and rendering in one frame.".to_owned(),
                "Keyboard and pointer events stay attached to the active panel.".to_owned(),
            ]),
            "clear" => self.demo_lines.clear(),
            other => self.demo_lines.push(format!("unknown command: {other}")),
        }
        if self.demo_lines.len() > 18 {
            let drain = self.demo_lines.len() - 18;
            self.demo_lines.drain(0..drain);
        }
    }

    fn forward_event(&mut self, event: &Event<NoUserEvent>) -> Vec<Msg> {
        let msg = match self.focus {
            Focus::Menu => self.menu.on(event),
            Focus::Detail => self.detail.on(event),
        };
        msg.into_iter().collect()
    }

    fn update(&mut self, msg: Msg) -> Result<(), JsValue> {
        match msg {
            Msg::Move(delta) => {
                self.selected = wrap(self.selected as isize + delta, sections().len());
                self.hovered_menu = None;
                self.detail_scroll = 0;
            }
            Msg::Focus(focus) => {
                self.focus = focus;
                self.menu
                    .attr(Attribute::Focus, AttrValue::Flag(focus == Focus::Menu));
                self.detail
                    .attr(Attribute::Focus, AttrValue::Flag(focus == Focus::Detail));
            }
            Msg::ToggleFocus => {
                self.update(Msg::Focus(match self.focus {
                    Focus::Menu => Focus::Detail,
                    Focus::Detail => Focus::Menu,
                }))?;
            }
            Msg::Scroll(delta) => {
                self.detail_scroll = if delta == isize::MIN {
                    0
                } else if delta == isize::MAX {
                    u16::MAX
                } else {
                    self.detail_scroll.saturating_add_signed(delta as i16)
                };
            }
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum Msg {
    Move(isize),
    Focus(Focus),
    ToggleFocus,
    Scroll(isize),
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Focus {
    Menu,
    Detail,
}

#[derive(Clone, Copy)]
struct Section {
    icon: SectionIcon,
    label: &'static str,
    plain_label: &'static str,
    title: &'static str,
    accent: Color,
    lines: &'static [&'static str],
    links: &'static [SectionLink],
}

#[derive(Clone, Copy)]
enum SectionIcon {
    Frame,
    Guide,
    Doom,
    Renderer,
    Palette,
    Runtime,
    Shell,
    App,
    Github,
}

impl SectionIcon {
    fn glyph(self) -> &'static str {
        match self {
            SectionIcon::Frame => "\u{f2d0}",
            SectionIcon::Guide => "\u{f02d}",
            SectionIcon::Doom => "\u{f11b}",
            SectionIcon::Renderer => "\u{f03e}",
            SectionIcon::Palette => "\u{f1fc}",
            SectionIcon::Runtime => "\u{f017}",
            SectionIcon::Shell => "\u{f120}",
            SectionIcon::App => "\u{f108}",
            SectionIcon::Github => "\u{f09b}",
        }
    }
}

#[derive(Clone, Copy)]
struct SectionLink {
    text: &'static str,
    url: &'static str,
}

struct SiteViewState<'a> {
    selected: usize,
    focus: Focus,
    detail_scroll: u16,
    demo_lines: &'a [String],
    demo_input: &'a str,
}

#[derive(Default)]
struct Menu {
    selected: usize,
    focused: bool,
}

impl Component for Menu {
    fn view(&mut self, _frame: &mut Frame<'_>, _area: Rect) {}

    fn query<'a>(&'a self, _attr: Attribute) -> Option<QueryResult<'a>> {
        None
    }

    fn attr(&mut self, attr: Attribute, value: AttrValue) {
        match (attr, value) {
            (Attribute::Value, AttrValue::Number(value)) => self.selected = value.max(0) as usize,
            (Attribute::Focus, AttrValue::Flag(focus)) => self.focused = focus,
            _ => {}
        }
    }

    fn state(&self) -> State {
        State::None
    }

    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        match cmd {
            Cmd::Move(Direction::Down | Direction::Up) => CmdResult::Custom("move", State::None),
            Cmd::Submit => CmdResult::Submit(State::None),
            _ => CmdResult::Invalid(cmd),
        }
    }
}

impl AppComponent<Msg, NoUserEvent> for Menu {
    fn on(&mut self, ev: &Event<NoUserEvent>) -> Option<Msg> {
        match ev {
            Event::Keyboard(KeyEvent {
                code: Key::Down | Key::Char('j'),
                ..
            }) => {
                let _ = self.perform(Cmd::Move(Direction::Down));
                Some(Msg::Move(1))
            }
            Event::Keyboard(KeyEvent {
                code: Key::Up | Key::Char('k'),
                ..
            }) => {
                let _ = self.perform(Cmd::Move(Direction::Up));
                Some(Msg::Move(-1))
            }
            Event::Keyboard(KeyEvent {
                code: Key::Tab | Key::Right | Key::Char('l'),
                ..
            }) => Some(Msg::Focus(Focus::Detail)),
            Event::Keyboard(KeyEvent {
                code: Key::Enter, ..
            }) => Some(Msg::ToggleFocus),
            _ => None,
        }
    }
}

#[derive(Default)]
struct Detail {
    section: usize,
    focused: bool,
    scroll: u16,
    demo_lines: Vec<String>,
    demo_input: String,
}

impl Component for Detail {
    fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let section = sections()[self.section.min(sections().len() - 1)];
        if section.plain_label == "Getting Started" {
            let text = getting_started_text();
            let max_scroll = max_scroll(text.lines.len(), area.height);
            self.scroll = self.scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(text)
                    .scroll((self.scroll, 0))
                    .wrap(Wrap { trim: false }),
                area,
            );
            return;
        }
        if section.plain_label == "Palette" {
            let lines = palette_demo_lines(section);
            let max_scroll = max_scroll(lines.len(), area.height);
            self.scroll = self.scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(lines)
                    .scroll((self.scroll, 0))
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }
        let mut lines = if section.plain_label == "Shell" {
            vec![
                Line::from(Span::styled(
                    "Demo shell",
                    Style::default()
                        .fg(section.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ]
        } else {
            vec![
                Line::from(Span::styled(
                    section.title,
                    Style::default()
                        .fg(section.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ]
        };
        if section.plain_label == "Shell" {
            lines.extend(self.demo_lines.iter().map(|line| {
                Line::from(Span::styled(line.clone(), Style::default().fg(Color::Gray)))
            }));
            lines.push(Line::from(vec![
                Span::styled(
                    "> ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(self.demo_input.clone(), Style::default().fg(Color::White)),
                Span::styled("_", Style::default().fg(Color::Magenta)),
            ]));
        } else {
            lines.extend(section.lines.iter().map(|line| Line::from(*line)));
            lines.extend(section.links.iter().map(|link| {
                Line::from(Span::styled(
                    link.text,
                    Style::default()
                        .fg(section.accent)
                        .add_modifier(Modifier::UNDERLINED),
                ))
            }));
        }
        let max_scroll = max_scroll(lines.len(), area.height);
        self.scroll = self.scroll.min(max_scroll);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn query<'a>(&'a self, _attr: Attribute) -> Option<QueryResult<'a>> {
        None
    }

    fn attr(&mut self, attr: Attribute, value: AttrValue) {
        match (attr, value) {
            (Attribute::Value, AttrValue::Number(value)) => self.section = value.max(0) as usize,
            (Attribute::Focus, AttrValue::Flag(focus)) => self.focused = focus,
            _ => {}
        }
    }

    fn state(&self) -> State {
        State::None
    }

    fn perform(&mut self, cmd: Cmd) -> CmdResult {
        match cmd {
            Cmd::Submit => CmdResult::Submit(State::None),
            _ => CmdResult::Invalid(cmd),
        }
    }
}

impl AppComponent<Msg, NoUserEvent> for Detail {
    fn on(&mut self, ev: &Event<NoUserEvent>) -> Option<Msg> {
        match ev {
            Event::Keyboard(KeyEvent {
                code: Key::Left | Key::Esc | Key::BackTab | Key::Char('h'),
                ..
            }) => Some(Msg::Focus(Focus::Menu)),
            Event::Keyboard(KeyEvent {
                code: Key::Down | Key::Char('j'),
                ..
            }) => Some(Msg::Scroll(1)),
            Event::Keyboard(KeyEvent {
                code: Key::Up | Key::Char('k'),
                ..
            }) => Some(Msg::Scroll(-1)),
            Event::Keyboard(KeyEvent {
                code: Key::PageDown,
                ..
            }) => Some(Msg::Scroll(8)),
            Event::Keyboard(KeyEvent {
                code: Key::PageUp, ..
            }) => Some(Msg::Scroll(-8)),
            Event::Keyboard(KeyEvent {
                code: Key::Home, ..
            }) => Some(Msg::Scroll(isize::MIN)),
            Event::Keyboard(KeyEvent { code: Key::End, .. }) => Some(Msg::Scroll(isize::MAX)),
            Event::Keyboard(KeyEvent {
                code: Key::Enter, ..
            }) => Some(Msg::ToggleFocus),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DemoRgb {
    r: u8,
    g: u8,
    b: u8,
}

impl DemoRgb {
    const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    fn color(self) -> Color {
        Color::Rgb(self.r, self.g, self.b)
    }
}

#[derive(Clone, Copy)]
struct DemoLab {
    l: f32,
    a: f32,
    b: f32,
}

impl DemoLab {
    fn from_rgb(rgb: DemoRgb) -> Self {
        let red = srgb_to_linear(f32::from(rgb.r) / 255.0);
        let green = srgb_to_linear(f32::from(rgb.g) / 255.0);
        let blue = srgb_to_linear(f32::from(rgb.b) / 255.0);

        let x = xyz_to_lab_curve(
            (red * 0.412_456_4 + green * 0.357_576_1 + blue * 0.180_437_5) / 0.950_47,
        );
        let y = xyz_to_lab_curve(red * 0.212_672_9 + green * 0.715_152_2 + blue * 0.072_175);
        let z = xyz_to_lab_curve(
            (red * 0.019_333_9 + green * 0.119_192 + blue * 0.950_304_1) / 1.088_83,
        );

        Self {
            l: 116.0 * y - 16.0,
            a: 500.0 * (x - y),
            b: 200.0 * (y - z),
        }
    }

    fn to_rgb(self) -> DemoRgb {
        let y = (self.l + 16.0) / 116.0;
        let x = self.a / 500.0 + y;
        let z = y - self.b / 200.0;

        let x = lab_to_xyz_curve(x, x * x * x) * 0.950_47;
        let y = lab_to_xyz_curve(y, y * y * y);
        let z = lab_to_xyz_curve(z, z * z * z) * 1.088_83;

        DemoRgb {
            r: linear_to_srgb_byte(x * 3.240_454_2 - y * 1.537_138_5 - z * 0.498_531_4),
            g: linear_to_srgb_byte(-x * 0.969_266 + y * 1.876_010_8 + z * 0.041_556),
            b: linear_to_srgb_byte(x * 0.055_643_4 - y * 0.204_025_9 + z * 1.057_225_2),
        }
    }

    fn lerp(t: f32, a: Self, b: Self) -> Self {
        Self {
            l: a.l + t * (b.l - a.l),
            a: a.a + t * (b.a - a.a),
            b: a.b + t * (b.b - a.b),
        }
    }
}

fn palette_demo_lines(section: Section) -> Vec<Line<'static>> {
    let palette = harmonious_demo_palette();
    let mut lines = vec![Line::from(Span::styled(
        section.title,
        Style::default()
            .fg(section.accent)
            .add_modifier(Modifier::BOLD),
    ))];
    for row in 0..12 {
        lines.push(palette_board_row(row, &palette));
    }
    lines.extend([
        palette_spacer_row(),
        palette_default_row(&palette),
        palette_ansi_row(&palette, 0),
        palette_ansi_row(&palette, 8),
    ]);
    lines
}

fn palette_board_row(row: usize, palette: &[DemoRgb; 256]) -> Line<'static> {
    let red_start = if row < 6 { 0 } else { 3 };
    let green = 5 - row % 6;
    let mut spans = vec![palette_edge_cell(
        (15usize.saturating_sub(row)) & 0xf,
        row < 6,
    )];
    for red in red_start..red_start + 3 {
        for blue in 0..6 {
            let index = 16 + red * 36 + green * 6 + blue;
            spans.push(palette_board_cell(index, palette[index]));
        }
        spans.push(Span::raw(" "));
    }
    spans.push(palette_edge_cell((row + 3) & 0xf, row < 6));
    if row < 8 {
        spans.push(Span::raw("   "));
        spans.push(palette_legend_cell(row, palette[row]));
        spans.push(palette_legend_cell(row + 8, palette[row + 8]));
    }
    Line::from(spans)
}

fn palette_default_row(palette: &[DemoRgb; 256]) -> Line<'static> {
    let mut spans = Vec::with_capacity(30);
    for (index, color) in palette.iter().enumerate().take(238).skip(232) {
        spans.push(palette_gray_cell(index, *color));
    }
    spans.push(Span::styled(
        "  Default  ",
        Style::default().fg(Color::White),
    ));
    for (index, color) in palette.iter().enumerate().take(256).skip(250) {
        spans.push(palette_gray_cell(index, *color));
    }
    spans.push(Span::raw("   "));
    for (index, color) in palette.iter().enumerate().take(250).skip(238) {
        spans.push(palette_gray_cell(index, *color));
    }
    Line::from(spans)
}

fn palette_ansi_row(palette: &[DemoRgb; 256], start: usize) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    for (index, color) in palette.iter().enumerate().skip(start).take(8) {
        spans.push(palette_legend_cell(index, *color));
    }
    Line::from(spans)
}

fn palette_spacer_row() -> Line<'static> {
    Line::from("")
}

fn palette_board_cell(index: usize, color: DemoRgb) -> Span<'static> {
    let glyph = format!("{:x} ", index & 0xf);
    if color_luma(color) < 75_000 {
        Span::styled(glyph, Style::default().fg(color.color()))
    } else {
        Span::styled(
            glyph,
            Style::default()
                .fg(readable_palette_text(color))
                .bg(color.color()),
        )
    }
}

fn palette_edge_cell(value: usize, filled: bool) -> Span<'static> {
    let color = DemoRgb::new(176, 181, 196);
    if filled {
        Span::styled(
            format!("{value:x} "),
            Style::default().fg(Color::Black).bg(color.color()),
        )
    } else {
        Span::styled(format!("{value:x} "), Style::default().fg(color.color()))
    }
}

fn palette_legend_cell(index: usize, color: DemoRgb) -> Span<'static> {
    Span::styled(
        format!("{:x} ", index & 0xf),
        Style::default()
            .fg(readable_palette_text(color))
            .bg(color.color()),
    )
}

fn palette_gray_cell(index: usize, color: DemoRgb) -> Span<'static> {
    palette_legend_cell(index, color)
}

fn readable_palette_text(color: DemoRgb) -> Color {
    if color_luma(color) > 140_000 {
        Color::Black
    } else {
        Color::White
    }
}

fn color_luma(color: DemoRgb) -> u32 {
    u32::from(color.r) * 299 + u32::from(color.g) * 587 + u32::from(color.b) * 114
}

fn harmonious_demo_palette() -> [DemoRgb; 256] {
    let mut palette = [DemoRgb::new(0, 0, 0); 256];
    for (index, color) in demo_base16().into_iter().enumerate() {
        palette[index] = color;
    }
    let anchors = [
        DemoLab::from_rgb(DEMO_BACKGROUND),
        DemoLab::from_rgb(palette[1]),
        DemoLab::from_rgb(palette[2]),
        DemoLab::from_rgb(palette[3]),
        DemoLab::from_rgb(palette[4]),
        DemoLab::from_rgb(palette[5]),
        DemoLab::from_rgb(palette[6]),
        DemoLab::from_rgb(DEMO_FOREGROUND),
    ];

    let mut index = 16;
    for red in 0..6 {
        let c0 = DemoLab::lerp(red as f32 / 5.0, anchors[0], anchors[1]);
        let c1 = DemoLab::lerp(red as f32 / 5.0, anchors[2], anchors[3]);
        let c2 = DemoLab::lerp(red as f32 / 5.0, anchors[4], anchors[5]);
        let c3 = DemoLab::lerp(red as f32 / 5.0, anchors[6], anchors[7]);
        for green in 0..6 {
            let c4 = DemoLab::lerp(green as f32 / 5.0, c0, c1);
            let c5 = DemoLab::lerp(green as f32 / 5.0, c2, c3);
            for blue in 0..6 {
                palette[index] = DemoLab::lerp(blue as f32 / 5.0, c4, c5).to_rgb();
                index += 1;
            }
        }
    }

    for step in 0..24 {
        palette[index] = DemoLab::lerp((step + 1) as f32 / 25.0, anchors[0], anchors[7]).to_rgb();
        index += 1;
    }

    palette
}

const DEMO_BACKGROUND: DemoRgb = DemoRgb::new(17, 18, 26);
const DEMO_FOREGROUND: DemoRgb = DemoRgb::new(192, 202, 245);

fn demo_base16() -> [DemoRgb; 16] {
    [
        DEMO_BACKGROUND,
        DemoRgb::new(247, 118, 142),
        DemoRgb::new(158, 206, 106),
        DemoRgb::new(224, 175, 104),
        DemoRgb::new(122, 162, 247),
        DemoRgb::new(255, 79, 176),
        DemoRgb::new(125, 207, 255),
        DemoRgb::new(169, 177, 214),
        DemoRgb::new(86, 95, 137),
        DemoRgb::new(255, 139, 161),
        DemoRgb::new(177, 214, 123),
        DemoRgb::new(239, 196, 126),
        DemoRgb::new(151, 184, 255),
        DemoRgb::new(255, 122, 201),
        DemoRgb::new(154, 220, 255),
        DEMO_FOREGROUND,
    ]
}

fn srgb_to_linear(value: f32) -> f32 {
    if value > 0.040_45 {
        ((value + 0.055) / 1.055).powf(2.4)
    } else {
        value / 12.92
    }
}

fn xyz_to_lab_curve(value: f32) -> f32 {
    if value > 0.008_856 {
        value.cbrt()
    } else {
        7.787 * value + 16.0 / 116.0
    }
}

fn lab_to_xyz_curve(value: f32, cubed: f32) -> f32 {
    if cubed > 0.008_856 {
        cubed
    } else {
        (value - 16.0 / 116.0) / 7.787
    }
}

fn linear_to_srgb_byte(value: f32) -> u8 {
    let srgb = if value > 0.003_130_8 {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    } else {
        12.92 * value
    };
    (srgb.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn draw_site(
    frame: &mut Frame<'_>,
    menu_component: &mut Menu,
    detail_component: &mut Detail,
    state: SiteViewState<'_>,
) {
    let area = frame.area();
    let detail = site_layout(area.width, area.height).detail;

    detail_component.attr(Attribute::Value, AttrValue::Number(state.selected as isize));
    detail_component.attr(
        Attribute::Focus,
        AttrValue::Flag(state.focus == Focus::Detail),
    );
    detail_component.scroll = state.detail_scroll;
    detail_component.demo_lines = state.demo_lines.to_vec();
    detail_component.demo_input = state.demo_input.to_owned();
    let _ = menu_component;
    detail_component.view(frame, detail);
}

fn site_layout(cols: u16, rows: u16) -> SiteLayout {
    let area = Rect::new(0, 0, cols, rows);
    let [header, body, footer] = Layout::default()
        .direction(LayoutDirection::Vertical)
        .constraints([
            Constraint::Length(EGUI_HEADER_ROWS),
            Constraint::Min(8),
            Constraint::Length(EGUI_FOOTER_ROWS),
        ])
        .areas(area);
    let narrow = body.width < 78;
    let menu_height = egui_sidebar_rows()
        .min(body.height.saturating_sub(8))
        .max(egui_sidebar_rows());
    let [menu, detail] = Layout::default()
        .direction(if narrow {
            LayoutDirection::Vertical
        } else {
            LayoutDirection::Horizontal
        })
        .constraints(if narrow {
            [Constraint::Length(menu_height), Constraint::Min(8)]
        } else {
            [
                Constraint::Length(EGUI_SIDEBAR_WIDTH_COLS),
                Constraint::Min(24),
            ]
        })
        .areas(body);

    SiteLayout {
        header,
        menu,
        detail: inset(detail, 2, 1),
        footer,
    }
}

fn egui_sidebar_rows() -> u16 {
    ((EGUI_SIDEBAR_TOP_PX + sections().len() as f32 * EGUI_SIDEBAR_ROW_HEIGHT_PX)
        / CELL_HEIGHT as f32)
        .ceil() as u16
}

fn sections() -> &'static [Section] {
    &[
        Section {
            icon: SectionIcon::Frame,
            label: "Frame",
            plain_label: "Frame",
            title: "A terminal UI toolkit with a real renderer.",
            accent: Color::Magenta,
            lines: &[
                "Cells, images, cursor state, selections, and links travel in one frame.",
                "Hosts render the frame directly instead of scraping terminal output.",
                "",
                "Use it for:",
                "  - native Rust tools with terminal ergonomics",
                "  - shell surfaces embedded in app chrome",
                "  - demos where text and pixels need to compose cleanly",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Guide,
            label: "Getting Started",
            plain_label: "Getting Started",
            title: "Build a host around Bootty frames.",
            accent: Color::Green,
            lines: &[],
            links: &[],
        },
        Section {
            icon: SectionIcon::Doom,
            label: "DOOM",
            plain_label: "DOOM",
            title: "Play DOOM in the terminal surface.",
            accent: Color::Red,
            lines: &[
                "Click the panel and play with WASD or arrows.",
                "Fire with F or Control. Use Space for doors and switches.",
                "Number keys 1-7 switch weapons.",
                "",
                "The game is a texture layer in the same frame as the terminal UI.",
                "Keyboard focus stays with the frame so gameplay and shell input share routing.",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Renderer,
            label: "Renderer",
            plain_label: "Renderer",
            title: "GPU rendering for terminal UI.",
            accent: Color::Blue,
            lines: &[
                "Glyphs, box drawing, images, cursor shapes, and decorations batch together.",
                "Terminal geometry remains explicit: cells, pixels, scale, and padding are separate.",
                "",
                "Renderer path:",
                "  frame -> paint plan -> glyph atlas -> image layers -> swapchain",
                "",
                "The same drawing contract drives the desktop app, bare host, and site demo.",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Palette,
            label: "Palette",
            plain_label: "Palette",
            title: "Harmonious generated 256-color palette.",
            accent: Color::Cyan,
            lines: &[
                "The extended ANSI palette is generated from the active base16 theme.",
                "The cube and grayscale ramp interpolate in CIELAB so related colors keep apparent brightness.",
                "Harmonious mode preserves foreground/background semantics for light and dark themes.",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Runtime,
            label: "Runtime",
            plain_label: "Runtime",
            title: "A runtime built for responsive terminals.",
            accent: Color::Green,
            lines: &[
                "PTY draining, resize handling, frame publication, and input encoding stay explicit.",
                "The UI consumes frames instead of parsing shell output on the render path.",
                "",
                "Runtime responsibilities:",
                "  - coalesce resize and repaint work",
                "  - publish frames without blocking input",
                "  - keep backpressure visible to the host",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Shell,
            label: "Shell",
            plain_label: "Shell",
            title: "Demo shell.",
            accent: Color::Yellow,
            lines: &["Type `help`, `frame`, `renderer`, `runtime`, or `clear`."],
            links: &[],
        },
        Section {
            icon: SectionIcon::App,
            label: "App",
            plain_label: "App",
            title: "The desktop app uses the same pieces.",
            accent: Color::Cyan,
            lines: &[
                "Bootty ships a native terminal shell with sidebar chrome and shared WGPU rendering.",
                "Examples cover the bare host and egui-tab integration paths.",
                "",
                "App surfaces:",
                "  - full desktop shell",
                "  - bare WGPU host",
                "  - egui tab integration",
                "  - browser-backed docs and demos",
            ],
            links: &[],
        },
        Section {
            icon: SectionIcon::Github,
            label: "GitHub",
            plain_label: "GitHub",
            title: "Source",
            accent: Color::Red,
            lines: &["Read the source on:"],
            links: GITHUB_LINKS,
        },
    ]
}

fn parse_input(input: &str) -> Vec<Event<NoUserEvent>> {
    let mut events = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        let event = if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            match chars.next() {
                Some('A') => key(Key::Up),
                Some('B') => key(Key::Down),
                Some('C') => key(Key::Right),
                Some('D') => key(Key::Left),
                Some('F') => key(Key::End),
                Some('H') => key(Key::Home),
                Some('5') if chars.next() == Some('~') => key(Key::PageUp),
                Some('6') if chars.next() == Some('~') => key(Key::PageDown),
                _ => key(Key::Esc),
            }
        } else {
            match ch {
                '\u{1b}' => key(Key::Esc),
                '\r' | '\n' => key(Key::Enter),
                '\t' => key(Key::Tab),
                '\u{7f}' | '\u{8}' => key(Key::Backspace),
                _ => key(Key::Char(ch)),
            }
        };
        events.push(event);
    }
    events
}

fn key(code: Key) -> Event<NoUserEvent> {
    Event::Keyboard(KeyEvent {
        code,
        modifiers: KeyModifiers::empty(),
    })
}

#[derive(Clone, Copy)]
struct WebFrameState {
    selected: usize,
    hovered_menu: Option<usize>,
    tick: u64,
    focus: Focus,
    fps: f64,
}

#[derive(Clone, Copy)]
struct EguiShellRects {
    shell: WebRect,
    header: WebRect,
    sidebar: WebRect,
    footer: WebRect,
}

fn web_frame(egui: &EguiContext, buffer: &Buffer, state: WebFrameState) -> WebTerminalFrame {
    let mut cells = Vec::with_capacity(buffer.content.len());
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            cells.push(web_cell(
                x,
                y,
                cell,
                osc8_link_at(buffer.area.width, buffer.area.height, state.selected, x, y),
            ));
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
        cells,
        images: Vec::new(),
        egui: Some(egui_shell_frame(
            egui,
            buffer.area.width,
            buffer.area.height,
            state,
        )),
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
    let mut textures = output
        .textures_delta
        .set
        .into_iter()
        .map(|(id, delta)| egui_texture(id, delta.image))
        .collect::<Vec<_>>();
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
        labels.push(WebEguiLabel::center(
            row.left() + 32.0,
            row.center().y + 3.0,
            section.icon.glyph().to_owned(),
            18.0,
            color,
        ));
        labels.push(WebEguiLabel::left(
            row.left() + 58.0,
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

fn egui_shell_links(rects: EguiShellRects) -> Vec<WebEguiLink> {
    let sidebar = egui_rect(rects.sidebar);
    let mut links = Vec::new();
    let mut row_y = sidebar.top() + EGUI_SIDEBAR_TOP_PX;
    for section in sections() {
        let row = EguiRect::from_min_size(
            Pos2::new(sidebar.left(), row_y),
            Vec2::new(sidebar.width(), EGUI_SIDEBAR_ROW_HEIGHT_PX),
        );
        if section.plain_label == "GitHub" {
            links.push(WebEguiLink {
                rect: WebRect {
                    min_x: row.left(),
                    min_y: row.top(),
                    max_x: row.right(),
                    max_y: row.bottom(),
                },
                url: GITHUB_URL,
            });
        }
        row_y += EGUI_SIDEBAR_ROW_HEIGHT_PX;
    }
    links
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
    static ICON: OnceLock<IconImage> = OnceLock::new();
    ICON.get_or_init(decode_site_icon)
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

fn osc8_link_at(cols: u16, rows: u16, selected: usize, x: u16, y: u16) -> Option<&'static str> {
    let section = sections().get(selected)?;
    let layout = site_layout(cols, rows);
    let content = inset(layout.detail, 1, 1);
    let link_y = content
        .y
        .saturating_add(SECTION_DETAIL_STATIC_ROWS)
        .saturating_add(section.lines.len() as u16);

    section.links.iter().enumerate().find_map(|(index, link)| {
        let row = link_y.saturating_add(index as u16);
        let end_x = content.x.saturating_add(link.text.chars().count() as u16);
        (y == row && x >= content.x && x < end_x).then_some(link.url)
    })
}

fn web_cell(x: u16, y: u16, cell: &Cell, osc8: Option<&str>) -> WebCell {
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

fn inset(area: Rect, horizontal: u16, vertical: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(horizontal),
        y: area.y.saturating_add(vertical),
        width: area.width.saturating_sub(horizontal.saturating_mul(2)),
        height: area.height.saturating_sub(vertical.saturating_mul(2)),
    }
}

fn max_scroll(line_count: usize, area_height: u16) -> u16 {
    let content_height = area_height as usize;
    line_count.saturating_sub(content_height) as u16
}

#[derive(Clone, Copy)]
struct SiteLayout {
    header: Rect,
    menu: Rect,
    detail: Rect,
    footer: Rect,
}

#[derive(Debug, Eq, PartialEq)]
enum HitTarget {
    Menu(usize),
    Detail,
}

fn hit_target(cols: u16, rows: u16, x: u16, y: u16) -> Option<HitTarget> {
    let layout = site_layout(cols, rows);
    if contains(layout.detail, x, y) {
        return Some(HitTarget::Detail);
    }

    if !contains(layout.menu, x, y) {
        return None;
    }

    let pointer_y = f32::from(y) * CELL_HEIGHT as f32;
    let first_row_y = f32::from(layout.menu.y) * CELL_HEIGHT as f32 + EGUI_SIDEBAR_TOP_PX;
    if pointer_y < first_row_y {
        return None;
    }
    let index = ((pointer_y - first_row_y) / EGUI_SIDEBAR_ROW_HEIGHT_PX) as usize;
    (index < sections().len()).then_some(HitTarget::Menu(index))
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}

fn wrap(value: isize, len: usize) -> usize {
    value.rem_euclid(len as isize) as usize
}

struct IconImage {
    rgba: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebTerminalFrame {
    selected: usize,
    focus: &'static str,
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
    colors: WebFrameColors,
    cursor: Option<WebCursor>,
    cells: Vec<WebCell>,
    images: Vec<WebImage>,
    egui: Option<WebEguiFrame>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebFrameColors {
    background: WebColor,
    foreground: WebColor,
    cursor: Option<WebColor>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebColor {
    r: u8,
    g: u8,
    b: u8,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebCursor {
    x: u16,
    y: u16,
    color: Option<WebColor>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebImage {
    key: String,
    layer: String,
    image_width: u32,
    image_height: u32,
    source: WebRect,
    destination: WebRect,
    rgba: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRect {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEguiFrame {
    textures: Vec<WebEguiTexture>,
    meshes: Vec<WebEguiMesh>,
    labels: Vec<WebEguiLabel>,
    links: Vec<WebEguiLink>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEguiLabel {
    x: f32,
    y: f32,
    text: String,
    size: f32,
    color: WebColor,
    align: &'static str,
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

    fn center(x: f32, y: f32, text: String, size: f32, color: WebColor) -> Self {
        Self {
            x,
            y,
            text,
            size,
            color,
            align: "center",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEguiLink {
    rect: WebRect,
    url: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEguiTexture {
    id: String,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebEguiMesh {
    texture_id: String,
    clip: WebRect,
    vertices: Vec<f32>,
    indices: Vec<u32>,
}
#[wasm_bindgen]
pub struct SiteBackend {
    egui: EguiContext,
    menu: Menu,
    detail: Detail,
    terminal: Option<TestTerminalAdapter>,
    selected: usize,
    hovered_menu: Option<usize>,
    focus: Focus,
    detail_scroll: u16,
    tick: u64,
    fps: f64,
    demo_input: String,
    demo_lines: Vec<String>,
    cols: u16,
    rows: u16,
}

#[wasm_bindgen]
impl SiteBackend {
    #[must_use]
    pub fn new() -> Self {
        let mut menu = Menu::default();
        let mut detail = Detail::default();
        menu.attr(Attribute::Focus, AttrValue::Flag(true));
        detail.attr(Attribute::Focus, AttrValue::Flag(false));

        Self {
            egui: new_egui_context(),
            menu,
            detail,
            terminal: Some(
                TestTerminalAdapter::new(Size::new(DEFAULT_COLS, DEFAULT_ROWS))
                    .expect("test terminal starts"),
            ),
            selected: 0,
            hovered_menu: None,
            focus: Focus::Menu,
            detail_scroll: 0,
            tick: 0,
            fps: 0.0,
            demo_input: String::new(),
            demo_lines: vec![
                "bootty demo shell".to_owned(),
                "commands: help, ls, cat README.md, cat demo.txt, clear".to_owned(),
                "".to_owned(),
            ],
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        _device_pixel_ratio: f32,
    ) -> Result<JsValue, JsValue> {
        self.cols = cols.max(40);
        self.rows = rows.max(18);
        self.egui = new_egui_context();
        self.terminal = Some(
            TestTerminalAdapter::new(Size::new(self.cols, self.rows))
                .map_err(|error| JsValue::from_str(&format!("{error:?}")))?,
        );
        self.frame()
    }

    pub fn input(&mut self, input: &str) -> Result<JsValue, JsValue> {
        for event in parse_input(input) {
            self.handle_event(event)?;
        }
        self.frame()
    }

    pub fn mouse(&mut self, kind: &str, x: u16, y: u16, button: i16) -> Result<JsValue, JsValue> {
        self.handle_mouse(kind, x, y, button)?;
        self.frame()
    }

    pub fn set_fps(&mut self, fps: f64) -> Result<JsValue, JsValue> {
        self.fps = fps;
        self.frame()
    }

    pub fn frame(&mut self) -> Result<JsValue, JsValue> {
        self.tick = self.tick.wrapping_add(1);
        let selected = self.selected;
        let hovered_menu = self.hovered_menu;
        let focus = self.focus;
        let detail_scroll = self.detail_scroll;
        let tick = self.tick;
        let fps = self.fps;
        let demo_lines = self.demo_lines.clone();
        let demo_input = self.demo_input.clone();
        let mut terminal = self
            .terminal
            .take()
            .ok_or_else(|| JsValue::from_str("terminal backend missing"))?;
        let completed = terminal
            .draw(|ratatui_frame| {
                draw_site(
                    ratatui_frame,
                    &mut self.menu,
                    &mut self.detail,
                    SiteViewState {
                        selected,
                        focus,
                        detail_scroll,
                        demo_lines: &demo_lines,
                        demo_input: &demo_input,
                    },
                );
            })
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        let value = serde_wasm_bindgen::to_value(&web_frame(
            &self.egui,
            completed.buffer,
            WebFrameState {
                selected,
                hovered_menu,
                tick,
                focus,
                fps,
            },
        ))
        .map_err(|error| JsValue::from_str(&error.to_string()));
        self.terminal = Some(terminal);
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_menu_hit_tracks_drawn_menu_rows() {
        assert_eq!(hit_target(96, 32, 2, 5), Some(HitTarget::Menu(0)));
        assert_eq!(hit_target(96, 32, 2, 7), Some(HitTarget::Menu(1)));
        assert_eq!(hit_target(96, 32, 2, 15), Some(HitTarget::Menu(5)));
    }

    #[test]
    fn wide_menu_hit_rejects_border_and_detail() {
        assert_eq!(hit_target(96, 32, 0, 4), None);
        assert_eq!(hit_target(96, 32, 40, 5), Some(HitTarget::Detail));
    }

    #[test]
    fn narrow_menu_hit_uses_vertical_layout() {
        assert_eq!(hit_target(60, 32, 2, 5), Some(HitTarget::Menu(0)));
        assert_eq!(hit_target(60, 32, 2, 15), Some(HitTarget::Menu(5)));
        assert_eq!(hit_target(60, 32, 2, 25), Some(HitTarget::Detail));
    }

    #[test]
    fn menu_hover_highlights_without_changing_selected_page() {
        let mut site = SiteBackend::new();

        site.handle_mouse("move", 2, 10, 0)
            .expect("mouse move handles");

        assert_eq!(site.selected, 0);
        assert_eq!(site.hovered_menu, Some(2));
    }

    #[test]
    fn menu_click_changes_selected_page() {
        let mut site = SiteBackend::new();

        site.handle_mouse("down", 2, 10, 0)
            .expect("mouse down handles");

        assert_eq!(site.selected, 2);
        assert_eq!(site.hovered_menu, Some(2));
    }

    #[test]
    fn detail_wheel_scrolls_without_changing_selected_page() {
        let mut site = SiteBackend::new();

        site.handle_mouse("wheel", 40, 8, 3).expect("wheel handles");

        assert_eq!(site.selected, 0);
        assert_eq!(site.focus, Focus::Detail);
        assert_eq!(site.detail_scroll, 3);
    }

    #[test]
    fn menu_click_resets_detail_scroll() {
        let mut site = SiteBackend::new();
        site.detail_scroll = 12;

        site.handle_mouse("down", 2, 10, 0)
            .expect("mouse down handles");

        assert_eq!(site.selected, 2);
        assert_eq!(site.detail_scroll, 0);
    }

    #[test]
    fn web_frame_exports_egui_sidebar_meshes() {
        let mut terminal =
            TestTerminalAdapter::new(Size::new(96, 32)).expect("test terminal starts");
        let completed = terminal
            .draw(|frame| {
                draw_site(
                    frame,
                    &mut Menu::default(),
                    &mut Detail::default(),
                    SiteViewState {
                        selected: 0,
                        focus: Focus::Menu,
                        detail_scroll: 0,
                        demo_lines: &[],
                        demo_input: "",
                    },
                );
            })
            .expect("site draws");
        let egui = new_egui_context();
        let frame = web_frame(
            &egui,
            completed.buffer,
            WebFrameState {
                selected: 0,
                hovered_menu: None,
                tick: 1,
                focus: Focus::Menu,
                fps: 0.0,
            },
        );
        let sidebar = site_layout(96, 32).menu;
        let sidebar_min_x = f32::from(sidebar.x) * CELL_WIDTH as f32;
        let sidebar_max_x = f32::from(sidebar.x.saturating_add(sidebar.width)) * CELL_WIDTH as f32;
        let egui_frame = frame.egui.expect("site frame exports egui geometry");

        assert!(
            !egui_frame.meshes.is_empty(),
            "egui sidebar produced no meshes"
        );
        assert!(
            egui_frame.meshes.iter().any(|mesh| {
                !mesh.vertices.is_empty()
                    && mesh.clip.min_x <= sidebar_min_x
                    && mesh.clip.max_x >= sidebar_max_x
            }),
            "egui sidebar meshes do not cover the menu column"
        );
        assert!(
            egui_frame.links.iter().any(|link| {
                link.url == GITHUB_URL
                    && link.rect.min_x <= sidebar_min_x
                    && link.rect.max_x >= sidebar_max_x
            }),
            "egui sidebar does not export the GitHub row link"
        );
    }

    #[test]
    fn reset_cell_colors_fall_back_to_frame_defaults() {
        let cell = web_cell(0, 0, &Cell::new("A"), None);

        assert_eq!(cell.fg, None);
        assert_eq!(cell.bg, None);
    }

    #[test]
    fn explicit_cell_colors_are_serialized() {
        let mut source = Cell::new("A");
        source.fg = Color::Green;
        source.bg = Color::Black;
        let cell = web_cell(0, 0, &source, None);

        assert_eq!(
            cell.fg,
            Some(WebColor {
                r: 158,
                g: 206,
                b: 106,
            })
        );
        assert_eq!(
            cell.bg,
            Some(WebColor {
                r: 17,
                g: 18,
                b: 26,
            })
        );
    }

    #[test]
    fn github_section_uses_github_icon_and_osc8_link() {
        let section = sections().last().expect("github section exists");

        assert!(matches!(section.icon, SectionIcon::Github));
        assert_eq!(section.icon.glyph(), "\u{f09b}");
        assert_eq!(section.label, "GitHub");
        assert_eq!(section.plain_label, "GitHub");
        assert_eq!(section.title, "Source");
        assert_eq!(section.links[0].text, "GitHub");
        assert_eq!(section.links[0].url, GITHUB_URL);
    }

    #[test]
    fn palette_section_renders_harmonious_demo_swatches() {
        let palette_section = sections()
            .iter()
            .find(|section| section.plain_label == "Palette")
            .expect("palette section exists");
        let palette = harmonious_demo_palette();
        let lines = palette_demo_lines(*palette_section);
        let content = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        let palette_glyph_count = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| {
                let content = span.content.as_ref();
                content.len() == 2
                    && content.ends_with(' ')
                    && content
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_hexdigit())
            })
            .count();

        assert!(matches!(palette_section.icon, SectionIcon::Palette));
        assert_eq!(palette_section.icon.glyph(), "\u{f1fc}");
        assert!(content.contains("Default"));
        assert!(!content.contains("216-color cube"));
        assert_eq!(lines.len(), 17);
        assert!(palette_glyph_count >= 256);
        assert_eq!(palette[16], DEMO_BACKGROUND);
        assert_eq!(palette[231], DEMO_FOREGROUND);
    }

    #[test]
    fn github_link_cells_export_model_osc8_url() {
        let github = sections()
            .iter()
            .position(|section| section.plain_label == "GitHub")
            .expect("github section exists");
        let layout = site_layout(96, 32);
        let content = inset(layout.detail, 1, 1);
        let link_y = content
            .y
            .saturating_add(SECTION_DETAIL_STATIC_ROWS)
            .saturating_add(sections()[github].lines.len() as u16);

        assert_eq!(
            osc8_link_at(96, 32, github, content.x, link_y),
            Some(GITHUB_URL)
        );
        assert_eq!(
            osc8_link_at(96, 32, github, content.x + 5, link_y),
            Some(GITHUB_URL)
        );
        assert_eq!(osc8_link_at(96, 32, github, content.x + 6, link_y), None);
        assert_eq!(osc8_link_at(96, 32, 0, content.x, link_y), None);
    }

    #[test]
    fn web_cell_serializes_osc8_url() {
        let cell = web_cell(0, 0, &Cell::new("g"), Some(GITHUB_URL));

        assert_eq!(cell.osc8.as_deref(), Some(GITHUB_URL));
    }

    #[test]
    fn getting_started_markdown_contains_both_hosts_without_fences() {
        let text = getting_started_text();
        let content = text
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(content.contains("Tauri command host"));
        assert!(content.contains("Winit/WGPU host"));
        assert!(!content.contains("```"));
    }

    #[test]
    fn getting_started_winit_section_stays_in_first_panel() {
        let text = getting_started_text();
        let winit_line = text
            .lines
            .iter()
            .position(|line| line_text(line).contains("Winit/WGPU host"))
            .expect("getting started text includes Winit heading");

        assert!(winit_line < 24);
    }

    #[test]
    fn getting_started_code_lines_are_syntax_highlighted() {
        let text = getting_started_text();
        let line = text
            .lines
            .iter()
            .find(|line| line_text(line).contains("fn write_terminal"))
            .expect("getting started text includes Tauri code");

        assert!(line.spans.len() > 1);
        assert!(
            line.spans
                .iter()
                .any(|span| matches!(span.style.fg, Some(Color::Rgb(_, _, _))))
        );
    }
}
