use bootty_app::{
    geometry::SurfaceRect,
    paint_plan::{
        BackgroundRect, CursorPlan, CursorShape, CursorTextPlan, DecorationLine, PlanColor,
        TerminalPaintPlan, TextAttrs, TextRun,
    },
    terminal_image::{KittyImageFrame, KittyImageLayer, KittyImagePlacement},
    terminal_render::{FillRole, TerminalRenderCommand, TerminalRenderFrame},
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
    terminal_wgpu::terminal_sprite_draws,
};
use std::sync::Arc;

fn color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}

fn attrs() -> TextAttrs {
    TextAttrs {
        fg: color(220, 221, 222),
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn text_run(rect: SurfaceRect, cells: u16, text: &str) -> TextRun {
    TextRun {
        rect,
        cells,
        text: text.to_owned(),
        attrs: attrs(),
    }
}

fn plan_with_text_runs(surface: SurfaceRect, text_runs: Vec<TextRun>) -> TerminalPaintPlan {
    TerminalPaintPlan {
        surface,
        default_background: color(1, 2, 3),
        backgrounds: Vec::new(),
        text_runs,
        decorations: Vec::new(),
        cursor: None,
    }
}

fn text_contract() -> TerminalTextContract {
    TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    )
}

fn image(layer: KittyImageLayer, id: u32) -> KittyImagePlacement {
    KittyImagePlacement {
        image_id: id,
        placement_id: id + 10,
        layer,
        image_width: 1,
        image_height: 1,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        destination: SurfaceRect::from_min_size(id as f32, 0.0, 1.0, 1.0),
        data: Arc::new(vec![255, 0, 0, 255]),
    }
}

#[test]
fn command_boundary_places_kitty_image_layers_around_surface_text() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0),
            1,
            "a",
        )],
    );
    let images = KittyImageFrame {
        placements: vec![
            image(KittyImageLayer::AboveText, 3),
            image(KittyImageLayer::BelowText, 2),
            image(KittyImageLayer::BelowBackground, 1),
        ],
        ..Default::default()
    };

    let frame = TerminalRenderFrame::from_plan_and_images(&plan, &text_contract(), &images);

    assert!(matches!(
        frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Image(below_background),
            TerminalRenderCommand::Image(below_text),
            TerminalRenderCommand::Text(_),
            TerminalRenderCommand::Image(above_text),
        ] if below_background.layer == KittyImageLayer::BelowBackground
            && below_text.layer == KittyImageLayer::BelowText
            && above_text.layer == KittyImageLayer::AboveText
    ));
}

#[test]
fn command_boundary_translates_kitty_images_to_live_terminal_surface() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(300.0, 40.0, 40.0, 20.0),
        Vec::new(),
    );
    let images = KittyImageFrame {
        placements: vec![image(KittyImageLayer::BelowText, 2)],
        ..Default::default()
    };

    let frame = TerminalRenderFrame::from_plan_and_images(&plan, &text_contract(), &images);

    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Image(image)
            if image.destination == SurfaceRect::from_min_size(302.0, 40.0, 1.0, 1.0)
    )));
}

#[test]
fn command_boundary_routes_prompt_sprites_away_from_ordinary_text() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
            4,
            "a┃b\u{E0B8}",
        )],
    );

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());

    assert!(matches!(
        frame.commands[0],
        TerminalRenderCommand::FillRect(ref fill) if fill.role == FillRole::SurfaceBackground
    ));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "a"
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "b"
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite) if sprite.ch == '┃' && !sprite.commands.is_empty()
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite) if sprite.ch == '\u{E0B8}' && !sprite.commands.is_empty()
    )));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text.contains('┃') || text.text.contains('\u{E0B8}')
    )));
}

#[test]
fn command_boundary_keeps_ordinary_text_run_as_single_text_command() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
            4,
            "abcd",
        )],
    );

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());
    let text_commands = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Text(text) => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(text_commands.len(), 1);
    assert_eq!(text_commands[0].text, "abcd");
    assert_eq!(
        text_commands[0].rect,
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0)
    );
}

#[test]
fn command_boundary_preserves_prompt_separator_foreground_color() {
    let prompt_color = color(125, 207, 255);
    let mut run = text_run(SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0), 1, "❯");
    run.attrs.fg = prompt_color;
    let plan = plan_with_text_runs(SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0), vec![run]);

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());

    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Sprite(sprite)
            if sprite.ch == '❯'
                && sprite.color == prompt_color
                && !sprite.commands.is_empty()
    )));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text.contains('❯')
    )));

    let draws = terminal_sprite_draws(&frame);
    assert!(draws.iter().any(|draw| {
        draw.ch == '❯'
            && !draw.vertices.is_empty()
            && draw
                .vertices
                .iter()
                .all(|vertex| vertex.color == [125, 207, 255, 255])
    }));
}

