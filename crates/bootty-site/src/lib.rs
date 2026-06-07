use std::io::Cursor;
use std::sync::OnceLock;

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
use tuirealm::ratatui::widgets::{Block, Borders, Paragraph, Wrap};
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
const GITHUB_URL: &str = "https://github.com/majinboos/bootty";
const GITHUB_LINKS: &[SectionLink] = &[SectionLink {
    text: "github.com/majinboos/bootty",
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

    fn handle_mouse(&mut self, kind: &str, x: u16, y: u16) -> Result<(), JsValue> {
        if kind == "leave" {
            self.notice = "mouse left terminal".to_owned();
            return Ok(());
        }

        match hit_target(self.cols, self.rows, x, y) {
            Some(HitTarget::Menu(index)) => {
                self.selected = index.min(sections().len() - 1);
                self.focus = Focus::Menu;
                self.notice = format!("{} selected", sections()[self.selected].plain_label);
                self.menu.attr(Attribute::Focus, AttrValue::Flag(true));
                self.detail.attr(Attribute::Focus, AttrValue::Flag(false));
                if kind == "down" {
                    self.update(Msg::ToggleFocus)?;
                }
            }
            Some(HitTarget::Detail) if kind == "down" => {
                self.update(Msg::Focus(Focus::Detail))?;
            }
            _ => {}
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
                "This prompt is inside the Rust/WASM ratatui frame.".to_owned(),
                "Keyboard and pointer events round-trip through the browser host.".to_owned(),
            ]),
            "clear" => self.demo_lines.clear(),
            other => self.demo_lines.push(format!("unknown command: {other}")),
        }
        if self.demo_lines.len() > 18 {
            let drain = self.demo_lines.len() - 18;
            self.demo_lines.drain(0..drain);
        }
        self.notice = "demo shell updated".to_owned();
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
                self.notice = format!(
                    "{} selected",
                    sections()[self.selected].plain_label.to_lowercase()
                );
            }
            Msg::Focus(focus) => {
                self.focus = focus;
                self.notice = match focus {
                    Focus::Menu => "menu focus".to_owned(),
                    Focus::Detail => "detail focus".to_owned(),
                };
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
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
enum Msg {
    Move(isize),
    Focus(Focus),
    ToggleFocus,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum Focus {
    Menu,
    Detail,
}

#[derive(Clone, Copy)]
struct Section {
    icon: &'static str,
    label: &'static str,
    plain_label: &'static str,
    title: &'static str,
    accent: Color,
    lines: &'static [&'static str],
    links: &'static [SectionLink],
}

#[derive(Clone, Copy)]
struct SectionLink {
    text: &'static str,
    url: &'static str,
}

struct SiteViewState<'a> {
    selected: usize,
    focus: Focus,
    notice: &'a str,
    tick: u64,
    fps: f64,
    demo_lines: &'a [String],
    demo_input: &'a str,
}

#[derive(Default)]
struct Menu {
    selected: usize,
    focused: bool,
}

impl Component for Menu {
    fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .title(" nav ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if self.focused {
                Color::Magenta
            } else {
                Color::Gray
            }));
        let lines = sections()
            .iter()
            .enumerate()
            .map(|(index, section)| {
                let selected = index == self.selected;
                Line::from(Span::styled(
                    format!(
                        "{} {} {}",
                        if selected { ">" } else { " " },
                        section.icon,
                        section.label
                    ),
                    Style::default()
                        .fg(if selected {
                            section.accent
                        } else {
                            Color::Gray
                        })
                        .bg(if selected { Color::Black } else { Color::Reset })
                        .add_modifier(if selected {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        }),
                ))
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

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
    demo_lines: Vec<String>,
    demo_input: String,
}

impl Component for Detail {
    fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let section = sections()[self.section.min(sections().len() - 1)];
        let block = Block::default()
            .title(" bootty ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if self.focused {
                Color::Blue
            } else {
                Color::Gray
            }));
        if section.plain_label == "Getting Started" {
            frame.render_widget(
                Paragraph::new(getting_started_text())
                    .block(block)
                    .wrap(Wrap { trim: false }),
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
                Line::from(Span::styled(
                    "native terminal crates / Rust WASM site / canvas frame host",
                    Style::default().fg(Color::Gray),
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
        }
        frame.render_widget(
            Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
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
                code: Key::Enter, ..
            }) => Some(Msg::ToggleFocus),
            _ => None,
        }
    }
}

