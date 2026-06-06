use super::super::*;

fn ghostty_font_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/ghostty/src/font/res")
        .join(file)
}

fn bootty_font_path(file: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/fonts")
        .join(file)
}

fn font_library_from_paths(paths: impl IntoIterator<Item = std::path::PathBuf>) -> FontLibrary {
    let mut database = fontdb::Database::new();
    for path in paths {
        database.load_font_file(path).expect("fixture font loads");
    }
    FontLibrary {
        database,
        fonts: HashMap::new(),
    }
}

fn bootty_font_library(files: &[&str]) -> FontLibrary {
    font_library_from_paths(files.iter().map(|file| bootty_font_path(file)))
}

fn regular_face(family: &str, fallback_families: &[&str]) -> ResolvedFontFace {
    ResolvedFontFace {
        family: family.to_owned(),
        fallback_families: fallback_families
            .iter()
            .map(|family| (*family).to_owned())
            .collect(),
        style: FontStyle::Regular,
    }
}

fn shaped_cluster(text: &str, cells: u16) -> ShapedCluster {
    ShapedCluster {
        text: text.to_owned(),
        cell: 0,
        cells,
    }
}

#[allow(clippy::too_many_arguments)]
fn rasterize_cluster_for_test(
    library: &mut FontLibrary,
    face: &ResolvedFontFace,
    cluster: &ShapedCluster,
    font_size: f32,
    pixels_per_point: f32,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> Vec<u8> {
    library
        .rasterize_cluster(RasterizeClusterRequest {
            face,
            cluster,
            font_size,
            pixels_per_point,
            constraint_cells,
            tile: (width, height),
            format: GlyphAtlasFormat::Alpha,
        })
        .pixels
}

#[allow(clippy::too_many_arguments)]
fn positioned_cluster_glyph_for_test(
    font: &FontArc,
    ch: char,
    glyph_id: GlyphId,
    scale: PxScale,
    position: ab_glyph::Point,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    tile: (u32, u32),
) -> ab_glyph::Glyph {
    positioned_cluster_glyph(
        font,
        PositionedClusterGlyphRequest {
            ch,
            glyph_id,
            scale,
            position,
            metrics,
            constraint_cells,
            tile,
        },
    )
}

#[test]
#[ignore = "requires Ghostty private-use icon fixture font that is not vendored in this rewrite"]
fn atlas_rasterizer_uses_fallback_font_for_missing_icon_glyphs() {
    let mut library = font_library_from_paths([
        ghostty_font_path("JetBrainsMonoNoNF-Regular.ttf"),
        ghostty_font_path("JetBrainsMonoNerdFont-Regular.ttf"),
    ]);
    let face = regular_face("JetBrains Mono", &["JetBrainsMono Nerd Font"]);
    let cluster = shaped_cluster("\u{f126}", 1);

    let primary = library
        .font_for_face(&regular_face("JetBrains Mono", &[]))
        .expect("primary fixture face resolves");
    assert!(!font_supports_char(&primary, '\u{f126}'));

    let fallback = library
        .font_for_cluster(&face, &cluster, 24.0)
        .expect("fallback fixture face resolves");

    assert!(font_supports_char(&fallback, '\u{f126}'));
}

#[test]
#[ignore = "requires Ghostty private-use icon fixture font that is not vendored in this rewrite"]
fn atlas_rasterizer_centers_private_use_icon_bounds_in_tile() {
    let bytes = std::fs::read(ghostty_font_path("JetBrainsMonoNerdFont-Regular.ttf"))
        .expect("fixture font reads");
    let font = FontArc::try_from_vec(bytes).expect("fixture font parses");
    let scale = PxScale::from(24.0);
    let scaled = font.as_scaled(scale);
    let glyph_id = scaled.glyph_id('\u{f126}');
    let baseline = ((24.0 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();

    let glyph = positioned_cluster_glyph_for_test(
        &font,
        '\u{f126}',
        glyph_id,
        scale,
        point(0.0, baseline),
        font_face_metrics(&font, scale, 2, 48, 24),
        2,
        (48, 24),
    );
    let bounds = font
        .as_scaled(glyph.scale)
        .outline_glyph(glyph)
        .expect("fixture glyph outlines")
        .px_bounds();

    assert!(bounds.min.x >= 0.0, "{bounds:?}");
    assert!(bounds.min.y >= 0.0, "{bounds:?}");
    assert!(bounds.max.x <= 24.0, "{bounds:?}");
    assert!(bounds.max.y <= 24.0, "{bounds:?}");
}

#[test]
#[ignore = "requires Ghostty private-use icon fixture font that is not vendored in this rewrite"]
fn atlas_rasterizer_outputs_constrained_nerd_font_icon_pixels() {
    let mut library =
        font_library_from_paths([ghostty_font_path("JetBrainsMonoNerdFont-Regular.ttf")]);
    let face = regular_face("JetBrainsMono Nerd Font", &[]);
    let cluster = shaped_cluster("\u{f06ca}", 1);

    let alpha = rasterize_cluster_for_test(&mut library, &face, &cluster, 24.0, 1.0, 1, 24, 24);
    let bounds = alpha_bounds(&alpha, 24, 24).expect("icon has ink");

    assert!(bounds.width() >= 10, "{bounds:?}");
    assert!(bounds.height() >= 8, "{bounds:?}");
    assert!(bounds.max_x < 24, "{bounds:?}");
    assert!(bounds.max_y < 24, "{bounds:?}");
}

#[test]
fn atlas_rasterizer_matches_maple_nerd_font_icon_pixels() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf", "MapleMono-NF-Regular.ttf"]);
    let face = regular_face("Maple Mono", &["Maple Mono NF"]);
    let clusters = TerminalTextShaper::default().shape("\u{f06ca}", 0);
    let alpha = rasterize_cluster_for_test(
        &mut library,
        &face,
        &clusters[0],
        15.666_667,
        2.0,
        2,
        48,
        48,
    );
    let bounds = alpha_bounds_at(&alpha, 48, 48, 128).expect("icon has ink");

    assert_eq!(bounds.width(), 20, "{bounds:?}");
    assert_eq!(bounds.height(), 17, "{bounds:?}");
}

#[test]
fn glyph_atlas_lazy_insert_skips_pixel_generation_for_cached_glyphs() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let key = GlyphAtlasKey {
        face: regular_face("Maple Mono", &[]),
        text: "A".to_owned(),
        font_size_bits: 16.0_f32.to_bits(),
        pixels_per_point_bits: 2.0_f32.to_bits(),
        width: 16,
        height: 24,
    };
    let mut generated = 0;

    let first = atlas.insert_or_get_with(key.clone(), 16, 24, || {
        generated += 1;
        vec![255; 16 * 24]
    });
    let second = atlas.insert_or_get_with(key, 16, 24, || {
        generated += 1;
        vec![0; 16 * 24]
    });

    assert_eq!(first, second);
    assert_eq!(generated, 1);
    assert_eq!(atlas.modified_count(), 1);
}

#[test]
fn atlas_rasterizer_bold_text_has_more_coverage_than_regular_text() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let regular_face = regular_face("Maple Mono", &[]);
    let bold_face = ResolvedFontFace {
        style: FontStyle::Bold,
        ..regular_face.clone()
    };
    let cluster = shaped_cluster("dotfiles", 8);
    let regular = rasterize_cluster_for_test(
        &mut library,
        &regular_face,
        &cluster,
        15.666_667,
        2.0,
        8,
        160,
        48,
    );
    let bold = rasterize_cluster_for_test(
        &mut library,
        &bold_face,
        &cluster,
        15.666_667,
        2.0,
        8,
        160,
        48,
    );

    assert!(alpha_sum(&bold) > alpha_sum(&regular));
}

