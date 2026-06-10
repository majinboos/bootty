use super::{SpriteCommand, SpriteFamily, SpriteRegistry, SpriteShape};
use crate::geometry::SurfaceRect;

fn family(ch: u32) -> Option<SpriteFamily> {
    let ch = char::from_u32(ch).expect("test codepoint");
    SpriteRegistry::prompt_graphics()
        .glyph_for(ch)
        .map(|glyph| glyph.family)
}

#[test]
fn family_ranges_have_exact_edges() {
    use SpriteFamily::{
        Block, BoxDrawing, Braille, LegacyComputing, LegacyComputingSupplement, Powerline,
        ProgressIndicator, Separator, Shade,
    };

    let cases: &[(u32, Option<SpriteFamily>)] = &[
        (0x24FF, None),
        (0x2500, Some(BoxDrawing)),
        (0x257F, Some(BoxDrawing)),
        (0x2580, Some(Block)),
        (0x2590, Some(Block)),
        (0x2591, Some(Shade)),
        (0x2593, Some(Shade)),
        (0x2594, Some(Block)),
        (0x2595, Some(Block)),
        (0x2596, Some(Block)),
        (0x259F, Some(Block)),
        (0x25A0, None),
        (0x276E, Some(Separator)),
        (0x276F, Some(Separator)),
        (0x27FF, None),
        (0x2800, Some(Braille)),
        (0x28FF, Some(Braille)),
        (0x2900, None),
        (0xE0AF, None),
        (0xE0B0, Some(Powerline)),
        (0xE0BF, Some(Powerline)),
        (0xE0C0, None),
        (0xE0D1, None),
        (0xE0D2, Some(Powerline)),
        (0xE0D3, None),
        (0xE0D4, Some(Powerline)),
        (0xE0D5, None),
        (0xEDFF, None),
        (0xEE00, Some(ProgressIndicator)),
        (0xEE0B, Some(ProgressIndicator)),
        (0xEE0C, None),
        (0x1FAFF, None),
        (0x1FB00, Some(LegacyComputing)),
        (0x1FBAF, Some(LegacyComputing)),
        (0x1FBB0, None),
        (0x1FBBD, Some(LegacyComputing)),
        (0x1FBC0, None),
        (0x1FBEF, Some(LegacyComputing)),
        (0x1FBF0, None),
        (0x1CC1A, None),
        (0x1CC1B, Some(LegacyComputingSupplement)),
        (0x1CC1E, Some(LegacyComputingSupplement)),
        (0x1CC1F, None),
        (0x1CC21, Some(LegacyComputingSupplement)),
        (0x1CEAF, Some(LegacyComputingSupplement)),
        (0x1CEB0, None),
    ];

    for (codepoint, expected) in cases {
        assert_eq!(family(*codepoint), *expected, "U+{codepoint:04X}");
    }
}

fn rect() -> SurfaceRect {
    SurfaceRect::from_min_size(10.0, 20.0, 8.0, 16.0)
}

fn commands(ch: char, rect: SurfaceRect) -> Vec<SpriteCommand> {
    let registry = SpriteRegistry::prompt_graphics();
    let glyph = registry.glyph_for(ch).expect("sprite-owned glyph");
    registry.commands_for(glyph, rect)
}

#[test]
fn block_fills_use_literal_eighth_rects() {
    // Asymmetric glyphs distinguish row/column transposition in the spec
    // table: '▄' spans the bottom half, '▌' the left half.
    let cases = [
        ('█', SurfaceRect::from_min_size(10.0, 20.0, 8.0, 16.0)),
        ('▄', SurfaceRect::from_min_size(10.0, 28.0, 8.0, 8.0)),
        ('▌', SurfaceRect::from_min_size(10.0, 20.0, 4.0, 16.0)),
    ];

    for (ch, expected) in cases {
        assert_eq!(
            commands(ch, rect()),
            vec![SpriteCommand::FillRect {
                rect: expected,
                alpha: 1.0
            }],
            "{ch}"
        );
    }
}

#[test]
fn shade_glyphs_fill_the_cell_with_graded_alpha() {
    for (ch, alpha) in [('░', 0.25), ('▒', 0.50), ('▓', 0.75)] {
        assert_eq!(
            commands(ch, rect()),
            vec![SpriteCommand::FillRect {
                rect: rect(),
                alpha
            }],
            "{ch}"
        );
    }
}

#[test]
fn powerline_right_arrow_is_a_full_height_triangle() {
    let commands = commands('\u{E0B0}', rect());

    assert_eq!(commands.len(), 1);
    let SpriteCommand::FillPolygon {
        shape,
        points,
        alpha,
    } = &commands[0]
    else {
        panic!("expected a filled polygon, got {commands:?}");
    };

    assert_eq!(*shape, SpriteShape::Triangle);
    assert_eq!(*alpha, 1.0);
    let min_y = points.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    assert_eq!(min_y, rect().min_y);
    assert_eq!(max_y, rect().max_y);
}

const SWEEP_BLOCKS: &[(u32, u32)] = &[
    (0x2500, 0x28FF),
    (0xE000, 0xF8FF),
    (0x1FB00, 0x1FBFF),
    (0x1CC00, 0x1CEFF),
];

fn owned_glyphs() -> impl Iterator<Item = char> {
    SWEEP_BLOCKS
        .iter()
        .flat_map(|(start, end)| *start..=*end)
        .filter_map(char::from_u32)
        .filter(|ch| SpriteRegistry::prompt_graphics().owns(*ch))
}