fn draw_site(
    frame: &mut Frame<'_>,
    menu_component: &mut Menu,
    detail_component: &mut Detail,
    state: SiteViewState<'_>,
) {
    let area = frame.area();
    let SiteLayout {
        header,
        menu,
        detail,
        footer,
    } = site_layout(area.width, area.height);

    draw_header(frame, header, state.tick, state.fps);
    menu_component.attr(Attribute::Value, AttrValue::Number(state.selected as isize));
    menu_component.attr(
        Attribute::Focus,
        AttrValue::Flag(state.focus == Focus::Menu),
    );
    detail_component.attr(Attribute::Value, AttrValue::Number(state.selected as isize));
    detail_component.attr(
        Attribute::Focus,
        AttrValue::Flag(state.focus == Focus::Detail),
    );
    detail_component.demo_lines = state.demo_lines.to_vec();
    detail_component.demo_input = state.demo_input.to_owned();
    menu_component.view(frame, menu);
    detail_component.view(frame, detail);
    draw_footer(frame, footer, state.notice, state.focus);
}

fn site_layout(cols: u16, rows: u16) -> SiteLayout {
    let area = Rect::new(0, 0, cols, rows);
    let [header, body, footer] = Layout::default()
        .direction(LayoutDirection::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .areas(area);
    let [menu, detail] = Layout::default()
        .direction(if body.width < 78 {
            LayoutDirection::Vertical
        } else {
            LayoutDirection::Horizontal
        })
        .constraints(if body.width < 78 {
            [Constraint::Length(9), Constraint::Min(8)]
        } else {
            [Constraint::Length(28), Constraint::Min(20)]
        })
        .areas(body);

    SiteLayout {
        header,
        menu: inset(menu, 1, 0),
        detail: inset(detail, 1, 0),
        footer,
    }
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, tick: u64, fps_value: f64) {
    let pulse = if tick % 90 < 45 {
        Color::Magenta
    } else {
        Color::Blue
    };
    let fps = format!("{:05.1} fps", fps_value);
    let line = Line::from(vec![
        Span::raw("       "),
        Span::styled(
            "BOOTTY",
            Style::default().fg(pulse).add_modifier(Modifier::BOLD),
        ),
        Span::styled("    bootty.org", Style::default().fg(Color::Gray)),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(Block::default()),
        inset(area, 2, 1),
    );
    let fps_x = area
        .width
        .saturating_sub((fps.chars().count() as u16).saturating_add(2));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            fps,
            Style::default().fg(Color::Green),
        ))),
        Rect::new(fps_x, area.y + 1, area.width.saturating_sub(fps_x), 1),
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, notice: &str, focus: Focus) {
    let line = Line::from(vec![
        Span::styled(
            "tab",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" focus  "),
        Span::styled(
            "arrows/jk",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" move  "),
        Span::styled(
            "enter",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" activate  "),
        Span::styled(
            format!("{focus:?}: {notice}"),
            Style::default().fg(Color::Blue),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Black)),
        area,
    );
}

