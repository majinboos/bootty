use std::sync::OnceLock;

use bootty_render::{
    paint_plan::PlanColor,
    terminal_render::SpriteCommandBatch,
    terminal_sprite::{SpriteCommand, SpriteFamily, SpriteGlyph},
    terminal_text_atlas::TextAtlasBuilder,
};
use bootty_surface::geometry::SurfaceRect;
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use rstest::rstest;

fn color() -> PlanColor {
    PlanColor {
        r: 220,
        g: 221,
        b: 222,
        a: 255,
    }
}

fn sprite_batch(ch: char, rect: SurfaceRect) -> SpriteCommandBatch {
    let glyph = SpriteGlyph::from_char(ch).unwrap_or_else(|| panic!("missing sprite {ch}"));
    SpriteCommandBatch {
        glyph,
        rect,
        color: color(),
    }
}

fn rasterized(ch: char, rect: SurfaceRect) -> Vec<u8> {
    let mut builder = TextAtlasBuilder::new(32, 32);
    builder.prepare_sprite_command(&sprite_batch(ch, rect), 1.0);
    builder.atlas_pixels().to_vec()
}

fn fixture() -> SurfaceRect {
    SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0)
}

fn fill_rects(ch: char, rect: SurfaceRect) -> Vec<(SurfaceRect, f32)> {
    SpriteGlyph::from_char(ch)
        .unwrap_or_else(|| panic!("missing sprite {ch}"))
        .commands_for(rect)
        .into_iter()
        .map(|command| match command {
            SpriteCommand::FillRect { rect, alpha } => (rect, alpha),
            other => panic!("{ch} emitted non-rectangle geometry: {other:?}"),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum GeometrySummary {
    Polygon {
        points: Vec<[i32; 2]>,
        alpha: i32,
    },
    Stroke {
        clear: bool,
        points: usize,
        first: [i32; 2],
        last: [i32; 2],
        bounds: [i32; 4],
        width: i32,
        alpha: i32,
    },
}

fn quantize(value: f32) -> i32 {
    (value * 1_000.0).round() as i32
}

fn point(point: &bootty_render::terminal_sprite::SpritePoint) -> [i32; 2] {
    [quantize(point.x), quantize(point.y)]
}

fn stroke_summary(
    clear: bool,
    points: &[bootty_render::terminal_sprite::SpritePoint],
    width: f32,
    alpha: f32,
) -> GeometrySummary {
    let xs = points.iter().map(|point| quantize(point.x));
    let ys = points.iter().map(|point| quantize(point.y));
    GeometrySummary::Stroke {
        clear,
        points: points.len(),
        first: point(points.first().expect("stroke has a first point")),
        last: point(points.last().expect("stroke has a last point")),
        bounds: [
            xs.clone().min().expect("stroke has x coordinates"),
            ys.clone().min().expect("stroke has y coordinates"),
            xs.max().expect("stroke has x coordinates"),
            ys.max().expect("stroke has y coordinates"),
        ],
        width: quantize(width),
        alpha: quantize(alpha),
    }
}

fn geometry(ch: char, rect: SurfaceRect) -> Vec<GeometrySummary> {
    SpriteGlyph::from_char(ch)
        .unwrap_or_else(|| panic!("missing sprite {ch}"))
        .commands_for(rect)
        .into_iter()
        .map(|command| match command {
            SpriteCommand::FillPolygon { points, alpha, .. } => GeometrySummary::Polygon {
                points: points.iter().map(point).collect(),
                alpha: quantize(alpha),
            },
            SpriteCommand::StrokePolyline {
                points,
                width,
                alpha,
            } => stroke_summary(false, &points, width, alpha),
            SpriteCommand::ClearStrokePolyline {
                points,
                width,
                alpha,
            } => stroke_summary(true, &points, width, alpha),
            SpriteCommand::FillRect { .. } => panic!("{ch} emitted rectangle geometry"),
        })
        .collect()
}

#[rstest]
#[case::powerline('\u{E0B0}', SpriteFamily::Powerline)]
#[case::progress('\u{EE00}', SpriteFamily::ProgressIndicator)]
#[case::separator('❯', SpriteFamily::Separator)]
#[case::block('█', SpriteFamily::Block)]
#[case::shade('▒', SpriteFamily::Shade)]
#[case::box_drawing('┼', SpriteFamily::BoxDrawing)]
#[case::braille('⣿', SpriteFamily::Braille)]
#[case::legacy('\u{1FB3C}', SpriteFamily::LegacyComputing)]
#[case::legacy_supplement('\u{1CC1B}', SpriteFamily::LegacyComputingSupplement)]
fn native_terminal_glyphs_have_the_expected_renderer(
    #[case] ch: char,
    #[case] expected: SpriteFamily,
) {
    assert_eq!(
        SpriteGlyph::from_char(ch).map(|glyph| glyph.family),
        Some(expected)
    );
}

#[rstest]
#[case::full('█', SurfaceRect::from_min_size(2.0, 3.0, 16.0, 24.0))]
#[case::left_half('▌', SurfaceRect::from_min_size(2.0, 3.0, 8.0, 24.0))]
#[case::lower_half('▄', SurfaceRect::from_min_size(2.0, 15.0, 16.0, 12.0))]
fn block_elements_fill_their_declared_cell_fraction(
    #[case] ch: char,
    #[case] expected: SurfaceRect,
) {
    let cell = SurfaceRect::from_min_size(2.0, 3.0, 16.0, 24.0);
    let actual = SpriteGlyph::from_char(ch)
        .expect("block element is renderer-owned")
        .commands_for(cell);

    assert_eq!(
        actual,
        vec![SpriteCommand::FillRect {
            rect: expected,
            alpha: 1.0,
        }]
    );
}

#[rstest]
#[case::blank_braille('\u{2800}')]
#[case::legacy_transparent_cell('\u{1FB93}')]
fn transparent_sprites_emit_no_geometry(#[case] ch: char) {
    let cell = SurfaceRect::from_min_size(0.0, 0.0, 16.0, 24.0);
    let glyph = SpriteGlyph::from_char(ch).expect("transparent sprite is renderer-owned");

    assert_eq!(glyph.commands_for(cell), Vec::new());
}

#[rstest]
#[case::junction(
    '┼',
    vec![
        (SurfaceRect::from_min_size(7.0, 0.0, 2.0, 13.0), 1.0),
        (SurfaceRect::from_min_size(7.0, 11.0, 2.0, 13.0), 1.0),
        (SurfaceRect::from_min_size(0.0, 11.0, 9.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(7.0, 11.0, 9.0, 2.0), 1.0),
    ]
)]
#[case::dash(
    '┄',
    vec![
        (SurfaceRect::from_min_size(1.0, 11.0, 4.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(7.0, 11.0, 3.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(12.0, 11.0, 3.0, 2.0), 1.0),
    ]
)]
#[case::double_junction(
    '╬',
    vec![
        (SurfaceRect::from_min_size(5.0, 0.0, 2.0, 11.0), 1.0),
        (SurfaceRect::from_min_size(9.0, 0.0, 2.0, 11.0), 1.0),
        (SurfaceRect::from_min_size(5.0, 13.0, 2.0, 11.0), 1.0),
        (SurfaceRect::from_min_size(9.0, 13.0, 2.0, 11.0), 1.0),
        (SurfaceRect::from_min_size(0.0, 9.0, 7.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(0.0, 13.0, 7.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(9.0, 9.0, 7.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(9.0, 13.0, 7.0, 2.0), 1.0),
    ]
)]
fn box_lines_have_exact_cell_geometry(#[case] ch: char, #[case] expected: Vec<(SurfaceRect, f32)>) {
    assert_eq!(fill_rects(ch, fixture()), expected);
}

#[rstest]
#[case::upper_left('╭', [8_000, 24_000], [16_000, 12_000], [8_000, 12_000, 16_000, 24_000])]
#[case::upper_right('╮', [8_000, 24_000], [0, 12_000], [0, 12_000, 8_000, 24_000])]
#[case::lower_right('╯', [8_000, 0], [0, 12_000], [0, 0, 8_000, 12_000])]
#[case::lower_left('╰', [8_000, 0], [16_000, 12_000], [8_000, 0, 16_000, 12_000])]
fn rounded_box_corners_follow_the_expected_quarter_curve(
    #[case] ch: char,
    #[case] first: [i32; 2],
    #[case] last: [i32; 2],
    #[case] bounds: [i32; 4],
) {
    assert_eq!(
        geometry(ch, fixture()),
        vec![GeometrySummary::Stroke {
            clear: false,
            points: 10,
            first,
            last,
            bounds,
            width: 2_000,
            alpha: 1_000,
        }]
    );
}

#[rstest]
#[case::filled_chevron(
    '\u{E0B0}',
    vec![GeometrySummary::Polygon {
        points: vec![[0, 0], [16_000, 12_000], [0, 24_000]],
        alpha: 1_000,
    }]
)]
#[case::outline_chevron(
    '\u{E0B1}',
    vec![
        GeometrySummary::Stroke { clear: false, points: 2, first: [0, 0], last: [16_000, 12_000], bounds: [0, 0, 16_000, 12_000], width: 2_000, alpha: 1_000 },
        GeometrySummary::Stroke { clear: false, points: 2, first: [0, 24_000], last: [16_000, 12_000], bounds: [0, 12_000, 16_000, 24_000], width: 2_000, alpha: 1_000 },
    ]
)]
#[case::split_left(
    '\u{E0D2}',
    vec![
        GeometrySummary::Polygon { points: vec![[0, 0], [16_000, 0], [8_000, 11_000], [0, 11_000]], alpha: 1_000 },
        GeometrySummary::Polygon { points: vec![[0, 24_000], [16_000, 24_000], [8_000, 13_000], [0, 13_000]], alpha: 1_000 },
    ]
)]
fn powerline_variants_have_exact_points(#[case] ch: char, #[case] expected: Vec<GeometrySummary>) {
    assert_eq!(geometry(ch, fixture()), expected);
}

