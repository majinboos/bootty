use std::sync::Arc;

use crate::{
    geometry::SurfaceRect,
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, DecorationStyle, PlanColor, TerminalPaintPlan,
        TextAttrs, TextRun, cursor_fill_rect,
    },
    terminal_image::{
        KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement,
    },
    terminal_sprite::SpriteGlyph,
    terminal_text::{ResolvedFontFace, TerminalTextContract},
};

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalRenderFrame {
    pub surface: SurfaceRect,
    pub commands: Vec<TerminalRenderCommand>,
}

impl TerminalRenderFrame {
    pub fn background_from_plan(plan: &TerminalPaintPlan) -> Self {
        let mut frame = Self {
            surface: plan.surface,
            commands: Vec::with_capacity(1 + plan.backgrounds.len()),
        };

        frame.push_fill(
            plan.surface,
            plan.default_background,
            FillRole::SurfaceBackground,
        );
        for background in &plan.backgrounds {
            frame.push_background(background);
        }

        frame
    }

    pub fn from_plan(plan: &TerminalPaintPlan, text_contract: &TerminalTextContract) -> Self {
        Self::from_plan_and_images(plan, text_contract, &KittyImageFrame::default())
    }

    pub fn from_plan_and_images(
        plan: &TerminalPaintPlan,
        text_contract: &TerminalTextContract,
        images: &KittyImageFrame,
    ) -> Self {
        let mut frame = Self {
            surface: plan.surface,
            commands: Vec::with_capacity(command_capacity_for_plan(plan, images)),
        };
        // One-shot path: no pooled strings, so every text command allocates fresh.
        frame.populate(plan, text_contract, images, &mut Vec::new());
        frame
    }

    /// Build the command list for `plan` into `self.commands`, drawing text-command
    /// string buffers from `text_pool`. The pool is empty on the one-shot path (each
    /// command allocates fresh) and pre-filled by [`RenderFramePool`] on the reuse
    /// path so a steady stream of frames allocates nothing.
    fn populate(
        &mut self,
        plan: &TerminalPaintPlan,
        text_contract: &TerminalTextContract,
        images: &KittyImageFrame,
        text_pool: &mut Vec<String>,
    ) {
        self.surface = plan.surface;
        self.push_fill(
            plan.surface,
            plan.default_background,
            FillRole::SurfaceBackground,
        );
        self.push_image_layer(images, KittyImageLayer::BelowBackground);
        for background in &plan.backgrounds {
            self.push_background(background);
        }
        self.push_image_layer(images, KittyImageLayer::BelowText);
        for run in &plan.text_runs {
            self.push_text_run(run, text_contract, text_pool);
        }
        self.commands
            .extend(plan.decorations.iter().map(|decoration| {
                TerminalRenderCommand::Decoration(LineCommand {
                    start_x: decoration.start_x,
                    start_y: decoration.start_y,
                    end_x: decoration.end_x,
                    end_y: decoration.end_y,
                    color: decoration.color,
                    style: decoration.style,
                })
            }));
        self.push_image_layer(images, KittyImageLayer::AboveText);
        self.commands.extend(
            images
                .virtual_placements
                .iter()
                .copied()
                .map(TerminalRenderCommand::KittyVirtualPlacement),
        );
        if let Some(cursor) = &plan.cursor {
            self.push_cursor(cursor, text_contract, text_pool);
        }
    }

    fn push_background(&mut self, background: &BackgroundRect) {
        self.push_fill(background.rect, background.color, FillRole::CellBackground);
    }

    fn push_fill(&mut self, rect: SurfaceRect, color: PlanColor, role: FillRole) {
        self.commands
            .push(TerminalRenderCommand::FillRect(FillCommand {
                rect,
                color,
                role,
            }));
    }

