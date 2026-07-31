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
        database: Box::leak(Box::new(database)),
        fonts: HashMap::new(),
        fonts_by_id: HashMap::new(),
        fallback_font_ids: HashMap::new(),
        metrics: HashMap::new(),
        shaping_capable: HashMap::new(),
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
        is_whitespace: text.chars().all(char::is_whitespace),
        glyphs: Default::default(),
    }
}

fn text_command(text: &str) -> TextCommand {
    TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, text.len() as f32 * 10.0, 20.0),
        text: text.to_owned(),
        attrs: crate::paint_plan::TextAttrs {
            fg: PlanColor {
                r: 240,
                g: 240,
                b: 240,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        },
        face: Arc::new(regular_face("Maple Mono", &[])),
        font_size: 16.0,
    }
}

fn force_atlas_resize(builder: &mut TextAtlasBuilder) {
    builder
        .atlas
        .grow(builder.atlas.width * 2, builder.atlas.height * 2);
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
fn atlas_rasterizer_uses_database_fallback_for_private_use_icon_without_face_fallbacks() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf", "MapleMono-NF-Regular.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let clusters = TerminalTextShaper::default().shape("\u{f0770}", 0);
    let alpha = rasterize_cluster_for_test(&mut library, &face, &clusters[0], 15.0, 2.0, 1, 20, 44);
    let bounds = alpha_bounds_at(&alpha, 20, 44, 128).expect("icon has ink");

    assert!(bounds.width() > 6, "{bounds:?}");
    assert!(bounds.height() > 6, "{bounds:?}");
}

#[test]
fn atlas_rasterizer_shares_loaded_font_for_multiple_database_fallback_chars() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf", "MapleMono-NF-Regular.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let clusters = [
        TerminalTextShaper::default().shape("\u{f06ca}", 0),
        TerminalTextShaper::default().shape("\u{f0770}", 0),
    ];

    for cluster in &clusters {
        let alpha =
            rasterize_cluster_for_test(&mut library, &face, &cluster[0], 15.0, 2.0, 1, 20, 44);
        assert!(alpha_bounds_at(&alpha, 20, 44, 128).is_some());
    }

    assert_eq!(library.fallback_font_ids.len(), 2);
    assert_eq!(library.fonts_by_id.len(), 1);
}

#[test]
fn glyph_atlas_lazy_insert_skips_pixel_generation_for_cached_glyphs() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let key = GlyphAtlasKey {
        face: GlyphAtlasFaceKey::new(regular_face("Maple Mono", &[])),
        text: GlyphAtlasTextKey::new("A"),
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
fn glyph_atlas_tracks_dirty_bounds_by_modification() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let face = GlyphAtlasFaceKey::new(regular_face("Maple Mono", &[]));
    for (text, width, height) in [("A", 4, 5), ("B", 3, 2)] {
        atlas.insert_or_get_with(
            GlyphAtlasKey {
                face: face.clone(),
                text: GlyphAtlasTextKey::new(text),
                font_size_bits: 16.0_f32.to_bits(),
                pixels_per_point_bits: 1.0_f32.to_bits(),
                width,
                height,
            },
            width,
            height,
            || vec![255; (width * height) as usize],
        );
    }

    assert_eq!(
        atlas.dirty_rect_since(0),
        Some(GlyphAtlasEntry {
            x: 1,
            y: 1,
            width: 8,
            height: 5,
        })
    );
    assert_eq!(
        atlas.dirty_rect_since(1),
        Some(GlyphAtlasEntry {
            x: 6,
            y: 1,
            width: 3,
            height: 2,
        })
    );
    assert_eq!(atlas.dirty_rect_since(2), None);
}