#[rstest]
#[case::edge_triangle(
    '\u{1FB6C}',
    vec![GeometrySummary::Polygon { points: vec![[8_000, 12_000], [0, 0], [0, 24_000]], alpha: 1_000 }]
)]
#[case::cell_diagonal(
    '\u{1FBD0}',
    vec![GeometrySummary::Stroke { clear: false, points: 2, first: [16_000, 12_000], last: [0, 24_000], bounds: [0, 12_000, 16_000, 24_000], width: 2_000, alpha: 1_000 }]
)]
fn legacy_triangles_and_diagonals_have_exact_points(
    #[case] ch: char,
    #[case] expected: Vec<GeometrySummary>,
) {
    assert_eq!(geometry(ch, fixture()), expected);
}

#[rstest]
#[case::two_thirds('\u{1FBCE}', SurfaceRect::from_min_size(0.0, 0.0, 12.0, 24.0))]
#[case::centered_upper_half('\u{1FBE4}', SurfaceRect::from_min_size(4.5, 0.0, 9.0, 12.0))]
fn legacy_fractional_blocks_have_exact_rectangles(#[case] ch: char, #[case] expected: SurfaceRect) {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 18.0, 24.0);
    assert_eq!(fill_rects(ch, rect), vec![(expected, 1.0)]);
}

