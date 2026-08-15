//! Wasm-bindgen backend exposed to browser hosts.

use bootty_surface::selection::{SelectionPoint, TerminalSelection, TerminalSelectionState};
use egui::Context as EguiContext;
use serde::Serialize;
use tuirealm::component::{AppComponent, Component};
use tuirealm::event::{Event, NoUserEvent};
use tuirealm::props::{AttrValue, Attribute};
use tuirealm::ratatui::buffer::Buffer;
use tuirealm::ratatui::layout::Size;
use tuirealm::terminal::{TerminalAdapter, TestTerminalAdapter};
use wasm_bindgen::prelude::*;

use crate::components::{Detail, Menu, SiteViewState, TabHit, draw_site, page_tab_hit};
use crate::constants::{DEFAULT_COLS, DEFAULT_ROWS};
use crate::content::{section_leaf_tab_count, sections};
use crate::input::{Focus, Msg, parse_input, wrap};
use crate::web_frame::{WebFrameState, WebTerminalFrame, new_egui_context, web_frame};

impl Default for SiteBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteBackend {
    fn handle_event(&mut self, event: Event<NoUserEvent>) {
        for msg in self.forward_event(&event) {
            self.update(msg);
        }
    }

    fn handle_mouse(&mut self, kind: &str, x: u16, y: u16, button: i16) {
        if kind == "leave" {
            self.hovered_menu = None;
            self.hovered_tab = None;
            self.hovered_subtab = None;
            self.hovered_leaf_tab = None;
            return;
        }
        self.hovered_menu = None;
        match page_tab_hit(
            self.selected,
            self.current_tab(),
            self.current_subtab(),
            x,
            y,
            self.cols,
            self.rows,
        ) {
            Some(TabHit::Primary(tab)) => {
                self.hovered_tab = Some(tab);
                self.hovered_subtab = None;
                self.hovered_leaf_tab = None;
                if kind == "down" {
                    self.set_active_tab(tab);
                    self.selection.clear();
                    self.detail_scroll = 0;
                    self.update(Msg::Focus(Focus::Detail));
                    return;
                }
            }
            Some(TabHit::Secondary(tab)) => {
                self.hovered_tab = None;
                self.hovered_subtab = Some(tab);
                self.hovered_leaf_tab = None;
                if kind == "down" {
                    self.set_active_subtab(tab);
                    self.selection.clear();
                    self.detail_scroll = 0;
                    self.update(Msg::Focus(Focus::Detail));
                    return;
                }
            }
            Some(TabHit::Tertiary(tab)) => {
                self.hovered_tab = None;
                self.hovered_subtab = None;
                self.hovered_leaf_tab = Some(tab);
                if kind == "down" {
                    self.set_active_leaf_tab(tab);
                    self.selection.clear();
                    self.detail_scroll = 0;
                    self.update(Msg::Focus(Focus::Detail));
                    return;
                }
            }
            None => {
                self.hovered_tab = None;
                self.hovered_subtab = None;
                self.hovered_leaf_tab = None;
            }
        }

        let point = SelectionPoint::new(x, y);
        match kind {
            "down" if button >= 3 => self.select_line_at(y),
            "down" if button == 2 => self.select_word_at(x, y),
            "down" => self.selection.begin(point),
            "move" if self.selection.is_dragging() => self.selection.drag_to(point),
            "up" => self.selection.finish(point),
            _ => {}
        }

        if kind == "wheel" {
            self.update(Msg::Focus(Focus::Detail));
            self.update(Msg::Scroll(isize::from(button)));
            return;
        }

        if kind == "down" {
            self.update(Msg::Focus(Focus::Detail));
        }
        self.hovered_menu = None;
    }

    fn select_line_at(&mut self, y: u16) {
        let Some(row) = self.last_rows.get(y as usize) else {
            self.selection.clear();
            return;
        };
        let Some(end) = last_non_space_cell(row, self.cols) else {
            self.selection.clear();
            return;
        };
        self.selection
            .select_range(SelectionPoint::new(0, y), SelectionPoint::new(end, y));
    }

