use std::{fs, path::PathBuf};

use bootty_app::{
    geometry::SurfaceRect,
    paint_plan::{PlanColor, TextAttrs, TextRun},
    terminal_render::SpriteCommandBatch,
    terminal_sprite::{SpriteCommand, SpriteFamily, SpritePoint, SpriteRegistry, SpriteShape},
    terminal_text::{
        NativeSymbolClass, NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract,
        TerminalTextFragment,
    },
    terminal_text_atlas::{TextAtlasBuilder, TexturedGlyphQuad},
};

fn sprite_fixture() -> (SpriteRegistry, SurfaceRect) {
    (
        SpriteRegistry::prompt_graphics(),
        SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0),
    )
}

fn sprite_command_batch(
    registry: &SpriteRegistry,
    ch: char,
    rect: SurfaceRect,
) -> SpriteCommandBatch {
    let glyph = registry
        .glyph_for(ch)
        .unwrap_or_else(|| panic!("missing glyph {ch}"));
    SpriteCommandBatch {
        ch: glyph.ch,
        glyph,
        rect,
        color: color(),
        commands: registry.commands_for(glyph, rect),
    }
}

fn prepare_sprite_quads(
    ch: char,
    rect: SurfaceRect,
    atlas_width: u32,
    atlas_height: u32,
) -> (TextAtlasBuilder, Vec<TexturedGlyphQuad>) {
    let registry = SpriteRegistry::prompt_graphics();
    let batch = sprite_command_batch(&registry, ch, rect);
    let mut builder = TextAtlasBuilder::new(atlas_width, atlas_height);
    let quads = builder.prepare_sprite_command(&batch, 1.0);
    (builder, quads)
}