#[rstest]
#[case::top_arc(
    '\u{1FBE0}',
    GeometrySummary::Stroke { clear: false, points: 9, first: [16_000, 0], last: [0, 0], bounds: [0, 0, 16_000, 8_000], width: 2_000, alpha: 1_000 }
)]
#[case::top_right_sector(
    '\u{1FBEC}',
    GeometrySummary::Polygon { points: vec![[16_000, 0], [16_000, 8_000], [12_939, 7_391], [10_343, 5_657], [8_609, 3_061], [8_000, 0]], alpha: 1_000 }
)]
fn legacy_circle_geometry_has_exact_arc_extent(
    #[case] ch: char,
    #[case] expected: GeometrySummary,
) {
    assert_eq!(geometry(ch, fixture()), vec![expected]);
}

#[rstest]
#[case::horizontal_corner(
    '\u{1CC1B}',
    vec![
        (SurfaceRect::from_min_size(0.0, 11.0, 16.0, 2.0), 1.0),
        (SurfaceRect::from_min_size(14.0, 0.0, 2.0, 12.0), 1.0),
    ]
)]
#[case::vertical_corner(
    '\u{1CE16}',
    vec![
        (SurfaceRect::from_min_size(7.0, 0.0, 2.0, 24.0), 1.0),
        (SurfaceRect::from_min_size(8.0, 0.0, 8.0, 2.0), 1.0),
    ]
)]
fn legacy_supplement_fragments_have_exact_rectangles(
    #[case] ch: char,
    #[case] expected: Vec<(SurfaceRect, f32)>,
) {
    assert_eq!(fill_rects(ch, fixture()), expected);
}

