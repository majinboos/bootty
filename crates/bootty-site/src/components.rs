//! Ratatui components used by the wasm site backend.

use tuirealm::command::{Cmd, CmdResult, Direction};
use tuirealm::component::{AppComponent, Component};
use tuirealm::event::{Event, Key, KeyEvent, NoUserEvent};
use tuirealm::props::{AttrValue, Attribute, QueryResult};
use tuirealm::ratatui::Frame;
use tuirealm::ratatui::layout::{Constraint, Direction as LayoutDirection, Layout, Rect};
use tuirealm::ratatui::style::{Color, Modifier, Style};
use tuirealm::ratatui::text::{Line, Span, Text};
use tuirealm::ratatui::widgets::{Paragraph, Wrap};
use tuirealm::state::State;

use crate::content::{
    FeatureCard, Section, section_has_alternative_tabs, section_leaf_tab_count,
    section_leaf_tab_label, section_nested_texts_for_width, section_subtab_count,
    section_subtab_label, section_tab_label, section_text_for_width, sections,
};
use crate::input::{Focus, Msg};
use crate::layout::max_scroll;
use crate::palette::palette_demo_lines;

const HERO_ROWS: u16 = 4;
const TAB_ROWS: u16 = 1;
const USAGE_HEADING_ROWS: u16 = 2;

pub(crate) struct SiteViewState {
    pub(crate) selected: usize,
    pub(crate) active_tab: usize,
    pub(crate) active_subtab: usize,
    pub(crate) active_leaf_tab: usize,
    pub(crate) hovered_tab: Option<usize>,
    pub(crate) hovered_subtab: Option<usize>,
    pub(crate) hovered_leaf_tab: Option<usize>,
    pub(crate) focus: Focus,
    pub(crate) detail_scroll: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TabHit {
    Primary(usize),
    Secondary(usize),
    Tertiary(usize),
}

#[derive(Default)]
pub(crate) struct Menu {
    pub(crate) selected: usize,
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
pub(crate) struct Detail {
    section: usize,
    active_tab: usize,
    active_subtab: usize,
    active_leaf_tab: usize,
    hovered_tab: Option<usize>,
    hovered_subtab: Option<usize>,
    hovered_leaf_tab: Option<usize>,
    focused: bool,
    scroll: u16,
}

#[derive(Clone, Copy)]
struct MarkdownState {
    active_tab: usize,
    active_subtab: usize,
    active_leaf_tab: usize,
    hovered_subtab: Option<usize>,
    hovered_leaf_tab: Option<usize>,
    scroll: u16,
}

impl Component for Detail {
    fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let selected = self.section.min(sections().len() - 1);
        let section = sections()[selected];
        let active_tab = self.active_tab.min(section.tabs.len().saturating_sub(1));
        let primary_tabs = section_has_alternative_tabs(section);
        let primary_rows = if primary_tabs { TAB_ROWS } else { 0 };
        let subtab_count = section_subtab_count(section, active_tab);
        let active_subtab = if subtab_count == 0 {
            0
        } else {
            self.active_subtab.min(subtab_count - 1)
        };
        let leaf_tab_count = section_leaf_tab_count(section, active_tab, active_subtab);
        let active_leaf_tab = if leaf_tab_count == 0 {
            0
        } else {
            self.active_leaf_tab.min(leaf_tab_count - 1)
        };
        let inline_secondary_tabs = section_leaf_tab_count(section, active_tab, active_subtab) > 0;
        let secondary_rows = if subtab_count == 0 || inline_secondary_tabs || !primary_tabs {
            0
        } else {
            TAB_ROWS
        };
        let shell = shell_area(area);
        let chunks = Layout::default()
            .direction(LayoutDirection::Vertical)
            .constraints([
                Constraint::Length(HERO_ROWS),
                Constraint::Length(primary_rows),
                Constraint::Length(secondary_rows),
                Constraint::Length(if section.cards.is_empty() { 0 } else { 7 }),
                Constraint::Min(0),
            ])
            .split(shell);

        render_hero(frame, chunks[0], section);
        if primary_tabs {
            render_primary_tabs(frame, chunks[1], section, active_tab, self.hovered_tab);
        }
        if subtab_count > 0 && !inline_secondary_tabs {
            render_secondary_tabs(
                frame,
                chunks[2],
                section,
                active_tab,
                active_subtab,
                self.hovered_subtab,
            );
        }
        if !section.cards.is_empty() {
            render_cards(frame, chunks[3], section.cards);
        }
        render_markdown_block(
            frame,
            chunks[4],
            section,
            MarkdownState {
                active_tab,
                active_subtab,
                active_leaf_tab,
                hovered_subtab: self.hovered_subtab,
                hovered_leaf_tab: self.hovered_leaf_tab,
                scroll: self.scroll,
            },
        );
    }

