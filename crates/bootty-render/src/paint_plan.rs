use libghostty_vt::{
    render::CursorVisualStyle,
    style::{RgbColor, Underline},
};

use crate::{
    geometry::{SurfaceRect, TerminalSurface},
    terminal::{FrameSelection, RenderCell, RenderFrame},
};

const TEXT_Y_OFFSET: f32 = 2.0;
const OVERLAY_SELECTION: u8 = 1;
const OVERLAY_ACTIVE_SEARCH: u8 = 2;
const OVERLAY_SEARCH: u8 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlanColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl PlanColor {
    pub fn opaque(color: RgbColor) -> Self {
        Self {
            r: color.r,
            g: color.g,
            b: color.b,
            a: 255,
        }
    }

    pub fn gamma_multiply(self, factor: f32) -> Self {
        Self {
            r: ((f32::from(self.r) * factor).round()).clamp(0.0, 255.0) as u8,
            g: ((f32::from(self.g) * factor).round()).clamp(0.0, 255.0) as u8,
            b: ((f32::from(self.b) * factor).round()).clamp(0.0, 255.0) as u8,
            a: self.a,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextAttrs {
    pub fg: PlanColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub strikethrough: bool,
    pub overline: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundRect {
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub rect: SurfaceRect,
    pub cell_rect: SurfaceRect,
    pub cells: u16,
    pub text: String,
    pub attrs: TextAttrs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationLine {
    pub start_x: f32,
    pub start_y: f32,
    pub end_x: f32,
    pub end_y: f32,
    pub color: PlanColor,
    pub style: DecorationStyle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecorationStyle {
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
    Strikethrough,
    Overline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    HollowBlock,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorBlinkPhase {
    opacity: f32,
}

impl CursorBlinkPhase {
    pub const fn visible() -> Self {
        Self { opacity: 1.0 }
    }

    pub const fn hidden() -> Self {
        Self { opacity: 0.0 }
    }

    pub fn from_opacity(opacity: f32) -> Self {
        Self {
            opacity: opacity.clamp(0.0, 1.0),
        }
    }

    pub fn opacity(self) -> f32 {
        self.opacity
    }

    fn alpha(self) -> u8 {
        (self.opacity * 255.0).round().clamp(0.0, 255.0) as u8
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorPlan {
    pub rect: SurfaceRect,
    pub color: PlanColor,
    pub shape: CursorShape,
    pub text_under_cursor: Option<CursorTextPlan>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorTextPlan {
    pub rect: SurfaceRect,
    pub text: String,
    pub color: PlanColor,
}

pub fn cursor_fill_rect(shape: CursorShape, rect: SurfaceRect) -> SurfaceRect {
    match shape {
        CursorShape::Bar => {
            let width = rect.width().clamp(1.0, 2.0);
            SurfaceRect::from_min_size(
                rect.min_x - ((width + 1.0) * 0.5).floor(),
                rect.min_y,
                width,
                rect.height(),
            )
        }
        CursorShape::Underline => SurfaceRect::from_min_size(
            rect.min_x,
            (rect.max_y - 2.0).max(rect.min_y),
            rect.width(),
            2.0_f32.min(rect.height()).max(1.0),
        ),
        CursorShape::Block | CursorShape::HollowBlock => rect,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalPaintPlan {
    pub surface: SurfaceRect,
    pub default_background: PlanColor,
    pub backgrounds: Vec<BackgroundRect>,
    pub text_runs: Vec<TextRun>,
    pub decorations: Vec<DecorationLine>,
    pub cursor: Option<CursorPlan>,
}

impl Default for TerminalPaintPlan {
    fn default() -> Self {
        Self {
            surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
            default_background: PlanColor::default(),
            backgrounds: Vec::new(),
            text_runs: Vec::new(),
            decorations: Vec::new(),
            cursor: None,
        }
    }
}

#[derive(Default)]
pub struct PaintPlanner {
    plan: TerminalPaintPlan,
    run_text_pool: Vec<String>,
    overlay_mask: Vec<u8>,
}

impl PaintPlanner {
    pub fn plan(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
    ) -> &TerminalPaintPlan {
        self.plan_with_cursor_blink_phase_and_text_cell_height(
            surface,
            frame,
            font_size,
            surface.cell.height,
            CursorBlinkPhase::visible(),
        )
    }

    pub fn plan_with_minimum_contrast(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
    ) -> &TerminalPaintPlan {
        self.plan_with_options(
            surface,
            frame,
            font_size,
            surface.cell.height,
            CursorBlinkPhase::visible(),
            true,
        )
    }

    pub fn plan_with_cursor_blink_phase_and_text_cell_height(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        text_cell_height: f32,
        cursor_blink_phase: CursorBlinkPhase,
    ) -> &TerminalPaintPlan {
        self.plan_with_options(
            surface,
            frame,
            font_size,
            text_cell_height,
            cursor_blink_phase,
            false,
        )
    }

    fn plan_with_options(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        text_cell_height: f32,
        cursor_blink_phase: CursorBlinkPhase,
        minimum_contrast: bool,
    ) -> &TerminalPaintPlan {
        let default_bg = PlanColor::opaque(frame.colors.background);
        let default_fg = PlanColor::opaque(frame.colors.foreground);
        recycle_plan(
            &mut self.plan,
            &mut self.run_text_pool,
            surface.grid_rect(frame.cols, frame.rows),
            default_bg,
        );

        plan_backgrounds(&mut self.plan, surface, frame, default_fg, default_bg);
        plan_search_matches(&mut self.plan, surface, frame);
        plan_active_search_match(&mut self.plan, surface, frame);
        plan_selections(&mut self.plan, surface, frame, default_fg);
        prepare_overlay_mask(&mut self.overlay_mask, frame);
        plan_text_runs(
            &mut self.plan,
            &mut self.run_text_pool,
            &self.overlay_mask,
            surface,
            frame,
            TextPlanContext {
                default_fg,
                default_bg,
                font_size,
                text_cell_height,
                minimum_contrast,
            },
        );
        plan_cursor(
            &mut self.plan,
            surface,
            frame,
            default_fg,
            default_bg,
            text_cell_height,
            cursor_blink_phase,
        );

        &self.plan
    }
}

fn recycle_plan(
    plan: &mut TerminalPaintPlan,
    pool: &mut Vec<String>,
    surface: SurfaceRect,
    default_background: PlanColor,
) {
    for run in plan.text_runs.drain(..) {
        let mut text = run.text;
        text.clear();
        pool.push(text);
    }
    plan.surface = surface;
    plan.default_background = default_background;
    plan.backgrounds.clear();
    plan.decorations.clear();
    plan.cursor = None;
}

fn plan_backgrounds(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_fg: PlanColor,
    default_bg: PlanColor,
) {
    for cell in &frame.cells {
        if !cell.style.inverse && cell.bg.is_none() {
            continue;
        }
        let bg = cell_background(cell, default_fg, default_bg);
        if bg != default_bg {
            push_background(&mut plan.backgrounds, surface.cell_rect(cell.x, cell.y), bg);
        }
    }
}

fn push_background(backgrounds: &mut Vec<BackgroundRect>, rect: SurfaceRect, color: PlanColor) {
    if let Some(last) = backgrounds.last_mut()
        && last.color == color
        && last.rect.min_y == rect.min_y
        && last.rect.max_y == rect.max_y
        && (last.rect.max_x - rect.min_x).abs() <= f32::EPSILON
    {
        last.rect.max_x = rect.max_x;
        return;
    }

    backgrounds.push(BackgroundRect { rect, color });
}

#[derive(Clone, Copy)]
struct TextPlanContext {
    default_fg: PlanColor,
    default_bg: PlanColor,
    font_size: f32,
    text_cell_height: f32,
    minimum_contrast: bool,
}

fn prepare_overlay_mask(mask: &mut Vec<u8>, frame: &RenderFrame) {
    if frame.selections.is_empty()
        && frame.search_matches.is_empty()
        && frame.active_search_match.is_none()
    {
        mask.clear();
        return;
    }
    mask.resize(usize::from(frame.cols) * usize::from(frame.rows), 0);
    mask.fill(0);
    mark_overlay_ranges(
        mask,
        frame.cols,
        frame.rows,
        &frame.search_matches,
        OVERLAY_SEARCH,
    );
    if let Some(active) = frame.active_search_match {
        mark_overlay_ranges(
            mask,
            frame.cols,
            frame.rows,
            std::slice::from_ref(&active),
            OVERLAY_ACTIVE_SEARCH,
        );
    }
    mark_overlay_ranges(
        mask,
        frame.cols,
        frame.rows,
        &frame.selections,
        OVERLAY_SELECTION,
    );
}

fn mark_overlay_ranges(mask: &mut [u8], cols: u16, rows: u16, ranges: &[FrameSelection], flag: u8) {
    for range in ranges {
        if range.row >= rows || range.start_col >= cols {
            continue;
        }
        let end_col = range.end_col.min(cols - 1);
        if end_col < range.start_col {
            continue;
        }
        let start = usize::from(range.row) * usize::from(cols) + usize::from(range.start_col);
        let end = usize::from(range.row) * usize::from(cols) + usize::from(end_col);
        mask[start..=end].fill(flag);
    }
}

fn cell_overlay(mask: &[u8], cols: u16, cell: &RenderCell) -> u8 {
    if mask.is_empty() {
        return 0;
    }
    mask[usize::from(cell.y) * usize::from(cols) + usize::from(cell.x)]
}

fn plan_text_runs(
    plan: &mut TerminalPaintPlan,
    pool: &mut Vec<String>,
    overlay_mask: &[u8],
    surface: TerminalSurface,
    frame: &RenderFrame,
    context: TextPlanContext,
) {
    let colors = OverlayTextColors {
        selection: selection_text_foreground(frame, context.default_bg),
        search: search_match_text_foreground(),
        active_search: active_search_match_text_foreground(),
    };
    let mut cell_index = 0;
    while cell_index < frame.cells.len() {
        let first = &frame.cells[cell_index];
        let first_text = frame.cell_text(first);

        if first.style.invisible || first_text.is_empty() {
            cell_index += 1;
            continue;
        }

        let attrs = paint_attrs(
            first,
            first_text,
            cell_overlay(overlay_mask, frame.cols, first),
            context.default_fg,
            context.default_bg,
            colors,
            context.minimum_contrast,
        );
        let mut run_text = pool.pop().unwrap_or_default();
        run_text.clear();
        run_text.extend(first_text);

        let start_x = first.x;
        let start_y = first.y;
        let mut end_x = first.x + cell_text_width(first_text);
        let mut next_index = cell_index + 1;

        if !context.minimum_contrast {
            while let Some(next) = frame.cells.get(next_index) {
                let next_text = frame.cell_text(next);
                if next.y != start_y
                    || next.x != end_x
                    || next.style.invisible
                    || next_text.is_empty()
                    || paint_attrs(
                        next,
                        next_text,
                        cell_overlay(overlay_mask, frame.cols, next),
                        context.default_fg,
                        context.default_bg,
                        colors,
                        context.minimum_contrast,
                    ) != attrs
                {
                    break;
                }

                run_text.extend(next_text);
                end_x += cell_text_width(next_text);
                next_index += 1;
            }
        }

        let row_rect = surface.run_rect(start_x, start_y, end_x - start_x);
        let rect = text_rect_for_row(row_rect, context.text_cell_height);
        plan.text_runs.push(TextRun {
            cell_rect: row_rect,
            rect,
            cells: end_x - start_x,
            text: run_text,
            attrs,
        });

        plan_decorations(&mut plan.decorations, rect, attrs, context.font_size);
        cell_index = next_index;
    }
}

fn plan_decorations(
    decorations: &mut Vec<DecorationLine>,
    rect: SurfaceRect,
    attrs: TextAttrs,
    font_size: f32,
) {
    if attrs.underline != Underline::None {
        let style = match attrs.underline {
            Underline::None => unreachable!("none handled above"),
            Underline::Single => DecorationStyle::Single,
            Underline::Double => DecorationStyle::Double,
            Underline::Curly => DecorationStyle::Curly,
            Underline::Dotted => DecorationStyle::Dotted,
            Underline::Dashed => DecorationStyle::Dashed,
            _ => DecorationStyle::Single,
        };
        decorations.push(DecorationLine {
            start_x: rect.min_x,
            start_y: rect.min_y + font_size + 3.0,
            end_x: rect.max_x,
            end_y: rect.min_y + font_size + 3.0,
            color: attrs.fg,
            style,
        });
    }
    if attrs.strikethrough {
        decorations.push(DecorationLine {
            start_x: rect.min_x,
            start_y: rect.min_y + rect.height() * 0.55,
            end_x: rect.max_x,
            end_y: rect.min_y + rect.height() * 0.55,
            color: attrs.fg,
            style: DecorationStyle::Strikethrough,
        });
    }
    if attrs.overline {
        decorations.push(DecorationLine {
            start_x: rect.min_x,
            start_y: rect.min_y + TEXT_Y_OFFSET,
            end_x: rect.max_x,
            end_y: rect.min_y + TEXT_Y_OFFSET,
            color: attrs.fg,
            style: DecorationStyle::Overline,
        });
    }
}

fn plan_cursor(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_fg: PlanColor,
    default_bg: PlanColor,
    text_cell_height: f32,
    cursor_blink_phase: CursorBlinkPhase,
) {
    let Some(cursor) = frame.cursor else {
        return;
    };
    let cursor_alpha = if cursor.blinking {
        cursor_blink_phase.alpha()
    } else {
        255
    };
    if cursor_alpha == 0 {
        return;
    }
    let color = cursor
        .color
        .or(frame.colors.cursor)
        .map_or(default_fg, PlanColor::opaque);
    let color = PlanColor {
        a: cursor_alpha,
        ..color
    };
    let shape = match cursor.style {
        CursorVisualStyle::Bar => CursorShape::Bar,
        CursorVisualStyle::Underline => CursorShape::Underline,
        CursorVisualStyle::BlockHollow => CursorShape::HollowBlock,
        CursorVisualStyle::Block => CursorShape::Block,
        _ => CursorShape::Block,
    };
    let cursor_x = if cursor.at_wide_tail {
        cursor.x.saturating_sub(1)
    } else {
        cursor.x
    };
    let rect = if cursor.at_wide_tail {
        surface.run_rect(cursor_x, cursor.y, 2)
    } else {
        surface.cell_rect(cursor.x, cursor.y)
    };
    let text_under_cursor = if shape == CursorShape::Block {
        cursor_cell(frame, cursor_x, cursor.y).and_then(|cell| {
            if cell.style.invisible {
                return None;
            }
            let text = frame.cell_text(cell).iter().collect::<String>();
            let (_, cell_bg) = cell_colors(cell, default_fg, default_bg);
            (!text.is_empty()).then_some(CursorTextPlan {
                rect: text_rect_for_row(rect, text_cell_height),
                text,
                color: frame
                    .colors
                    .cursor_text
                    .map(PlanColor::opaque)
                    .unwrap_or_else(|| cursor_text_color(cell_bg, color, default_fg, default_bg)),
            })
        })
    } else {
        None
    };

    plan.cursor = Some(CursorPlan {
        rect,
        color,
        shape,
        text_under_cursor,
    });
}

fn text_rect_for_row(row_rect: SurfaceRect, text_cell_height: f32) -> SurfaceRect {
    let height = if text_cell_height.is_finite() && text_cell_height > 0.0 {
        text_cell_height.min(row_rect.height())
    } else {
        row_rect.height()
    };
    let y_offset = ((row_rect.height() - height) * 0.5).max(0.0);
    SurfaceRect::from_min_size(
        row_rect.min_x,
        row_rect.min_y + y_offset,
        row_rect.width(),
        height,
    )
}

fn cursor_cell(frame: &RenderFrame, x: u16, y: u16) -> Option<&RenderCell> {
    let dense_index = usize::from(y)
        .checked_mul(usize::from(frame.cols))
        .and_then(|offset| offset.checked_add(usize::from(x)));
    dense_index
        .and_then(|index| frame.cells.get(index))
        .filter(|cell| cell.x == x && cell.y == y)
        .or_else(|| frame.cells.iter().find(|cell| cell.x == x && cell.y == y))
}

fn cursor_text_color(
    cell_bg: PlanColor,
    cursor_color: PlanColor,
    default_fg: PlanColor,
    default_bg: PlanColor,
) -> PlanColor {
    let mut color = if same_rgb(cell_bg, cursor_color) {
        if same_rgb(default_bg, cursor_color) {
            default_fg
        } else {
            default_bg
        }
    } else {
        cell_bg
    };
    color.a = cursor_color.a;
    color
}

fn same_rgb(left: PlanColor, right: PlanColor) -> bool {
    left.r == right.r && left.g == right.g && left.b == right.b
}

fn cell_colors(
    cell: &RenderCell,
    default_fg: PlanColor,
    default_bg: PlanColor,
) -> (PlanColor, PlanColor) {
    let mut fg = cell.fg.map_or(default_fg, PlanColor::opaque);
    let mut bg = cell.bg.map_or(default_bg, PlanColor::opaque);
    if cell.style.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.style.faint {
        fg = fg.gamma_multiply(0.62);
    }
    (fg, bg)
}

fn plan_selections(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
    default_fg: PlanColor,
) {
    let background = frame
        .colors
        .selection_background
        .map(PlanColor::opaque)
        .unwrap_or(default_fg);
    for selection in &frame.selections {
        plan_frame_selection_background(plan, surface, *selection, background);
    }
}

fn plan_search_matches(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
) {
    let background = search_match_background();
    for selection in &frame.search_matches {
        plan_frame_selection_background(plan, surface, *selection, background);
    }
}

fn plan_active_search_match(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    frame: &RenderFrame,
) {
    let Some(selection) = frame.active_search_match else {
        return;
    };
    plan_frame_selection_background(plan, surface, selection, active_search_match_background());
}

fn plan_frame_selection_background(
    plan: &mut TerminalPaintPlan,
    surface: TerminalSurface,
    selection: FrameSelection,
    background: PlanColor,
) {
    if selection.end_col < selection.start_col {
        return;
    }
    let cells = selection
        .end_col
        .saturating_sub(selection.start_col)
        .saturating_add(1);
    push_background(
        &mut plan.backgrounds,
        surface.run_rect(selection.start_col, selection.row, cells),
        background,
    );
}

pub(crate) fn search_match_background() -> PlanColor {
    PlanColor {
        r: 245,
        g: 194,
        b: 66,
        a: 210,
    }
}

pub(crate) fn search_match_text_foreground() -> PlanColor {
    PlanColor {
        r: 20,
        g: 20,
        b: 20,
        a: 255,
    }
}

pub(crate) fn active_search_match_background() -> PlanColor {
    PlanColor {
        r: 255,
        g: 235,
        b: 120,
        a: 255,
    }
}

pub(crate) fn active_search_match_text_foreground() -> PlanColor {
    PlanColor {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }
}

fn selection_text_foreground(frame: &RenderFrame, default_bg: PlanColor) -> PlanColor {
    frame
        .colors
        .selection_foreground
        .map(PlanColor::opaque)
        .unwrap_or(default_bg)
}

fn cell_background(cell: &RenderCell, default_fg: PlanColor, default_bg: PlanColor) -> PlanColor {
    if cell.style.inverse {
        cell.fg.map_or(default_fg, PlanColor::opaque)
    } else {
        cell.bg.map_or(default_bg, PlanColor::opaque)
    }
}

#[derive(Clone, Copy)]
struct OverlayTextColors {
    selection: PlanColor,
    search: PlanColor,
    active_search: PlanColor,
}

fn paint_attrs(
    cell: &RenderCell,
    text: &[char],
    overlay: u8,
    default_fg: PlanColor,
    default_bg: PlanColor,
    colors: OverlayTextColors,
    minimum_contrast: bool,
) -> TextAttrs {
    let (mut fg, bg) = cell_colors(cell, default_fg, default_bg);
    if minimum_contrast && overlay == 0 {
        fg = adjust_text_contrast(text, fg, bg);
    }
    if overlay == OVERLAY_SELECTION {
        fg = colors.selection;
    } else if overlay == OVERLAY_ACTIVE_SEARCH {
        fg = colors.active_search;
    } else if overlay == OVERLAY_SEARCH {
        fg = colors.search;
    }
    TextAttrs {
        fg,
        bold: cell.style.bold,
        italic: cell.style.italic,
        underline: cell.style.underline,
        strikethrough: cell.style.strikethrough,
        overline: cell.style.overline,
    }
}

fn adjust_text_contrast(text: &[char], foreground: PlanColor, background: PlanColor) -> PlanColor {
    if (text.len() == 1 && is_old_graphics_character(text[0]))
        || contrast_distance(foreground, background) >= 96
    {
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

fn is_old_graphics_character(ch: char) -> bool {
    matches!(
        ch,
        '▀'..='▐'
            | '▔'
            | '▕'
            | '░'
            | '▒'
            | '▓'
            | '▖'..='▟'
            | '─'..='╿'
            | '\u{E0B0}'..='\u{E0BF}'
            | '\u{2800}'..='\u{28FF}'
            | '\u{25A0}'..='\u{25FF}'
            | '\u{1FB00}'..='\u{1FBFF}'
    )
}

fn contrast_distance(left: PlanColor, right: PlanColor) -> u16 {
    let dr = i16::from(left.r) - i16::from(right.r);
    let dg = i16::from(left.g) - i16::from(right.g);
    let db = i16::from(left.b) - i16::from(right.b);
    dr.unsigned_abs() + dg.unsigned_abs() + db.unsigned_abs()
}

fn cell_text_width(text: &[char]) -> u16 {
    crate::terminal_text::terminal_grapheme_cells(text)
}