#[test]
fn split_circle_supplement_has_two_complementary_arcs() {
    assert_eq!(
        geometry('\u{1CE00}', fixture()),
        vec![
            GeometrySummary::Stroke {
                clear: false,
                points: 9,
                first: [0, 4_000],
                last: [0, 20_000],
                bounds: [0, 4_000, 8_000, 20_000],
                width: 2_000,
                alpha: 1_000
            },
            GeometrySummary::Stroke {
                clear: false,
                points: 9,
                first: [16_000, 20_000],
                last: [16_000, 4_000],
                bounds: [8_000, 4_000, 16_000, 20_000],
                width: 2_000,
                alpha: 1_000
            },
        ]
    );
}

#[rstest]
#[case::box_drawing(0x2500, 0x257F, SpriteFamily::BoxDrawing)]
#[case::braille(0x2800, 0x28FF, SpriteFamily::Braille)]
#[case::progress(0xEE00, 0xEE0B, SpriteFamily::ProgressIndicator)]
#[case::legacy_sextants(0x1FB00, 0x1FB67, SpriteFamily::LegacyComputing)]
#[case::legacy_triangles(0x1FB68, 0x1FB6F, SpriteFamily::LegacyComputing)]
#[case::legacy_blocks(0x1FB70, 0x1FB99, SpriteFamily::LegacyComputing)]
#[case::legacy_shades(0x1FB9A, 0x1FB9F, SpriteFamily::LegacyComputing)]
#[case::legacy_corner_diagonals(0x1FBA0, 0x1FBAF, SpriteFamily::LegacyComputing)]
#[case::legacy_inverse_diagonals(0x1FBBD, 0x1FBBF, SpriteFamily::LegacyComputing)]
#[case::legacy_fractional_columns(0x1FBCE, 0x1FBCF, SpriteFamily::LegacyComputing)]
#[case::legacy_diagonals(0x1FBD0, 0x1FBDF, SpriteFamily::LegacyComputing)]
#[case::legacy_circles(0x1FBE0, 0x1FBEF, SpriteFamily::LegacyComputing)]
#[case::supplement_fragments(0x1CC1B, 0x1CC1E, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_quadrants(0x1CC21, 0x1CC2F, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_circle_pieces(0x1CC30, 0x1CC3F, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_octants(0x1CD00, 0x1CDE5, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_split_circles(0x1CE00, 0x1CE01, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_ellipses(0x1CE0B, 0x1CE0C, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_vertical_fragments(0x1CE16, 0x1CE19, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_sextants(0x1CE51, 0x1CE8F, SpriteFamily::LegacyComputingSupplement)]
#[case::supplement_sixteenths(0x1CE90, 0x1CEAF, SpriteFamily::LegacyComputingSupplement)]
fn sprite_family_range_edges_are_owned(
    #[case] start: u32,
    #[case] end: u32,
    #[case] family: SpriteFamily,
) {
    let actual = [start, end].map(|codepoint| {
        SpriteGlyph::from_char(char::from_u32(codepoint).expect("valid range edge"))
            .map(|glyph| glyph.family)
    });

    assert_eq!(actual, [Some(family), Some(family)]);
}