    fn query<'a>(&'a self, _attr: Attribute) -> Option<QueryResult<'a>> {
        None
    }

    fn attr(&mut self, attr: Attribute, value: AttrValue) {
        match (attr, value) {
            (Attribute::Value, AttrValue::Number(value)) => self.section = value.max(0) as usize,
            (Attribute::Custom("tab"), AttrValue::Number(value)) => {
                self.active_tab = value.max(0) as usize
            }
            (Attribute::Custom("subtab"), AttrValue::Number(value)) => {
                self.active_subtab = value.max(0) as usize
            }
            (Attribute::Custom("leaf_tab"), AttrValue::Number(value)) => {
                self.active_leaf_tab = value.max(0) as usize
            }
            (Attribute::Custom("hovered_tab"), AttrValue::Number(value)) => {
                self.hovered_tab = (value >= 0).then_some(value as usize)
            }
            (Attribute::Custom("hovered_subtab"), AttrValue::Number(value)) => {
                self.hovered_subtab = (value >= 0).then_some(value as usize)
            }
            (Attribute::Custom("hovered_leaf_tab"), AttrValue::Number(value)) => {
                self.hovered_leaf_tab = (value >= 0).then_some(value as usize)
            }
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
                code: Key::Esc | Key::BackTab,
                ..
            }) => Some(Msg::Focus(Focus::Menu)),
            Event::Keyboard(KeyEvent {
                code: Key::Char('['),
                ..
            }) => Some(Msg::SwitchTab(-1)),
            Event::Keyboard(KeyEvent {
                code: Key::Char(']'),
                ..
            }) => Some(Msg::SwitchTab(1)),
            Event::Keyboard(KeyEvent {
                code: Key::Left | Key::Char('h'),
                ..
            }) => Some(Msg::SwitchSubTab(-1)),
            Event::Keyboard(KeyEvent {
                code: Key::Right | Key::Char('l') | Key::Tab,
                ..
            }) => Some(Msg::SwitchSubTab(1)),
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

pub(crate) fn draw_site(
    frame: &mut Frame<'_>,
    menu_component: &mut Menu,
    detail_component: &mut Detail,
    state: SiteViewState,
) {
    detail_component.attr(Attribute::Value, AttrValue::Number(state.selected as isize));
    detail_component.attr(
        Attribute::Custom("tab"),
        AttrValue::Number(state.active_tab as isize),
    );
    detail_component.attr(
        Attribute::Custom("subtab"),
        AttrValue::Number(state.active_subtab as isize),
    );
    detail_component.attr(
        Attribute::Custom("leaf_tab"),
        AttrValue::Number(state.active_leaf_tab as isize),
    );
    detail_component.attr(
        Attribute::Custom("hovered_tab"),
        AttrValue::Number(state.hovered_tab.map_or(-1, |tab| tab as isize)),
    );
    detail_component.attr(
        Attribute::Custom("hovered_subtab"),
        AttrValue::Number(state.hovered_subtab.map_or(-1, |tab| tab as isize)),
    );
    detail_component.attr(
        Attribute::Custom("hovered_leaf_tab"),
        AttrValue::Number(state.hovered_leaf_tab.map_or(-1, |tab| tab as isize)),
    );
    detail_component.attr(
        Attribute::Focus,
        AttrValue::Flag(state.focus == Focus::Detail),
    );
    detail_component.scroll = state.detail_scroll;
    let _ = menu_component;
    detail_component.view(frame, frame.area());
}

