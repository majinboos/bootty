use bootty_render::{
    paint_plan::PlanColor,
    terminal_render::SpriteCommandBatch,
    terminal_sprite::{SpriteCommand, SpriteFamily, SpriteGlyph, SpritePoint, SpriteShape},
    terminal_text_atlas::{TextAtlasBuilder, TexturedGlyphQuad},
};
use bootty_surface::geometry::SurfaceRect;

fn sprite_fixture() -> SurfaceRect {
    SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0)
}

fn sprite_command_batch(ch: char, rect: SurfaceRect) -> SpriteCommandBatch {
    let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph {ch}"));
    SpriteCommandBatch {
        ch: glyph.ch,
        glyph,
        rect,
        color: color(),
        commands: glyph.commands_for(rect),
    }
}

fn prepare_sprite_quads(
    ch: char,
    rect: SurfaceRect,
    atlas_width: u32,
    atlas_height: u32,
) -> (TextAtlasBuilder, Vec<TexturedGlyphQuad>) {
    let batch = sprite_command_batch(ch, rect);
    let mut builder = TextAtlasBuilder::new(atlas_width, atlas_height);
    let quads = vec![builder.prepare_sprite_command(&batch, 1.0)];
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
    let rect = sprite_fixture();

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
        let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(
            glyph.commands_for(rect),
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
    let rect = sprite_fixture();

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
        let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(glyph.family, SpriteFamily::ProgressIndicator);
        assert_sprite_command_close(&glyph.commands_for(rect), &[expected], ch);
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F linesChar cases for light/heavy line junctions.
#[test]
fn terminal_box_drawing_line_junctions_match_upstream_geometry() {
    let rect = sprite_fixture();

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
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty line-junction geometry",
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F dashHorizontal, dashVertical, and lightDiagonal* cases.
#[test]
fn terminal_box_drawing_dashes_and_diagonals_match_upstream_geometry() {
    let rect = sprite_fixture();

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
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty dash/diagonal geometry",
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F linesChar double-line cases.
#[test]
fn terminal_box_drawing_double_lines_match_upstream_geometry() {
    let rect = sprite_fixture();

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
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            expected,
            "Ghostty double-line geometry",
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/box.zig
// draw2500_257F arc cases for rounded corners.
#[test]
fn terminal_box_drawing_rounded_corners_match_upstream_geometry() {
    let rect = sprite_fixture();

    for (ch, corner) in [
        ('╭', "upper_left"),
        ('╮', "upper_right"),
        ('╯', "lower_right"),
        ('╰', "lower_left"),
    ] {
        assert_sprite_commands(
            rect,
            ch,
            SpriteFamily::BoxDrawing,
            vec![rounded_corner(corner)],
            "Ghostty rounded-corner geometry",
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/powerline.zig drawE0D2/drawE0D4.
#[test]
fn terminal_powerline_extra_split_glyphs_use_upstream_polygons() {
    let rect = sprite_fixture();

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
        let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph {ch}"));
        assert_eq!(
            glyph.commands_for(rect),
            expected,
            "{ch} should match Ghostty Powerline Extra split geometry"
        );
    }
}

// Ported from Ghostty ce6a00b src/font/sprite/draw/powerline.zig drawE0B0..drawE0BF.
#[test]
fn terminal_powerline_draw_uses_upstream_geometry_for_all_drawn_codepoints() {
    let rect = sprite_fixture();
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
            rect,
            ch,
            SpriteFamily::Powerline,
            expected,
            "Ghostty Powerline geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FB68_1FB6F
// and draw1FB9A_1FB9B.
#[test]
fn terminal_legacy_edge_triangles_match_upstream_geometry() {
    let rect = sprite_fixture();

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
    let rect = sprite_fixture();

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
            rect,
            cp,
            SpriteFamily::LegacyComputing,
            expected,
            "Ghostty corner triangle shade geometry",
        );
    }
}

// Ported from Ghostty ce6a00b
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBD0_1FBDF.
#[test]
fn terminal_legacy_cell_diagonals_match_upstream_geometry() {
    let rect = sprite_fixture();

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
    let rect = sprite_fixture();

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
    let rect = sprite_fixture();
    assert_sprite_commands(
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
// src/font/sprite/draw/symbols_for_legacy_computing.zig draw1FBCE,
// draw1FBCF, and draw1FBE0_1FBEF block cases.
#[test]
fn terminal_legacy_fractional_blocks_match_upstream_geometry() {
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
    let rect = sprite_fixture();

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
// draw1CC1B_1CC1E and draw1CE16_1CE19.
#[test]
fn terminal_legacy_supplement_box_fragments_match_upstream_geometry() {
    let rect = sprite_fixture();

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
// draw1CE00, draw1CE01, draw1CE0B, and draw1CE0C.
#[test]
fn terminal_legacy_supplement_split_circles_and_ellipses_match_upstream_geometry() {
    let rect = sprite_fixture();

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
            rect,
            cp,
            SpriteFamily::LegacyComputingSupplement,
            expected,
            "Ghostty supplement circle/ellipse geometry",
        );
    }
}

#[test]
fn terminal_sprite_face_owns_representative_native_symbols_before_font_fallback() {
    for (ch, family) in [
        ('╭', SpriteFamily::BoxDrawing),
        ('▟', SpriteFamily::Block),
        ('⣿', SpriteFamily::Braille),
        ('\u{E0D4}', SpriteFamily::Powerline),
        ('\u{1FB68}', SpriteFamily::LegacyComputing),
        ('\u{1FBAF}', SpriteFamily::LegacyComputing),
        ('\u{1CC1B}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE0B}', SpriteFamily::LegacyComputingSupplement),
    ] {
        assert_eq!(
            SpriteGlyph::from_char(ch).map(|glyph| glyph.family),
            Some(family)
        );
    }

    for recorded_not_implemented in ['■', '\u{E0C0}', '\u{1CC00}', '\u{1FBC0}', '\u{F5D0}'] {
        assert_eq!(SpriteGlyph::from_char(recorded_not_implemented), None);
    }
}

#[test]
fn terminal_sprite_family_range_edges_are_stable() {
    let ranges = [
        (0x2500, 0x257F, SpriteFamily::BoxDrawing),
        (0x2800, 0x28FF, SpriteFamily::Braille),
        (0xEE00, 0xEE0B, SpriteFamily::ProgressIndicator),
        (0x1FB00, 0x1FB67, SpriteFamily::LegacyComputing),
        (0x1FB68, 0x1FB6F, SpriteFamily::LegacyComputing),
        (0x1FB70, 0x1FB99, SpriteFamily::LegacyComputing),
        (0x1FB9A, 0x1FB9F, SpriteFamily::LegacyComputing),
        (0x1FBA0, 0x1FBAF, SpriteFamily::LegacyComputing),
        (0x1FBBD, 0x1FBBF, SpriteFamily::LegacyComputing),
        (0x1FBCE, 0x1FBCF, SpriteFamily::LegacyComputing),
        (0x1FBD0, 0x1FBDF, SpriteFamily::LegacyComputing),
        (0x1FBE0, 0x1FBEF, SpriteFamily::LegacyComputing),
        (0x1CC1B, 0x1CC1E, SpriteFamily::LegacyComputingSupplement),
        (0x1CC21, 0x1CC2F, SpriteFamily::LegacyComputingSupplement),
        (0x1CC30, 0x1CC3F, SpriteFamily::LegacyComputingSupplement),
        (0x1CD00, 0x1CDE5, SpriteFamily::LegacyComputingSupplement),
        (0x1CE00, 0x1CE01, SpriteFamily::LegacyComputingSupplement),
        (0x1CE0B, 0x1CE0C, SpriteFamily::LegacyComputingSupplement),
        (0x1CE16, 0x1CE19, SpriteFamily::LegacyComputingSupplement),
        (0x1CE51, 0x1CE8F, SpriteFamily::LegacyComputingSupplement),
        (0x1CE90, 0x1CEAF, SpriteFamily::LegacyComputingSupplement),
    ];

    for (start, end, family) in ranges {
        for cp in [start, end] {
            let ch = char::from_u32(cp).expect("range edge should be a Unicode scalar");
            assert_eq!(
                SpriteGlyph::from_char(ch).map(|glyph| glyph.family),
                Some(family),
                "U+{cp:04X} should stay in {family:?}"
            );
        }
    }

    for cp in [
        0x24FF, 0x27FF, 0x2900, 0xEE0C, 0x1FBB0, 0x1FBC0, 0x1CC1F, 0x1CC20, 0x1CC40, 0x1CE02,
        0x1CE0D, 0x1CE1A, 0x1CE50, 0x1CEB0,
    ] {
        let ch = char::from_u32(cp).expect("gap should be a Unicode scalar");
        assert_eq!(
            SpriteGlyph::from_char(ch),
            None,
            "U+{cp:04X} should stay unowned"
        );
    }
}

#[test]
fn owned_sprite_commands_fit_cell_bounds() {
    let cell = SurfaceRect::from_min_size(11.0, 7.0, 19.0, 23.0);

    for cp in 0..=0x10FFFF {
        let Some(ch) = char::from_u32(cp) else {
            continue;
        };
        let Some(glyph) = SpriteGlyph::from_char(ch) else {
            continue;
        };
        for command in glyph.commands_for(cell) {
            assert_sprite_command_fits_cell(&command, cell, ch);
        }
    }
}

fn assert_sprite_command_fits_cell(command: &SpriteCommand, cell: SurfaceRect, ch: char) {
    const EPSILON: f32 = 0.0001;
    match command {
        SpriteCommand::FillRect { rect, .. } => {
            assert_between(rect.min_x, cell.min_x, cell.max_x, EPSILON, ch);
            assert_between(rect.max_x, cell.min_x, cell.max_x, EPSILON, ch);
            assert_between(rect.min_y, cell.min_y, cell.max_y, EPSILON, ch);
            assert_between(rect.max_y, cell.min_y, cell.max_y, EPSILON, ch);
        }
        SpriteCommand::FillPolygon { points, .. } => {
            for point in points {
                assert_between(point.x, cell.min_x, cell.max_x, EPSILON, ch);
                assert_between(point.y, cell.min_y, cell.max_y, EPSILON, ch);
            }
        }
        SpriteCommand::StrokePolyline { points, width, .. }
        | SpriteCommand::ClearStrokePolyline { points, width, .. } => {
            let margin = *width * 0.5 + EPSILON;
            for point in points {
                assert_between(point.x, cell.min_x, cell.max_x, margin, ch);
                assert_between(point.y, cell.min_y, cell.max_y, margin, ch);
            }
        }
    }
}

fn assert_between(value: f32, min: f32, max: f32, margin: f32, ch: char) {
    assert!(
        value >= min - margin && value <= max + margin,
        "{ch} emitted coordinate {value} outside [{min}, {max}] with margin {margin}"
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
    rect: SurfaceRect,
    ch: char,
    family: SpriteFamily,
    expected: Vec<SpriteCommand>,
    detail: &str,
) {
    let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph {ch}"));
    assert_eq!(glyph.family, family, "{ch} should be owned as {family:?}");
    assert_eq!(
        glyph.commands_for(rect),
        expected,
        "{ch} should match {detail}"
    );
}

fn assert_sprite_commands_for_cp(
    rect: SurfaceRect,
    cp: u32,
    family: SpriteFamily,
    expected: Vec<SpriteCommand>,
    detail: &str,
) {
    let ch = char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}"));
    let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing glyph U+{cp:04X}"));
    assert_eq!(
        glyph.family, family,
        "U+{cp:04X} should be owned as {family:?}"
    );
    assert_eq!(
        glyph.commands_for(rect),
        expected,
        "U+{cp:04X} should match {detail}"
    );
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

fn circle_arc(position: &str) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: circle_arc_points(position).into_iter().collect(),
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
        points: points.into_iter().collect(),
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

fn stroke_points(points: Vec<SpritePoint>) -> SpriteCommand {
    SpriteCommand::StrokePolyline {
        points: points.into_iter().collect(),
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
