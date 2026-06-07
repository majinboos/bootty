use bootty_app::{
    geometry::SurfaceRect,
    paint_plan::{PlanColor, TextAttrs, TextRun},
    terminal_sprite::{
        SpriteCommand, SpriteFamily, SpriteRegistry, SpriteShape, WgpuSpriteBackend,
    },
    terminal_text::{
        NativeSymbolClass, NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract,
        TerminalTextFragment,
    },
};

fn attrs() -> TextAttrs {
    TextAttrs {
        fg: PlanColor {
            r: 220,
            g: 221,
            b: 222,
            a: 255,
        },
        bold: false,
        italic: false,
        underline: libghostty_vt::style::Underline::None,
        strikethrough: false,
        overline: false,
    }
}

fn run(text: &str) -> TextRun {
    TextRun {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 30.0, 20.0),
        cells: 3,
        text: text.to_owned(),
        attrs: attrs(),
    }
}

#[test]
fn registry_owns_reported_prompt_sprite_glyphs() {
    let registry = SpriteRegistry::prompt_graphics();

    assert_eq!(
        registry.glyph_for('┃').map(|glyph| glyph.family),
        Some(SpriteFamily::BoxDrawing)
    );
    assert_eq!(
        registry.glyph_for('\u{E0B8}').map(|glyph| glyph.family),
        Some(SpriteFamily::Powerline)
    );
    assert_eq!(
        registry.glyph_for('\u{E0B0}').map(|glyph| glyph.family),
        Some(SpriteFamily::Powerline)
    );
    assert_eq!(
        registry.glyph_for('❯').map(|glyph| glyph.family),
        Some(SpriteFamily::Separator),
        "prompt separators should be rendered as deterministic terminal sprites"
    );
}

#[test]
fn registry_owns_box_drawing_sprite_face_range() {
    let registry = SpriteRegistry::prompt_graphics();

    for ch in [
        '┌', '─', '┐', '│', '└', '┘', '╭', '╮', '╰', '╯', '╔', '╗', '╚', '╝', '┄', '╍',
    ] {
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            Some(SpriteFamily::BoxDrawing),
            "{ch} should be rendered as deterministic box drawing geometry"
        );
    }
}

#[test]
fn registry_owns_landed_block_and_shade_progress_glyphs() {
    let registry = SpriteRegistry::prompt_graphics();

    for ch in ['▏', '▍', '▌', '▋', '▉', '█'] {
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            Some(SpriteFamily::Block),
            "{ch} should be rendered as deterministic block geometry"
        );
    }
    for ch in ['░', '▒', '▓'] {
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            Some(SpriteFamily::Shade),
            "{ch} should be rendered as deterministic shade geometry"
        );
    }
    for ch in ['▖', '▗', '▘', '▝', '▞', '▟'] {
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            Some(SpriteFamily::Block),
            "{ch} should be rendered as deterministic block/quadrant geometry"
        );
    }
}

#[test]
fn text_contract_fragments_only_registry_owned_terminal_sprites() {
    let contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    let shaped = contract.shape_run(&run("┌┃❯\u{E0B8}"));

    assert_eq!(
        shaped.fragments,
        vec![
            TerminalTextFragment::NativeSymbol {
                cell: 0,
                ch: '┌',
                class: NativeSymbolClass::BoxDrawing
            },
            TerminalTextFragment::NativeSymbol {
                cell: 1,
                ch: '┃',
                class: NativeSymbolClass::BoxDrawing
            },
            TerminalTextFragment::NativeSymbol {
                cell: 2,
                ch: '❯',
                class: NativeSymbolClass::Separator
            },
            TerminalTextFragment::NativeSymbol {
                cell: 3,
                ch: '\u{E0B8}',
                class: NativeSymbolClass::Powerline
            },
        ]
    );
}

#[test]
fn sprite_commands_preserve_full_cell_prompt_geometry() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(10.0, 20.0, 8.0, 24.0);
    let glyph = registry.glyph_for('┃').expect("heavy vertical sprite");

    let commands = registry.commands_for(glyph, rect);

    assert_eq!(
        commands,
        vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(13.0, 20.0, 2.0, 12.5),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(13.0, 31.5, 2.0, 12.5),
                alpha: 1.0,
            },
        ]
    );
}

#[test]
fn sprite_commands_preserve_progress_row_geometry_and_alpha() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(10.0, 20.0, 16.0, 24.0);

    assert_eq!(
        registry
            .glyph_for('▌')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(10.0, 20.0, 8.0, 24.0),
            alpha: 1.0,
        }])
    );
    assert_eq!(
        registry
            .glyph_for('▉')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![SpriteCommand::FillRect {
            rect: SurfaceRect::from_min_size(10.0, 20.0, 14.0, 24.0),
            alpha: 1.0,
        }])
    );
    assert_eq!(
        registry
            .glyph_for('▓')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![SpriteCommand::FillRect { rect, alpha: 0.75 }])
    );
    assert_eq!(
        registry
            .glyph_for('▒')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![SpriteCommand::FillRect { rect, alpha: 0.5 }])
    );
    assert_eq!(
        registry
            .glyph_for('░')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![SpriteCommand::FillRect { rect, alpha: 0.25 }])
    );
}

#[test]
fn sprite_commands_preserve_single_line_border_geometry() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(10.0, 20.0, 20.0, 20.0);

    assert_eq!(
        registry
            .glyph_for('─')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(10.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
        ])
    );
    assert_eq!(
        registry
            .glyph_for('│')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 20.0, 2.0, 11.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 2.0, 11.0),
                alpha: 1.0,
            },
        ])
    );
    assert_eq!(
        registry
            .glyph_for('┌')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 2.0, 11.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
        ])
    );
    assert_eq!(
        registry
            .glyph_for('┐')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 2.0, 11.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(10.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
        ])
    );
    assert_eq!(
        registry
            .glyph_for('└')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 20.0, 2.0, 11.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
        ])
    );
    assert_eq!(
        registry
            .glyph_for('┘')
            .map(|glyph| registry.commands_for(glyph, rect)),
        Some(vec![
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(19.0, 20.0, 2.0, 11.0),
                alpha: 1.0,
            },
            SpriteCommand::FillRect {
                rect: SurfaceRect::from_min_size(10.0, 29.0, 11.0, 2.0),
                alpha: 1.0,
            },
        ])
    );
}

#[test]
fn wgpu_sprite_backend_builds_primitive_buffers_for_task_shapes() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 8.0, 24.0);
    let color = PlanColor {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    };
    let mut all_commands = Vec::new();

    for ch in ['┃', '\u{E0B8}', '\u{E0B1}', '\u{E0B4}'] {
        let glyph = registry.glyph_for(ch).expect("task sprite glyph");
        all_commands.extend(registry.commands_for(glyph, rect));
    }

    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::FillRect { .. }))
    );
    assert!(all_commands.iter().any(|command| matches!(
        command,
        SpriteCommand::FillPolygon {
            shape: SpriteShape::Triangle,
            ..
        }
    )));
    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::StrokePolyline { .. }))
    );

    let primitives = WgpuSpriteBackend::build_primitives(&all_commands, color);

    assert!(primitives.vertices.len() >= 18);
    assert!(primitives.indices.len() >= 24);
    assert!(primitives.indices.len().is_multiple_of(3));
}
