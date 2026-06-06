use libghostty_vt::{
    render::CursorVisualStyle,
    style::{RgbColor, Underline},
};
use unicode_width::UnicodeWidthChar;

use crate::{
    geometry::{SurfaceRect, TerminalSurface},
    terminal::{RenderCell, RenderFrame},
};

const TEXT_Y_OFFSET: f32 = 2.0;

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
}

impl PaintPlanner {
    pub fn plan(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
    ) -> &TerminalPaintPlan {
        self.plan_with_cursor_blink_phase(surface, frame, font_size, CursorBlinkPhase::visible())
    }

    pub fn plan_with_cursor_blink_phase(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        font_size: f32,
        cursor_blink_phase: CursorBlinkPhase,
    ) -> &TerminalPaintPlan {
        let default_bg = PlanColor::opaque(frame.colors.background);
        let default_fg = PlanColor::opaque(frame.colors.foreground);
        self.clear_plan(surface.grid_rect(frame.cols, frame.rows), default_bg);

        self.plan_backgrounds(surface, frame, default_fg, default_bg);
        self.plan_text_runs(surface, frame, default_fg, default_bg, font_size);
        self.plan_cursor(surface, frame, default_fg, default_bg, cursor_blink_phase);

        &self.plan
    }

    fn clear_plan(&mut self, surface: SurfaceRect, default_background: PlanColor) {
        for run in self.plan.text_runs.drain(..) {
            let mut text = run.text;
            text.clear();
            self.run_text_pool.push(text);
        }
        self.plan.surface = surface;
        self.plan.default_background = default_background;
        self.plan.backgrounds.clear();
        self.plan.decorations.clear();
        self.plan.cursor = None;
    }

    fn take_run_text(&mut self) -> String {
        self.run_text_pool.pop().unwrap_or_default()
    }

    fn plan_backgrounds(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        default_fg: PlanColor,
        default_bg: PlanColor,
    ) {
        for cell in &frame.cells {
            let (_, bg) = cell_colors(cell, default_fg, default_bg);
            if bg != default_bg {
                self.push_background(surface.cell_rect(cell.x, cell.y), bg);
            }
        }
    }

    fn push_background(&mut self, rect: SurfaceRect, color: PlanColor) {
        if let Some(last) = self.plan.backgrounds.last_mut()
            && last.color == color
            && last.rect.min_y == rect.min_y
            && last.rect.max_y == rect.max_y
            && (last.rect.max_x - rect.min_x).abs() <= f32::EPSILON
        {
            last.rect.max_x = rect.max_x;
            return;
        }

        self.plan.backgrounds.push(BackgroundRect { rect, color });
    }

    fn plan_text_runs(
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        default_fg: PlanColor,
        default_bg: PlanColor,
        font_size: f32,
    ) {
        let mut cell_index = 0;
        while cell_index < frame.cells.len() {
            let first = &frame.cells[cell_index];
            let attrs = paint_attrs(first, default_fg, default_bg);
            let first_text = frame.cell_text(first);

            if first.style.invisible || first_text.is_empty() {
                cell_index += 1;
                continue;
            }

            let mut run_text = self.take_run_text();
            run_text.clear();
            run_text.extend(first_text);

            let start_x = first.x;
            let start_y = first.y;
            let mut end_x = first.x + cell_text_width(first_text);
            let mut next_index = cell_index + 1;

            while let Some(next) = frame.cells.get(next_index) {
                let next_text = frame.cell_text(next);
                if next.y != start_y
                    || next.x != end_x
                    || next.style.invisible
                    || next_text.is_empty()
                    || paint_attrs(next, default_fg, default_bg) != attrs
                {
                    break;
                }

                run_text.extend(next_text);
                end_x += cell_text_width(next_text);
                next_index += 1;
            }

            let rect = surface.run_rect(start_x, start_y, end_x - start_x);
            self.plan.text_runs.push(TextRun {
                rect,
                cells: end_x - start_x,
                text: run_text,
                attrs,
            });

            self.plan_decorations(rect, attrs, font_size);
            cell_index = next_index;
        }
    }

