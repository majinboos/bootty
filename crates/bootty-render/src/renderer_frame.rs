use std::{collections::BTreeSet, ops::Range};

use libghostty_vt::{
    render::Dirty,
    style::{RgbColor, Underline},
};
use unicode_width::UnicodeWidthChar;

use crate::{
    geometry::{CellMetrics, SurfaceRect, TerminalPadding, TerminalSurface},
    paint_plan::{
        BackgroundRect, CursorBlinkPhase, CursorShape, DecorationLine, DecorationStyle,
        PaintPlanner, PlanColor, TerminalPaintPlan, TextAttrs, TextRun,
    },
    terminal::{CellStyle, CursorSnapshot, RenderFrame},
    terminal_image::KittyImageFrame,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};

#[derive(Clone, Debug, PartialEq)]
pub struct RendererFrame {
    pub metrics: RendererFrameMetrics,
    pub rows: Vec<RendererRow>,
    pub cells: Vec<RendererCell>,
    pub cursor: Option<RendererCursor>,
    pub default_foreground: PlanColor,
    pub default_background: PlanColor,
    pub selection_foreground: Option<PlanColor>,
    pub selection_background: Option<PlanColor>,
    pub images: KittyImageFrame,
    source_dirty: Dirty,
    paint_plan: TerminalPaintPlan,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererLinkMods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererLinkHighlight {
    Always,
    AlwaysWithMods(RendererLinkMods),
    Hover,
    HoverWithMods(RendererLinkMods),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererLinkPattern {
    pub pattern: String,
    pub highlight: RendererLinkHighlight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RendererCellPoint {
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererLinkCellMap {
    cells: BTreeSet<RendererCellPoint>,
}

impl RendererLinkCellMap {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.cells.contains(&RendererCellPoint { x, y })
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RendererPreedit {
    pub codepoints: Vec<RendererPreeditCodepoint>,
}

impl RendererPreedit {
    pub fn width(&self) -> u16 {
        self.codepoints
            .iter()
            .map(|codepoint| if codepoint.wide { 2 } else { 1 })
            .sum()
    }

    pub fn range(&self, start: u16, max: u16) -> RendererPreeditRange {
        let max_width = max.saturating_sub(start).saturating_add(1);
        let mut width = 0;
        let mut codepoint_offset = 0;

        for (index, codepoint) in self.codepoints.iter().enumerate().rev() {
            width += if codepoint.wide { 2 } else { 1 };
            if width > max_width {
                codepoint_offset = index;
                break;
            }
        }

        let end = if width > 0 {
            start.saturating_add(width - 1)
        } else {
            start
        };
        let start_offset = end.saturating_sub(max);

        RendererPreeditRange {
            start: start.saturating_sub(start_offset),
            end: end.saturating_sub(start_offset),
            codepoint_offset,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererPreeditCodepoint {
    pub codepoint: char,
    pub wide: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererPreeditRange {
    pub start: u16,
    pub end: u16,
    pub codepoint_offset: usize,
}

impl RendererFrame {
    pub fn from_terminal(
        frame: &RenderFrame,
        surface: TerminalSurface,
        text_config: &TerminalTextConfig,
    ) -> Self {
        let default_foreground = PlanColor::opaque(frame.colors.foreground);
        let default_background = PlanColor::opaque(frame.colors.background);
        let metrics = RendererFrameMetrics {
            viewport: surface.surface_rect(),
            grid: surface.grid_rect(frame.cols, frame.rows),
            cell: surface.cell,
            padding: surface.padding,
            font_size: text_config.font_size,
        };
        let mut planner = PaintPlanner::default();
        let paint_plan = planner
            .plan_with_cursor_blink_phase(
                surface,
                frame,
                text_config.font_size,
                CursorBlinkPhase::visible(),
            )
            .clone();

        let mut source_cells = frame.cells.iter().collect::<Vec<_>>();
        source_cells.sort_by_key(|cell| (cell.y, cell.x));

        let mut cells = Vec::with_capacity(source_cells.len());
        let mut rows = Vec::with_capacity(usize::from(frame.rows));
        let mut cell_index = 0;

        for y in 0..frame.rows {
            let start = cells.len();
            while cell_index < source_cells.len() && source_cells[cell_index].y == y {
                let cell = source_cells[cell_index];
                cells.push(RendererCell::from_terminal(
                    cell.x,
                    cell.y,
                    frame.cell_text(cell).iter().collect(),
                    cell.fg,
                    cell.bg,
                    cell.style,
                    metrics.cell_rect(cell.x, cell.y),
                ));
                cell_index += 1;
            }
            rows.push(RendererRow {
                y,
                cells: start..cells.len(),
            });
        }

        Self {
            metrics,
            rows,
            cells,
            cursor: frame.cursor.map(|cursor| {
                RendererCursor::from_terminal(cursor, frame.colors.cursor, default_foreground)
            }),
            default_foreground,
            default_background,
            selection_foreground: frame.colors.selection_foreground.map(PlanColor::opaque),
            selection_background: frame.colors.selection_background.map(PlanColor::opaque),
            images: frame.images.clone(),
            source_dirty: frame.dirty,
            paint_plan,
        }
    }

    pub fn to_terminal_render_frame(
        &self,
        text_config: &TerminalTextConfig,
    ) -> TerminalRenderFrame {
        let plan = self.to_paint_plan();
        let text_contract = TerminalTextContract::for_terminal_paint_plan(&plan, text_config);
        TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &self.images)
    }

    pub fn to_paint_plan(&self) -> TerminalPaintPlan {
        if !self.requires_surface_adjustment() {
            return self.paint_plan.clone();
        }
        let mut plan = TerminalPaintPlan {
            surface: self.paint_plan.surface,
            default_background: self.default_background,
            backgrounds: Vec::new(),
            text_runs: Vec::new(),
            decorations: Vec::new(),
            cursor: self.paint_plan.cursor.clone(),
        };
        for cell in &self.cells {
            let (foreground, background) = self.resolved_cell_colors(cell);
            if background != self.default_background {
                plan.backgrounds.push(BackgroundRect {
                    rect: cell.rect,
                    color: background,
                });
            }
            if cell.invisible || cell.text.is_empty() {
                continue;
            }
            plan.text_runs.push(TextRun {
                rect: cell.rect,
                cells: text_cell_width(&cell.text),
                text: cell.text.clone(),
                attrs: TextAttrs {
                    fg: foreground,
                    bold: cell.style.bold,
                    italic: cell.style.italic,
                    underline: cell.decor.underline_kind,
                    strikethrough: cell.decor.strikethrough,
                    overline: cell.decor.overline,
                },
            });
            push_cell_decorations(
                &mut plan.decorations,
                cell,
                foreground,
                self.metrics.font_size,
            );
        }
        plan
    }

    pub fn select_cells(&mut self, row: u16, cols: Range<u16>) {
        let foreground = self.selection_foreground.unwrap_or(self.default_background);
        let background = self.selection_background.unwrap_or(self.default_foreground);
        for cell in &mut self.cells {
            if cell.y == row && cols.contains(&cell.x) {
                cell.selection = RendererSelectionIntent::Selected {
                    foreground,
                    background,
                };
            }
        }
    }

    pub fn repaint_decision(&self) -> RendererRepaintDecision {
        match self.source_dirty {
            Dirty::Full | Dirty::Partial => RendererRepaintDecision::RedrawNow,
            Dirty::Clean if self.cursor.is_some_and(|cursor| cursor.blinking) => {
                RendererRepaintDecision::ScheduleBlink
            }
            Dirty::Clean => RendererRepaintDecision::Idle,
        }
    }

    pub fn link_cell_map(
        &self,
        links: &[RendererLinkPattern],
        hover: Option<RendererCellPoint>,
        mods: RendererLinkMods,
    ) -> RendererLinkCellMap {
        let mut text = String::new();
        let mut byte_to_cell = Vec::new();
        for row in &self.rows {
            for cell in &self.cells[row.cells.clone()] {
                for ch in cell.text.chars() {
                    text.push(ch);
                    for _ in 0..ch.len_utf8() {
                        byte_to_cell.push(RendererCellPoint {
                            x: cell.x,
                            y: cell.y,
                        });
                    }
                }
            }
        }

        let mut result = RendererLinkCellMap::default();
        for link in links {
            if !link.highlight.applies(hover, mods) {
                continue;
            }
            let mut offset = 0;
            while offset < text.len() {
                let Some(found) = text[offset..].find(&link.pattern) else {
                    break;
                };
                let start = offset + found;
                let end = start + link.pattern.len();
                offset = end;

                let matched_cells = &byte_to_cell[start..end];
                if !link.highlight.allows_cells(matched_cells, hover) {
                    continue;
                }
                result.cells.extend(matched_cells.iter().copied());
            }
        }
        result
    }

    fn resolved_cell_colors(&self, cell: &RendererCell) -> (PlanColor, PlanColor) {
        if let RendererSelectionIntent::Selected {
            foreground,
            background,
        } = cell.selection
        {
            return (foreground, background);
        }

        let mut foreground = cell.foreground.unwrap_or(self.default_foreground);
        let mut background = cell.background.unwrap_or(self.default_background);
        if cell.style.inverse {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.style.faint {
            foreground = foreground.gamma_multiply(0.62);
        }
        (
            cell.minimum_contrast_policy
                .resolve_foreground(foreground, background),
            background,
        )
    }

    fn requires_surface_adjustment(&self) -> bool {
        self.cells.iter().any(|cell| {
            if matches!(cell.selection, RendererSelectionIntent::Selected { .. }) {
                return true;
            }
            let mut foreground = cell.foreground.unwrap_or(self.default_foreground);
            let mut background = cell.background.unwrap_or(self.default_background);
            if cell.style.inverse {
                std::mem::swap(&mut foreground, &mut background);
            }
            if cell.style.faint {
                foreground = foreground.gamma_multiply(0.62);
            }
            cell.minimum_contrast_policy
                .resolve_foreground(foreground, background)
                != foreground
        })
    }
}

impl RendererLinkHighlight {
    fn applies(self, hover: Option<RendererCellPoint>, mods: RendererLinkMods) -> bool {
        match self {
            Self::Always => true,
            Self::AlwaysWithMods(required) => mods == required,
            Self::Hover => hover.is_some(),
            Self::HoverWithMods(required) => hover.is_some() && mods == required,
        }
    }

    fn allows_cells(self, cells: &[RendererCellPoint], hover: Option<RendererCellPoint>) -> bool {
        match self {
            Self::Always | Self::AlwaysWithMods(_) => true,
            Self::Hover | Self::HoverWithMods(_) => {
                hover.is_some_and(|point| cells.contains(&point))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RendererFrameMetrics {
    pub viewport: SurfaceRect,
    pub grid: SurfaceRect,
    pub cell: CellMetrics,
    pub padding: TerminalPadding,
    pub font_size: f32,
}

impl RendererFrameMetrics {
    fn cell_rect(self, col: u16, row: u16) -> SurfaceRect {
        SurfaceRect::from_min_size(
            self.grid.min_x + f32::from(col) * self.cell.width,
            self.grid.min_y + f32::from(row) * self.cell.height,
            self.cell.width,
            self.cell.height,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RendererRow {
    pub y: u16,
    pub cells: Range<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RendererCell {
    pub x: u16,
    pub y: u16,
    pub rect: SurfaceRect,
    pub text: String,
    pub foreground: Option<PlanColor>,
    pub background: Option<PlanColor>,
    pub style: RendererTextStyle,
    pub decor: RendererDecorIntent,
    pub selection: RendererSelectionIntent,
    pub graphics: RendererCellGraphics,
    pub minimum_contrast_policy: MinimumContrastPolicy,
    pub invisible: bool,
}

impl RendererCell {
    fn from_terminal(
        x: u16,
        y: u16,
        text: String,
        foreground: Option<RgbColor>,
        background: Option<RgbColor>,
        style: CellStyle,
        rect: SurfaceRect,
    ) -> Self {
        let graphics = RendererCellGraphics::classify(&text);
        Self {
            x,
            y,
            rect,
            text,
            foreground: foreground.map(PlanColor::opaque),
            background: background.map(PlanColor::opaque),
            style: RendererTextStyle::from_style(style),
            decor: RendererDecorIntent::from_style(style),
            selection: RendererSelectionIntent::None,
            minimum_contrast_policy: MinimumContrastPolicy::for_graphics(graphics),
            graphics,
            invisible: style.invisible,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererTextStyle {
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub blink: bool,
    pub inverse: bool,
}

impl RendererTextStyle {
    fn from_style(style: CellStyle) -> Self {
        Self {
            bold: style.bold,
            italic: style.italic,
            faint: style.faint,
            blink: style.blink,
            inverse: style.inverse,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererDecorIntent {
    pub underline: bool,
    pub underline_kind: Underline,
    pub strikethrough: bool,
    pub overline: bool,
}

impl Default for RendererDecorIntent {
    fn default() -> Self {
        Self {
            underline: false,
            underline_kind: Underline::None,
            strikethrough: false,
            overline: false,
        }
    }
}

impl RendererDecorIntent {
    fn from_style(style: CellStyle) -> Self {
        Self {
            underline: style.underline != Underline::None,
            underline_kind: style.underline,
            strikethrough: style.strikethrough,
            overline: style.overline,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererSelectionIntent {
    None,
    Selected {
        foreground: PlanColor,
        background: PlanColor,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererCellGraphics {
    Text,
    Ghostty(GhosttyGraphicsElement),
}

pub fn renderer_cell_constraint_width(cells: &[RendererCell], x: usize, cols: usize) -> u16 {
    let Some(cell) = cells.get(x) else {
        return 1;
    };
    let grid_width = text_cell_width(&cell.text);
    if grid_width > 1 {
        return grid_width;
    }

    let Some(ch) = single_cell_char(cell) else {
        return grid_width;
    };
    if !is_symbol_like(ch) {
        return grid_width;
    }
    if x == cols.saturating_sub(1) {
        return 1;
    }
    if x > 0
        && let Some(previous) = single_cell_char(&cells[x - 1])
        && is_symbol_like(previous)
        && !is_graphics_element(previous)
    {
        return 1;
    }
    let Some(next) = cells.get(x + 1).and_then(single_cell_char) else {
        return 2;
    };
    if next == ' ' || next == '\u{2002}' {
        return 2;
    }

    1
}

fn single_cell_char(cell: &RendererCell) -> Option<char> {
    let mut chars = cell.text.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(ch)
}

fn is_symbol_like(ch: char) -> bool {
    matches!(
        ch,
        '\u{2190}'..='\u{21FF}'
            | '\u{2460}'..='\u{24FF}'
            | '\u{25A0}'..='\u{25FF}'
            | '\u{2600}'..='\u{27BF}'
            | '\u{1F300}'..='\u{1FAFF}'
            | '\u{E000}'..='\u{F8FF}'
            | '\u{F0000}'..='\u{FFFFD}'
            | '\u{100000}'..='\u{10FFFD}'
    )
}

fn is_graphics_element(ch: char) -> bool {
    GhosttyGraphicsElement::classify(ch).is_some()
}

impl RendererCellGraphics {
    fn classify(text: &str) -> Self {
        let mut chars = text.chars();
        let Some(ch) = chars.next() else {
            return Self::Text;
        };
        if chars.next().is_some() {
            return Self::Text;
        }
        GhosttyGraphicsElement::classify(ch).map_or(Self::Text, Self::Ghostty)
    }

    pub fn is_text(self) -> bool {
        matches!(self, Self::Text)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhosttyGraphicsElement {
    Block,
    Shade,
    Quadrant,
    BoxDrawing,
    Powerline,
    Braille,
    GeometricShape,
    LegacyComputing,
}

impl GhosttyGraphicsElement {
    pub fn classify(ch: char) -> Option<Self> {
        match ch {
            '▀'..='▐' | '▔' | '▕' => Some(Self::Block),
            '░' | '▒' | '▓' => Some(Self::Shade),
            '▖'..='▟' => Some(Self::Quadrant),
            '─'..='╿' => Some(Self::BoxDrawing),
            '\u{E0B0}'..='\u{E0BF}' => Some(Self::Powerline),
            '\u{2800}'..='\u{28FF}' => Some(Self::Braille),
            '\u{25A0}'..='\u{25FF}' => Some(Self::GeometricShape),
            '\u{1FB00}'..='\u{1FBFF}' => Some(Self::LegacyComputing),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinimumContrastPolicy {
    EnforceForText,
    SkipForGraphicsElement,
}

impl MinimumContrastPolicy {
    fn for_graphics(graphics: RendererCellGraphics) -> Self {
        match graphics {
            RendererCellGraphics::Text => Self::EnforceForText,
            RendererCellGraphics::Ghostty(_) => Self::SkipForGraphicsElement,
        }
    }

    pub fn resolve_foreground(self, foreground: PlanColor, background: PlanColor) -> PlanColor {
        if self == Self::SkipForGraphicsElement || contrast_distance(foreground, background) >= 96 {
            return foreground;
        }
        let light = PlanColor {
            r: 255,
            g: 255,
            b: 255,
            a: foreground.a,
        };
        let dark = PlanColor {
            r: 0,
            g: 0,
            b: 0,
            a: foreground.a,
        };
        if contrast_distance(light, background) >= contrast_distance(dark, background) {
            light
        } else {
            dark
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererRepaintDecision {
    RedrawNow,
    ScheduleBlink,
    Idle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RendererCursorState {
    pub in_viewport: bool,
    pub visible: bool,
    pub visual_style: RendererCursorShape,
    pub blinking: bool,
}

impl Default for RendererCursorState {
    fn default() -> Self {
        Self {
            in_viewport: true,
            visible: true,
            visual_style: RendererCursorShape::Block,
            blinking: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererCursorOptions {
    pub preedit: bool,
    pub focused: bool,
    pub blink_visible: bool,
}

pub fn renderer_cursor_shape(
    state: RendererCursorState,
    options: RendererCursorOptions,
) -> Option<RendererCursorShape> {
    if !state.in_viewport {
        return None;
    }
    if options.preedit {
        return Some(RendererCursorShape::Block);
    }
    if !state.visible {
        return None;
    }
    if !options.focused {
        return Some(RendererCursorShape::HollowBlock);
    }
    if state.blinking && !options.blink_visible {
        return None;
    }

    Some(state.visual_style)
}

fn contrast_distance(left: PlanColor, right: PlanColor) -> u16 {
    let dr = i16::from(left.r) - i16::from(right.r);
    let dg = i16::from(left.g) - i16::from(right.g);
    let db = i16::from(left.b) - i16::from(right.b);
    dr.unsigned_abs() + dg.unsigned_abs() + db.unsigned_abs()
}

fn push_cell_decorations(
    decorations: &mut Vec<DecorationLine>,
    cell: &RendererCell,
    color: PlanColor,
    font_size: f32,
) {
    if cell.decor.underline_kind != Underline::None {
        decorations.push(DecorationLine {
            start_x: cell.rect.min_x,
            start_y: cell.rect.min_y + font_size + 3.0,
            end_x: cell.rect.max_x,
            end_y: cell.rect.min_y + font_size + 3.0,
            color,
            style: decoration_style_for_underline(cell.decor.underline_kind),
        });
    }
    if cell.decor.strikethrough {
        decorations.push(DecorationLine {
            start_x: cell.rect.min_x,
            start_y: cell.rect.min_y + cell.rect.height() * 0.55,
            end_x: cell.rect.max_x,
            end_y: cell.rect.min_y + cell.rect.height() * 0.55,
            color,
            style: DecorationStyle::Strikethrough,
        });
    }
    if cell.decor.overline {
        decorations.push(DecorationLine {
            start_x: cell.rect.min_x,
            start_y: cell.rect.min_y + 2.0,
            end_x: cell.rect.max_x,
            end_y: cell.rect.min_y + 2.0,
            color,
            style: DecorationStyle::Overline,
        });
    }
}

fn text_cell_width(text: &str) -> u16 {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0) as u16)
        .sum::<u16>()
        .max(1)
}

fn decoration_style_for_underline(underline: Underline) -> DecorationStyle {
    match underline {
        Underline::None | Underline::Single => DecorationStyle::Single,
        Underline::Double => DecorationStyle::Double,
        Underline::Curly => DecorationStyle::Curly,
        Underline::Dotted => DecorationStyle::Dotted,
        Underline::Dashed => DecorationStyle::Dashed,
        _ => DecorationStyle::Single,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RendererCursor {
    pub x: u16,
    pub y: u16,
    pub at_wide_tail: bool,
    pub shape: RendererCursorShape,
    pub blinking: bool,
    pub color: PlanColor,
}

impl RendererCursor {
    fn from_terminal(
        cursor: CursorSnapshot,
        frame_cursor_color: Option<RgbColor>,
        default_foreground: PlanColor,
    ) -> Self {
        Self {
            x: cursor.x,
            y: cursor.y,
            at_wide_tail: cursor.at_wide_tail,
            shape: RendererCursorShape::from_terminal(cursor),
            blinking: cursor.blinking,
            color: cursor
                .color
                .or(frame_cursor_color)
                .map_or(default_foreground, PlanColor::opaque),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererCursorShape {
    Block,
    HollowBlock,
    Bar,
    Underline,
}

impl RendererCursorShape {
    fn from_terminal(cursor: CursorSnapshot) -> Self {
        match cursor.style {
            libghostty_vt::render::CursorVisualStyle::Bar => Self::Bar,
            libghostty_vt::render::CursorVisualStyle::Underline => Self::Underline,
            libghostty_vt::render::CursorVisualStyle::BlockHollow => Self::HollowBlock,
            libghostty_vt::render::CursorVisualStyle::Block => Self::Block,
            _ => Self::Block,
        }
    }

    pub fn to_plan(self) -> CursorShape {
        match self {
            Self::Block => CursorShape::Block,
            Self::HollowBlock => CursorShape::HollowBlock,
            Self::Bar => CursorShape::Bar,
            Self::Underline => CursorShape::Underline,
        }
    }
}