    fn push_image_layer(&mut self, images: &KittyImageFrame, layer: KittyImageLayer) {
        self.commands.extend(
            images
                .placements
                .iter()
                .filter(|placement| placement.layer == layer)
                .map(|placement| translate_image_placement(placement, self.surface))
                .map(TerminalRenderCommand::Image),
        );
    }

    fn push_text_run(
        &mut self,
        run: &TextRun,
        text_contract: &TerminalTextContract,
        text_pool: &mut Vec<String>,
    ) {
        let cell_width = run.rect.width() / f32::from(run.cells.max(1));
        if run.text.is_ascii() || !text_contract.has_native_symbol_fragments(&run.text) {
            let face = text_contract.resolve_face_handle_for_run(run);
            self.push_text_fragment(
                run,
                cell_width,
                TextCellSpan {
                    start: 0,
                    width: run.cells,
                },
                &run.text,
                RunFont::new(face, text_contract),
                text_pool,
            );
            return;
        }

        let mut text_start_byte = 0_usize;
        let mut text_start_cell = 0_u16;
        let mut text_active = false;
        let mut cell = 0_u16;
        let mut face = None;

        for (byte_index, ch) in run.text.char_indices() {
            if let Some(glyph) = text_contract.native_symbol_glyph(ch) {
                if text_active {
                    let face = Arc::clone(
                        face.get_or_insert_with(|| text_contract.resolve_face_handle_for_run(run)),
                    );
                    self.push_text_fragment(
                        run,
                        cell_width,
                        TextCellSpan {
                            start: text_start_cell,
                            width: cell.saturating_sub(text_start_cell),
                        },
                        &run.text[text_start_byte..byte_index],
                        RunFont::new(face, text_contract),
                        text_pool,
                    );
                    text_active = false;
                }
                self.push_sprite_fragment(run, cell, glyph);
                cell = cell.saturating_add(crate::terminal_text::terminal_char_cell_delta(ch));
                continue;
            }

            if !text_active {
                text_start_byte = byte_index;
                text_start_cell = cell;
                text_active = true;
            }
            cell = cell.saturating_add(crate::terminal_text::terminal_char_cell_delta(ch));
        }

        if text_active {
            let face = face
                .take()
                .unwrap_or_else(|| text_contract.resolve_face_handle_for_run(run));
            self.push_text_fragment(
                run,
                cell_width,
                TextCellSpan {
                    start: text_start_cell,
                    width: cell.saturating_sub(text_start_cell),
                },
                &run.text[text_start_byte..],
                RunFont::new(face, text_contract),
                text_pool,
            );
        }
    }

    fn push_text_fragment(
        &mut self,
        run: &TextRun,
        cell_width: f32,
        cells: TextCellSpan,
        text: &str,
        font: RunFont,
        text_pool: &mut Vec<String>,
    ) {
        let mut fragment_start_byte = 0_usize;
        let mut fragment_start_cell = 0_u16;
        let mut cell = 0_u16;
        let mut previous = None;

        for (byte_index, ch) in text.char_indices() {
            if previous.is_some_and(|previous| is_bad_ligature_pair(previous, ch)) {
                self.push_text_command(
                    run,
                    cell_width,
                    TextCellSpan {
                        start: cells.start.saturating_add(fragment_start_cell),
                        width: cell.saturating_sub(fragment_start_cell),
                    },
                    &text[fragment_start_byte..byte_index],
                    font.clone(),
                    text_pool,
                );
                fragment_start_byte = byte_index;
                fragment_start_cell = cell;
            }
            previous = Some(ch);
            cell = cell.saturating_add(crate::terminal_text::terminal_char_cell_delta(ch));
        }

        self.push_text_command(
            run,
            cell_width,
            TextCellSpan {
                start: cells.start.saturating_add(fragment_start_cell),
                width: cell.saturating_sub(fragment_start_cell),
            },
            &text[fragment_start_byte..],
            font,
            text_pool,
        );
    }