#[rstest]
#[case(0x24FF)]
#[case(0x27FF)]
#[case(0x2900)]
#[case(0xEE0C)]
#[case(0x1FBB0)]
#[case(0x1FBC0)]
#[case(0x1CC1F)]
#[case(0x1CC20)]
#[case(0x1CC40)]
#[case(0x1CE02)]
#[case(0x1CE0D)]
#[case(0x1CE1A)]
#[case(0x1CE50)]
#[case(0x1CEB0)]
fn gaps_between_sprite_ranges_stay_unowned(#[case] codepoint: u32) {
    assert_eq!(
        SpriteGlyph::from_char(char::from_u32(codepoint).expect("valid gap codepoint")),
        None
    );
}

fn command_fits_cell(command: &SpriteCommand, cell: SurfaceRect) -> bool {
    const EPSILON: f32 = 0.0001;
    let between = |value: f32, min: f32, max: f32, margin: f32| {
        value.is_finite()
            && value >= (min - margin).next_down()
            && value <= (max + margin).next_up()
    };
    let normalized = |alpha: f32| alpha.is_finite() && (0.0..=1.0).contains(&alpha);
    match command {
        SpriteCommand::FillRect { rect, alpha } => {
            normalized(*alpha)
                && between(rect.min_x, cell.min_x, cell.max_x, EPSILON)
                && between(rect.max_x, cell.min_x, cell.max_x, EPSILON)
                && between(rect.min_y, cell.min_y, cell.max_y, EPSILON)
                && between(rect.max_y, cell.min_y, cell.max_y, EPSILON)
        }
        SpriteCommand::FillPolygon { points, alpha, .. } => {
            points.len() >= 3
                && normalized(*alpha)
                && points.iter().all(|point| {
                    between(point.x, cell.min_x, cell.max_x, EPSILON)
                        && between(point.y, cell.min_y, cell.max_y, EPSILON)
                })
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
            let margin = *width * 0.5 + EPSILON;
            width.is_finite()
                && *width > 0.0
                && normalized(*alpha)
                && points.iter().all(|point| {
                    between(point.x, cell.min_x, cell.max_x, margin)
                        && between(point.y, cell.min_y, cell.max_y, margin)
                })
        }
    }
}

fn command_emits_geometry(command: &SpriteCommand) -> bool {
    match command {
        SpriteCommand::FillRect { rect, .. } => rect.width() > 0.0 && rect.height() > 0.0,
        SpriteCommand::FillPolygon { points, .. } => points.len() >= 3,
        SpriteCommand::StrokePolyline { points, .. }
        | SpriteCommand::ClearStrokePolyline { points, .. } => points.len() >= 2,
    }
}

fn owned_glyphs() -> &'static [SpriteGlyph] {
    static GLYPHS: OnceLock<Vec<SpriteGlyph>> = OnceLock::new();
    GLYPHS.get_or_init(|| {
        (0..=0x10FFFF)
            .filter_map(char::from_u32)
            .filter_map(SpriteGlyph::from_char)
            .collect()
    })
}

fn drawable_glyphs() -> &'static [SpriteGlyph] {
    static GLYPHS: OnceLock<Vec<SpriteGlyph>> = OnceLock::new();
    GLYPHS.get_or_init(|| {
        owned_glyphs()
            .iter()
            .copied()
            .filter(|glyph| {
                glyph
                    .commands_for(fixture())
                    .iter()
                    .any(command_emits_geometry)
            })
            .collect()
    })
}