#[test]
fn command_boundary_routes_block_shade_progress_row_through_sprites() {
    const PROGRESS_ROW_GLYPHS: [char; 6] = ['▏', '▌', '█', '▓', '▒', '░'];
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 100.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 100.0, 20.0),
            10,
            "0▏▌█▓▒░1",
        )],
    );

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());

    for ch in PROGRESS_ROW_GLYPHS {
        assert!(
            frame.commands.iter().any(|command| matches!(
                command,
                TerminalRenderCommand::Sprite(sprite) if sprite.ch == ch && !sprite.commands.is_empty()
            )),
            "{ch} should route through sprite commands"
        );
    }
    assert!(matches!(
        frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Text(text_0),
            TerminalRenderCommand::Sprite(thin_block),
            TerminalRenderCommand::Sprite(half_block),
            TerminalRenderCommand::Sprite(full_block),
            TerminalRenderCommand::Sprite(dark_shade),
            TerminalRenderCommand::Sprite(medium_shade),
            TerminalRenderCommand::Sprite(light_shade),
            TerminalRenderCommand::Text(text_1),
        ] if text_0.text == "0"
            && text_0.rect == SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0)
            && thin_block.ch == '▏'
            && thin_block.rect == SurfaceRect::from_min_size(10.0, 0.0, 10.0, 20.0)
            && half_block.ch == '▌'
            && half_block.rect == SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0)
            && full_block.ch == '█'
            && full_block.rect == SurfaceRect::from_min_size(30.0, 0.0, 10.0, 20.0)
            && dark_shade.ch == '▓'
            && dark_shade.rect == SurfaceRect::from_min_size(40.0, 0.0, 10.0, 20.0)
            && medium_shade.ch == '▒'
            && medium_shade.rect == SurfaceRect::from_min_size(50.0, 0.0, 10.0, 20.0)
            && light_shade.ch == '░'
            && light_shade.rect == SurfaceRect::from_min_size(60.0, 0.0, 10.0, 20.0)
            && text_1.text == "1"
            && text_1.rect == SurfaceRect::from_min_size(70.0, 0.0, 10.0, 20.0)
    ));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "0"
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text) if text.text == "1"
    )));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if PROGRESS_ROW_GLYPHS.iter().any(|ch| text.text.contains(*ch))
    )));
}

#[test]
fn command_boundary_routes_single_line_border_box_through_sprites() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 50.0, 30.0),
        vec![
            text_run(SurfaceRect::from_min_size(0.0, 0.0, 50.0, 10.0), 5, "a┌─┐b"),
            text_run(
                SurfaceRect::from_min_size(0.0, 10.0, 50.0, 10.0),
                5,
                " │ │ ",
            ),
            text_run(
                SurfaceRect::from_min_size(0.0, 20.0, 50.0, 10.0),
                5,
                " └─┘ ",
            ),
        ],
    );

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());
    let sprites = frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Sprite(sprite) => Some((sprite.ch, sprite.rect)),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        sprites,
        vec![
            ('┌', SurfaceRect::from_min_size(10.0, 0.0, 10.0, 10.0)),
            ('─', SurfaceRect::from_min_size(20.0, 0.0, 10.0, 10.0)),
            ('┐', SurfaceRect::from_min_size(30.0, 0.0, 10.0, 10.0)),
            ('│', SurfaceRect::from_min_size(10.0, 10.0, 10.0, 10.0)),
            ('│', SurfaceRect::from_min_size(30.0, 10.0, 10.0, 10.0)),
            ('└', SurfaceRect::from_min_size(10.0, 20.0, 10.0, 10.0)),
            ('─', SurfaceRect::from_min_size(20.0, 20.0, 10.0, 10.0)),
            ('┘', SurfaceRect::from_min_size(30.0, 20.0, 10.0, 10.0)),
        ]
    );
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text == "a" && text.rect == SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0)
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text == "b" && text.rect == SurfaceRect::from_min_size(40.0, 0.0, 10.0, 10.0)
    )));
    assert!(!frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if ['┌', '─', '┐', '│', '└', '┘'].iter().any(|ch| text.text.contains(*ch))
    )));
}

#[test]
fn command_boundary_represents_backgrounds_decorations_and_cursor() {
    let cursor_rect = SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0);
    let plan = TerminalPaintPlan {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        default_background: color(1, 2, 3),
        backgrounds: vec![BackgroundRect {
            rect: SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0),
            color: color(4, 5, 6),
        }],
        text_runs: Vec::new(),
        decorations: vec![DecorationLine {
            start_x: 0.0,
            start_y: 18.0,
            end_x: 40.0,
            end_y: 18.0,
            color: color(7, 8, 9),
            style: bootty_app::paint_plan::DecorationStyle::Single,
        }],
        cursor: Some(CursorPlan {
            rect: cursor_rect,
            color: color(10, 11, 12),
            shape: CursorShape::Block,
            text_under_cursor: Some(CursorTextPlan {
                rect: cursor_rect,
                text: "x".to_owned(),
                color: color(13, 14, 15),
            }),
        }),
    };

    let frame = TerminalRenderFrame::from_plan(&plan, &text_contract());

    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::CellBackground
                && fill.rect == SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0)
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Decoration(line)
            if line.start_x == 0.0 && line.end_x == 40.0 && line.color == color(7, 8, 9)
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Cursor(cursor)
            if cursor.shape == CursorShape::Block
                && cursor.rect == cursor_rect
                && cursor.fill_rect == cursor_rect
    )));
    assert!(frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text == "x" && text.rect == cursor_rect && text.attrs.fg == color(13, 14, 15)
    )));

    let cursor_index = frame
        .commands
        .iter()
        .position(|command| matches!(command, TerminalRenderCommand::Cursor(_)))
        .expect("cursor command");
    let cursor_text_index = frame
        .commands
        .iter()
        .position(|command| {
            matches!(
                command,
                TerminalRenderCommand::Text(text) if text.text == "x" && text.rect == cursor_rect
            )
        })
        .expect("text under cursor command");
    assert!(
        cursor_index < cursor_text_index,
        "block cursor fill must be emitted before text-under-cursor so backend compositing keeps the glyph legible"
    );
}

#[test]
fn background_only_frame_does_not_generate_text_or_sprite_commands() {
    let plan = plan_with_text_runs(
        SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        vec![text_run(
            SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
            4,
            "a┃b\u{E0B8}",
        )],
    );

    let frame = TerminalRenderFrame::background_from_plan(&plan);

    assert!(
        frame
            .commands
            .iter()
            .all(|command| matches!(command, TerminalRenderCommand::FillRect(_)))
    );
}