#[test]
fn glyph_atlas_grows_to_fit_instead_of_dropping_glyphs() {
    // Far more glyphs than a 64x64 atlas holds. Each must get a real slot by growing the atlas;
    // the 1x1 fallback would render as missing characters or black boxes (the regression hit when
    // supersampled zoom glyphs overflowed the fixed atlas).
    let mut atlas = GlyphAtlas::new(64, 64);
    let face = GlyphAtlasFaceKey::new(regular_face("Maple Mono", &[]));
    for i in 0..40 {
        let key = GlyphAtlasKey {
            face: face.clone(),
            text: GlyphAtlasTextKey::new(format!("g{i}")),
            font_size_bits: 16.0_f32.to_bits(),
            pixels_per_point_bits: 2.0_f32.to_bits(),
            width: 16,
            height: 24,
        };
        let entry = atlas.insert_or_get_with(key, 16, 24, || vec![255; 16 * 24]);
        assert_eq!(
            (entry.width, entry.height),
            (16, 24),
            "glyph {i} was dropped to the 1x1 fallback instead of growing the atlas"
        );
    }
    assert!(
        atlas.resized_count() > 0,
        "atlas should have grown to fit the glyphs"
    );
}

#[test]
fn glyph_atlas_recycles_at_the_size_cap_instead_of_dropping_glyphs() {
    let mut atlas = GlyphAtlas::new(4096, 4096);
    let face = GlyphAtlasFaceKey::new(regular_face("Maple Mono", &[]));
    let mut first_key = None;

    for i in 0..5 {
        let key = GlyphAtlasKey {
            face: face.clone(),
            text: GlyphAtlasTextKey::new(format!("large-{i}")),
            font_size_bits: 16.0_f32.to_bits(),
            pixels_per_point_bits: 2.0_f32.to_bits(),
            width: 2046,
            height: 2046,
        };
        if i == 0 {
            first_key = Some(key.clone());
        }
        let entry = atlas.insert_or_get_with(key, 2046, 2046, || vec![255; 2046 * 2046]);
        assert_eq!(
            (entry.width, entry.height),
            (2046, 2046),
            "glyph {i} was dropped after the atlas reached its size cap"
        );
    }

    assert!(atlas.get(&first_key.expect("first key")).is_none());
    assert_eq!(atlas.len(), 1);
    assert_eq!(atlas.resized_count(), 1);
}

#[test]
fn ascii_glyph_cache_refreshes_after_atlas_recycle() {
    let mut builder = TextAtlasBuilder::new(64, 64);
    let command = text_command("A");

    assert_eq!(builder.prepare_text_command(&command, 1.0).len(), 1);
    assert_eq!(builder.atlas.len(), 1);
    builder.atlas.recycle();
    assert!(builder.atlas.is_empty());

    assert_eq!(builder.prepare_text_command(&command, 1.0).len(), 1);
    assert_eq!(builder.atlas.len(), 1);
}