#[test]
fn every_owned_sprite_keeps_its_emitted_geometry_inside_the_cell() {
    let cell = SurfaceRect::from_min_size(11.0, 7.0, 19.0, 23.0);

    for glyph in owned_glyphs() {
        for command in glyph.commands_for(cell) {
            assert!(
                command_fits_cell(&command, cell),
                "{} emitted out-of-cell geometry: {command:?}",
                glyph.ch
            );
        }
    }
}

#[derive(Arbitrary, Clone, Copy, Debug)]
struct SpriteCase {
    glyph_index: u16,
    origin_x: i16,
    origin_y: i16,
    // Terminal cells are taller than they are wide. Broaden this only if the renderer supports
    // arbitrary sprite surfaces rather than glyph cells.
    #[proptest(strategy = "4_u8..=127")]
    cell_width: u8,
}

impl SpriteCase {
    fn glyph(self, cell: SurfaceRect) -> SpriteGlyph {
        let glyphs = drawable_glyphs();
        let start = usize::from(self.glyph_index) % glyphs.len();
        glyphs
            .iter()
            .cycle()
            .skip(start)
            .take(glyphs.len())
            .find(|glyph| glyph.commands_for(cell).iter().any(command_emits_geometry))
            .copied()
            .expect("every terminal cell has a drawable native sprite")
    }

    fn cell(self) -> SurfaceRect {
        SurfaceRect::from_min_size(
            f32::from(self.origin_x),
            f32::from(self.origin_y),
            f32::from(self.cell_width),
            f32::from(self.cell_width) * 2.0,
        )
    }
}

#[test]
fn atlas_reuses_identical_sprites_and_allocates_distinct_glyphs() {
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
    let mut builder = TextAtlasBuilder::new(128, 128);

    let first = builder.prepare_sprite_command(&sprite_batch('⣿', rect), 1.0);
    let repeated = builder.prepare_sprite_command(&sprite_batch('⣿', rect), 1.0);
    assert_eq!((first.rect, first.color), (rect, color()));
    assert!(first.snap_to_pixel_grid);
    assert_eq!(repeated, first);
    assert_eq!(builder.atlas_len(), 1);

    let distinct = builder.prepare_sprite_command(&sprite_batch('█', rect), 1.0);
    assert_ne!(distinct.uv, first.uv);
    assert_eq!(builder.atlas_len(), 2);
}

#[rstest]
#[case('\u{E0B0}', (10.0, 10.0), vec![((8, 5), 255), ((1, 5), 255), ((8, 1), 0), ((8, 8), 0)])]
#[case('⣿', (10.0, 20.0), vec![((1, 1), 255), ((6, 1), 255), ((1, 16), 255), ((6, 16), 255), ((4, 10), 0)])]
#[case('\u{1FBBD}', (16.0, 16.0), vec![((0, 0), 0), ((15, 0), 0), ((8, 7), 0), ((8, 0), 255), ((0, 8), 255)])]
fn sprite_rasters_match_independent_pixel_samples(
    #[case] ch: char,
    #[case] size: (f32, f32),
    #[case] expected: Vec<((usize, usize), u8)>,
) {
    let pixels = rasterized(ch, SurfaceRect::from_min_size(0.0, 0.0, size.0, size.1));
    let actual = expected
        .iter()
        .map(|&((x, y), _)| ((x, y), pixels[(y + 1) * 32 + x + 1]))
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
}

proptest! {
    /// Property: a generated drawable sprite is always renderer-owned, emits at least one command,
    /// and its complete stroked or filled extent remains clipped to its cell.
    #[test]
    fn generated_drawable_sprites_stay_inside_their_cell(sample in any::<SpriteCase>()) {
        let cell = sample.cell();
        let glyph = sample.glyph(cell);
        let commands = glyph.commands_for(cell);

        prop_assert_eq!(SpriteGlyph::from_char(glyph.ch), Some(glyph));
        prop_assert!(commands.iter().any(command_emits_geometry));
        prop_assert!(commands.iter().all(|command| command_fits_cell(command, cell)),
            "{} emitted out-of-cell geometry in {cell:?}: {commands:?}", glyph.ch);
    }
}