fn sections() -> &'static [Section] {
    &[
        Section {
            icon: "\u{f2d0}",
            label: "Frame",
            plain_label: "Frame",
            title: "This website is a terminal frame.",
            accent: Color::Magenta,
            lines: &[
                "The page is not a screenshot and not a DOM mockup.",
                "Rust/WASM draws ratatui widgets into terminal cells.",
                "The browser receives cells, colors, cursor state, and image layers.",
                "The same frame contract can host demos, docs, and app surfaces.",
            ],
            links: &[],
        },
        Section {
            icon: "\u{f05a9}",
            label: "Getting Started",
            plain_label: "Getting Started",
            title: "Build a terminal host with Bootty.",
            accent: Color::Green,
            lines: &[],
            links: &[],
        },
        Section {
            icon: "\u{f0078}",
            label: "DOOM",
            plain_label: "DOOM",
            title: "Doom runs inside this terminal frame.",
            accent: Color::Red,
            lines: &[
                "The browser runs a doomgeneric WebAssembly engine.",
                "Bootty receives the framebuffer as an image layer in this tab.",
                "Focus the detail panel to play; use WASD/arrows, F, Space, and 1-7.",
                "The rest of the website remains a normal terminal frame.",
            ],
            links: &[],
        },
        Section {
            icon: "\u{f03a7}",
            label: "Renderer",
            plain_label: "Renderer",
            title: "Renderer seams are Bootty-owned.",
            accent: Color::Blue,
            lines: &[
                "bootty-render plans backgrounds, text, sprites, decorations, and cursors.",
                "bootty-surface keeps grid geometry and pointer math shared.",
                "The native app submits WGPU terminal commands through eframe.",
                "This site proves the terminal frame can travel outside the desktop host.",
            ],
            links: &[],
        },
        Section {
            icon: "\u{e795}",
            label: "Runtime",
            plain_label: "Runtime",
            title: "PTY work stays out of the UI layer.",
            accent: Color::Green,
            lines: &[
                "bootty-runtime owns PTY sessions, drain budgets, and frame publication.",
                "bootty-terminal adapts libghostty-vt for state, colors, and encoders.",
                "The app consumes published frames instead of parsing shell output on the UI thread.",
                "Status metrics make latency visible while benchmarks keep regressions concrete.",
            ],
            links: &[],
        },
        Section {
            icon: "\u{f489}",
            label: "Shell",
            plain_label: "Shell",
            title: "Demo shell.",
            accent: Color::Yellow,
            lines: &[],
            links: &[],
        },
        Section {
            icon: "\u{f0222}",
            label: "App",
            plain_label: "App",
            title: "Bootty is the working terminal home.",
            accent: Color::Cyan,
            lines: &[
                "The default binary opens tmux chrome, status metrics, and terminal glyphs.",
                "The bare example opens a minimal native winit/WGPU terminal host.",
                "The egui-tabs example routes tabs through the shared renderer path.",
                "Validation covers fmt, clippy, tests, benchmark builds, and paint-plan benches.",
            ],
            links: &[],
        },
        Section {
            icon: "\u{f09b}",
            label: "GitHub",
            plain_label: "GitHub",
            title: "Read the source on GitHub.",
            accent: Color::Red,
            lines: &["github.com/majinboos/bootty"],
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

fn web_frame(buffer: &Buffer) -> WebTerminalFrame {
    let mut cells = Vec::with_capacity(buffer.content.len());
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            cells.push(web_cell(x, y, cell, osc8_link_at(buffer, x, y)));
        }
    }
    WebTerminalFrame {
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
        images: icon_images(buffer.area.width),
    }
}

fn icon_images(cols: u16) -> Vec<WebImage> {
    if cols < 24 {
        return Vec::new();
    }

    let icon = site_icon();
    let texture_size = ICON_TEXTURE_SIZE as f32;
    let render_size = ICON_RENDER_SIZE as f32;
    let min_x = CELL_WIDTH as f32 * 2.0;
    let min_y = 6.0;
    vec![WebImage {
        key: "bootty-mascot".to_owned(),
        layer: WebImageLayer::AboveText,
        image_width: ICON_TEXTURE_SIZE,
        image_height: ICON_TEXTURE_SIZE,
        source: WebRect {
            min_x: 0.0,
            min_y: 0.0,
            max_x: texture_size,
            max_y: texture_size,
        },
        destination: WebRect {
            min_x,
            min_y,
            max_x: min_x + render_size,
            max_y: min_y + render_size,
        },
        rgba: icon.rgba.clone(),
    }]
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

fn osc8_link_at(buffer: &Buffer, x: u16, y: u16) -> Option<&'static str> {
    let row = row_text(buffer, y);
    sections()
        .iter()
        .flat_map(|section| section.links)
        .find_map(|link| {
            let start = row.find(link.text)?;
            let end = start + link.text.len();
            ((x as usize) >= start && (x as usize) < end).then_some(link.url)
        })
}