#[test]
fn sprite_quad_uvs_sample_texel_centers_not_transparent_atlas_gutters() {
    let mut builder = TextAtlasBuilder::new_rgba(64, 64);
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0);
    let registry = crate::terminal_sprite::SpriteRegistry::prompt_graphics();
    let glyph = registry.glyph_for('─').expect("box glyph is sprite-owned");
    let command = crate::terminal_render::SpriteCommandBatch {
        ch: '─',
        glyph,
        rect,
        color: PlanColor {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        },
        commands: registry.commands_for(glyph, rect),
    };

    let quad = builder.prepare_sprite_command(&command, 1.0);

    assert_eq!(quad.uv.min_x, 1.5 / 64.0);
    assert_eq!(quad.uv.min_y, 1.5 / 64.0);
    assert_eq!(quad.uv.max_x, 10.5 / 64.0);
    assert_eq!(quad.uv.max_y, 20.5 / 64.0);
}
#[test]
fn prepared_text_frame_cache_refreshes_uvs_after_atlas_resize() {
    let mut builder = TextAtlasBuilder::new(64, 64);
    let command = text_command("A");
    let mut first = Vec::new();

    builder.begin_text_frame();
    builder.prepare_text_command_into(&command, 1.0, &mut first);
    builder.finish_text_frame();
    let old_uv = first[0].uv;
    let old_resized_count = builder.atlas_resized_count();

    force_atlas_resize(&mut builder);
    assert!(builder.atlas_resized_count() > old_resized_count);

    let mut second = Vec::new();
    builder.begin_text_frame();
    builder.prepare_text_command_into(&command, 1.0, &mut second);
    builder.finish_text_frame();

    assert_ne!(second[0].uv, old_uv);
    assert_eq!(
        builder.prepared_text_cache[0].atlas_resized_count,
        builder.atlas_resized_count()
    );
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
fn monospace_generic_resolves_to_a_monospaced_face() {
    // The CoreText fallback must not pass the generic "monospace" to CTFontCreateWithName, which
    // resolves it to Helvetica (proportional) and shadows the cascade. It should land on a real
    // fixed-pitch face from the shared font database.
    let resolved = coretext::coretext_family_name("monospace");
    assert_ne!(
        resolved.as_ref(),
        "monospace",
        "generic leaked through unresolved"
    );

    let database = crate::font_database::system_font_database();
    let face = database
        .faces()
        .find(|face| {
            face.families
                .iter()
                .any(|(name, _)| name == resolved.as_ref())
        })
        .expect("resolved monospace family is present in the font database");
    assert!(
        face.monospaced,
        "monospace generic resolved to a non-monospaced face: {resolved}"
    );
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

#[test]
fn text_presentation_symbols_stay_off_the_color_emoji_path() {
    assert!(!is_color_emoji_cluster(&shaped_cluster("⚠", 1)));
    assert!(!is_color_emoji_cluster(&shaped_cluster("✔", 1)));
    assert!(is_color_emoji_cluster(&shaped_cluster("⚠\u{fe0f}", 2)));
    assert!(!is_color_emoji_cluster(&shaped_cluster("⚡\u{fe0e}", 1)));
    assert!(is_color_emoji_cluster(&shaped_cluster("⚡", 1)));
    assert!(is_color_emoji_cluster(&shaped_cluster("🥟", 2)));
}

#[cfg(target_os = "macos")]
#[test]
fn coretext_color_rasterizer_rejects_monochrome_fonts() {
    let cluster = shaped_cluster("⚠", 1);
    assert!(coretext::rasterize_color_with_family("Menlo", &cluster, 31.333_334, 48, 48).is_none());
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

fn alpha_sum_columns(alpha: &[u8], width: u32, min_x: u32, max_x: u32) -> u64 {
    let height = alpha.len() as u32 / width;
    (0..height)
        .flat_map(|y| (min_x..max_x).map(move |x| (y * width + x) as usize))
        .map(|index| u64::from(alpha[index]))
        .sum()
}

#[cfg(target_os = "macos")]
fn alpha_sum_rows(alpha: &[u8], width: u32, min_y: u32, max_y: u32) -> u64 {
    (min_y..=max_y)
        .flat_map(|y| (0..width).map(move |x| (y * width + x) as usize))
        .map(|index| u64::from(alpha[index]))
        .sum()
}

#[cfg(target_os = "macos")]
fn rgba_alpha_bounds(rgba: &[u8], width: u32, height: u32) -> Option<AlphaBounds> {
    let mut alpha = vec![0; (width * height) as usize];
    for index in 0..alpha.len() {
        alpha[index] = rgba[index * 4 + 3];
    }
    alpha_bounds(&alpha, width, height)
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
#[test]
fn emoji_presentation_cluster_renders_in_color_even_with_a_primary_glyph() {
    // Regression: when the primary font carries a monochrome glyph for an emoji's base symbol,
    // the by-glyph path used to consume the cluster and draw a theme-tinted text glyph instead of
    // the color emoji the VS16 selector requests. The color path must win on an Rgba atlas.
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    // Any valid primary-font glyph stands in for a font that happens to have a ⚠ glyph; the point
    // is that the cluster carries glyphs yet must still take the color path because of the FE0F.
    let glyph_id = library
        .font_for_face(&face)
        .expect("Maple Mono loads")
        .glyph_id('A');

    let mut cluster = shaped_cluster("\u{26A0}\u{FE0F}", 1);
    cluster.glyphs.push(ShapedGlyph {
        glyph_id: glyph_id.0,
        cluster: 0,
        x_offset: 0.0,
        y_offset: 0.0,
    });

    let rasterized = library.rasterize_cluster(RasterizeClusterRequest {
        face: &face,
        cluster: &cluster,
        font_size: 16.0,
        pixels_per_point: 2.0,
        constraint_cells: 1,
        tile: (24, 40),
        format: GlyphAtlasFormat::Rgba,
    });

    assert!(
        rasterized.color,
        "emoji-presentation cluster must take the color path, not the monochrome glyph path"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn color_emoji_quad_sized_to_em_in_narrow_cells() {
    // Regression: with a narrow cell, two cells are far thinner than the line, so sizing the emoji
    // to its grid-cell width rendered it tiny; sizing it to the full padded cell height overshot
    // and overlapped neighbors. The quad must be a square at the text em (font size), centered over
    // its grid span — matching the visual weight of surrounding glyphs the way Ghostty draws it.
    let mut builder = TextAtlasBuilder::new_rgba(512, 512);
    builder.fonts = bootty_font_library(&["MapleMono-wght.ttf"]);
    let (cw, ch) = (7.0_f32, 23.0_f32); // narrow real-world aspect (cell far taller than wide)
    let mut command = text_command("\u{26A0}\u{FE0F}");
    let em = command.font_size;
    command.rect = SurfaceRect::from_min_size(0.0, 0.0, 2.0 * cw, ch);

    let mut quads = Vec::new();
    builder.prepare_text_command_into(&command, 1.0, &mut quads);

    let emoji = quads.first().expect("emoji produced a quad");
    assert!(
        (emoji.rect.width() - emoji.rect.height()).abs() < 0.5,
        "emoji quad should be square: {}x{}",
        emoji.rect.width(),
        emoji.rect.height()
    );
    assert!(
        (emoji.rect.height() - em).abs() < 0.5,
        "emoji quad side {} should be the text em {em}",
        emoji.rect.height()
    );
    // Between the two failure modes: bigger than the thin 2-cell width, smaller than the padded cell.
    assert!(emoji.rect.width() > 2.0 * cw && emoji.rect.height() < ch);
}

#[cfg(target_os = "macos")]
#[test]
fn color_emoji_fills_its_cell_without_clipping() {
    // Regressions: the color path drew the emoji at its natural font size and center-blitted it,
    // so Apple Color Emoji (smaller than the cell) rendered tiny, and oversized glyphs clipped at
    // the tile edge. It must now scale to fill the cell box, centered, with no edge clipping.
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let (w, h) = (48u32, 40u32);
    let cluster = shaped_cluster("\u{26A0}\u{FE0F}", 2);

    let rasterized = library.rasterize_cluster(RasterizeClusterRequest {
        face: &face,
        cluster: &cluster,
        font_size: 16.0,
        pixels_per_point: 2.0,
        constraint_cells: 2,
        tile: (w, h),
        format: GlyphAtlasFormat::Rgba,
    });
    assert!(rasterized.color, "renders via the color path");

    let alpha_at = |x: u32, y: u32| rasterized.pixels[((y * w + x) * 4 + 3) as usize];
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (w, h, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if alpha_at(x, y) > 16 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    assert!(max_y >= min_y, "emoji produced ink");
    // Not clipped: ink stays off every edge (the fit leaves a margin).
    assert!(
        min_x > 0 && min_y > 0 && max_x < w - 1 && max_y < h - 1,
        "emoji ink touches a tile edge (clipped): x[{min_x}..{max_x}] y[{min_y}..{max_y}] in {w}x{h}"
    );
    // Not tiny: fills most of the cell height (the limiting dimension for a square glyph).
    assert!(
        max_y - min_y + 1 >= h / 2,
        "emoji ink is too small: {} tall in {h}",
        max_y - min_y + 1
    );
}

#[test]
fn glyph_id_cluster_rasterizes_the_same_ink_as_its_character() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let glyph_id = library
        .font_for_face(&face)
        .expect("Maple Mono loads")
        .glyph_id('A');
    assert_ne!(glyph_id.0, 0, "Maple Mono has an 'A' glyph");

    let by_char = shaped_cluster("A", 1);
    let mut by_glyph = shaped_cluster("A", 1);
    by_glyph.glyphs.push(ShapedGlyph {
        glyph_id: glyph_id.0,
        cluster: 0,
        x_offset: 0.0,
        y_offset: 0.0,
    });

    let char_alpha =
        rasterize_cluster_for_test(&mut library, &face, &by_char, 16.0, 2.0, 1, 24, 40);
    let glyph_alpha =
        rasterize_cluster_for_test(&mut library, &face, &by_glyph, 16.0, 2.0, 1, 24, 40);

    // The glyph-id path must reproduce exactly what the per-character path draws
    // for the same glyph; only the source (shaped id vs. cmap lookup) differs.
    assert_eq!(glyph_alpha, char_alpha);
}

#[test]
fn shaping_emits_legacy_clusters_for_plain_text_in_a_gsub_font() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let features = crate::terminal_text::default_font_features();
    let mut clusters = Vec::new();

    let (total_cells, len) = library
        .shape_into_clusters(&face, "abc", 16.0, &features, &mut clusters)
        .expect("Maple Mono advertises GSUB features, so the run is shaped");

    assert_eq!((total_cells, len), (3, 3));
    assert_eq!(clusters[0].text, "a");
    assert!(
        clusters[..len]
            .iter()
            .all(|cluster| cluster.glyphs.is_empty()),
        "plain ASCII needs no glyph-id drawing: {clusters:?}"
    );
}

#[test]
fn default_shaping_groups_contextual_operator_ligature_pieces() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let features = crate::terminal_text::default_font_features();
    let mut clusters = Vec::new();

    for operator in ["<>", "!=", "==", "->", "<=", ">=", "=>", "&&", "||"] {
        let (total_cells, len) = library
            .shape_into_clusters(&face, operator, 16.0, &features, &mut clusters)
            .expect("Maple Mono advertises GSUB features, so the run is shaped");

        assert_eq!(total_cells, 2, "{operator}");
        assert_eq!(len, 1, "{operator}: {clusters:?}");
        assert_eq!(clusters[0].text, operator, "{operator}");
        assert_eq!(clusters[0].cell, 0, "{operator}");
        assert_eq!(clusters[0].cells, 2, "{operator}");
        assert_eq!(clusters[0].glyphs.len(), 2, "{operator}: {clusters:?}");
    }
}

#[test]
fn contextual_operator_ligature_rasterizes_across_merged_cells() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let features = crate::terminal_text::default_font_features();
    let mut clusters = Vec::new();
    let (_, len) = library
        .shape_into_clusters(&face, "<>", 16.0, &features, &mut clusters)
        .expect("Maple Mono advertises GSUB features, so the run is shaped");
    assert_eq!(len, 1);

    let alpha = rasterize_cluster_for_test(&mut library, &face, &clusters[0], 16.0, 2.0, 2, 48, 40);

    assert!(alpha_sum_columns(&alpha, 48, 0, 24) > 0);
    assert!(alpha_sum_columns(&alpha, 48, 24, 48) > 0);
}

#[test]
fn shaped_run_cache_reuses_shaping_for_moved_text_without_changing_output() {
    // A ligature-bearing run forces the cached (Some) shaping path rather than the
    // per-character ascii fast path, so the shaped-run cache is actually exercised.
    let text = "fn area() -> i32 { a != b && c == d }";
    let command_at = |x: f32, y: f32| {
        let mut command = text_command(text);
        command.rect = SurfaceRect::from_min_size(x, y, text.len() as f32 * 9.0, 22.0);
        command
    };

    // Fresh builder shapes the run directly at the target position.
    let mut fresh = TextAtlasBuilder::new(512, 512);
    fresh.fonts = bootty_font_library(&["MapleMono-wght.ttf"]);
    let mut expected = Vec::new();
    fresh.prepare_text_command_into(&command_at(30.0, 44.0), 1.0, &mut expected);

    // Reused builder shapes the run once elsewhere (warming the cache), then prepares
    // it at the target position, which must hit the cache and translate, not reshape.
    let mut reused = TextAtlasBuilder::new(512, 512);
    reused.fonts = bootty_font_library(&["MapleMono-wght.ttf"]);
    let mut warm = Vec::new();
    reused.prepare_text_command_into(&command_at(0.0, 0.0), 1.0, &mut warm);
    assert_eq!(
        reused.shaped_run_cache.len(),
        1,
        "the ligature run shaped through the Some path and populated the cache"
    );
    let mut actual = Vec::new();
    reused.prepare_text_command_into(&command_at(30.0, 44.0), 1.0, &mut actual);

    assert_eq!(
        actual, expected,
        "cached shaping must produce the same quads as fresh shaping at the new position"
    );
    assert!(!actual.is_empty(), "the run produced glyph quads");
}

#[test]
fn color_emoji_constraint_is_independent_of_following_character() {
    // The bug: a VS16 emoji (one grid cell) was widened to two cells when alone or before a
    // space by the lone-symbol heuristic, so its rendered size flipped with whatever followed
    // it and the glyph spilled into the next column, eating a following space.
    let emoji = shaped_cluster("\u{26A0}\u{FE0F}", 1);
    let space = shaped_cluster(" ", 1);
    let period = shaped_cluster(".", 1);
    assert_eq!(cluster_constraint_cells(None, &emoji, None), 1, "alone");
    assert_eq!(
        cluster_constraint_cells(None, &emoji, Some(&space)),
        1,
        "before a space"
    );
    assert_eq!(
        cluster_constraint_cells(None, &emoji, Some(&period)),
        1,
        "before a period"
    );
}

#[test]
fn configured_variants_and_style_sets_shape_feature_heavy_samples() {
    let mut library = bootty_font_library(&["MapleMono-wght.ttf"]);
    let face = regular_face("Maple Mono", &[]);
    let mut features = crate::terminal_text::default_font_features();
    features.extend(
        [
            "cv01", "cv02", "cv33", "cv34", "cv35", "cv36", "cv61", "cv62", "ss05", "ss06", "ss07",
            "ss08",
        ]
        .into_iter()
        .map(|tag| crate::terminal_text::FontFeature::parse(tag).expect("feature parses")),
    );
    let mut clusters = Vec::new();
    let mut shaped_samples = 0;

    for sample in [
        r"~!@#$%^&* {} [] () I1l O0o",
        r"!== \\ <= #{ -> ~@ |> 0x12",
        r"|=>==<==>=|======|===|===>",
        r"<---|--|--------|-<->--<-|",
        r"[INFO] [TODO] [FIXME]",
    ] {
        let (total_cells, len) = library
            .shape_into_clusters(&face, sample, 16.0, &features, &mut clusters)
            .expect("Maple Mono advertises GSUB features, so the run is shaped");
        let expected_cells = sample
            .chars()
            .map(crate::terminal_text::terminal_char_width)
            .sum::<u16>();

        assert_eq!(total_cells, expected_cells, "{sample}");
        assert!(len > 0, "{sample}");
        shaped_samples += usize::from(
            clusters[..len]
                .iter()
                .any(|cluster| !cluster.glyphs.is_empty()),
        );
    }

    assert_eq!(shaped_samples, 5);
}

#[test]
fn fit_constrained_symbols_route_to_the_constraint_path() {
    // A glyph that fit-scales to the cell must also be routed to the per-character path; the
    // direct glyph-id path skips the fit and draw_outline_glyph clips the overflow. Geometric
    // Shapes circles (U+25A0..U+25FF) regressed exactly this way — they gained a Fit constraint
    // while the routing gate still stopped at U+259F, so they clipped on the right edge.
    for cp in [0x25A0u32, 0x25CB, 0x25CF, 0x25D0, 0x25EF, 0x25FF] {
        let ch = char::from_u32(cp).unwrap();
        assert!(
            terminal_glyph_constraint(cp).does_anything(),
            "U+{cp:04X} should carry a fit constraint",
        );
        assert!(
            is_symbol_like(ch),
            "U+{cp:04X} must take the constraint path, not the direct glyph-id path",
        );
    }
}