fn sprite_atlas_pixels(ch: char, rect: SurfaceRect) -> Vec<u8> {
    let (builder, _) = prepare_sprite_quads(ch, rect, 32, 32);
    builder.atlas_pixels().to_vec()
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/block.zig draw2580_259F.
// Ghostty's upstream test compares rendered atlases; Bootty asserts the same
// codepoint-to-block geometry at the renderer command boundary.
#[test]
fn terminal_block_elements_draw_complete_known_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        ('▀', vec![fill(0.0, 0.0, 16.0, 12.0, 1.0)]),
        ('▁', vec![fill(0.0, 21.0, 16.0, 3.0, 1.0)]),
        ('▂', vec![fill(0.0, 18.0, 16.0, 6.0, 1.0)]),
        ('▃', vec![fill(0.0, 15.0, 16.0, 9.0, 1.0)]),
        ('▄', vec![fill(0.0, 12.0, 16.0, 12.0, 1.0)]),
        ('▅', vec![fill(0.0, 9.0, 16.0, 15.0, 1.0)]),
        ('▆', vec![fill(0.0, 6.0, 16.0, 18.0, 1.0)]),
        ('▇', vec![fill(0.0, 3.0, 16.0, 21.0, 1.0)]),
        ('█', vec![fill(0.0, 0.0, 16.0, 24.0, 1.0)]),
        ('▉', vec![fill(0.0, 0.0, 14.0, 24.0, 1.0)]),
        ('▊', vec![fill(0.0, 0.0, 12.0, 24.0, 1.0)]),
        ('▋', vec![fill(0.0, 0.0, 10.0, 24.0, 1.0)]),
        ('▌', vec![fill(0.0, 0.0, 8.0, 24.0, 1.0)]),
        ('▍', vec![fill(0.0, 0.0, 6.0, 24.0, 1.0)]),
        ('▎', vec![fill(0.0, 0.0, 4.0, 24.0, 1.0)]),
        ('▏', vec![fill(0.0, 0.0, 2.0, 24.0, 1.0)]),
        ('▐', vec![fill(8.0, 0.0, 8.0, 24.0, 1.0)]),
        ('░', vec![fill(0.0, 0.0, 16.0, 24.0, 0.25)]),
        ('▒', vec![fill(0.0, 0.0, 16.0, 24.0, 0.5)]),
        ('▓', vec![fill(0.0, 0.0, 16.0, 24.0, 0.75)]),
        ('▔', vec![fill(0.0, 0.0, 16.0, 3.0, 1.0)]),
        ('▕', vec![fill(14.0, 0.0, 2.0, 24.0, 1.0)]),
        ('▖', vec![fill(0.0, 12.0, 8.0, 12.0, 1.0)]),
        ('▗', vec![fill(8.0, 12.0, 8.0, 12.0, 1.0)]),
        ('▘', vec![fill(0.0, 0.0, 8.0, 12.0, 1.0)]),
        (
            '▙',
            vec![
                fill(0.0, 0.0, 8.0, 12.0, 1.0),
                fill(0.0, 12.0, 8.0, 12.0, 1.0),
                fill(8.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
        (
            '▚',
            vec![
                fill(0.0, 0.0, 8.0, 12.0, 1.0),
                fill(8.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
        (
            '▛',
            vec![
                fill(0.0, 0.0, 8.0, 12.0, 1.0),
                fill(8.0, 0.0, 8.0, 12.0, 1.0),
                fill(0.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
        (
            '▜',
            vec![
                fill(0.0, 0.0, 8.0, 12.0, 1.0),
                fill(8.0, 0.0, 8.0, 12.0, 1.0),
                fill(8.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
        ('▝', vec![fill(8.0, 0.0, 8.0, 12.0, 1.0)]),
        (
            '▞',
            vec![
                fill(8.0, 0.0, 8.0, 12.0, 1.0),
                fill(0.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
        (
            '▟',
            vec![
                fill(8.0, 0.0, 8.0, 12.0, 1.0),
                fill(0.0, 12.0, 8.0, 12.0, 1.0),
                fill(8.0, 12.0, 8.0, 12.0, 1.0),
            ],
        ),
    ] {
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(
            registry.commands_for(glyph, rect),
            expected,
            "{ch} should match Ghostty block element geometry"
        );
    }
}

// Ported from Ghostty ce6a00b src/font/nerd_font_attributes.zig Progress
// Indicators constraints. Bootty renders these font-constrained private-use
// glyphs as deterministic native sprite geometry before font fallback.
#[test]
fn terminal_nerd_progress_indicators_use_upstream_constrained_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        (
            '\u{EE00}',
            fill(2.1030195, 1.6479691, 13.889875, 20.70406, 1.0),
        ),
        ('\u{EE01}', fill(0.0, 1.6479691, 16.0, 20.70406, 1.0)),
        ('\u{EE02}', fill(0.0, 1.6479691, 13.89698, 20.70406, 1.0)),
        (
            '\u{EE03}',
            fill(2.1030195, 1.6479691, 13.889875, 20.70406, 1.0),
        ),
        ('\u{EE04}', fill(0.0, 1.6479691, 16.0, 20.70406, 1.0)),
        ('\u{EE05}', fill(0.0, 1.6479691, 13.89698, 20.70406, 1.0)),
        (
            '\u{EE06}',
            fill(2.3524673, 18.63714, 11.295066, 5.362859, 1.0),
        ),
        ('\u{EE07}', fill(8.0, 6.00302, 8.0, 17.99698, 1.0)),
        ('\u{EE08}', fill(5.921_45, 0.0, 10.07855, 20.485153, 1.0)),
        ('\u{EE09}', fill(0.0, 0.0, 16.0, 11.993961, 1.0)),
        ('\u{EE0A}', fill(0.0, 0.0, 10.07855, 20.485153, 1.0)),
        ('\u{EE0B}', fill(0.0, 6.00302, 8.0, 17.99698, 1.0)),
    ] {
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(glyph.family, SpriteFamily::ProgressIndicator);
        assert_sprite_command_close(&registry.commands_for(glyph, rect), &[expected], ch);
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F linesChar cases for light/heavy line junctions.
#[test]
fn terminal_box_drawing_line_junctions_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        (
            '─',
            vec![
                fill(0.0, 11.0, 9.0, 2.0, 1.0),
                fill(7.0, 11.0, 9.0, 2.0, 1.0),
            ],
        ),
        (
            '━',
            vec![
                fill(0.0, 10.0, 9.0, 4.0, 1.0),
                fill(7.0, 10.0, 9.0, 4.0, 1.0),
            ],
        ),
        (
            '│',
            vec![
                fill(7.0, 0.0, 2.0, 13.0, 1.0),
                fill(7.0, 11.0, 2.0, 13.0, 1.0),
            ],
        ),
        (
            '┃',
            vec![
                fill(6.0, 0.0, 4.0, 13.0, 1.0),
                fill(6.0, 11.0, 4.0, 13.0, 1.0),
            ],
        ),
        (
            '┌',
            vec![
                fill(7.0, 11.0, 2.0, 13.0, 1.0),
                fill(7.0, 11.0, 9.0, 2.0, 1.0),
            ],
        ),
        (
            '┝',
            vec![
                fill(7.0, 0.0, 2.0, 14.0, 1.0),
                fill(7.0, 10.0, 2.0, 14.0, 1.0),
                fill(9.0, 10.0, 7.0, 4.0, 1.0),
            ],
        ),
        (
            '┼',
            vec![
                fill(7.0, 0.0, 2.0, 13.0, 1.0),
                fill(7.0, 11.0, 2.0, 13.0, 1.0),
                fill(0.0, 11.0, 9.0, 2.0, 1.0),
                fill(7.0, 11.0, 9.0, 2.0, 1.0),
            ],
        ),
        (
            '╼',
            vec![
                fill(0.0, 11.0, 9.0, 2.0, 1.0),
                fill(7.0, 10.0, 9.0, 4.0, 1.0),
            ],
        ),
    ] {
        assert_sprite_commands(
            &registry,
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty line-junction geometry",
        );
    }
}

#[test]
fn terminal_box_drawing_line_junctions_cover_ported_upstream_range() {
    let (registry, rect) = sprite_fixture();

    assert_sprite_has_commands_for_codepoints(
        &registry,
        rect,
        box_line_junction_codepoints(),
        SpriteFamily::BoxDrawing,
    );
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F dashHorizontal, dashVertical, and lightDiagonal* cases.
#[test]
fn terminal_box_drawing_dashes_and_diagonals_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        (
            '┄',
            vec![
                fill(1.0, 11.0, 4.0, 2.0, 1.0),
                fill(7.0, 11.0, 3.0, 2.0, 1.0),
                fill(12.0, 11.0, 3.0, 2.0, 1.0),
            ],
        ),
        (
            '┉',
            vec![
                fill(1.0, 10.0, 2.0, 4.0, 1.0),
                fill(5.0, 10.0, 2.0, 4.0, 1.0),
                fill(9.0, 10.0, 2.0, 4.0, 1.0),
                fill(13.0, 10.0, 2.0, 4.0, 1.0),
            ],
        ),
        (
            '┆',
            vec![
                fill(7.0, 0.0, 2.0, 4.0, 1.0),
                fill(7.0, 8.0, 2.0, 4.0, 1.0),
                fill(7.0, 16.0, 2.0, 4.0, 1.0),
            ],
        ),
        (
            '┋',
            vec![
                fill(6.0, 0.0, 4.0, 3.0, 1.0),
                fill(6.0, 6.0, 4.0, 3.0, 1.0),
                fill(6.0, 12.0, 4.0, 3.0, 1.0),
                fill(6.0, 18.0, 4.0, 3.0, 1.0),
            ],
        ),
        (
            '╍',
            vec![
                fill(1.0, 10.0, 6.0, 4.0, 1.0),
                fill(9.0, 10.0, 6.0, 4.0, 1.0),
            ],
        ),
        (
            '╎',
            vec![
                fill(7.0, 0.0, 2.0, 8.0, 1.0),
                fill(7.0, 12.0, 2.0, 8.0, 1.0),
            ],
        ),
        (
            '╱',
            vec![stroke_points(vec![
                SpritePoint {
                    x: 16.333334,
                    y: -0.5,
                },
                SpritePoint {
                    x: -0.33333334,
                    y: 24.5,
                },
            ])],
        ),
        (
            '╲',
            vec![stroke_points(vec![
                SpritePoint {
                    x: -0.33333334,
                    y: -0.5,
                },
                SpritePoint {
                    x: 16.333334,
                    y: 24.5,
                },
            ])],
        ),
        (
            '╳',
            vec![
                stroke_points(vec![
                    SpritePoint {
                        x: 16.333334,
                        y: -0.5,
                    },
                    SpritePoint {
                        x: -0.33333334,
                        y: 24.5,
                    },
                ]),
                stroke_points(vec![
                    SpritePoint {
                        x: -0.33333334,
                        y: -0.5,
                    },
                    SpritePoint {
                        x: 16.333334,
                        y: 24.5,
                    },
                ]),
            ],
        ),
    ] {
        assert_sprite_commands(
            &registry,
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty dash/diagonal geometry",
        );
    }
}

#[test]
fn terminal_box_drawing_dashes_and_diagonals_cover_ported_upstream_range() {
    let (registry, rect) = sprite_fixture();

    assert_sprite_has_commands_for_codepoints(
        &registry,
        rect,
        box_dash_diagonal_codepoints(),
        SpriteFamily::BoxDrawing,
    );
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F linesChar double-line cases.
#[test]
fn terminal_box_drawing_double_lines_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        (
            '═',
            vec![
                fill(0.0, 9.0, 9.0, 2.0, 1.0),
                fill(0.0, 13.0, 9.0, 2.0, 1.0),
                fill(7.0, 9.0, 9.0, 2.0, 1.0),
                fill(7.0, 13.0, 9.0, 2.0, 1.0),
            ],
        ),
        (
            '║',
            vec![
                fill(5.0, 0.0, 2.0, 13.0, 1.0),
                fill(9.0, 0.0, 2.0, 13.0, 1.0),
                fill(5.0, 11.0, 2.0, 13.0, 1.0),
                fill(9.0, 11.0, 2.0, 13.0, 1.0),
            ],
        ),
        (
            '╔',
            vec![
                fill(5.0, 9.0, 2.0, 15.0, 1.0),
                fill(9.0, 13.0, 2.0, 11.0, 1.0),
                fill(5.0, 9.0, 11.0, 2.0, 1.0),
                fill(9.0, 13.0, 7.0, 2.0, 1.0),
            ],
        ),
        (
            '╬',
            vec![
                fill(5.0, 0.0, 2.0, 11.0, 1.0),
                fill(9.0, 0.0, 2.0, 11.0, 1.0),
                fill(5.0, 13.0, 2.0, 11.0, 1.0),
                fill(9.0, 13.0, 2.0, 11.0, 1.0),
                fill(0.0, 9.0, 7.0, 2.0, 1.0),
                fill(0.0, 13.0, 7.0, 2.0, 1.0),
                fill(9.0, 9.0, 7.0, 2.0, 1.0),
                fill(9.0, 13.0, 7.0, 2.0, 1.0),
            ],
        ),
    ] {
        assert_sprite_commands(
            &registry,
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty double-line geometry",
        );
    }
}

#[test]
fn terminal_box_drawing_double_lines_cover_ported_upstream_range() {
    let (registry, rect) = sprite_fixture();

    assert_sprite_has_commands_for_codepoints(
        &registry,
        rect,
        box_double_line_codepoints(),
        SpriteFamily::BoxDrawing,
    );
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F arc cases for rounded corners.
#[test]
fn terminal_box_drawing_rounded_corners_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (ch, corner) in [
        ('╭', "upper_left"),
        ('╮', "upper_right"),
        ('╯', "lower_right"),
        ('╰', "lower_left"),
    ] {
        assert_sprite_commands(
            &registry,
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            vec![rounded_corner(corner)],
            "Ghostty rounded-corner geometry",
        );
    }
}

#[test]
fn terminal_box_drawing_draws_cover_all_upstream_range() {
    let (registry, rect) = sprite_fixture();

    assert_sprite_has_commands_for_codepoints(
        &registry,
        rect,
        0x2500..=0x257F,
        SpriteFamily::BoxDrawing,
    );
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/braille.zig draw2800_28FF.
#[test]
fn terminal_braille_draw_uses_upstream_dot_layout() {
    let (registry, rect) = sprite_fixture();
    let dot_rects = [
        fill(2.0, 1.0, 3.0, 3.0, 1.0),
        fill(2.0, 7.0, 3.0, 3.0, 1.0),
        fill(2.0, 13.0, 3.0, 3.0, 1.0),
        fill(10.0, 1.0, 3.0, 3.0, 1.0),
        fill(10.0, 7.0, 3.0, 3.0, 1.0),
        fill(10.0, 13.0, 3.0, 3.0, 1.0),
        fill(2.0, 19.0, 3.0, 3.0, 1.0),
        fill(10.0, 19.0, 3.0, 3.0, 1.0),
    ];

    for offset in 0..=0xFF {
        let ch = char::from_u32(0x2800 + offset).unwrap();
        let expected = sprite_commands_from_mask(offset, &dot_rects);
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(
            registry.commands_for(glyph, rect),
            expected,
            "{ch} should match Ghostty braille dot geometry"
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/Face.zig range collection over
// src/font/sprite/draw/powerline.zig draw* functions.
#[test]
fn terminal_powerline_face_owns_only_upstream_drawn_codepoints() {
    let registry = SpriteRegistry::prompt_graphics();
    let ghostty_powerline = [
        '\u{E0B0}', '\u{E0B1}', '\u{E0B2}', '\u{E0B3}', '\u{E0B4}', '\u{E0B5}', '\u{E0B6}',
        '\u{E0B7}', '\u{E0B8}', '\u{E0B9}', '\u{E0BA}', '\u{E0BB}', '\u{E0BC}', '\u{E0BD}',
        '\u{E0BE}', '\u{E0BF}', '\u{E0D2}', '\u{E0D4}',
    ];

    for cp in 0xE0B0..=0xE0D4 {
        let ch = char::from_u32(cp).unwrap();
        let expected = ghostty_powerline.contains(&ch);
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            expected.then_some(SpriteFamily::Powerline),
            "Powerline ownership for U+{cp:04X} should match Ghostty draw functions"
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/powerline.zig drawE0D2/drawE0D4.
#[test]
fn terminal_powerline_extra_split_glyphs_use_upstream_polygons() {
    let (registry, rect) = sprite_fixture();

    for (ch, expected) in [
        (
            '\u{E0D2}',
            vec![
                polygon(vec![(0.0, 0.0), (16.0, 0.0), (8.0, 11.0), (0.0, 11.0)]),
                polygon(vec![(0.0, 24.0), (16.0, 24.0), (8.0, 13.0), (0.0, 13.0)]),
            ],
        ),
        (
            '\u{E0D4}',
            vec![
                polygon(vec![(16.0, 0.0), (0.0, 0.0), (8.0, 11.0), (16.0, 11.0)]),
                polygon(vec![(16.0, 24.0), (0.0, 24.0), (8.0, 13.0), (16.0, 13.0)]),
            ],
        ),
    ] {
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(
            registry.commands_for(glyph, rect),
            expected,
            "{ch} should match Ghostty Powerline Extra split geometry"
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/powerline.zig drawE0B0..drawE0BF.
#[test]
fn terminal_powerline_draw_uses_upstream_geometry_for_all_drawn_codepoints() {
    let (registry, rect) = sprite_fixture();
    let right_round = terminal_right_round_points(rect);
    let left_round = flip_points(&right_round, rect);

    for (ch, expected) in [
        (
            '\u{E0B0}',
            vec![triangle([(0.0, 0.0), (16.0, 12.0), (0.0, 24.0)])],
        ),
        (
            '\u{E0B1}',
            vec![
                stroke(vec![(0.0, 0.0), (16.0, 12.0)]),
                stroke(vec![(0.0, 24.0), (16.0, 12.0)]),
            ],
        ),
        (
            '\u{E0B2}',
            vec![triangle([(16.0, 0.0), (0.0, 12.0), (16.0, 24.0)])],
        ),
        (
            '\u{E0B3}',
            vec![
                stroke(vec![(16.0, 0.0), (0.0, 12.0)]),
                stroke(vec![(16.0, 24.0), (0.0, 12.0)]),
            ],
        ),
        ('\u{E0B4}', vec![polygon_points(right_round.clone())]),
        ('\u{E0B5}', vec![stroke_points(right_round)]),
        ('\u{E0B6}', vec![polygon_points(left_round.clone())]),
        ('\u{E0B7}', vec![stroke_points(left_round)]),
        (
            '\u{E0B8}',
            vec![triangle([(0.0, 0.0), (0.0, 24.0), (16.0, 24.0)])],
        ),
        ('\u{E0B9}', vec![stroke(vec![(0.0, 0.0), (16.0, 24.0)])]),
        (
            '\u{E0BA}',
            vec![triangle([(16.0, 0.0), (16.0, 24.0), (0.0, 24.0)])],
        ),
        ('\u{E0BB}', vec![stroke(vec![(0.0, 24.0), (16.0, 0.0)])]),
        (
            '\u{E0BC}',
            vec![triangle([(0.0, 0.0), (16.0, 0.0), (0.0, 24.0)])],
        ),
        ('\u{E0BD}', vec![stroke(vec![(0.0, 24.0), (16.0, 0.0)])]),
        (
            '\u{E0BE}',
            vec![triangle([(0.0, 0.0), (16.0, 0.0), (16.0, 24.0)])],
        ),
        ('\u{E0BF}', vec![stroke(vec![(0.0, 0.0), (16.0, 24.0)])]),
        (
            '\u{E0D2}',
            vec![
                polygon(vec![(0.0, 0.0), (16.0, 0.0), (8.0, 11.0), (0.0, 11.0)]),
                polygon(vec![(0.0, 24.0), (16.0, 24.0), (8.0, 13.0), (0.0, 13.0)]),
            ],
        ),
        (
            '\u{E0D4}',
            vec![
                polygon(vec![(16.0, 0.0), (0.0, 0.0), (8.0, 11.0), (16.0, 11.0)]),
                polygon(vec![(16.0, 24.0), (0.0, 24.0), (8.0, 13.0), (16.0, 13.0)]),
            ],
        ),
    ] {
        assert_sprite_commands(
            &registry,
            rect,
            ch,
            SpriteFamily::Powerline,
            expected,
            "Ghostty Powerline geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB00_1FB3B.
#[test]
fn terminal_legacy_sextants_draw_from_upstream_bit_mapping() {
    let (registry, rect) = sprite_fixture();

    for cp in 0x1FB00..=0x1FB3B {
        let idx = cp - 0x1FB00;
        let pattern = idx + (idx / 0x14) + 1;
        let expected = grid_fills(pattern as u8, 3, 2, rect);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty sextant geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB3C_1FB67.
#[test]
fn terminal_legacy_smooth_mosaics_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (offset, pattern) in SMOOTH_MOSAIC_PATTERNS.iter().enumerate() {
        let cp = 0x1FB3C + offset as u32;
        let expected = smooth_mosaic_expected(pattern, rect);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty smooth mosaic geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB68_1FB6F
// and draw1FB9A_1FB9B.
#[test]
fn terminal_legacy_edge_triangles_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FB68, inverted_edge_triangles("left")),
        (0x1FB69, inverted_edge_triangles("top")),
        (0x1FB6A, inverted_edge_triangles("right")),
        (0x1FB6B, inverted_edge_triangles("bottom")),
        (0x1FB6C, vec![edge_triangle("left")]),
        (0x1FB6D, vec![edge_triangle("top")]),
        (0x1FB6E, vec![edge_triangle("right")]),
        (0x1FB6F, vec![edge_triangle("bottom")]),
        (0x1FB9A, vec![edge_triangle("top"), edge_triangle("bottom")]),
        (0x1FB9B, vec![edge_triangle("left"), edge_triangle("right")]),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty edge triangle geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB9A_1FB9F
// cornerTriangleShade cases.
#[test]
fn terminal_legacy_corner_triangle_shades_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (
            0x1FB9C,
            vec![triangle_alpha([(0.0, 0.0), (16.0, 0.0), (0.0, 24.0)], 0.5)],
        ),
        (
            0x1FB9D,
            vec![triangle_alpha([(0.0, 0.0), (16.0, 0.0), (16.0, 24.0)], 0.5)],
        ),
        (
            0x1FB9E,
            vec![triangle_alpha(
                [(16.0, 0.0), (16.0, 24.0), (0.0, 24.0)],
                0.5,
            )],
        ),
        (
            0x1FB9F,
            vec![triangle_alpha([(0.0, 0.0), (0.0, 24.0), (16.0, 24.0)], 0.5)],
        ),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty corner triangle shade geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB70_1FB97.
#[test]
fn terminal_legacy_block_extensions_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for cp in 0x1FB70..=0x1FB97 {
        let expected = legacy_block_extension_expected(cp, rect);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty legacy block extension geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB98/draw1FB99.
#[test]
fn terminal_legacy_hatches_match_upstream_clipped_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FB98, hatch_expected(rect, false)),
        (0x1FB99, hatch_expected(rect, true)),
    ] {
        let ch = char::from_u32(cp).unwrap();
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing glyph U+{cp:04X}"));

        assert_eq!(
            glyph.family,
            SpriteFamily::LegacyComputing,
            "U+{cp:04X} should be owned as legacy computing"
        );
        assert_eq!(
            registry.commands_for(glyph, rect),
            expected,
            "U+{cp:04X} should match Ghostty clipped hatch geometry"
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBD0_1FBDF.
#[test]
fn terminal_legacy_cell_diagonals_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FBD0, diagonal_segments(&[((16.0, 12.0), (0.0, 24.0))])),
        (0x1FBD1, diagonal_segments(&[((16.0, 0.0), (0.0, 12.0))])),
        (0x1FBD2, diagonal_segments(&[((0.0, 0.0), (16.0, 12.0))])),
        (0x1FBD3, diagonal_segments(&[((0.0, 12.0), (16.0, 24.0))])),
        (0x1FBD4, diagonal_segments(&[((0.0, 0.0), (8.0, 24.0))])),
        (0x1FBD5, diagonal_segments(&[((8.0, 0.0), (16.0, 24.0))])),
        (0x1FBD6, diagonal_segments(&[((16.0, 0.0), (8.0, 24.0))])),
        (0x1FBD7, diagonal_segments(&[((8.0, 0.0), (0.0, 24.0))])),
        (
            0x1FBD8,
            diagonal_segments(&[((0.0, 0.0), (8.0, 12.0)), ((8.0, 12.0), (16.0, 0.0))]),
        ),
        (
            0x1FBD9,
            diagonal_segments(&[((16.0, 0.0), (8.0, 12.0)), ((8.0, 12.0), (16.0, 24.0))]),
        ),
        (
            0x1FBDA,
            diagonal_segments(&[((0.0, 24.0), (8.0, 12.0)), ((8.0, 12.0), (16.0, 24.0))]),
        ),
        (
            0x1FBDB,
            diagonal_segments(&[((0.0, 0.0), (8.0, 12.0)), ((8.0, 12.0), (0.0, 24.0))]),
        ),
        (
            0x1FBDC,
            diagonal_segments(&[((0.0, 0.0), (8.0, 24.0)), ((8.0, 24.0), (16.0, 0.0))]),
        ),
        (
            0x1FBDD,
            diagonal_segments(&[((16.0, 0.0), (0.0, 12.0)), ((0.0, 12.0), (16.0, 24.0))]),
        ),
        (
            0x1FBDE,
            diagonal_segments(&[((0.0, 24.0), (8.0, 0.0)), ((8.0, 0.0), (16.0, 24.0))]),
        ),
        (
            0x1FBDF,
            diagonal_segments(&[((0.0, 0.0), (16.0, 12.0)), ((16.0, 12.0), (0.0, 24.0))]),
        ),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty cell diagonal geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBA0_1FBAE.
#[test]
fn terminal_legacy_corner_diagonal_lines_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FBA0, corner_diagonal_segments(&["tl"])),
        (0x1FBA1, corner_diagonal_segments(&["tr"])),
        (0x1FBA2, corner_diagonal_segments(&["bl"])),
        (0x1FBA3, corner_diagonal_segments(&["br"])),
        (0x1FBA4, corner_diagonal_segments(&["tl", "bl"])),
        (0x1FBA5, corner_diagonal_segments(&["tr", "br"])),
        (0x1FBA6, corner_diagonal_segments(&["bl", "br"])),
        (0x1FBA7, corner_diagonal_segments(&["tl", "tr"])),
        (0x1FBA8, corner_diagonal_segments(&["tl", "br"])),
        (0x1FBA9, corner_diagonal_segments(&["tr", "bl"])),
        (0x1FBAA, corner_diagonal_segments(&["tr", "bl", "br"])),
        (0x1FBAB, corner_diagonal_segments(&["tl", "bl", "br"])),
        (0x1FBAC, corner_diagonal_segments(&["tl", "tr", "br"])),
        (0x1FBAD, corner_diagonal_segments(&["tl", "tr", "bl"])),
        (0x1FBAE, corner_diagonal_segments(&["tl", "tr", "bl", "br"])),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty corner diagonal geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBAF.
#[test]
fn terminal_legacy_mixed_box_connector_matches_upstream_geometry() {
    let (registry, rect) = sprite_fixture();
    assert_sprite_commands(
        &registry,
        rect,
        '\u{1FBAF}',
        SpriteFamily::LegacyComputing,
        vec![
            fill(6.0, 0.0, 4.0, 13.0, 1.0),
            fill(6.0, 11.0, 4.0, 13.0, 1.0),
            fill(0.0, 11.0, 16.0, 2.0, 1.0),
        ],
        "Ghostty heavy-vertical light-horizontal connector geometry",
    );
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBBD..draw1FBBF.
#[test]
fn terminal_legacy_inverse_diagonals_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FBBD, inverse_diagonal_cross(rect)),
        (0x1FBBE, inverse_corner_diagonals(rect, &["br"])),
        (
            0x1FBBF,
            inverse_corner_diagonals(rect, &["tl", "tr", "bl", "br"]),
        ),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty inverse diagonal geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBCE,
// draw1FBCF, and draw1FBE0_1FBEF block cases.
#[test]
fn terminal_legacy_fractional_blocks_match_upstream_geometry() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 18.0, 24.0);

    for (cp, expected) in [
        (0x1FBCE, vec![fill(0.0, 0.0, 12.0, 24.0, 1.0)]),
        (0x1FBCF, vec![fill(0.0, 0.0, 6.0, 24.0, 1.0)]),
        (0x1FBE4, vec![fill(4.5, 0.0, 9.0, 12.0, 1.0)]),
        (0x1FBE5, vec![fill(4.5, 12.0, 9.0, 12.0, 1.0)]),
        (0x1FBE6, vec![fill(0.0, 6.0, 9.0, 12.0, 1.0)]),
        (0x1FBE7, vec![fill(9.0, 6.0, 9.0, 12.0, 1.0)]),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty fractional block geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBE0_1FBEF
// circle cases.
#[test]
fn terminal_legacy_circles_match_upstream_clipped_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1FBE0, vec![circle_arc("top")]),
        (0x1FBE1, vec![circle_arc("right")]),
        (0x1FBE2, vec![circle_arc("bottom")]),
        (0x1FBE3, vec![circle_arc("left")]),
        (0x1FBE8, vec![circle_sector("top")]),
        (0x1FBE9, vec![circle_sector("right")]),
        (0x1FBEA, vec![circle_sector("bottom")]),
        (0x1FBEB, vec![circle_sector("left")]),
        (0x1FBEC, vec![circle_sector("top_right")]),
        (0x1FBED, vec![circle_sector("bottom_left")]),
        (0x1FBEE, vec![circle_sector("bottom_right")]),
        (0x1FBEF, vec![circle_sector("top_left")]),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty clipped circle geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CC21_1CC2F.
#[test]
fn terminal_legacy_supplement_separated_quadrants_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();
    let quad_rects = [
        fill(1.0, 1.0, 6.0, 10.0, 1.0),
        fill(9.0, 1.0, 6.0, 10.0, 1.0),
        fill(1.0, 13.0, 6.0, 10.0, 1.0),
        fill(9.0, 13.0, 6.0, 10.0, 1.0),
    ];

    for cp in 0x1CC21..=0x1CC2F {
        let pattern = cp - 0x1CC20;
        let expected = sprite_commands_from_mask(pattern, &quad_rects);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty separated-quadrant geometry",
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/octants.txt and
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CD00_1CDE5.
#[test]
fn terminal_legacy_supplement_octants_match_upstream_fixture_mapping() {
    let (registry, rect) = sprite_fixture();

    for (offset, pattern) in OCTANT_PATTERNS.iter().copied().enumerate() {
        let cp = 0x1CD00 + offset as u32;
        let expected = grid_fills(pattern, 4, 2, rect);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty octants fixture geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CC1B_1CC1E and draw1CE16_1CE19.
#[test]
fn terminal_legacy_supplement_box_fragments_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (
            0x1CC1B,
            vec![
                fill(0.0, 11.0, 16.0, 2.0, 1.0),
                fill(14.0, 0.0, 2.0, 12.0, 1.0),
            ],
        ),
        (
            0x1CC1C,
            vec![
                fill(0.0, 11.0, 16.0, 2.0, 1.0),
                fill(14.0, 12.0, 2.0, 12.0, 1.0),
            ],
        ),
        (
            0x1CC1D,
            vec![
                fill(0.0, 0.0, 16.0, 2.0, 1.0),
                fill(0.0, 0.0, 2.0, 12.0, 1.0),
            ],
        ),
        (
            0x1CC1E,
            vec![
                fill(0.0, 22.0, 16.0, 2.0, 1.0),
                fill(0.0, 12.0, 2.0, 12.0, 1.0),
            ],
        ),
        (
            0x1CE16,
            vec![
                fill(7.0, 0.0, 2.0, 24.0, 1.0),
                fill(8.0, 0.0, 8.0, 2.0, 1.0),
            ],
        ),
        (
            0x1CE17,
            vec![
                fill(7.0, 0.0, 2.0, 24.0, 1.0),
                fill(8.0, 22.0, 8.0, 2.0, 1.0),
            ],
        ),
        (
            0x1CE18,
            vec![
                fill(7.0, 0.0, 2.0, 24.0, 1.0),
                fill(0.0, 0.0, 8.0, 2.0, 1.0),
            ],
        ),
        (
            0x1CE19,
            vec![
                fill(7.0, 0.0, 2.0, 24.0, 1.0),
                fill(0.0, 22.0, 8.0, 2.0, 1.0),
            ],
        ),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty supplement box fragment geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CE51_1CE8F.
#[test]
fn terminal_legacy_supplement_sextants_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();
    let sextant_rects = [
        fill(1.0, 1.0, 6.0, 6.0, 1.0),
        fill(9.0, 1.0, 6.0, 6.0, 1.0),
        fill(1.0, 9.0, 6.0, 6.0, 1.0),
        fill(9.0, 9.0, 6.0, 6.0, 1.0),
        fill(1.0, 17.0, 6.0, 6.0, 1.0),
        fill(9.0, 17.0, 6.0, 6.0, 1.0),
    ];

    for cp in 0x1CE51..=0x1CE8F {
        let pattern = cp - 0x1CE50;
        let expected = sprite_commands_from_mask(pattern, &sextant_rects);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty separated-sextant geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CE90_1CEAF.
#[test]
fn terminal_legacy_supplement_sixteenth_blocks_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for cp in 0x1CE90..=0x1CEAF {
        let expected = sixteenth_block_expected(cp);
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty sixteenth-block geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CC30_1CC3F.
#[test]
fn terminal_legacy_supplement_circle_pieces_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, spec) in [
        (0x1CC30, (0.0, 0.0, 2.0, 2.0, "upper_left")),
        (0x1CC31, (1.0, 0.0, 2.0, 2.0, "upper_left")),
        (0x1CC32, (2.0, 0.0, 2.0, 2.0, "upper_right")),
        (0x1CC33, (3.0, 0.0, 2.0, 2.0, "upper_right")),
        (0x1CC34, (0.0, 1.0, 2.0, 2.0, "upper_left")),
        (0x1CC35, (0.0, 0.0, 1.0, 1.0, "upper_left")),
        (0x1CC36, (1.0, 0.0, 1.0, 1.0, "upper_right")),
        (0x1CC37, (3.0, 1.0, 2.0, 2.0, "upper_right")),
        (0x1CC38, (0.0, 2.0, 2.0, 2.0, "lower_left")),
        (0x1CC39, (0.0, 1.0, 1.0, 1.0, "lower_left")),
        (0x1CC3A, (1.0, 1.0, 1.0, 1.0, "lower_right")),
        (0x1CC3B, (3.0, 2.0, 2.0, 2.0, "lower_right")),
        (0x1CC3C, (0.0, 3.0, 2.0, 2.0, "lower_left")),
        (0x1CC3D, (1.0, 3.0, 2.0, 2.0, "lower_left")),
        (0x1CC3E, (2.0, 3.0, 2.0, 2.0, "lower_right")),
        (0x1CC3F, (3.0, 3.0, 2.0, 2.0, "lower_right")),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            vec![supplement_circle_piece(rect, spec)],
            "Ghostty supplement circle-piece geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig
// draw1CE00, draw1CE01, draw1CE0B, and draw1CE0C.
#[test]
fn terminal_legacy_supplement_split_circles_and_ellipses_match_upstream_geometry() {
    let (registry, rect) = sprite_fixture();

    for (cp, expected) in [
        (0x1CE00, vec![circle_arc("left"), circle_arc("right")]),
        (0x1CE01, vec![circle_arc("top"), circle_arc("bottom")]),
        (
            0x1CE0B,
            vec![
                supplement_circle_piece(rect, (0.0, 0.0, 1.0, 0.5, "upper_left")),
                supplement_circle_piece(rect, (0.0, 0.0, 1.0, 0.5, "lower_left")),
            ],
        ),
        (
            0x1CE0C,
            vec![
                supplement_circle_piece(rect, (1.0, 0.0, 1.0, 0.5, "upper_right")),
                supplement_circle_piece(rect, (1.0, 0.0, 1.0, 0.5, "lower_right")),
            ],
        ),
    ] {
        assert_sprite_commands_for_cp(
            &registry,
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty supplement circle/ellipse geometry",
        );
    }
}

#[test]
#[ignore = "requires Ghostty sprite range fixture that is not vendored in this rewrite"]
fn terminal_legacy_computing_supplement_draws_cover_all_upstream_ranges() {
    let (registry, rect) = sprite_fixture();
    let mut covered = 0usize;

    for cp in ghostty_legacy_computing_supplement_draw_codepoints() {
        let ch = char::from_u32(cp).unwrap_or_else(|| panic!("invalid upstream U+{cp:04X}"));
        let glyph = registry.glyph_for(ch).unwrap_or_else(|| {
            panic!("missing legacy computing supplement glyph for upstream U+{cp:04X}")
        });

        assert_eq!(
            glyph.family,
            SpriteFamily::LegacyComputingSupplement,
            "upstream U+{cp:04X} should be owned by the legacy computing supplement sprite family"
        );
        assert!(
            !registry.commands_for(glyph, rect).is_empty(),
            "upstream U+{cp:04X} should have renderer commands"
        );
        covered += 1;
    }

    assert_eq!(
        covered, 368,
        "Ghostty legacy computing supplement draw inventory changed; update the ported coverage"
    );
}

#[test]
#[ignore = "requires Ghostty sprite range fixture that is not vendored in this rewrite"]
fn terminal_legacy_computing_draws_cover_all_upstream_ranges() {
    let (registry, rect) = sprite_fixture();
    let mut covered = 0usize;

    for cp in ghostty_legacy_computing_draw_codepoints() {
        let ch = char::from_u32(cp).unwrap_or_else(|| panic!("invalid upstream U+{cp:04X}"));
        let glyph = registry
            .glyph_for(ch)
            .unwrap_or_else(|| panic!("missing legacy computing glyph for upstream U+{cp:04X}"));

        assert_eq!(
            glyph.family,
            SpriteFamily::LegacyComputing,
            "upstream U+{cp:04X} should be owned by the legacy computing sprite family"
        );
        if cp != 0x1FB93 {
            assert!(
                !registry.commands_for(glyph, rect).is_empty(),
                "upstream U+{cp:04X} should have renderer commands"
            );
        }
        covered += 1;
    }

    assert_eq!(
        covered, 213,
        "Ghostty legacy computing draw inventory changed; update the ported coverage"
    );
}

#[test]
fn terminal_sprite_face_owns_matrix_covered_ranges_before_font_fallback() {
    let registry = SpriteRegistry::prompt_graphics();

    for (ch, family) in [
        ('╭', SpriteFamily::BoxDrawing),
        ('▟', SpriteFamily::Block),
        ('⣿', SpriteFamily::Braille),
        ('\u{E0D4}', SpriteFamily::Powerline),
        ('\u{1FB00}', SpriteFamily::LegacyComputing),
        ('\u{1FB3C}', SpriteFamily::LegacyComputing),
        ('\u{1FB68}', SpriteFamily::LegacyComputing),
        ('\u{1FB70}', SpriteFamily::LegacyComputing),
        ('\u{1FB9A}', SpriteFamily::LegacyComputing),
        ('\u{1FB9C}', SpriteFamily::LegacyComputing),
        ('\u{1FB98}', SpriteFamily::LegacyComputing),
        ('\u{1FBA0}', SpriteFamily::LegacyComputing),
        ('\u{1FBAF}', SpriteFamily::LegacyComputing),
        ('\u{1FBBD}', SpriteFamily::LegacyComputing),
        ('\u{1FBCE}', SpriteFamily::LegacyComputing),
        ('\u{1FBD0}', SpriteFamily::LegacyComputing),
        ('\u{1FBE4}', SpriteFamily::LegacyComputing),
        ('\u{1FBE8}', SpriteFamily::LegacyComputing),
        ('\u{1CC1B}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CC2F}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CC30}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CD00}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE00}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE0B}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE16}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE51}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE90}', SpriteFamily::LegacyComputingSupplement),
    ] {
        assert_eq!(
            registry.glyph_for(ch).map(|glyph| glyph.family),
            Some(family)
        );
    }

    for recorded_not_implemented in ['■', '\u{1CC00}', '\u{1FBC0}', '\u{F5D0}'] {
        assert_eq!(registry.glyph_for(recorded_not_implemented), None);
    }
}

const OCTANT_PATTERNS: &[u8] = &[
    0x04, 0x06, 0x07, 0x08, 0x09, 0x0B, 0x0C, 0x0D, 0x0E, 0x10, 0x11, 0x12, 0x13, 0x15, 0x16, 0x17,
    0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27,
    0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38,
    0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A,
    0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x51, 0x52, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C, 0x5D,
    0x5E, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E,
    0x6F, 0x70, 0x71, 0x72, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E,
    0x7F, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D, 0x8E, 0x8F,
    0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9D, 0x9E, 0x9F,
    0xA1, 0xA2, 0xA3, 0xA4, 0xA6, 0xA7, 0xA8, 0xA9, 0xAB, 0xAC, 0xAD, 0xAE, 0xB0, 0xB1, 0xB2, 0xB3,
    0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF, 0xC1, 0xC2, 0xC3, 0xC4,
    0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF, 0xD0, 0xD1, 0xD2, 0xD3, 0xD4,
    0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF, 0xE0, 0xE1, 0xE2, 0xE3, 0xE4,
    0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF, 0xF1, 0xF2, 0xF3, 0xF4, 0xF6,
    0xF7, 0xF8, 0xF9, 0xFB, 0xFD, 0xFE,
];

fn ghostty_legacy_computing_draw_codepoints() -> Vec<u32> {
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/ghostty/src/font/sprite/draw/symbols_for_legacy_computing.zig");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    let mut codepoints = Vec::new();

    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub fn draw") else {
            continue;
        };
        let Some(name) = rest.split('(').next() else {
            continue;
        };
        let Some((start, end)) = parse_draw_range(name) else {
            continue;
        };
        if !(0x1FB00..=0x1FBFF).contains(&start) {
            continue;
        }
        codepoints.extend(start..=end);
    }

    codepoints.sort_unstable();
    codepoints.dedup();
    codepoints
}

fn box_line_junction_codepoints() -> impl Iterator<Item = u32> {
    (0x2500..=0x254B)
        .filter(|cp| !matches!(cp, 0x2504..=0x250B))
        .chain(0x2574..=0x257F)
}

fn box_dash_diagonal_codepoints() -> impl Iterator<Item = u32> {
    [
        0x2504, 0x2505, 0x2506, 0x2507, 0x2508, 0x2509, 0x250A, 0x250B, 0x254C, 0x254D, 0x254E,
        0x254F, 0x2571, 0x2572, 0x2573,
    ]
    .into_iter()
}

fn box_double_line_codepoints() -> impl Iterator<Item = u32> {
    0x2550..=0x256C
}

fn ghostty_legacy_computing_supplement_draw_codepoints() -> Vec<u32> {
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../vendor/ghostty/src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig",
    );
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    let mut codepoints = Vec::new();

    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub fn draw") else {
            continue;
        };
        let Some(name) = rest.split('(').next() else {
            continue;
        };
        let Some((start, end)) = parse_draw_range(name) else {
            continue;
        };
        if !(0x1CC00..=0x1CEBF).contains(&start) {
            continue;
        }
        codepoints.extend(start..=end);
    }

    codepoints.sort_unstable();
    codepoints.dedup();
    codepoints
}

fn parse_draw_range(name: &str) -> Option<(u32, u32)> {
    let (start, end) = name.split_once('_').unwrap_or((name, name));
    let start = u32::from_str_radix(start, 16).ok()?;
    let end = u32::from_str_radix(end, 16).ok()?;
    Some((start, end))
}

const SMOOTH_MOSAIC_PATTERNS: &[&[u8; 12]] = &[
    b"......#..##.",
    b"......#\\.###",
    b"...#..#\\.##.",
    b"...#..##.###",
    b"#..#..##.##.",
    b"/###########",
    b"./##########",
    b".##.########",
    b"..#.########",
    b".##.##.#####",
    b"..../#######",
    b"........#.##",
    b"......./####",
    b".....#./#.##",
    b".....#.#####",
    b"..#..#.##.##",
    b"##\\#########",
    b"#\\.#########",
    b"##.##.######",
    b"#..##.######",
    b"##.##.##.###",
    b"...#\\.######",
    b"#########\\##",
    b"#########.\\#",
    b"######.##.##",
    b"######.##..#",
    b"###.##.##.##",
    b"##.#........",
    b"####/.......",
    b"##.#/.#.....",
    b"#####.#.....",
    b"##.##.#..#..",
    b"#######/....",
    b"###########/",
    b"##########/.",
    b"########.##.",
    b"########.#..",
    b"#####.##.##.",
    b".##..#......",
    b"###.\\#......",
    b".##.\\#..#...",
    b"###.##..#...",
    b".##.##..#..#",
    b"######.\\#...",
];

#[test]
fn text_contract_resolves_sprite_owned_codepoints_before_text_fallback() {
    let contract = TerminalTextContract::new(
        TerminalTextConfig::default(),
        NativeSymbolPolicy::terminal_glyph_primitives(),
    );

    let shaped = contract.shape_run(&run("A⣿B"));

    assert_eq!(
        shaped.fragments,
        vec![
            TerminalTextFragment::Text {
                cell: 0,
                text: "A".to_owned()
            },
            TerminalTextFragment::NativeSymbol {
                cell: 1,
                ch: '⣿',
                class: NativeSymbolClass::Braille,
            },
            TerminalTextFragment::Text {
                cell: 2,
                text: "B".to_owned()
            },
        ]
    );
}

#[test]
fn sprite_batches_prepare_textured_atlas_quads() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
    let (builder, quads) = prepare_sprite_quads('⣿', rect, 128, 128);

    assert_eq!(quads.len(), 1);
    assert_eq!(quads[0].rect, rect);
    assert_eq!(quads[0].color, color());
    assert_eq!(builder.atlas_len(), 1);
}

#[test]
fn sprite_atlas_rasterizes_powerline_triangles_without_shearing() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0);
    let pixels = sprite_atlas_pixels('\u{E0B0}', rect);
    assert_eq!(
        pixels[(5 + 1) * 32 + 8 + 1],
        255,
        "right center should be filled"
    );
    assert_eq!(
        pixels[(5 + 1) * 32 + 1],
        255,
        "left center should be filled"
    );
    assert_eq!(
        pixels[(1 + 1) * 32 + 8 + 1],
        0,
        "right upper corner should stay empty"
    );
    assert_eq!(
        pixels[(8 + 1) * 32 + 8 + 1],
        0,
        "right lower corner should stay empty"
    );
}

#[test]
fn sprite_atlas_rasterizes_full_braille_as_discrete_dots() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
    let pixels = sprite_atlas_pixels('⣿', rect);
    assert_eq!(
        pixels[(1 + 1) * 32 + 1 + 1],
        255,
        "top-left dot should be filled"
    );
    assert_eq!(
        pixels[(1 + 1) * 32 + 6 + 1],
        255,
        "top-right dot should be filled"
    );
    assert_eq!(
        pixels[(16 + 1) * 32 + 1 + 1],
        255,
        "bottom-left dot should be filled"
    );
    assert_eq!(
        pixels[(16 + 1) * 32 + 6 + 1],
        255,
        "bottom-right dot should be filled"
    );
    assert_eq!(
        pixels[(10 + 1) * 32 + 4 + 1],
        0,
        "center gap should stay empty"
    );
}

#[test]
fn sprite_atlas_rasterizes_inverse_diagonal_clear_masks() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 16.0, 16.0);
    let pixels = sprite_atlas_pixels('\u{1FBBD}', rect);
    assert_eq!(pixels[32 + 1], 0, "upper-left diagonal should be cleared");
    assert_eq!(
        pixels[32 + 15 + 1],
        0,
        "upper-right diagonal should be cleared"
    );
    assert_eq!(
        pixels[(7 + 1) * 32 + 8 + 1],
        0,
        "cross center should be cleared"
    );
    assert_eq!(pixels[32 + 8 + 1], 255, "top center should stay filled");
    assert_eq!(
        pixels[(8 + 1) * 32 + 1],
        255,
        "left center should stay filled"
    );
}

fn run(text: &str) -> TextRun {
    TextRun {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 30.0, 20.0),
        cells: 3,
        text: text.to_owned(),
        attrs: TextAttrs {
            fg: color(),
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        },
    }
}

fn color() -> PlanColor {
    PlanColor {
        r: 220,
        g: 221,
        b: 222,
        a: 255,
    }
}

fn fill(x: f32, y: f32, width: f32, height: f32, alpha: f32) -> SpriteCommand {
    SpriteCommand::FillRect {
        rect: SurfaceRect::from_min_size(x, y, width, height),
        alpha,
    }
}

fn assert_sprite_commands(
    registry: &SpriteRegistry,
    rect: SurfaceRect,
    ch: char,
    family: SpriteFamily,
    expected: Vec<SpriteCommand>,
    detail: &str,
) {
    let glyph = registry
        .glyph_for(ch)
        .unwrap_or_else(|| panic!("missing glyph {ch}"));
    assert_eq!(glyph.family, family, "{ch} should be owned as {family:?}");
    assert_eq!(
        registry.commands_for(glyph, rect),
        expected,
        "{ch} should match {detail}"
    );
}

fn assert_sprite_commands_for_cp(
    registry: &SpriteRegistry,
    rect: SurfaceRect,
    cp: u32,
    family: SpriteFamily,
    expected: Vec<SpriteCommand>,
    detail: &str,
) {
    let ch = char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}"));
    let glyph = registry
        .glyph_for(ch)
        .unwrap_or_else(|| panic!("missing glyph U+{cp:04X}"));
    assert_eq!(
        glyph.family, family,
        "U+{cp:04X} should be owned as {family:?}"
    );
    assert_eq!(
        registry.commands_for(glyph, rect),
        expected,
        "U+{cp:04X} should match {detail}"
    );
}

fn assert_sprite_has_commands_for_codepoints<I>(
    registry: &SpriteRegistry,
    rect: SurfaceRect,
    codepoints: I,
    family: SpriteFamily,
) where
    I: IntoIterator<Item = u32>,
{
    for cp in codepoints {
        assert_sprite_has_commands_for_cp(registry, rect, cp, family);
    }
}

fn assert_sprite_has_commands_for_cp(
    registry: &SpriteRegistry,
    rect: SurfaceRect,
    cp: u32,
    family: SpriteFamily,
) {
    let ch = char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}"));
    let glyph = registry
        .glyph_for(ch)
        .unwrap_or_else(|| panic!("missing glyph U+{cp:04X}"));
    assert_eq!(
        glyph.family, family,
        "U+{cp:04X} should be owned as {family:?}"
    );
    assert!(
        !registry.commands_for(glyph, rect).is_empty(),
        "U+{cp:04X} should have renderer commands"
    );
}

fn sprite_commands_from_mask(pattern: u32, commands: &[SpriteCommand]) -> Vec<SpriteCommand> {
    commands
        .iter()
        .enumerate()
        .filter_map(|(bit, command)| {
            if pattern & (1 << bit) != 0 {
                Some(command.clone())
            } else {
                None
            }
        })
        .collect()
}

fn assert_sprite_command_close(actual: &[SpriteCommand], expected: &[SpriteCommand], ch: char) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "{ch} should emit the expected command count"
    );
    for (actual, expected) in actual.iter().zip(expected) {
        match (actual, expected) {
            (
                SpriteCommand::FillRect {
                    rect: actual_rect,
                    alpha: actual_alpha,
                },
                SpriteCommand::FillRect {
                    rect: expected_rect,
                    alpha: expected_alpha,
                },
            ) => {
                assert_close(actual_rect.min_x, expected_rect.min_x, ch);
                assert_close(actual_rect.min_y, expected_rect.min_y, ch);
                assert_close(actual_rect.width(), expected_rect.width(), ch);
                assert_close(actual_rect.height(), expected_rect.height(), ch);
                assert_close(*actual_alpha, *expected_alpha, ch);
            }
            _ => panic!("{ch} emitted unexpected sprite commands: {actual:?}"),
        }
    }
}

fn assert_close(actual: f32, expected: f32, ch: char) {
    assert!(
        (actual - expected).abs() <= 0.0001,
        "{ch} expected {expected}, got {actual}"
    );
}

fn grid_fills(pattern: u8, rows: u8, cols: u8, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let cell_width = rect.width() / f32::from(cols);
    let cell_height = rect.height() / f32::from(rows);
    let mut commands = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let bit = row * cols + col;
            if pattern & (1 << bit) == 0 {
                continue;
            }
            commands.push(fill(
                rect.min_x + f32::from(col) * cell_width,
                rect.min_y + f32::from(row) * cell_height,
                cell_width,
                cell_height,
                1.0,
            ));
        }
    }
    commands
}

fn sixteenth_block_expected(cp: u32) -> Vec<SpriteCommand> {
    if (0x1CE90..=0x1CE9F).contains(&cp) {
        let index = cp - 0x1CE90;
        let row = index / 4;
        let col = index % 4;
        return vec![quarter_fill(col, col + 1, row, row + 1)];
    }

    let spec = match cp {
        0x1CEA0 => (2, 4, 3, 4),
        0x1CEA1 => (1, 4, 3, 4),
        0x1CEA2 => (0, 3, 3, 4),
        0x1CEA3 => (0, 2, 3, 4),
        0x1CEA4 => (0, 1, 2, 4),
        0x1CEA5 => (0, 1, 1, 4),
        0x1CEA6 => (0, 1, 0, 3),
        0x1CEA7 => (0, 1, 0, 2),
        0x1CEA8 => (0, 2, 0, 1),
        0x1CEA9 => (0, 3, 0, 1),
        0x1CEAA => (1, 4, 0, 1),
        0x1CEAB => (2, 4, 0, 1),
        0x1CEAC => (3, 4, 0, 2),
        0x1CEAD => (3, 4, 0, 3),
        0x1CEAE => (3, 4, 1, 4),
        0x1CEAF => (3, 4, 2, 4),
        _ => panic!("unexpected sixteenth block U+{cp:04X}"),
    };

    vec![quarter_fill(spec.0, spec.1, spec.2, spec.3)]
}

fn quarter_fill(left: u32, right: u32, top: u32, bottom: u32) -> SpriteCommand {
    fill(
        left as f32 * 4.0,
        top as f32 * 6.0,
        (right - left) as f32 * 4.0,
        (bottom - top) as f32 * 6.0,
        1.0,
    )
}

fn smooth_mosaic_expected(pattern: &[u8; 12], rect: SurfaceRect) -> Vec<SpriteCommand> {
    let upper = rect.min_y + rect.height() / 3.0;
    let lower = rect.min_y + rect.height() * 2.0 / 3.0;
    let center = rect.min_x + rect.width() * 0.5;
    let checks = [
        (
            pattern[0] == b'#',
            SpritePoint {
                x: rect.min_x,
                y: rect.min_y,
            },
        ),
        (
            pattern[3] == b'#' && (pattern[0] != b'#' || pattern[6] != b'#'),
            SpritePoint {
                x: rect.min_x,
                y: upper,
            },
        ),
        (
            pattern[6] == b'#' && (pattern[3] != b'#' || pattern[9] != b'#'),
            SpritePoint {
                x: rect.min_x,
                y: lower,
            },
        ),
        (
            pattern[9] == b'#',
            SpritePoint {
                x: rect.min_x,
                y: rect.max_y,
            },
        ),
        (
            pattern[10] == b'#' && (pattern[9] != b'#' || pattern[11] != b'#'),
            SpritePoint {
                x: center,
                y: rect.max_y,
            },
        ),
        (
            pattern[11] == b'#',
            SpritePoint {
                x: rect.max_x,
                y: rect.max_y,
            },
        ),
        (
            pattern[8] == b'#' && (pattern[11] != b'#' || pattern[5] != b'#'),
            SpritePoint {
                x: rect.max_x,
                y: lower,
            },
        ),
        (
            pattern[5] == b'#' && (pattern[8] != b'#' || pattern[2] != b'#'),
            SpritePoint {
                x: rect.max_x,
                y: upper,
            },
        ),
        (
            pattern[2] == b'#',
            SpritePoint {
                x: rect.max_x,
                y: rect.min_y,
            },
        ),
        (
            pattern[1] == b'#' && (pattern[2] != b'#' || pattern[0] != b'#'),
            SpritePoint {
                x: center,
                y: rect.min_y,
            },
        ),
    ];
    let points = checks
        .into_iter()
        .filter_map(|(enabled, point)| enabled.then_some(point))
        .collect::<Vec<_>>();

    if points.len() < 3 {
        Vec::new()
    } else {
        vec![polygon_points(points)]
    }
}

fn legacy_block_extension_expected(cp: u32, rect: SurfaceRect) -> Vec<SpriteCommand> {
    if (0x1FB70..=0x1FB75).contains(&cp) {
        let slot = (cp - 0x1FB6F) as u8;
        return vec![column_fractions(rect, slot, slot + 1, 1.0)];
    }
    if (0x1FB76..=0x1FB7B).contains(&cp) {
        let slot = (cp - 0x1FB75) as u8;
        return vec![row_fractions(rect, slot, slot + 1, 1.0)];
    }

    match cp {
        0x1FB7C => vec![
            column_fractions(rect, 0, 1, 1.0),
            row_fractions(rect, 7, 8, 1.0),
        ],
        0x1FB7D => vec![
            column_fractions(rect, 0, 1, 1.0),
            row_fractions(rect, 0, 1, 1.0),
        ],
        0x1FB7E => vec![
            column_fractions(rect, 7, 8, 1.0),
            row_fractions(rect, 0, 1, 1.0),
        ],
        0x1FB7F => vec![
            column_fractions(rect, 7, 8, 1.0),
            row_fractions(rect, 7, 8, 1.0),
        ],
        0x1FB80 => vec![
            row_fractions(rect, 0, 1, 1.0),
            row_fractions(rect, 7, 8, 1.0),
        ],
        0x1FB81 => vec![
            row_fractions(rect, 0, 1, 1.0),
            row_fractions(rect, 2, 3, 1.0),
            row_fractions(rect, 4, 5, 1.0),
            row_fractions(rect, 7, 8, 1.0),
        ],
        0x1FB82 => vec![row_fractions(rect, 0, 2, 1.0)],
        0x1FB83 => vec![row_fractions(rect, 0, 3, 1.0)],
        0x1FB84 => vec![row_fractions(rect, 0, 5, 1.0)],
        0x1FB85 => vec![row_fractions(rect, 0, 6, 1.0)],
        0x1FB86 => vec![row_fractions(rect, 0, 7, 1.0)],
        0x1FB87 => vec![column_fractions(rect, 6, 8, 1.0)],
        0x1FB88 => vec![column_fractions(rect, 5, 8, 1.0)],
        0x1FB89 => vec![column_fractions(rect, 3, 8, 1.0)],
        0x1FB8A => vec![column_fractions(rect, 2, 8, 1.0)],
        0x1FB8B => vec![column_fractions(rect, 1, 8, 1.0)],
        0x1FB8C => vec![column_fractions(rect, 0, 4, 0.5)],
        0x1FB8D => vec![column_fractions(rect, 4, 8, 0.5)],
        0x1FB8E => vec![row_fractions(rect, 0, 4, 0.5)],
        0x1FB8F => vec![row_fractions(rect, 4, 8, 0.5)],
        0x1FB90 => vec![fill(
            rect.min_x,
            rect.min_y,
            rect.width(),
            rect.height(),
            0.5,
        )],
        0x1FB91 => vec![
            fill(rect.min_x, rect.min_y, rect.width(), rect.height(), 0.5),
            row_fractions(rect, 0, 4, 1.0),
        ],
        0x1FB92 => vec![
            fill(rect.min_x, rect.min_y, rect.width(), rect.height(), 0.5),
            row_fractions(rect, 4, 8, 1.0),
        ],
        0x1FB93 => Vec::new(),
        0x1FB94 => vec![
            fill(rect.min_x, rect.min_y, rect.width(), rect.height(), 0.5),
            column_fractions(rect, 4, 8, 1.0),
        ],
        0x1FB95 => checkerboard_fills(rect, 0),
        0x1FB96 => checkerboard_fills(rect, 1),
        0x1FB97 => vec![
            row_fractions(rect, 2, 4, 1.0),
            row_fractions(rect, 6, 8, 1.0),
        ],
        _ => unreachable!("unexpected legacy block extension U+{cp:04X}"),
    }
}

fn column_fractions(rect: SurfaceRect, start: u8, end: u8, alpha: f32) -> SpriteCommand {
    let width = rect.width() / 8.0;
    fill(
        rect.min_x + f32::from(start) * width,
        rect.min_y,
        f32::from(end - start) * width,
        rect.height(),
        alpha,
    )
}

fn row_fractions(rect: SurfaceRect, start: u8, end: u8, alpha: f32) -> SpriteCommand {
    let height = rect.height() / 8.0;
    fill(
        rect.min_x,
        rect.min_y + f32::from(start) * height,
        rect.width(),
        f32::from(end - start) * height,
        alpha,
    )
}

fn checkerboard_fills(rect: SurfaceRect, parity: usize) -> Vec<SpriteCommand> {
    let x_cells = 4usize;
    let y_cells = (4.0 * (rect.height() / rect.width())).round().max(1.0) as usize;
    let width = rect.width() / x_cells as f32;
    let height = rect.height() / y_cells as f32;
    let mut commands = Vec::new();
    for x in 0..x_cells {
        for y in 0..y_cells {
            if (x + y) % 2 == parity {
                commands.push(fill(
                    rect.min_x + x as f32 * width,
                    rect.min_y + y as f32 * height,
                    width,
                    height,
                    1.0,
                ));
            }
        }
    }
    commands
}

fn edge_triangle(edge: &str) -> SpriteCommand {
    let center = (8.0, 12.0);
    let (a, b) = edge_span(edge);
    triangle([center, a, b])
}

fn inverted_edge_triangles(edge: &str) -> Vec<SpriteCommand> {
    match edge {
        "left" => vec![
            triangle([(0.0, 0.0), (16.0, 0.0), (8.0, 12.0)]),
            triangle([(8.0, 12.0), (16.0, 24.0), (0.0, 24.0)]),
        ],
        "top" => vec![
            triangle([(0.0, 0.0), (0.0, 24.0), (8.0, 12.0)]),
            triangle([(8.0, 12.0), (16.0, 24.0), (16.0, 0.0)]),
        ],
        "right" => vec![
            triangle([(16.0, 0.0), (0.0, 0.0), (8.0, 12.0)]),
            triangle([(8.0, 12.0), (0.0, 24.0), (16.0, 24.0)]),
        ],
        "bottom" => vec![
            triangle([(0.0, 24.0), (0.0, 0.0), (8.0, 12.0)]),
            triangle([(8.0, 12.0), (16.0, 0.0), (16.0, 24.0)]),
        ],
        _ => unreachable!("unexpected edge {edge}"),
    }
}

fn edge_span(edge: &str) -> ((f32, f32), (f32, f32)) {
    match edge {
        "top" => ((16.0, 0.0), (0.0, 0.0)),
        "left" => ((0.0, 0.0), (0.0, 24.0)),
        "bottom" => ((0.0, 24.0), (16.0, 24.0)),
        "right" => ((16.0, 24.0), (16.0, 0.0)),
        _ => unreachable!("unexpected edge {edge}"),
    }
}

type Point = (f32, f32);
type Segment = (Point, Point);

fn diagonal_segments(segments: &[Segment]) -> Vec<SpriteCommand> {
    segments
        .iter()
        .map(|(from, to)| stroke(vec![*from, *to]))
        .collect()
}

fn hatch_expected(rect: SurfaceRect, descending: bool) -> Vec<SpriteCommand> {
    let line_count = (rect.width() / 4.0).floor().max(1.0) as i32;
    let stride = (rect.width() / line_count as f32).round();
    (-line_count..=line_count)
        .map(|i| hatch_line(rect, i as f32 * stride, descending))
        .collect()
}

fn hatch_line(rect: SurfaceRect, offset: f32, descending: bool) -> SpriteCommand {
    let w = rect.width();
    let h = rect.height();
    let mut points = Vec::new();
    let add_unique = |points: &mut Vec<SpritePoint>, x: f32, y: f32| {
        let point = SpritePoint { x, y };
        if !points.iter().any(|existing: &SpritePoint| {
            (existing.x - point.x).abs() < 0.001 && (existing.y - point.y).abs() < 0.001
        }) {
            points.push(point);
        }
    };

    if descending {
        let top_x = w + offset;
        let bottom_x = offset;
        if (0.0..=w).contains(&top_x) {
            add_unique(&mut points, rect.min_x + top_x, rect.min_y);
        }
        if (0.0..=w).contains(&bottom_x) {
            add_unique(&mut points, rect.min_x + bottom_x, rect.max_y);
        }
        let left_y = h * (w + offset) / w;
        if (0.0..=h).contains(&left_y) {
            add_unique(&mut points, rect.min_x, rect.min_y + left_y);
        }
        let right_y = h * offset / w;
        if (0.0..=h).contains(&right_y) {
            add_unique(&mut points, rect.max_x, rect.min_y + right_y);
        }
    } else {
        let top_x = offset;
        let bottom_x = w + offset;
        if (0.0..=w).contains(&top_x) {
            add_unique(&mut points, rect.min_x + top_x, rect.min_y);
        }
        if (0.0..=w).contains(&bottom_x) {
            add_unique(&mut points, rect.min_x + bottom_x, rect.max_y);
        }
        let left_y = -offset * h / w;
        if (0.0..=h).contains(&left_y) {
            add_unique(&mut points, rect.min_x, rect.min_y + left_y);
        }
        let right_y = (w - offset) * h / w;
        if (0.0..=h).contains(&right_y) {
            add_unique(&mut points, rect.max_x, rect.min_y + right_y);
        }
    }

    SpriteCommand::StrokePolyline {
        points,
        width: 2.0,
        alpha: 1.0,
    }
}

fn inverse_diagonal_cross(rect: SurfaceRect) -> Vec<SpriteCommand> {
    let slope_x = rect.width().min(rect.height()) / rect.height().max(1.0);
    let slope_y = rect.height().min(rect.width()) / rect.width().max(1.0);
    vec![
        fill(rect.min_x, rect.min_y, rect.width(), rect.height(), 1.0),
        clear_stroke_points(vec![
            SpritePoint {
                x: rect.max_x + 0.5 * slope_x,
                y: rect.min_y - 0.5 * slope_y,
            },
            SpritePoint {
                x: rect.min_x - 0.5 * slope_x,
                y: rect.max_y + 0.5 * slope_y,
            },
        ]),
        clear_stroke_points(vec![
            SpritePoint {
                x: rect.min_x - 0.5 * slope_x,
                y: rect.min_y - 0.5 * slope_y,
            },
            SpritePoint {
                x: rect.max_x + 0.5 * slope_x,
                y: rect.max_y + 0.5 * slope_y,
            },
        ]),
    ]
}

fn inverse_corner_diagonals(rect: SurfaceRect, corners: &[&str]) -> Vec<SpriteCommand> {
    let mut commands = vec![fill(
        rect.min_x,
        rect.min_y,
        rect.width(),
        rect.height(),
        1.0,
    )];
    commands.extend(corners.iter().map(|corner| {
        let (from, to) = corner_diagonal_segment(corner);
        clear_stroke(vec![from, to])
    }));
    commands
}

fn circle_arc(position: &str) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: circle_arc_points(position),
        width: 2.0,
        alpha: 1.0,
    }
}

fn circle_sector(position: &str) -> SpriteCommand {
    let mut points = vec![circle_center(position)];
    points.extend(circle_arc_points(position));
    polygon_points(points)
}

fn circle_arc_points(position: &str) -> Vec<SpritePoint> {
    let (start, end) = circle_angles(position);
    let center = circle_center(position);
    let radius = 8.0;
    let steps = if (end - start).abs() > std::f32::consts::FRAC_PI_2 {
        8
    } else {
        4
    };
    (0..=steps)
        .map(|step| {
            let angle = start + (end - start) * (step as f32 / steps as f32);
            SpritePoint {
                x: center.x + radius * angle.cos(),
                y: center.y + radius * angle.sin(),
            }
        })
        .collect()
}

fn rounded_corner(corner: &str) -> SpriteCommand {
    let center_x = 8.0;
    let center_y = 12.0;
    let radius = 8.0;
    let s = 0.25;
    let mut points = Vec::new();

    match corner {
        "upper_left" => {
            points.push(SpritePoint {
                x: center_x,
                y: 24.0,
            });
            points.push(SpritePoint {
                x: center_x,
                y: center_y + radius,
            });
            sample_cubic_points(
                [
                    SpritePoint {
                        x: center_x,
                        y: center_y + radius,
                    },
                    SpritePoint {
                        x: center_x,
                        y: center_y + s * radius,
                    },
                    SpritePoint {
                        x: center_x + s * radius,
                        y: center_y,
                    },
                    SpritePoint {
                        x: center_x + radius,
                        y: center_y,
                    },
                ],
                &mut points,
            );
        }
        "upper_right" => {
            points.push(SpritePoint {
                x: center_x,
                y: 24.0,
            });
            points.push(SpritePoint {
                x: center_x,
                y: center_y + radius,
            });
            sample_cubic_points(
                [
                    SpritePoint {
                        x: center_x,
                        y: center_y + radius,
                    },
                    SpritePoint {
                        x: center_x,
                        y: center_y + s * radius,
                    },
                    SpritePoint {
                        x: center_x - s * radius,
                        y: center_y,
                    },
                    SpritePoint {
                        x: center_x - radius,
                        y: center_y,
                    },
                ],
                &mut points,
            );
        }
        "lower_right" => {
            points.push(SpritePoint {
                x: center_x,
                y: 0.0,
            });
            points.push(SpritePoint {
                x: center_x,
                y: center_y - radius,
            });
            sample_cubic_points(
                [
                    SpritePoint {
                        x: center_x,
                        y: center_y - radius,
                    },
                    SpritePoint {
                        x: center_x,
                        y: center_y - s * radius,
                    },
                    SpritePoint {
                        x: center_x - s * radius,
                        y: center_y,
                    },
                    SpritePoint {
                        x: center_x - radius,
                        y: center_y,
                    },
                ],
                &mut points,
            );
        }
        "lower_left" => {
            points.push(SpritePoint {
                x: center_x,
                y: 0.0,
            });
            points.push(SpritePoint {
                x: center_x,
                y: center_y - radius,
            });
            sample_cubic_points(
                [
                    SpritePoint {
                        x: center_x,
                        y: center_y - radius,
                    },
                    SpritePoint {
                        x: center_x,
                        y: center_y - s * radius,
                    },
                    SpritePoint {
                        x: center_x + s * radius,
                        y: center_y,
                    },
                    SpritePoint {
                        x: center_x + radius,
                        y: center_y,
                    },
                ],
                &mut points,
            );
        }
        _ => panic!("unknown rounded corner {corner}"),
    }

    stroke_points(points)
}

fn supplement_circle_piece(
    rect: SurfaceRect,
    (x, y, width, height, corner): (f32, f32, f32, f32, &str),
) -> SpriteCommand {
    let wdth = rect.width() * width;
    let hght = rect.height() * height;
    let xp = rect.width() * x;
    let yp = rect.height() * y;
    let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let cw = c * wdth;
    let ch = c * hght;
    let ht = 1.0;
    let point = |px: f32, py: f32| SpritePoint {
        x: rect.min_x + px,
        y: rect.min_y + py,
    };

    let mut points = match corner {
        "upper_left" => {
            let mut points = vec![point(wdth - xp, ht - yp)];
            sample_cubic_points(
                [
                    point(wdth - xp, ht - yp),
                    point(wdth - cw - xp, ht - yp),
                    point(ht - xp, hght - ch - yp),
                    point(ht - xp, hght - yp),
                ],
                &mut points,
            );
            points
        }
        "upper_right" => {
            let mut points = vec![point(wdth - xp, ht - yp)];
            sample_cubic_points(
                [
                    point(wdth - xp, ht - yp),
                    point(wdth + cw - xp, ht - yp),
                    point(wdth * 2.0 - ht - xp, hght - ch - yp),
                    point(wdth * 2.0 - ht - xp, hght - yp),
                ],
                &mut points,
            );
            points
        }
        "lower_left" => {
            let mut points = vec![point(ht - xp, hght - yp)];
            sample_cubic_points(
                [
                    point(ht - xp, hght - yp),
                    point(ht - xp, hght + ch - yp),
                    point(wdth - cw - xp, hght * 2.0 - ht - yp),
                    point(wdth - xp, hght * 2.0 - ht - yp),
                ],
                &mut points,
            );
            points
        }
        "lower_right" => {
            let mut points = vec![point(wdth * 2.0 - ht - xp, hght - yp)];
            sample_cubic_points(
                [
                    point(wdth * 2.0 - ht - xp, hght - yp),
                    point(wdth * 2.0 - ht - xp, hght + ch - yp),
                    point(wdth + cw - xp, hght * 2.0 - ht - yp),
                    point(wdth - xp, hght * 2.0 - ht - yp),
                ],
                &mut points,
            );
            points
        }
        _ => panic!("unknown supplement circle-piece corner {corner}"),
    };
    points.retain(|point| {
        point.x >= rect.min_x
            && point.x <= rect.max_x
            && point.y >= rect.min_y
            && point.y <= rect.max_y
    });
    stroke_points(points)
}

fn circle_center(position: &str) -> SpritePoint {
    match position {
        "top" => SpritePoint { x: 8.0, y: 0.0 },
        "right" => SpritePoint { x: 16.0, y: 12.0 },
        "bottom" => SpritePoint { x: 8.0, y: 24.0 },
        "left" => SpritePoint { x: 0.0, y: 12.0 },
        "top_right" => SpritePoint { x: 16.0, y: 0.0 },
        "bottom_left" => SpritePoint { x: 0.0, y: 24.0 },
        "bottom_right" => SpritePoint { x: 16.0, y: 24.0 },
        "top_left" => SpritePoint { x: 0.0, y: 0.0 },
        _ => unreachable!("unexpected circle position {position}"),
    }
}

fn circle_angles(position: &str) -> (f32, f32) {
    let pi = std::f32::consts::PI;
    let half = std::f32::consts::FRAC_PI_2;
    match position {
        "top" => (0.0, pi),
        "right" => (half, pi + half),
        "bottom" => (pi, 2.0 * pi),
        "left" => (-half, half),
        "top_right" => (half, pi),
        "bottom_left" => (-half, 0.0),
        "bottom_right" => (pi, pi + half),
        "top_left" => (0.0, half),
        _ => unreachable!("unexpected circle position {position}"),
    }
}

fn corner_diagonal_segments(corners: &[&str]) -> Vec<SpriteCommand> {
    corners
        .iter()
        .map(|corner| {
            let segment = match *corner {
                "tl" => ((8.0, 0.0), (0.0, 12.0)),
                "tr" => ((8.0, 0.0), (16.0, 12.0)),
                "bl" => ((8.0, 24.0), (0.0, 12.0)),
                "br" => ((8.0, 24.0), (16.0, 12.0)),
                _ => unreachable!("unexpected corner {corner}"),
            };
            stroke(vec![segment.0, segment.1])
        })
        .collect()
}

fn corner_diagonal_segment(corner: &str) -> ((f32, f32), (f32, f32)) {
    match corner {
        "tl" => ((8.0, 0.0), (0.0, 12.0)),
        "tr" => ((8.0, 0.0), (16.0, 12.0)),
        "bl" => ((8.0, 24.0), (0.0, 12.0)),
        "br" => ((8.0, 24.0), (16.0, 12.0)),
        _ => unreachable!("unexpected corner {corner}"),
    }
}

fn polygon(points: Vec<(f32, f32)>) -> SpriteCommand {
    polygon_points(
        points
            .into_iter()
            .map(|(x, y)| SpritePoint { x, y })
            .collect(),
    )
}

fn polygon_points(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Polygon,
        points,
        alpha: 1.0,
    }
}

fn triangle(points: [(f32, f32); 3]) -> SpriteCommand {
    triangle_alpha(points, 1.0)
}

fn triangle_alpha(points: [(f32, f32); 3], alpha: f32) -> SpriteCommand {
    SpriteCommand::FillPolygon {
        shape: SpriteShape::Triangle,
        points: points
            .into_iter()
            .map(|(x, y)| SpritePoint { x, y })
            .collect(),
        alpha,
    }
}

fn stroke(points: Vec<(f32, f32)>) -> SpriteCommand {
    stroke_points(
        points
            .into_iter()
            .map(|(x, y)| SpritePoint { x, y })
            .collect(),
    )
}

fn clear_stroke(points: Vec<(f32, f32)>) -> SpriteCommand {
    clear_stroke_points(
        points
            .into_iter()
            .map(|(x, y)| SpritePoint { x, y })
            .collect(),
    )
}

fn clear_stroke_points(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::ClearStrokePolyline {
        points,
        width: 2.0,
        alpha: 1.0,
    }
}

fn stroke_points(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points,
        width: 2.0,
        alpha: 1.0,
    }
}

fn terminal_right_round_points(rect: SurfaceRect) -> Vec<SpritePoint> {
    let radius = rect.width().min(rect.height() * 0.5);
    let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let mut points = vec![SpritePoint {
        x: rect.min_x,
        y: rect.min_y,
    }];
    sample_cubic(
        [
            (rect.min_x, rect.min_y),
            (rect.min_x + radius * c, rect.min_y),
            (rect.min_x + radius, rect.min_y + radius - radius * c),
            (rect.min_x + radius, rect.min_y + radius),
        ],
        &mut points,
    );
    points.push(SpritePoint {
        x: rect.min_x + radius,
        y: rect.max_y - radius,
    });
    sample_cubic(
        [
            (rect.min_x + radius, rect.max_y - radius),
            (rect.min_x + radius, rect.max_y - radius + radius * c),
            (rect.min_x + radius * c, rect.max_y),
            (rect.min_x, rect.max_y),
        ],
        &mut points,
    );
    points
}

fn sample_cubic(points: [(f32, f32); 4], out: &mut Vec<SpritePoint>) {
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let mt = 1.0 - t;
        out.push(SpritePoint {
            x: mt.powi(3) * points[0].0
                + 3.0 * mt.powi(2) * t * points[1].0
                + 3.0 * mt * t.powi(2) * points[2].0
                + t.powi(3) * points[3].0,
            y: mt.powi(3) * points[0].1
                + 3.0 * mt.powi(2) * t * points[1].1
                + 3.0 * mt * t.powi(2) * points[2].1
                + t.powi(3) * points[3].1,
        });
    }
}

fn sample_cubic_points(points: [SpritePoint; 4], out: &mut Vec<SpritePoint>) {
    for step in 1..=8 {
        let t = step as f32 / 8.0;
        let mt = 1.0 - t;
        out.push(SpritePoint {
            x: mt.powi(3) * points[0].x
                + 3.0 * mt.powi(2) * t * points[1].x
                + 3.0 * mt * t.powi(2) * points[2].x
                + t.powi(3) * points[3].x,
            y: mt.powi(3) * points[0].y
                + 3.0 * mt.powi(2) * t * points[1].y
                + 3.0 * mt * t.powi(2) * points[2].y
                + t.powi(3) * points[3].y,
        });
    }
}

fn flip_points(points: &[SpritePoint], rect: SurfaceRect) -> Vec<SpritePoint> {
    points
        .iter()
        .map(|point| SpritePoint {
            x: rect.min_x + rect.max_x - point.x,
            y: point.y,
        })
        .collect()
}