    fn plan_decorations(&mut self, rect: SurfaceRect, attrs: TextAttrs, font_size: f32) {
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
            self.plan.decorations.push(DecorationLine {
                start_x: rect.min_x,
                start_y: rect.min_y + font_size + 3.0,
                end_x: rect.max_x,
                end_y: rect.min_y + font_size + 3.0,
                color: attrs.fg,
                style,
            });
        }
        if attrs.strikethrough {
            self.plan.decorations.push(DecorationLine {
                start_x: rect.min_x,
                start_y: rect.min_y + rect.height() * 0.55,
                end_x: rect.max_x,
                end_y: rect.min_y + rect.height() * 0.55,
                color: attrs.fg,
                style: DecorationStyle::Strikethrough,
            });
        }
        if attrs.overline {
            self.plan.decorations.push(DecorationLine {
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
        &mut self,
        surface: TerminalSurface,
        frame: &RenderFrame,
        default_fg: PlanColor,
        default_bg: PlanColor,
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
                    rect,
                    text,
                    color: frame
                        .colors
                        .cursor_text
                        .map(PlanColor::opaque)
                        .unwrap_or_else(|| {
                            cursor_text_color(cell_bg, color, default_fg, default_bg)
                        }),
                })
            })
        } else {
            None
        };

        self.plan.cursor = Some(CursorPlan {
            rect,
            color,
            shape,
            text_under_cursor,
        });
    }
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