    fn select_word_at(&mut self, x: u16, y: u16) {
        let Some(row) = self.last_rows.get(y as usize) else {
            self.selection.clear();
            return;
        };
        let chars = row.chars().take(self.cols as usize).collect::<Vec<_>>();
        let Some(clicked) = chars.get(x as usize).copied() else {
            self.selection.clear();
            return;
        };
        let Some(class) = SelectionClass::for_char(clicked) else {
            self.selection.clear();
            return;
        };

        let mut start = x as usize;
        while start > 0 && SelectionClass::for_char(chars[start - 1]) == Some(class) {
            start -= 1;
        }
        let mut end = x as usize;
        while end + 1 < chars.len() && SelectionClass::for_char(chars[end + 1]) == Some(class) {
            end += 1;
        }

        self.selection.select_range(
            SelectionPoint::new(start as u16, y),
            SelectionPoint::new(end as u16, y),
        );
    }

    fn forward_event(&mut self, event: &Event<NoUserEvent>) -> Vec<Msg> {
        let msg = match self.focus {
            Focus::Menu => self.menu.on(event),
            Focus::Detail => self.detail.on(event),
        };
        msg.into_iter().collect()
    }

    fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Move(delta) => {
                self.selected = wrap(self.selected as isize + delta, sections().len());
                self.hovered_menu = None;
                self.hovered_tab = None;
                self.hovered_subtab = None;
                self.hovered_leaf_tab = None;
                self.detail_scroll = 0;
            }
            Msg::SwitchTab(delta) => {
                let tab_count = sections()[self.selected].tabs.len();
                let next = wrap(self.current_tab() as isize + delta, tab_count);
                self.set_active_tab(next);
                self.detail_scroll = 0;
            }
            Msg::SwitchSubTab(delta) => {
                let section = sections()[self.selected];
                let tab = self.current_tab();
                let leaf_tab_count = section_leaf_tab_count(section, tab, self.current_subtab());
                if leaf_tab_count > 0 {
                    let next = wrap(self.current_leaf_tab() as isize + delta, leaf_tab_count);
                    self.set_active_leaf_tab(next);
                    self.detail_scroll = 0;
                } else {
                    let subtab_count = section.tabs[tab].subtabs.len();
                    if subtab_count > 0 {
                        let next = wrap(self.current_subtab() as isize + delta, subtab_count);
                        self.set_active_subtab(next);
                        self.detail_scroll = 0;
                    } else {
                        let tab_count = section.tabs.len();
                        let next = wrap(self.current_tab() as isize + delta, tab_count);
                        self.set_active_tab(next);
                        self.detail_scroll = 0;
                    }
                }
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
                }));
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
    }

    pub fn render_frame(&mut self) -> Result<WebTerminalFrame, String> {
        self.tick = self.tick.wrapping_add(1);
        let selected = self.selected;
        let hovered_menu = self.hovered_menu;
        let focus = self.focus;
        let detail_scroll = self.detail_scroll;
        let active_tab = self.current_tab();
        let active_subtab = self.current_subtab();
        let active_leaf_tab = self.current_leaf_tab();
        let hovered_tab = self.hovered_tab;
        let hovered_subtab = self.hovered_subtab;
        let hovered_leaf_tab = self.hovered_leaf_tab;
        let tick = self.tick;
        let fps = self.fps;
        let selection = self.selection.selection();
        let mut terminal = self
            .terminal
            .take()
            .ok_or_else(|| "terminal backend missing".to_owned())?;
        let completed = match terminal.draw(|ratatui_frame| {
            draw_site(
                ratatui_frame,
                &mut self.menu,
                &mut self.detail,
                SiteViewState {
                    selected,
                    active_tab,
                    active_subtab,
                    hovered_tab,
                    hovered_subtab,
                    active_leaf_tab,
                    focus,
                    hovered_leaf_tab,
                    detail_scroll,
                },
            );
        }) {
            Ok(completed) => completed,
            Err(error) => {
                self.terminal = Some(terminal);
                return Err(format!("{error:?}"));
            }
        };
        let text_rows = text_rows_from_buffer(completed.buffer);
        let frame = web_frame(
            &self.egui,
            completed.buffer,
            WebFrameState {
                selected,
                hovered_menu,
                tick,
                focus,
                fps,
                selection,
            },
        );
        self.last_rows = text_rows;
        self.terminal = Some(terminal);
        Ok(frame)
    }

    pub fn resize_frame(&mut self, cols: u16, rows: u16) -> Result<WebTerminalFrame, String> {
        self.cols = cols.max(40);
        self.rows = rows.max(18);
        self.selection.clear();
        self.egui = new_egui_context();
        self.terminal = Some(
            TestTerminalAdapter::new(Size::new(self.cols, self.rows))
                .map_err(|error| format!("{error:?}"))?,
        );
        self.render_frame()
    }

    pub fn input_frame(&mut self, input: &str) -> Result<WebTerminalFrame, String> {
        for event in parse_input(input) {
            self.handle_event(event);
        }
        self.render_frame()
    }

    pub fn mouse_frame(
        &mut self,
        kind: &str,
        x: u16,
        y: u16,
        button: i16,
    ) -> Result<WebTerminalFrame, String> {
        self.handle_mouse(kind, x, y, button);
        self.render_frame()
    }

    pub fn fps_frame(&mut self, fps: f64) -> Result<WebTerminalFrame, String> {
        self.fps = fps;
        self.render_frame()
    }

    fn current_tab(&self) -> usize {
        self.active_tabs
            .get(self.selected)
            .copied()
            .unwrap_or_default()
            .min(sections()[self.selected].tabs.len().saturating_sub(1))
    }

    fn current_subtab(&self) -> usize {
        self.active_subtabs
            .get(self.selected)
            .copied()
            .unwrap_or_default()
            .min(
                sections()[self.selected].tabs[self.current_tab()]
                    .subtabs
                    .len()
                    .saturating_sub(1),
            )
    }

    fn current_leaf_tab(&self) -> usize {
        self.active_leaf_tabs
            .get(self.selected)
            .copied()
            .unwrap_or_default()
            .min(
                section_leaf_tab_count(
                    sections()[self.selected],
                    self.current_tab(),
                    self.current_subtab(),
                )
                .saturating_sub(1),
            )
    }

    fn set_active_tab(&mut self, tab: usize) {
        if self.active_tabs.len() != sections().len() {
            self.active_tabs.resize(sections().len(), 0);
        }
        self.active_tabs[self.selected] =
            tab.min(sections()[self.selected].tabs.len().saturating_sub(1));
        self.set_active_subtab(0);
    }

    fn set_active_subtab(&mut self, tab: usize) {
        if self.active_subtabs.len() != sections().len() {
            self.active_subtabs.resize(sections().len(), 0);
        }
        let max = sections()[self.selected].tabs[self.current_tab()]
            .subtabs
            .len()
            .saturating_sub(1);
        self.active_subtabs[self.selected] = tab.min(max);
        self.set_active_leaf_tab(0);
    }

    fn set_active_leaf_tab(&mut self, tab: usize) {
        if self.active_leaf_tabs.len() != sections().len() {
            self.active_leaf_tabs.resize(sections().len(), 0);
        }
        let max = section_leaf_tab_count(
            sections()[self.selected],
            self.current_tab(),
            self.current_subtab(),
        )
        .saturating_sub(1);
        self.active_leaf_tabs[self.selected] = tab.min(max);
    }
}
fn section_index_for_page(page: &str) -> Option<usize> {
    let normalized = page.trim_matches('/').to_ascii_lowercase();
    let slug = if normalized.is_empty() {
        "overview"
    } else if normalized == "javascript" || normalized == "rust" {
        "docs"
    } else {
        normalized.as_str()
    };
    sections().iter().position(|section| {
        section.slug.eq_ignore_ascii_case(slug) || section.label.eq_ignore_ascii_case(slug)
    })
}