#[test]
fn atlas_rasterizer_draws_maple_arrow_without_missing_glyph_block() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &["Maple Mono NF"]);
    let cluster = shaped_cluster("\u{21e1}", 1);
    let alpha =
        rasterize_cluster_for_test(&mut library, &face, &cluster, 15.666_667, 2.0, 1, 24, 48);
    let bounds = alpha_bounds(&alpha, 24, 48).expect("arrow has ink");

    assert!(bounds.width() < 24, "{bounds:?}");
    assert!(bounds.height() < 32, "{bounds:?}");
}

#[test]
fn atlas_rasterizer_applies_symbol_fit_constraint_to_arrows() {
    let bytes =
        std::fs::read(bootty_font_path("MapleMono-wght.ttf")).expect("Maple Mono fixture reads");
    let font = FontArc::try_from_vec(bytes).expect("Maple Mono fixture parses");
    let scale = PxScale::from(31.333_334);
    let scaled = font.as_scaled(scale);
    let glyph_id = scaled.glyph_id('\u{21e1}');
    let baseline = ((48.0 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let metrics = font_face_metrics(&font, scale, 1, 24, 48);
    let original = scaled
        .outline_glyph(glyph_id.with_scale_and_position(scale, point(0.0, baseline)))
        .expect("arrow glyph outlines")
        .px_bounds();
    let expected = terminal_glyph_constraint('\u{21e1}' as u32).constrain(
        GlyphSize {
            width: f64::from(original.width()),
            height: f64::from(original.height()),
            x: f64::from(original.min.x),
            y: f64::from(original.min.y),
        },
        metrics,
        1,
    );

    let glyph = positioned_cluster_glyph_for_test(
        &font,
        '\u{21e1}',
        glyph_id,
        scale,
        point(0.0, baseline),
        metrics,
        1,
        (24, 48),
    );
    let bounds = font
        .as_scaled(glyph.scale)
        .outline_glyph(glyph)
        .expect("constrained arrow glyph outlines")
        .px_bounds();

    assert_close(f64::from(bounds.width()), expected.width);
    assert_close(f64::from(bounds.height()), expected.height);
    assert_close(f64::from(bounds.min.x), expected.x);
    assert_close(f64::from(bounds.min.y), expected.y);
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_symbol_rasterizer_produces_arrow_mask() {
    let bytes =
        std::fs::read(bootty_font_path("MapleMono-wght.ttf")).expect("Maple Mono fixture reads");
    let font = FontArc::try_from_vec(bytes).expect("Maple Mono fixture parses");
    let scale = PxScale::from(31.333_334);
    let metrics = font_face_metrics(&font, scale, 1, 24, 48);

    let alpha =
        coretext::rasterize_symbol_with_family("Menlo", '\u{21e1}', 31.333_334, metrics, 1, 24, 48)
            .expect("CoreText arrow mask");
    let bounds = alpha_bounds_at(&alpha, 24, 48, 32).expect("arrow has visible ink");

    assert!(bounds.width() <= 12, "{bounds:?}");
    assert!(bounds.height() <= 24, "{bounds:?}");
    assert!(
        alpha_sum_rows(&alpha, 24, bounds.min_y, bounds.min_y + bounds.height() / 3)
            > alpha_sum_rows(&alpha, 24, bounds.max_y - bounds.height() / 3, bounds.max_y),
        "up arrow mask is vertically inverted: {bounds:?}"
    );
    assert!(alpha_sum(&alpha) > 0);
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_fallback_matches_ghostty_for_maple_arrow_symbol() {
    let names = coretext::fallback_names("Maple Mono", '\u{21e1}', 23.5)
        .expect("CoreText fallback resolves");

    assert_eq!(names.family, "Menlo");
    assert_eq!(names.postscript, "Menlo-Regular");

    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    let face = regular_face("Maple Mono", &["Font Awesome 7 Brands", "Maple Mono NF"]);
    let id = font_id_for_postscript_or_family(&database, &names.postscript, &names.family, &face)
        .expect("CoreText fallback face is present in fontdb");
    let matched = database
        .faces()
        .find(|candidate| candidate.id == id)
        .expect("matched face");

    assert_eq!(matched.post_script_name, "Menlo-Regular");
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_color_rasterizer_draws_dumpling_emoji_pixels() {
    let bytes =
        std::fs::read(bootty_font_path("MapleMono-wght.ttf")).expect("Maple Mono fixture reads");
    let font = FontArc::try_from_vec(bytes).expect("Maple Mono fixture parses");
    let scale = PxScale::from(31.333_334);
    let metrics = font_face_metrics(&font, scale, 2, 48, 48);
    let face = regular_face("Maple Mono", &[]);
    let cluster = shaped_cluster("🥟", 2);

    let rgba = coretext::rasterize_color_cluster(&face, &cluster, 31.333_334, metrics, 2, 48, 48)
        .expect("CoreText emoji pixels");
    let bounds = rgba_alpha_bounds(&rgba, 48, 48).expect("emoji has alpha");

    assert!(bounds.width() >= 20, "{bounds:?}");
    assert!(bounds.height() >= 20, "{bounds:?}");
    assert!(rgba_color_pixel_count(&rgba) >= 100);
}

#[derive(Clone, Copy, Debug)]
struct AlphaBounds {
    min_x: u32,
    min_y: u32,
    max_x: u32,
    max_y: u32,
}

impl AlphaBounds {
    fn width(self) -> u32 {
        self.max_x - self.min_x + 1
    }

    fn height(self) -> u32 {
        self.max_y - self.min_y + 1
    }
}

fn alpha_bounds(alpha: &[u8], width: u32, height: u32) -> Option<AlphaBounds> {
    alpha_bounds_at(alpha, width, height, 0)
}

fn alpha_bounds_at(alpha: &[u8], width: u32, height: u32, threshold: u8) -> Option<AlphaBounds> {
    let mut bounds = AlphaBounds {
        min_x: width,
        min_y: height,
        max_x: 0,
        max_y: 0,
    };
    for y in 0..height {
        for x in 0..width {
            if alpha[(y * width + x) as usize] <= threshold {
                continue;
            }
            bounds.min_x = bounds.min_x.min(x);
            bounds.min_y = bounds.min_y.min(y);
            bounds.max_x = bounds.max_x.max(x);
            bounds.max_y = bounds.max_y.max(y);
        }
    }
    (bounds.min_x <= bounds.max_x).then_some(bounds)
}

fn alpha_sum(alpha: &[u8]) -> u64 {
    alpha.iter().map(|value| u64::from(*value)).sum()
}

fn alpha_sum_rows(alpha: &[u8], width: u32, min_y: u32, max_y: u32) -> u64 {
    (min_y..=max_y)
        .flat_map(|y| (0..width).map(move |x| (y * width + x) as usize))
        .map(|index| u64::from(alpha[index]))
        .sum()
}

fn rgba_alpha_bounds(rgba: &[u8], width: u32, height: u32) -> Option<AlphaBounds> {
    let mut alpha = vec![0; (width * height) as usize];
    for index in 0..alpha.len() {
        alpha[index] = rgba[index * 4 + 3];
    }
    alpha_bounds(&alpha, width, height)
}

fn rgba_color_pixel_count(rgba: &[u8]) -> usize {
    rgba.chunks_exact(4)
        .filter(|pixel| pixel[3] > 0 && (pixel[0] != pixel[1] || pixel[0] != pixel[2]))
        .count()
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() <= 1.0,
        "actual={actual} expected={expected}"
    );
}

#[test]
#[ignore = "requires Ghostty private-use icon fixture font that is not vendored in this rewrite"]
fn atlas_rasterizer_applies_nerd_font_icon_constraints() {
    let bytes = std::fs::read(ghostty_font_path("JetBrainsMonoNerdFont-Regular.ttf"))
        .expect("fixture font reads");
    let font = FontArc::try_from_vec(bytes).expect("fixture font parses");
    let scale = PxScale::from(24.0);
    let scaled = font.as_scaled(scale);
    let glyph_id = scaled.glyph_id('\u{f06ca}');
    let baseline = ((24.0 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let unconstrained = glyph_id.with_scale_and_position(scale, point(0.0, baseline));
    let unconstrained_bounds = scaled
        .outline_glyph(unconstrained)
        .expect("fixture glyph outlines")
        .px_bounds();

    let glyph = positioned_cluster_glyph_for_test(
        &font,
        '\u{f06ca}',
        glyph_id,
        scale,
        point(0.0, baseline),
        font_face_metrics(&font, scale, 2, 48, 24),
        2,
        (48, 24),
    );
    let bounds = font
        .as_scaled(glyph.scale)
        .outline_glyph(glyph)
        .expect("fixture glyph outlines")
        .px_bounds();

    assert!(bounds.width() >= unconstrained_bounds.width(), "{bounds:?}");
    assert!(
        bounds.height() >= unconstrained_bounds.height(),
        "{bounds:?}"
    );
    assert!(bounds.min.x >= 0.0, "{bounds:?}");
    assert!(bounds.min.y >= 0.0, "{bounds:?}");
    assert!(bounds.max.x <= 48.0, "{bounds:?}");
    assert!(bounds.max.y <= 24.0, "{bounds:?}");
}

#[test]
fn symbol_before_space_uses_two_cell_constraint_tile() {
    let clusters = TerminalTextShaper::default().shape("\u{f126} 4.9", 0);

    assert_eq!(
        cluster_constraint_cells(None, &clusters[0], clusters.get(1)),
        2
    );
    assert_eq!(
        cluster_constraint_cells(Some(&clusters[0]), &clusters[1], clusters.get(2)),
        1
    );
}

#[test]
fn symbol_before_implicit_blank_uses_two_cell_constraint_tile() {
    let clusters = TerminalTextShaper::default().shape("\u{f06ca}", 0);

    assert_eq!(cluster_constraint_cells(None, &clusters[0], None), 2);
}

#[test]
fn adjacent_symbols_stay_one_cell_wide() {
    let clusters = TerminalTextShaper::default().shape("\u{f126}\u{f0f4} ", 0);

    assert_eq!(
        cluster_constraint_cells(None, &clusters[0], clusters.get(1)),
        1
    );
    assert_eq!(
        cluster_constraint_cells(Some(&clusters[0]), &clusters[1], clusters.get(2)),
        1
    );
}

#[test]
fn terminal_graphics_symbols_stay_one_cell_wide() {
    let clusters = TerminalTextShaper::default().shape("\u{e0b0} ", 0);

    assert_eq!(
        cluster_constraint_cells(None, &clusters[0], clusters.get(1)),
        1
    );
}