pub(crate) fn page_tab_hit(
    selected: usize,
    active_tab: usize,
    active_subtab: usize,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Option<TabHit> {
    let section = sections().get(selected)?;
    let shell = shell_area(Rect::new(0, 0, width, height));
    let primary_tabs = section_has_alternative_tabs(*section);
    let primary_rows = if primary_tabs { TAB_ROWS } else { 0 };
    let primary_y = shell.y.saturating_add(HERO_ROWS);
    let mut secondary_y = primary_y.saturating_add(primary_rows);
    let inline_secondary_tabs =
        primary_tabs && section_leaf_tab_count(*section, active_tab, active_subtab) > 0;
    let secondary_rows =
        if primary_tabs && section_subtab_count(*section, active_tab) > 0 && !inline_secondary_tabs
        {
            TAB_ROWS
        } else {
            0
        };
    let card_rows = if section.cards.is_empty() { 0 } else { 7 };
    let markdown_y = shell
        .y
        .saturating_add(HERO_ROWS)
        .saturating_add(primary_rows)
        .saturating_add(secondary_rows)
        .saturating_add(card_rows);

    let mut tertiary_y = markdown_y;
    if inline_secondary_tabs {
        let markdown_height = shell
            .height
            .saturating_sub(HERO_ROWS)
            .saturating_sub(primary_rows)
            .saturating_sub(card_rows);
        if let Some((parent_height, group_height)) = nested_tab_row_heights(
            *section,
            active_tab,
            active_subtab,
            0,
            shell.width,
            markdown_height,
        ) {
            secondary_y = markdown_y.saturating_add(parent_height);
            tertiary_y = secondary_y
                .saturating_add(TAB_ROWS)
                .saturating_add(group_height)
                .saturating_add(USAGE_HEADING_ROWS);
        }
    }

    if primary_tabs && y == primary_y {
        return tab_hit(section.tabs.iter().map(|tab| tab.label), x).map(TabHit::Primary);
    }
    if y == secondary_y && section_subtab_count(*section, active_tab) > 0 {
        return tab_hit(
            (0..section_subtab_count(*section, active_tab))
                .map(|index| section_subtab_label(*section, active_tab, index)),
            x,
        )
        .map(TabHit::Secondary);
    }
    if y == tertiary_y && section_leaf_tab_count(*section, active_tab, active_subtab) > 0 {
        return tab_hit(
            (0..section_leaf_tab_count(*section, active_tab, active_subtab))
                .map(|index| section_leaf_tab_label(*section, active_tab, active_subtab, index)),
            x,
        )
        .map(TabHit::Tertiary);
    }
    None
}

fn tab_hit<'a>(labels: impl Iterator<Item = &'a str>, x: u16) -> Option<usize> {
    let mut cursor = 4;
    if x < cursor {
        return None;
    }
    for (index, label) in labels.enumerate() {
        let width = label.chars().count() as u16 + 4;
        if x >= cursor && x < cursor.saturating_add(width) {
            return Some(index);
        }
        cursor = cursor.saturating_add(width + 1);
    }
    None
}

fn shell_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(3),
        y: area.y.saturating_add(2),
        width: area.width.saturating_sub(6),
        height: area.height.saturating_sub(4),
    }
}

fn render_hero(frame: &mut Frame<'_>, area: Rect, section: Section) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            section.title,
            Style::default()
                .fg(section.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(section.tagline, Style::default())),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn render_primary_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    section: Section,
    active_tab: usize,
    hovered_tab: Option<usize>,
) {
    render_tabs(
        frame,
        area,
        section.tabs.iter().map(|tab| tab.label),
        section.accent,
        active_tab,
        hovered_tab,
    );
}

fn render_secondary_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    section: Section,
    active_tab: usize,
    active_subtab: usize,
    hovered_subtab: Option<usize>,
) {
    render_tabs(
        frame,
        area,
        (0..section_subtab_count(section, active_tab))
            .map(|index| section_subtab_label(section, active_tab, index)),
        section.accent,
        active_subtab,
        hovered_subtab,
    );
}

fn render_tertiary_tabs(
    frame: &mut Frame<'_>,
    area: Rect,
    section: Section,
    active_tab: usize,
    active_subtab: usize,
    active_leaf_tab: usize,
    hovered_leaf_tab: Option<usize>,
) {
    render_tabs(
        frame,
        area,
        (0..section_leaf_tab_count(section, active_tab, active_subtab))
            .map(|index| section_leaf_tab_label(section, active_tab, active_subtab, index)),
        section.accent,
        active_leaf_tab,
        hovered_leaf_tab,
    );
}