#[wasm_bindgen]
pub struct SiteBackend {
    egui: EguiContext,
    menu: Menu,
    detail: Detail,
    terminal: Option<TestTerminalAdapter>,
    pub(crate) selected: usize,
    pub(crate) hovered_menu: Option<usize>,
    hovered_tab: Option<usize>,
    hovered_subtab: Option<usize>,
    hovered_leaf_tab: Option<usize>,
    pub(crate) focus: Focus,
    pub(crate) detail_scroll: u16,
    active_tabs: Vec<usize>,
    active_subtabs: Vec<usize>,
    active_leaf_tabs: Vec<usize>,
    tick: u64,
    fps: f64,
    cols: u16,
    rows: u16,
    pub(crate) selection: TerminalSelectionState,
    last_rows: Vec<String>,
}

#[derive(Serialize)]
struct SiteNavigationItem {
    slug: &'static str,
    label: &'static str,
    path: String,
}

#[wasm_bindgen]
pub fn site_navigation() -> Result<JsValue, JsValue> {
    let items = sections()
        .iter()
        .map(|section| SiteNavigationItem {
            slug: section.slug,
            label: section.label,
            path: if section.slug == "overview" {
                "/".to_owned()
            } else {
                format!("/{}", section.slug)
            },
        })
        .collect::<Vec<_>>();
    serde_wasm_bindgen::to_value(&items).map_err(|error| JsValue::from_str(&error.to_string()))
}