fn row_text(buffer: &Buffer, y: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
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

    let menu_content = inset(layout.menu, 1, 1);
    if !contains(menu_content, x, y) {
        return None;
    }

    let index = y.saturating_sub(menu_content.y) as usize;
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
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
    colors: WebFrameColors,
    cursor: Option<WebCursor>,
    cells: Vec<WebCell>,
    images: Vec<WebImage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebFrameColors {
    background: WebColor,
    foreground: WebColor,
    cursor: Option<WebColor>,
}

#[derive(Debug, PartialEq, Serialize)]
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
    layer: WebImageLayer,
    image_width: u32,
    image_height: u32,
    source: WebRect,
    destination: WebRect,
    rgba: Vec<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum WebImageLayer {
    AboveText,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WebRect {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}
#[wasm_bindgen]
pub struct SiteBackend {
    menu: Menu,
    detail: Detail,
    terminal: Option<TestTerminalAdapter>,
    selected: usize,
    focus: Focus,
    notice: String,
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
            menu,
            detail,
            terminal: Some(
                TestTerminalAdapter::new(Size::new(DEFAULT_COLS, DEFAULT_ROWS))
                    .expect("test terminal starts"),
            ),
            selected: 0,
            focus: Focus::Menu,
            notice: "Bootty terminal website".to_owned(),
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

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<JsValue, JsValue> {
        self.cols = cols.max(40);
        self.rows = rows.max(18);
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

    pub fn mouse(&mut self, kind: &str, x: u16, y: u16, _button: i16) -> Result<JsValue, JsValue> {
        self.handle_mouse(kind, x, y)?;
        self.frame()
    }

    pub fn set_fps(&mut self, fps: f64) -> Result<JsValue, JsValue> {
        self.fps = fps;
        self.frame()
    }

    pub fn frame(&mut self) -> Result<JsValue, JsValue> {
        self.tick = self.tick.wrapping_add(1);
        let selected = self.selected;
        let focus = self.focus;
        let notice = self.notice.clone();
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
                        notice: &notice,
                        tick,
                        fps,
                        demo_lines: &demo_lines,
                        demo_input: &demo_input,
                    },
                );
            })
            .map_err(|error| JsValue::from_str(&format!("{error:?}")))?;
        let value = serde_wasm_bindgen::to_value(&web_frame(completed.buffer))
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
        assert_eq!(hit_target(96, 32, 2, 10), Some(HitTarget::Menu(5)));
    }

    #[test]
    fn wide_menu_hit_rejects_border_and_detail() {
        assert_eq!(hit_target(96, 32, 1, 5), None);
        assert_eq!(hit_target(96, 32, 30, 5), Some(HitTarget::Detail));
    }

    #[test]
    fn narrow_menu_hit_uses_vertical_layout() {
        assert_eq!(hit_target(60, 32, 2, 5), Some(HitTarget::Menu(0)));
        assert_eq!(hit_target(60, 32, 2, 10), Some(HitTarget::Menu(5)));
        assert_eq!(hit_target(60, 32, 2, 14), Some(HitTarget::Detail));
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
    fn github_section_uses_nerd_font_icon_and_osc8_link() {
        let section = sections().last().expect("github section exists");

        assert_eq!(section.icon, "\u{f09b}");
        assert_eq!(section.label, "GitHub");
        assert_eq!(section.plain_label, "GitHub");
        assert_eq!(section.links[0].text, "github.com/majinboos/bootty");
        assert_eq!(section.links[0].url, GITHUB_URL);
    }

    #[test]
    fn github_link_cells_export_osc8_url() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 1));
        buffer.set_string(2, 0, "github.com/majinboos/bootty", Style::default());

        assert_eq!(osc8_link_at(&buffer, 2, 0), Some(GITHUB_URL));
        assert_eq!(osc8_link_at(&buffer, 28, 0), Some(GITHUB_URL));
        assert_eq!(osc8_link_at(&buffer, 29, 0), None);
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