    fn push_text_command(
        &mut self,
        run: &TextRun,
        cell_width: f32,
        cells: TextCellSpan,
        text: &str,
        font: RunFont,
        text_pool: &mut Vec<String>,
    ) {
        if text.is_empty() {
            return;
        }
        // Reuse a reclaimed buffer when the pool has one (warm reuse path), else
        // allocate. Pooled buffers were cleared on reclaim, so just append.
        let mut owned = text_pool.pop().unwrap_or_default();
        owned.push_str(text);
        self.commands.push(TerminalRenderCommand::Text(TextCommand {
            rect: cell_rect(run.rect, cell_width, cells.start, cells.width),
            text: owned,
            attrs: run.attrs,
            face: font.face,
            font_size: font.font_size,
            font_features: font.font_features,
        }));
    }

    fn push_sprite_fragment(&mut self, run: &TextRun, cell: u16, glyph: SpriteGlyph) {
        let cell_width = run.cell_rect.width() / f32::from(run.cells.max(1));
        let rect = cell_rect(run.cell_rect, cell_width, cell, 1);
        self.commands
            .push(TerminalRenderCommand::Sprite(SpriteCommandBatch {
                glyph,
                rect,
                color: run.attrs.fg,
            }));
    }

    fn push_cursor(
        &mut self,
        cursor: &CursorPlan,
        text_contract: &TerminalTextContract,
        text_pool: &mut Vec<String>,
    ) {
        self.commands
            .push(TerminalRenderCommand::Cursor(CursorCommand {
                rect: cursor.rect,
                fill_rect: cursor_fill_rect(cursor.shape, cursor.rect),
                color: cursor.color,
                shape: cursor.shape,
            }));

        if let Some(cursor_text) = &cursor.text_under_cursor {
            let run = TextRun {
                cell_rect: cursor.rect,
                rect: cursor_text.rect,
                cells: text_cell_width(&cursor_text.text),
                text: cursor_text.text.clone(),
                attrs: TextAttrs {
                    fg: cursor_text.color,
                    bold: false,
                    italic: false,
                    underline: libghostty_vt::style::Underline::None,
                    strikethrough: false,
                    overline: false,
                },
            };
            self.push_text_run(&run, text_contract, text_pool);
        }
    }
}

/// Reusable scratch that rebuilds a [`TerminalRenderFrame`] in place, reclaiming the
/// previous frame's command buffer and text-command string allocations.
///
/// The render cache keeps the last frame alive between repaints, so a localized edit
/// can rebuild on top of it: the command `Vec` keeps its capacity and the `String`
/// behind each text command is recycled instead of freed and reallocated. After the
/// pool warms, a steady stream of same-shaped frames allocates nothing.
#[derive(Default)]
pub struct RenderFramePool {
    text_strings: Vec<String>,
}

impl RenderFramePool {
    pub fn rebuild_from_plan(
        &mut self,
        frame: &mut TerminalRenderFrame,
        plan: &TerminalPaintPlan,
        text_contract: &TerminalTextContract,
    ) {
        self.rebuild_from_plan_and_images(frame, plan, text_contract, &KittyImageFrame::default());
    }

