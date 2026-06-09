use std::sync::Arc;

use crate::{
    geometry::SurfaceRect,
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, DecorationLine, DecorationStyle, PlanColor,
        TerminalPaintPlan, TextAttrs, TextRun, cursor_fill_rect,
    },
    terminal_image::{
        KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement,
    },
    terminal_sprite::{SpriteCommand, SpriteGlyph},
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

        frame.push_fill(
            plan.surface,
            plan.default_background,
            FillRole::SurfaceBackground,
        );
        frame.push_image_layer(images, KittyImageLayer::BelowBackground);
        for background in &plan.backgrounds {
            frame.push_background(background);
        }
        frame.push_image_layer(images, KittyImageLayer::BelowText);
        for run in &plan.text_runs {
            frame.push_text_run(run, text_contract);
        }
        for decoration in &plan.decorations {
            frame.push_decoration(decoration);
        }
        frame.push_image_layer(images, KittyImageLayer::AboveText);
        frame.push_virtual_placements(images);
        if let Some(cursor) = &plan.cursor {
            frame.push_cursor(cursor, text_contract);
        }

        frame
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

    fn push_virtual_placements(&mut self, images: &KittyImageFrame) {
        self.commands.extend(
            images
                .virtual_placements
                .iter()
                .copied()
                .map(TerminalRenderCommand::KittyVirtualPlacement),
        );
    }

    fn push_text_run(&mut self, run: &TextRun, text_contract: &TerminalTextContract) {
        let cell_width = run.rect.width() / f32::from(run.cells.max(1));
        if run.text.is_ascii() {
            let face = text_contract.resolve_face_handle_for_run(run);
            self.push_text_fragment(
                run,
                cell_width,
                TextCellSpan {
                    start: 0,
                    width: run.cells,
                },
                &run.text,
                Arc::clone(&face),
                text_contract.config.font_size,
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
                        face,
                        text_contract.config.font_size,
                    );
                    text_active = false;
                }
                self.push_sprite_fragment(run, cell_width, cell, ch, glyph, text_contract);
                cell = cell.saturating_add(crate::terminal_text::terminal_char_width(ch));
                continue;
            }

            if !text_active {
                text_start_byte = byte_index;
                text_start_cell = cell;
                text_active = true;
            }
            cell = cell.saturating_add(crate::terminal_text::terminal_char_width(ch));
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
                face,
                text_contract.config.font_size,
            );
        }
    }

    fn push_text_fragment(
        &mut self,
        run: &TextRun,
        cell_width: f32,
        cells: TextCellSpan,
        text: &str,
        face: Arc<ResolvedFontFace>,
        font_size: f32,
    ) {
        if text.is_empty() {
            return;
        }
        self.commands.push(TerminalRenderCommand::Text(TextCommand {
            rect: cell_rect(run.rect, cell_width, cells.start, cells.width),
            text: text.to_owned(),
            attrs: run.attrs,
            face,
            font_size,
        }));
    }

    fn push_sprite_fragment(
        &mut self,
        run: &TextRun,
        cell_width: f32,
        cell: u16,
        ch: char,
        glyph: SpriteGlyph,
        text_contract: &TerminalTextContract,
    ) {
        let rect = cell_rect(run.rect, cell_width, cell, 1);
        self.commands
            .push(TerminalRenderCommand::Sprite(SpriteCommandBatch {
                ch,
                glyph,
                rect,
                color: run.attrs.fg,
                commands: text_contract.sprite_registry.commands_for(glyph, rect),
            }));
    }

    fn push_decoration(&mut self, decoration: &DecorationLine) {
        self.commands
            .push(TerminalRenderCommand::Decoration(LineCommand {
                start_x: decoration.start_x,
                start_y: decoration.start_y,
                end_x: decoration.end_x,
                end_y: decoration.end_y,
                color: decoration.color,
                style: decoration.style,
            }));
    }

    fn push_cursor(&mut self, cursor: &CursorPlan, text_contract: &TerminalTextContract) {
        self.commands
            .push(TerminalRenderCommand::Cursor(CursorCommand {
                rect: cursor.rect,
                fill_rect: cursor_fill_rect(cursor.shape, cursor.rect),
                color: cursor.color,
                shape: cursor.shape,
            }));

        if let Some(cursor_text) = &cursor.text_under_cursor {
            let run = TextRun {
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
            self.push_text_run(&run, text_contract);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextCellSpan {
    start: u16,
    width: u16,
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
    placement.destination = translate_rect(placement.destination, surface.min_x, surface.min_y);
    placement
}

fn translate_rect(rect: SurfaceRect, dx: f32, dy: f32) -> SurfaceRect {
    SurfaceRect {
        min_x: rect.min_x + dx,
        min_y: rect.min_y + dy,
        max_x: rect.max_x + dx,
        max_y: rect.max_y + dy,
    }
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

#[derive(Clone, Debug, PartialEq)]
pub struct TextCommand {
    pub rect: SurfaceRect,
    pub text: String,
    pub attrs: TextAttrs,
    pub face: Arc<ResolvedFontFace>,
    pub font_size: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpriteCommandBatch {
    pub ch: char,
    pub glyph: SpriteGlyph,
    pub rect: SurfaceRect,
    pub color: PlanColor,
    pub commands: Vec<SpriteCommand>,
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
    text.chars()
        .map(crate::terminal_text::terminal_char_width)
        .sum::<u16>()
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        paint_plan::{DecorationLine, TerminalPaintPlan},
        terminal_text::{NativeSymbolPolicy, TerminalTextConfig},
    };
    use proptest::prelude::*;

    fn color(value: u8) -> PlanColor {
        PlanColor {
            r: value,
            g: value,
            b: value,
            a: 255,
        }
    }

    fn attrs() -> TextAttrs {
        TextAttrs {
            fg: color(220),
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        }
    }

    fn text_contract() -> TerminalTextContract {
        TerminalTextContract::new(
            TerminalTextConfig::default(),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        )
    }

    #[test]
    fn mixed_text_and_native_symbol_run_preserves_fragments() {
        let plan = TerminalPaintPlan {
            surface: SurfaceRect::from_min_size(0.0, 0.0, 100.0, 20.0),
            default_background: color(0),
            backgrounds: Vec::new(),
            text_runs: vec![TextRun {
                rect: SurfaceRect::from_min_size(0.0, 0.0, 50.0, 10.0),
                cells: 5,
                text: "ab─cd".to_owned(),
                attrs: attrs(),
            }],
            decorations: Vec::new(),
            cursor: None,
        };

        let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());
        assert_eq!(frame.commands.len(), 4);

        match &frame.commands[1] {
            TerminalRenderCommand::Text(command) => {
                assert_eq!(command.text, "ab");
                assert_eq!(
                    command.rect,
                    SurfaceRect::from_min_size(0.0, 0.0, 20.0, 10.0)
                );
            }
            command => panic!("expected leading text command, got {command:?}"),
        }
        match &frame.commands[2] {
            TerminalRenderCommand::Sprite(command) => {
                assert_eq!(command.ch, '─');
                assert_eq!(
                    command.rect,
                    SurfaceRect::from_min_size(20.0, 0.0, 10.0, 10.0)
                );
            }
            command => panic!("expected sprite command, got {command:?}"),
        }
        match &frame.commands[3] {
            TerminalRenderCommand::Text(command) => {
                assert_eq!(command.text, "cd");
                assert_eq!(
                    command.rect,
                    SurfaceRect::from_min_size(30.0, 0.0, 20.0, 10.0)
                );
            }
            command => panic!("expected trailing text command, got {command:?}"),
        }
    }

    proptest! {
        #[test]
        fn property_ascii_render_command_count_matches_plan_resources(
            run_bytes in proptest::collection::vec(
                proptest::collection::vec(b'a'..=b'z', 1..16),
                0..8,
            ),
            background_count in 0_usize..8,
            decoration_count in 0_usize..8,
        ) {
            let backgrounds = (0..background_count)
                .map(|index| BackgroundRect {
                    rect: SurfaceRect::from_min_size(index as f32, 0.0, 1.0, 1.0),
                    color: color(index as u8),
                })
                .collect::<Vec<_>>();
            let decorations = (0..decoration_count)
                .map(|index| DecorationLine {
                    start_x: index as f32,
                    start_y: 0.0,
                    end_x: index as f32 + 1.0,
                    end_y: 0.0,
                    color: color(index as u8),
                    style: DecorationStyle::Single,
                })
                .collect::<Vec<_>>();
            let text_runs = run_bytes
                .iter()
                .enumerate()
                .map(|(index, bytes)| TextRun {
                    rect: SurfaceRect::from_min_size(0.0, index as f32 * 10.0, bytes.len() as f32 * 10.0, 10.0),
                    cells: bytes.len() as u16,
                    text: String::from_utf8(bytes.clone()).expect("generated ascii"),
                    attrs: attrs(),
                })
                .collect::<Vec<_>>();
            let expected_text_commands = run_bytes.len();
            let plan = TerminalPaintPlan {
                surface: SurfaceRect::from_min_size(0.0, 0.0, 200.0, 120.0),
                default_background: color(0),
                backgrounds,
                text_runs,
                decorations,
                cursor: None,
            };

            let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());
            let fill_count = frame
                .commands
                .iter()
                .filter(|command| matches!(command, TerminalRenderCommand::FillRect(_)))
                .count();
            let text_count = frame
                .commands
                .iter()
                .filter(|command| matches!(command, TerminalRenderCommand::Text(_)))
                .count();
            let decoration_command_count = frame
                .commands
                .iter()
                .filter(|command| matches!(command, TerminalRenderCommand::Decoration(_)))
                .count();

            prop_assert_eq!(fill_count, 1 + background_count);
            prop_assert_eq!(text_count, expected_text_commands);
            prop_assert_eq!(decoration_command_count, decoration_count);
            prop_assert_eq!(
                frame.commands.len(),
                1 + background_count + expected_text_commands + decoration_count
            );
        }
    }
}