pub fn text_baseline_y(rect: SurfaceRect) -> f32 {
    rect.min_y + TEXT_Y_OFFSET
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

fn paint_attrs(cell: &RenderCell, default_fg: PlanColor, default_bg: PlanColor) -> TextAttrs {
    let (fg, _) = cell_colors(cell, default_fg, default_bg);
    TextAttrs {
        fg,
        bold: cell.style.bold,
        italic: cell.style.italic,
        underline: cell.style.underline,
        strikethrough: cell.style.strikethrough,
        overline: cell.style.overline,
    }
}

fn cell_text_width(text: &[char]) -> u16 {
    text.iter()
        .map(|ch| UnicodeWidthChar::width(*ch).unwrap_or(0) as u16)
        .sum::<u16>()
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        geometry::{CellMetrics, TerminalPadding},
        terminal::{CellStyle, CursorSnapshot, FrameColors, RenderCell, RenderFrame},
    };
    use eframe::egui::Vec2;
    use libghostty_vt::{render::Dirty, style::Underline};
    use proptest::prelude::*;

    fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
        RgbColor { r, g, b }
    }

    fn style() -> CellStyle {
        CellStyle {
            bold: false,
            italic: false,
            faint: false,
            blink: false,
            inverse: false,
            invisible: false,
            strikethrough: false,
            overline: false,
            underline: Underline::None,
        }
    }

    fn frame_from_cells(cells: Vec<(u16, u16, char, CellStyle)>) -> RenderFrame {
        let mut frame = RenderFrame {
            cols: 10,
            rows: 2,
            dirty: Dirty::Full,
            colors: FrameColors {
                background: rgb(0, 0, 0),
                foreground: rgb(255, 255, 255),
                cursor: None,
                ..Default::default()
            },
            cursor: None,
            row_dirty: vec![true, true],
            cells: Vec::new(),
            text: Vec::new(),
            images: Default::default(),
            scrollbar: None,
            stats: Default::default(),
        };

        for (x, y, ch, style) in cells {
            let text_start = frame.text.len();
            frame.text.push(ch);
            frame.cells.push(RenderCell {
                x,
                y,
                text_start,
                text_len: 1,
                fg: None,
                bg: None,
                style,
            });
        }

        frame
    }

    fn surface() -> TerminalSurface {
        TerminalSurface::for_size(
            Vec2::new(200.0, 80.0),
            CellMetrics::new(10.0, 20.0),
            TerminalPadding::uniform(5.0),
        )
    }

    #[test]
    fn adjacent_cells_with_same_attrs_merge_into_one_text_run() {
        let frame = frame_from_cells(vec![
            (0, 0, 'a', style()),
            (1, 0, 'b', style()),
            (2, 0, 'c', style()),
        ]);
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.text_runs.len(), 1);
        assert_eq!(plan.text_runs[0].text, "abc");
    }

    #[test]
    fn planner_reuses_text_run_string_capacity_across_plans() {
        let long = frame_from_cells(vec![
            (0, 0, 'a', style()),
            (1, 0, 'b', style()),
            (2, 0, 'c', style()),
            (3, 0, 'd', style()),
            (4, 0, 'e', style()),
            (5, 0, 'f', style()),
        ]);
        let short = frame_from_cells(vec![(0, 0, 'z', style())]);
        let mut planner = PaintPlanner::default();

        let long_capacity = planner.plan(surface(), &long, 16.0).text_runs[0]
            .text
            .capacity();
        let short_capacity = planner.plan(surface(), &short, 16.0).text_runs[0]
            .text
            .capacity();

        assert!(
            short_capacity >= long_capacity,
            "{short_capacity} < {long_capacity}"
        );
    }

    #[test]
    fn adjacent_cells_with_same_background_merge_into_one_rect() {
        let mut frame = frame_from_cells(vec![
            (0, 0, ' ', style()),
            (1, 0, ' ', style()),
            (2, 0, ' ', style()),
        ]);
        for cell in &mut frame.cells {
            cell.bg = Some(rgb(10, 20, 30));
        }

        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.backgrounds.len(), 1);
        assert_eq!(plan.backgrounds[0].rect, surface().run_rect(0, 0, 3));
    }

    #[test]
    fn style_changes_split_text_runs() {
        let mut bold = style();
        bold.bold = true;
        let frame = frame_from_cells(vec![(0, 0, 'a', style()), (1, 0, 'b', bold)]);
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.text_runs.len(), 2);
        assert_eq!(plan.text_runs[0].text, "a");
        assert_eq!(plan.text_runs[1].text, "b");
    }

    #[test]
    fn cursor_shape_is_planned_without_egui_painter() {
        let mut frame = frame_from_cells(vec![(0, 0, 'a', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Bar,
            blinking: false,
            color: Some(rgb(1, 2, 3)),
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        let cursor = plan.cursor.as_ref().unwrap();
        assert_eq!(cursor.shape, CursorShape::Bar);
        assert_eq!(cursor.color, PlanColor::opaque(rgb(1, 2, 3)));
    }

    #[test]
    fn bar_cursor_fill_rect_stays_visible() {
        let rect = SurfaceRect::from_min_size(20.0, 40.0, 8.0, 18.0);
        let bar = cursor_fill_rect(CursorShape::Bar, rect);

        assert!(bar.width() >= 1.0);
        assert_eq!(bar.min_x, rect.min_x - 1.0);
        assert_eq!(bar.min_y, rect.min_y);
        assert_eq!(bar.max_y, rect.max_y);
    }

    #[test]
    fn underline_cursor_fill_rect_hugs_bottom_edge() {
        let rect = SurfaceRect::from_min_size(20.0, 40.0, 8.0, 18.0);
        let underline = cursor_fill_rect(CursorShape::Underline, rect);

        assert_eq!(underline.max_y, rect.max_y);
        assert!(underline.height() >= 1.0);
    }

    #[test]
    fn cursor_at_wide_tail_covers_the_wide_character_cells() {
        let mut frame = frame_from_cells(vec![(0, 0, '界', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: true,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: None,
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(
            plan.cursor.as_ref().unwrap().rect,
            surface().run_rect(0, 0, 2)
        );
    }

    #[test]
    fn block_cursor_carries_text_redraw_for_cell_under_cursor() {
        let mut frame = frame_from_cells(vec![(1, 0, 'x', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: Some(rgb(10, 20, 30)),
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(
            plan.cursor.as_ref().unwrap().text_under_cursor,
            Some(CursorTextPlan {
                rect: surface().cell_rect(1, 0),
                text: "x".to_owned(),
                color: plan.default_background,
            })
        );
    }

    #[test]
    fn block_cursor_text_redraw_uses_cell_background_for_contrast() {
        let mut frame = frame_from_cells(vec![(1, 0, 'x', style())]);
        frame.cells[0].bg = Some(rgb(20, 30, 40));
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: Some(rgb(220, 220, 220)),
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(
            plan.cursor
                .as_ref()
                .unwrap()
                .text_under_cursor
                .as_ref()
                .unwrap()
                .color,
            PlanColor::opaque(rgb(20, 30, 40))
        );
    }

    #[test]
    fn block_cursor_does_not_redraw_invisible_text() {
        let mut invisible = style();
        invisible.invisible = true;
        let mut frame = frame_from_cells(vec![(1, 0, 'x', invisible)]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: None,
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.cursor.as_ref().unwrap().text_under_cursor, None);
    }

    #[test]
    fn inverse_block_cursor_redraw_contrasts_with_cursor_fill() {
        let mut inverse = style();
        inverse.inverse = true;
        let mut frame = frame_from_cells(vec![(1, 0, 'x', inverse)]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: false,
            color: None,
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(
            plan.cursor
                .as_ref()
                .unwrap()
                .text_under_cursor
                .as_ref()
                .unwrap()
                .color,
            plan.default_background
        );
    }

    #[test]
    fn hollow_block_cursor_keeps_text_visible_without_redraw_overlay() {
        let mut frame = frame_from_cells(vec![(1, 0, 'x', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::BlockHollow,
            blinking: false,
            color: None,
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);
        let cursor = plan.cursor.as_ref().unwrap();

        assert_eq!(cursor.shape, CursorShape::HollowBlock);
        assert_eq!(cursor.text_under_cursor, None);
    }

    #[test]
    fn hidden_blink_phase_does_not_plan_a_cursor() {
        let mut frame = frame_from_cells(vec![(1, 0, 'x', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: true,
            color: None,
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan_with_cursor_blink_phase(
            surface(),
            &frame,
            16.0,
            CursorBlinkPhase::hidden(),
        );

        assert_eq!(plan.cursor, None);
    }

    #[test]
    fn blinking_block_cursor_and_text_under_cursor_use_phase_opacity() {
        let mut frame = frame_from_cells(vec![(1, 0, 'x', style())]);
        frame.cursor = Some(CursorSnapshot {
            x: 1,
            y: 0,
            at_wide_tail: false,
            style: CursorVisualStyle::Block,
            blinking: true,
            color: Some(rgb(20, 30, 40)),
        });
        let mut planner = PaintPlanner::default();
        let plan = planner.plan_with_cursor_blink_phase(
            surface(),
            &frame,
            16.0,
            CursorBlinkPhase::from_opacity(0.5),
        );

        let cursor = plan.cursor.as_ref().expect("half-opacity cursor");

        assert_eq!(cursor.color.a, 128);
        assert_eq!(
            cursor
                .text_under_cursor
                .as_ref()
                .expect("cursor text")
                .color
                .a,
            128
        );
    }

    #[test]
    fn terminal_symbol_glyphs_remain_in_text_runs() {
        let frame = frame_from_cells(vec![
            (0, 0, '█', style()),
            (1, 0, '▓', style()),
            (2, 0, '▒', style()),
            (3, 0, '░', style()),
            (4, 0, '│', style()),
            (5, 0, '\u{E0B6}', style()),
            (6, 0, '\u{E0B4}', style()),
            (7, 0, '\u{E0B0}', style()),
            (8, 0, '\u{E0B2}', style()),
        ]);
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.text_runs.len(), 1);
        assert_eq!(
            plan.text_runs[0].text,
            "█▓▒░│\u{E0B6}\u{E0B4}\u{E0B0}\u{E0B2}"
        );
    }

    #[test]
    fn wide_text_run_covers_the_cells_consumed_by_the_glyph() {
        let frame = frame_from_cells(vec![(0, 0, '界', style())]);
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.text_runs[0].text, "界");
        assert_eq!(plan.text_runs[0].cells, 2);
        assert_eq!(plan.text_runs[0].rect, surface().run_rect(0, 0, 2));
    }

    #[test]
    fn text_baseline_uses_stable_cell_top_offset() {
        let rect = SurfaceRect::from_min_size(10.0, 20.0, 80.0, 24.0);

        assert_eq!(text_baseline_y(rect), rect.min_y + TEXT_Y_OFFSET);
    }

    #[test]
    fn plan_surface_tracks_frame_grid_not_transient_window_size() {
        let frame = frame_from_cells(vec![(0, 0, 'a', style())]);
        let mut planner = PaintPlanner::default();
        let plan = planner.plan(surface(), &frame, 16.0);

        assert_eq!(plan.surface, surface().grid_rect(frame.cols, frame.rows));
        assert!(plan.surface.width() < surface().surface_rect().width());
    }

    proptest! {
        #[test]
        fn same_style_contiguous_cells_create_one_run(bytes in proptest::collection::vec(b'a'..=b'z', 1..40)) {
            let chars = bytes.into_iter().map(char::from).collect::<Vec<_>>();
            let cells = chars
                .iter()
                .enumerate()
                .map(|(index, ch)| (index as u16, 0, *ch, style()))
                .collect();
            let expected = chars.iter().collect::<String>();
            let frame = frame_from_cells(cells);
            let mut planner = PaintPlanner::default();
            let plan = planner.plan(surface(), &frame, 16.0);

            prop_assert_eq!(plan.text_runs.len(), 1);
            prop_assert_eq!(plan.text_runs[0].text.as_str(), expected.as_str());
        }
    }
}