    pub fn rebuild_from_plan_and_images(
        &mut self,
        frame: &mut TerminalRenderFrame,
        plan: &TerminalPaintPlan,
        text_contract: &TerminalTextContract,
        images: &KittyImageFrame,
    ) {
        // Reclaim the previous frame's text buffers into the pool, then clear the
        // command Vec while keeping its capacity. `drain` empties the Vec in place.
        for command in frame.commands.drain(..) {
            if let TerminalRenderCommand::Text(text) = command {
                let mut buffer = text.text;
                buffer.clear();
                self.text_strings.push(buffer);
            }
        }
        frame
            .commands
            .reserve(command_capacity_for_plan(plan, images));
        frame.populate(plan, text_contract, images, &mut self.text_strings);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextCellSpan {
    start: u16,
    width: u16,
}

/// The font identity shared by every command split out of one text run.
#[derive(Clone)]
struct RunFont {
    face: Arc<ResolvedFontFace>,
    font_size: f32,
    font_features: Arc<[crate::terminal_text::FontFeature]>,
}

impl RunFont {
    fn new(face: Arc<ResolvedFontFace>, contract: &TerminalTextContract) -> Self {
        Self {
            face,
            font_size: contract.config.font_size,
            font_features: Arc::clone(&contract.font_features),
        }
    }
}

fn command_capacity_for_plan(plan: &TerminalPaintPlan, images: &KittyImageFrame) -> usize {
    let cursor_commands = plan.cursor.as_ref().map_or(0, |cursor| {
        1 + usize::from(cursor.text_under_cursor.is_some())
    });

    1 + plan.backgrounds.len()
        + images.placements.len()
        + plan.text_runs.len()
        + plan.decorations.len()
        + images.virtual_placements.len()
        + cursor_commands
}

fn translate_image_placement(
    placement: &KittyImagePlacement,
    surface: SurfaceRect,
) -> KittyImagePlacement {
    let mut placement = placement.clone();
    let rect = placement.destination;
    placement.destination = SurfaceRect {
        min_x: rect.min_x + surface.min_x,
        min_y: rect.min_y + surface.min_y,
        max_x: rect.max_x + surface.min_x,
        max_y: rect.max_y + surface.min_y,
    };
    placement
}

#[derive(Clone, Debug, PartialEq)]
pub enum TerminalRenderCommand {
    FillRect(FillCommand),
    Text(TextCommand),
    Sprite(SpriteCommandBatch),
    Image(KittyImagePlacement),
    KittyVirtualPlacement(KittyVirtualPlacement),
    Decoration(LineCommand),
    Cursor(CursorCommand),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRole {
    SurfaceBackground,
    CellBackground,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FillCommand {
    pub rect: SurfaceRect,
    pub color: PlanColor,
    pub role: FillRole,
}

#[derive(Clone, Debug)]
pub struct TextCommand {
    pub rect: SurfaceRect,
    pub text: String,
    pub attrs: TextAttrs,
    pub face: Arc<ResolvedFontFace>,
    pub font_size: f32,
    pub font_features: Arc<[crate::terminal_text::FontFeature]>,
}

impl PartialEq for TextCommand {
    /// Compares the face by handle first. Faces come from a small interned set, so equal commands
    /// share one `Arc` and skip comparing the family and fallback family names — a string compare
    /// the per-frame prepared-text cache would otherwise pay on every hit.
    fn eq(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.text == other.text
            && self.attrs == other.attrs
            && self.font_size == other.font_size
            && self.font_features == other.font_features
            && (Arc::ptr_eq(&self.face, &other.face) || self.face == other.face)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpriteCommandBatch {
    pub glyph: SpriteGlyph,
    pub rect: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineCommand {
    pub start_x: f32,
    pub start_y: f32,
    pub end_x: f32,
    pub end_y: f32,
    pub color: PlanColor,
    pub style: DecorationStyle,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorCommand {
    pub rect: SurfaceRect,
    pub fill_rect: SurfaceRect,
    pub color: PlanColor,
    pub shape: CursorShape,
}

fn cell_rect(run_rect: SurfaceRect, cell_width: f32, start_cell: u16, cells: u16) -> SurfaceRect {
    SurfaceRect::from_min_size(
        run_rect.min_x + f32::from(start_cell) * cell_width,
        run_rect.min_y,
        f32::from(cells.max(1)) * cell_width,
        run_rect.height(),
    )
}

fn text_cell_width(text: &str) -> u16 {
    crate::terminal_text::terminal_grapheme_cells(&text.chars().collect::<Vec<_>>())
}

fn is_bad_ligature_pair(left: char, right: char) -> bool {
    matches!((left, right), ('f', 'i' | 'l') | ('s', 't'))
}