fn render_tabs<'a>(
    frame: &mut Frame<'_>,
    area: Rect,
    labels: impl Iterator<Item = &'a str>,
    accent: Color,
    active: usize,
    hovered: Option<usize>,
) {
    let mut spans = Vec::new();
    for (index, label) in labels.enumerate() {
        if index > 0 {
            spans.push(Span::styled(" ", Style::default()));
        }
        let style = if index == active {
            Style::default()
                .fg(Color::Rgb(17, 18, 26))
                .bg(accent)
                .add_modifier(Modifier::BOLD)
        } else if Some(index) == hovered {
            Style::default()
                .fg(Color::Rgb(214, 222, 247))
                .bg(Color::Rgb(30, 34, 48))
                .add_modifier(Modifier::UNDERLINED)
        } else {
            Style::default()
                .fg(Color::Rgb(139, 149, 182))
                .bg(Color::Rgb(18, 20, 30))
        };
        spans.push(Span::styled(format!("  {label}  "), style));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_cards(frame: &mut Frame<'_>, area: Rect, cards: &[FeatureCard]) {
    let constraints = vec![Constraint::Ratio(1, cards.len() as u32); cards.len()];
    let columns = Layout::default()
        .direction(LayoutDirection::Horizontal)
        .constraints(constraints)
        .split(area);
    for (index, card) in cards.iter().enumerate() {
        let mut lines = Vec::with_capacity(card.body.len() + 2);
        lines.push(Line::from(Span::styled(
            card.title,
            Style::default()
                .fg(card.accent)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.extend(card.body.iter().map(|item| {
            Line::from(vec![
                Span::styled("- ", Style::default().fg(Color::Rgb(139, 149, 182))),
                Span::from(*item),
            ])
        }));
        frame.render_widget(Paragraph::new(lines), columns[index]);
    }
}

fn render_markdown_block(
    frame: &mut Frame<'_>,
    area: Rect,
    section: Section,
    state: MarkdownState,
) {
    let code_width = area.width;
    if let Some((parent_text, group_text, leaf_text)) = section_nested_texts_for_width(
        section,
        state.active_tab,
        state.active_subtab,
        state.active_leaf_tab,
        code_width,
    ) {
        let Some((parent_height, group_height)) = nested_tab_row_heights(
            section,
            state.active_tab,
            state.active_subtab,
            state.active_leaf_tab,
            code_width,
            area.height,
        ) else {
            return;
        };
        let chunks = Layout::default()
            .direction(LayoutDirection::Vertical)
            .constraints([
                Constraint::Length(parent_height),
                Constraint::Length(TAB_ROWS),
                Constraint::Length(group_height),
                Constraint::Length(USAGE_HEADING_ROWS),
                Constraint::Length(TAB_ROWS),
                Constraint::Min(0),
            ])
            .split(area);
        render_markdown_paragraph(frame, chunks[0], parent_text, 0);
        render_secondary_tabs(
            frame,
            chunks[1],
            section,
            state.active_tab,
            state.active_subtab,
            state.hovered_subtab,
        );
        render_markdown_paragraph(frame, chunks[2], group_text, 0);
        render_usage_heading(frame, chunks[3], section);
        render_tertiary_tabs(
            frame,
            chunks[4],
            section,
            state.active_tab,
            state.active_subtab,
            state.active_leaf_tab,
            state.hovered_leaf_tab,
        );
        render_markdown_paragraph(frame, chunks[5], leaf_text, state.scroll);
        return;
    }

    let mut text = section_text_for_width(
        section,
        state.active_tab,
        state.active_subtab,
        state.active_leaf_tab,
        code_width,
    );
    if section.slug == "renderer" && section_tab_label(section, state.active_tab) == "Color" {
        text.lines.push(Line::from(""));
        text.lines
            .extend(palette_demo_lines(section).into_iter().skip(1));
    }
    render_markdown_paragraph(frame, area, text, state.scroll);
}

fn nested_tab_row_heights(
    section: Section,
    active_tab: usize,
    active_subtab: usize,
    active_leaf_tab: usize,
    code_width: u16,
    area_height: u16,
) -> Option<(u16, u16)> {
    let (parent_text, group_text, _) = section_nested_texts_for_width(
        section,
        active_tab,
        active_subtab,
        active_leaf_tab,
        code_width,
    )?;
    let fixed_rows = TAB_ROWS
        .saturating_add(USAGE_HEADING_ROWS)
        .saturating_add(TAB_ROWS);
    let parent_height =
        (parent_text.lines.len() as u16).min(area_height.saturating_sub(fixed_rows));
    let group_height = (group_text.lines.len() as u16).min(
        area_height
            .saturating_sub(parent_height)
            .saturating_sub(fixed_rows),
    );
    Some((parent_height, group_height))
}

fn render_usage_heading(frame: &mut Frame<'_>, area: Rect, section: Section) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "Usage",
            Style::default()
                .fg(section.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ]);
    frame.render_widget(Paragraph::new(text), area);
}

fn render_markdown_paragraph(frame: &mut Frame<'_>, area: Rect, text: Text<'static>, scroll: u16) {
    let max_scroll = max_scroll(text.lines.len(), area.height);
    let scroll = scroll.min(max_scroll);
    frame.render_widget(
        Paragraph::new(text)
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}