fn command_bounds_hold(command: &SpriteCommand, bounds: SurfaceRect) -> bool {
    const EPSILON: f32 = 0.5;
    let point_within = |x: f32, y: f32, inflate: f32| {
        x.is_finite()
            && y.is_finite()
            && x >= bounds.min_x - inflate - EPSILON
            && x <= bounds.max_x + inflate + EPSILON
            && y >= bounds.min_y - inflate - EPSILON
            && y <= bounds.max_y + inflate + EPSILON
    };
    match command {
        SpriteCommand::FillRect { rect, alpha } => {
            alpha.is_finite()
                && point_within(rect.min_x, rect.min_y, 0.0)
                && point_within(rect.max_x, rect.max_y, 0.0)
        }
        SpriteCommand::FillPolygon { points, alpha, .. } => {
            alpha.is_finite() && points.iter().all(|p| point_within(p.x, p.y, 0.0))
        }
        SpriteCommand::StrokePolyline {
            points,
            width,
            alpha,
        }
        | SpriteCommand::ClearStrokePolyline {
            points,
            width,
            alpha,
        } => {
            // Stroke points are centerlines; the painted area extends by
            // half the stroke width.
            width.is_finite()
                && alpha.is_finite()
                && points.iter().all(|p| point_within(p.x, p.y, width * 0.5))
        }
    }
}

#[test]
fn every_owned_glyph_paints_within_its_cell() {
    let bounds = SurfaceRect::from_min_size(3.0, 7.0, 9.0, 22.0);
    let registry = SpriteRegistry::prompt_graphics();
    let mut checked = 0u32;

    for ch in owned_glyphs() {
        let glyph = registry.glyph_for(ch).expect("owned glyph");
        for command in registry.commands_for(glyph, bounds) {
            assert!(
                command_bounds_hold(&command, bounds),
                "U+{:04X} escapes its cell: {command:?}",
                u32::from(ch),
            );
        }
        checked += 1;
    }

    assert!(checked > 1000, "sweep only visited {checked} glyphs");
}

fn translated(command: &SpriteCommand, dx: f32, dy: f32) -> SpriteCommand {
    let mut command = command.clone();
    match &mut command {
        SpriteCommand::FillRect { rect, .. } => {
            rect.min_x += dx;
            rect.max_x += dx;
            rect.min_y += dy;
            rect.max_y += dy;
        }
        SpriteCommand::FillPolygon { points, .. }
        | SpriteCommand::StrokePolyline { points, .. }
        | SpriteCommand::ClearStrokePolyline { points, .. } => {
            for point in points {
                point.x += dx;
                point.y += dy;
            }
        }
    }
    command
}

fn approx_eq(left: &SpriteCommand, right: &SpriteCommand) -> bool {
    const TOLERANCE: f32 = 0.01;
    let close = |a: f32, b: f32| (a - b).abs() <= TOLERANCE;
    match (left, right) {
        (
            SpriteCommand::FillRect { rect: a, alpha: aa },
            SpriteCommand::FillRect { rect: b, alpha: ba },
        ) => {
            close(a.min_x, b.min_x)
                && close(a.min_y, b.min_y)
                && close(a.max_x, b.max_x)
                && close(a.max_y, b.max_y)
                && close(*aa, *ba)
        }
        (
            SpriteCommand::FillPolygon {
                shape: a_shape,
                points: a,
                alpha: aa,
            },
            SpriteCommand::FillPolygon {
                shape: b_shape,
                points: b,
                alpha: ba,
            },
        ) => {
            a_shape == b_shape
                && close(*aa, *ba)
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(p, q)| close(p.x, q.x) && close(p.y, q.y))
        }
        (
            SpriteCommand::StrokePolyline {
                points: a,
                width: aw,
                alpha: aa,
            },
            SpriteCommand::StrokePolyline {
                points: b,
                width: bw,
                alpha: ba,
            },
        )
        | (
            SpriteCommand::ClearStrokePolyline {
                points: a,
                width: aw,
                alpha: aa,
            },
            SpriteCommand::ClearStrokePolyline {
                points: b,
                width: bw,
                alpha: ba,
            },
        ) => {
            close(*aw, *bw)
                && close(*aa, *ba)
                && a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(p, q)| close(p.x, q.x) && close(p.y, q.y))
        }
        _ => false,
    }
}

#[test]
fn sprite_commands_are_relative_to_their_cell() {
    let origin = SurfaceRect::from_min_size(3.0, 7.0, 9.0, 22.0);
    let moved = SurfaceRect::from_min_size(53.0, 107.0, 9.0, 22.0);
    let registry = SpriteRegistry::prompt_graphics();

    for ch in owned_glyphs() {
        let glyph = registry.glyph_for(ch).expect("owned glyph");
        let at_origin = registry.commands_for(glyph, origin);
        let at_moved = registry.commands_for(glyph, moved);

        assert_eq!(at_origin.len(), at_moved.len(), "U+{:04X}", u32::from(ch));
        for (original, moved_command) in at_origin.iter().zip(&at_moved) {
            let expected = translated(original, 50.0, 100.0);
            assert!(
                approx_eq(&expected, moved_command),
                "U+{:04X} leaks absolute coordinates:\n  {expected:?}\n  {moved_command:?}",
                u32::from(ch),
            );
        }
    }
}