#[wasm_bindgen]
impl SiteBackend {
    #[must_use]
    pub fn new() -> Self {
        let mut menu = Menu::default();
        let mut detail = Detail::default();
        menu.attr(Attribute::Focus, AttrValue::Flag(false));
        detail.attr(Attribute::Focus, AttrValue::Flag(true));

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
            hovered_tab: None,
            hovered_subtab: None,
            hovered_leaf_tab: None,
            focus: Focus::Detail,
            detail_scroll: 0,
            active_tabs: vec![0; sections().len()],
            active_subtabs: vec![0; sections().len()],
            active_leaf_tabs: vec![0; sections().len()],
            tick: 0,
            fps: 0.0,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            selection: TerminalSelectionState::default(),
            last_rows: Vec::new(),
        }
    }

    #[must_use]
    pub fn for_page(page: &str) -> Self {
        let mut site = Self::new();
        site.selected = section_index_for_page(page).unwrap_or(0);
        site
    }

    pub fn resize(
        &mut self,
        cols: u16,
        rows: u16,
        _device_pixel_ratio: f32,
    ) -> Result<JsValue, JsValue> {
        encode_frame(self.resize_frame(cols, rows))
    }

    pub fn input(&mut self, input: &str) -> Result<JsValue, JsValue> {
        encode_frame(self.input_frame(input))
    }

    pub fn mouse(&mut self, kind: &str, x: u16, y: u16, button: i16) -> Result<JsValue, JsValue> {
        encode_frame(self.mouse_frame(kind, x, y, button))
    }

    pub fn set_fps(&mut self, fps: f64) -> Result<JsValue, JsValue> {
        encode_frame(self.fps_frame(fps))
    }

    pub fn selected_text(&self) -> Option<String> {
        selected_text_from_rows(
            self.selection.selection()?,
            &self.last_rows,
            self.rows,
            self.cols,
        )
    }

    pub fn frame(&mut self) -> Result<JsValue, JsValue> {
        encode_frame(self.render_frame())
    }
}

fn encode_frame(frame: Result<WebTerminalFrame, String>) -> Result<JsValue, JsValue> {
    let frame = frame.map_err(|error| JsValue::from_str(&error))?;
    serde_wasm_bindgen::to_value(&frame).map_err(|error| JsValue::from_str(&error.to_string()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SelectionClass {
    Word,
    Symbol,
}

impl SelectionClass {
    fn for_char(ch: char) -> Option<Self> {
        if ch.is_whitespace() {
            None
        } else if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '#' | '@') {
            Some(Self::Word)
        } else {
            Some(Self::Symbol)
        }
    }
}

fn last_non_space_cell(row: &str, cols: u16) -> Option<u16> {
    row.chars()
        .take(cols as usize)
        .enumerate()
        .filter_map(|(index, ch)| (!ch.is_whitespace()).then_some(index as u16))
        .last()
}

fn text_rows_from_buffer(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|y| {
            let mut row = String::new();
            for x in 0..buffer.area.width {
                row.push_str(buffer[(x, y)].symbol());
            }
            row
        })
        .collect()
}

fn selected_text_from_rows(
    selection: TerminalSelection,
    rows: &[String],
    row_count: u16,
    cols: u16,
) -> Option<String> {
    let mut selected = Vec::new();
    for (row_index, range) in selection.row_ranges(row_count, cols) {
        let row = rows.get(row_index as usize)?;
        let text = row
            .chars()
            .skip(range.start as usize)
            .take(range.len())
            .collect::<String>();
        selected.push(text.trim_end().to_owned());
    }
    (!selected.is_empty()).then(|| selected.join("\n"))
}
