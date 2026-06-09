use std::sync::Arc;

use bootty_app::{
    geometry::{DEFAULT_FONT_SIZE, SurfaceRect},
    paint_plan::PlanColor,
    terminal_render::TextCommand,
    terminal_text::{FontFeature, FontStyle, ResolvedFontFace},
    terminal_text_atlas::{
        GlyphAtlas, GlyphAtlasError, GlyphAtlasFaceKey, GlyphAtlasFormat, GlyphAtlasKey,
        GlyphAtlasTextKey, TerminalTextShaper, TextAtlasBuilder, TexturedGlyphQuad,
    },
};

#[test]
fn text_shaper_keeps_ligature_combining_emoji_variation_and_fallback_cases() {
    let shaper = TerminalTextShaper::default();

    let clusters = shaper.shape("fi e\u{301} 😀 \u{2764}\u{FE0F}", 1);

    assert!(clusters.iter().any(|cluster| cluster.text == "fi"));
    assert!(clusters.iter().any(|cluster| cluster.text == "e\u{301}"));
    assert!(
        clusters
            .iter()
            .any(|cluster| cluster.text == "😀" && cluster.cells == 2)
    );
    assert!(
        clusters
            .iter()
            .any(|cluster| cluster.text == "\u{2764}\u{FE0F}" && cluster.cells == 1)
    );
}

#[test]
fn text_shaper_honors_ligature_font_feature_settings() {
    let default_clusters = TerminalTextShaper::default().shape("fi", 0);
    assert_eq!(default_clusters.len(), 1);
    assert_eq!(default_clusters[0].text, "fi");

    let without_liga = TerminalTextShaper::with_features(vec![FontFeature::new(*b"liga", 0)]);
    let clusters = without_liga.shape("fi", 0);

    assert_eq!(clusters.len(), 2);
    assert_eq!(clusters[0].text, "f");
    assert_eq!(clusters[1].text, "i");
}

#[test]
fn text_shaper_noop_contract_maps_codepoints_to_cells() {
    let shaper = TerminalTextShaper::with_features(vec![FontFeature::new(*b"liga", 0)]);

    let clusters = shaper.shape("A界e\u{301}", 4);

    assert_eq!(clusters.len(), 3);
    assert_eq!(clusters[0].text, "A");
    assert_eq!(clusters[0].cell, 4);
    assert_eq!(clusters[0].cells, 1);
    assert_eq!(clusters[1].text, "界");
    assert_eq!(clusters[1].cell, 5);
    assert_eq!(clusters[1].cells, 2);
    assert_eq!(clusters[2].text, "e\u{301}");
    assert_eq!(clusters[2].cell, 7);
    assert_eq!(clusters[2].cells, 1);
}

#[test]
fn text_shaper_shape_into_replaces_previous_clusters() {
    let shaper = TerminalTextShaper::default();
    let mut clusters = shaper.shape("stale", 0);

    let total_cells = shaper.shape_into("A界e\u{301}", 4, &mut clusters);

    assert_eq!(total_cells, 4);
    assert_eq!(clusters, shaper.shape("A界e\u{301}", 4));
    assert_eq!(clusters.len(), 3);
    assert_eq!(clusters[0].text, "A");
    assert_eq!(clusters[1].text, "界");
    assert_eq!(clusters[2].text, "e\u{301}");

    let total_cells = shaper.shape_into("fi", 0, &mut clusters);

    assert_eq!(total_cells, 2);
    assert_eq!(clusters.len(), 1);
    assert_eq!(clusters[0].text, "fi");
}

#[test]
fn text_shaper_ports_backend_complex_script_cell_cases() {
    let shaper = TerminalTextShaper::default();
    let samples = [
        ("arabic forced LTR", "مَرْحَبًا"),
        ("devanagari", "कर्म"),
        ("tai tham vowels", "ᨠᩣ"),
        ("tibetan", "བོད"),
        ("javanese", "ꦲꦤ"),
        ("chakma", "𑄇𑄧"),
        ("bengali", "কিরণ"),
    ];

    for (name, text) in samples {
        let clusters = shaper.shape(text, 0);
        assert!(!clusters.is_empty(), "{name} produced no clusters");
        assert_eq!(clusters[0].cell, 0, "{name} did not start at cell zero");
        assert!(
            clusters.iter().all(|cluster| cluster.cells >= 1),
            "{name} produced a zero-width terminal cluster"
        );
        assert_cells_are_monotonic(&clusters, name);
    }
}

#[test]
fn text_shaper_ports_backend_boundary_and_symbol_cases() {
    let shaper = TerminalTextShaper::default();

    let clusters = shaper.shape("a  b", 3);
    assert_eq!(clusters[0].cell, 3);
    assert_eq!(clusters[1].text, " ");
    assert_eq!(clusters[2].text, " ");
    assert_eq!(clusters[3].cell, 6);
    assert_cells_are_monotonic(&clusters, "empty cells with background");

    let emoji = shaper.shape("🥸🥸", 0);
    assert_eq!(emoji.len(), 2);
    assert!(emoji.iter().all(|cluster| cluster.cells == 2));
    assert_eq!(emoji[1].cell, 2);

    let variants = shaper.shape("✊\u{fe0e} ✊\u{fe0f}", 0);
    assert!(variants.iter().any(|cluster| cluster.text == "✊\u{fe0e}"));
    assert!(variants.iter().any(|cluster| cluster.text == "✊\u{fe0f}"));

    let box_glyph = shaper.shape("a─b", 0);
    assert_eq!(box_glyph[1].text, "─");
    assert_eq!(box_glyph[1].cell, 1);

    let cursor_boundary = shaper.shape("fi|fi", 0);
    assert_eq!(cursor_boundary[0].text, "fi");
    assert_eq!(cursor_boundary[1].text, "|");
    assert_eq!(cursor_boundary[2].text, "fi");
    assert_cells_are_monotonic(&cursor_boundary, "cursor boundary");
}

#[test]
fn atlas_reuses_cached_glyph_entries_for_same_key() {
    let mut atlas = GlyphAtlas::new(64, 64);
    let key = GlyphAtlasKey {
        face: GlyphAtlasFaceKey::new(face()),
        text: GlyphAtlasTextKey::new("A"),
        font_size_bits: DEFAULT_FONT_SIZE.to_bits(),
        pixels_per_point_bits: 1.0_f32.to_bits(),
        width: 8,
        height: 12,
    };

    let first = atlas.insert_or_get(key.clone(), 8, 12, vec![255; 8 * 12]);
    let second = atlas.insert_or_get(key, 8, 12, vec![0; 8 * 12]);

    assert_eq!(first, second);
    assert_eq!(atlas.len(), 1);
}

fn assert_cells_are_monotonic(
    clusters: &[bootty_app::terminal_text_atlas::ShapedCluster],
    name: &str,
) {
    for pair in clusters.windows(2) {
        let previous_end = pair[0].cell.saturating_add(pair[0].cells);
        assert!(
            pair[1].cell >= previous_end,
            "{name} cluster {:?} overlaps {:?}",
            pair[1],
            pair[0]
        );
    }
}

#[test]
fn glyph_atlas_ports_ghostty_reserve_fit_edges() {
    let mut exact = GlyphAtlas::new(34, 34);
    assert!(exact.reserve(32, 32).is_some());
    assert_eq!(exact.modified_count(), 0);
    assert!(exact.reserve(1, 1).is_none());

    let mut too_small = GlyphAtlas::new(32, 32);
    assert!(too_small.reserve(32, 32).is_none());

    let mut multiple = GlyphAtlas::new(32, 32);
    assert!(multiple.reserve(15, 30).is_some());
    assert!(multiple.reserve(15, 30).is_some());
    assert!(multiple.reserve(1, 1).is_none());
}

#[test]
fn glyph_atlas_ports_ghostty_write_and_crop_semantics() {
    let mut atlas = GlyphAtlas::new(32, 32);
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    let old = atlas.modified_count();

    atlas.set(entry, &[1, 2, 3, 4]);

    assert!(atlas.modified_count() > old);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
}

#[test]
fn glyph_atlas_ports_ghostty_larger_source_crop() {
    let mut atlas = GlyphAtlas::new(32, 32);
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");

    atlas.set_from_larger(
        entry,
        &[
            8, 8, 8, 8, 8, //
            8, 8, 1, 2, 8, //
            8, 8, 3, 4, 8, //
            8, 8, 8, 8, 8,
        ],
        5,
        2,
        1,
    );

    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
    assert!(!atlas.pixels().contains(&8));
}

#[test]
fn glyph_atlas_ports_ghostty_grow_preserves_data_and_opens_space() {
    let mut atlas = GlyphAtlas::new(4, 4);
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    assert!(atlas.reserve(1, 1).is_none());
    atlas.set(entry, &[1, 2, 3, 4]);

    let old_modified = atlas.modified_count();
    let old_resized = atlas.resized_count();
    atlas.grow(5, 5);

    assert!(atlas.modified_count() > old_modified);
    assert!(atlas.resized_count() > old_resized);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
    assert!(atlas.reserve(1, 1).is_some());
}

#[test]
fn glyph_atlas_ports_ghostty_bgr_write_and_grow_semantics() {
    let mut atlas = GlyphAtlas::with_format(4, 4, GlyphAtlasFormat::Bgr);
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    assert!(atlas.reserve(1, 1).is_none());

    atlas.set(
        entry,
        &[
            10, 11, 12, //
            13, 14, 15, //
            20, 21, 22, //
            23, 24, 25,
        ],
    );

    assert_eq!(atlas.atlas_pixel_channel(1, 1, 0), Some(10));
    assert_eq!(atlas.atlas_pixel_channel(1, 1, 1), Some(11));
    assert_eq!(atlas.atlas_pixel_channel(1, 1, 2), Some(12));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 0), Some(13));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 1), Some(14));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 2), Some(15));
    assert_eq!(atlas.atlas_pixel_channel(3, 1, 0), Some(0));
    assert_eq!(atlas.atlas_pixel_channel(1, 2, 0), Some(20));
    assert_eq!(atlas.atlas_pixel_channel(2, 2, 2), Some(25));

    atlas.grow(5, 5);

    assert_eq!(atlas.atlas_pixel_channel(1, 1, 0), Some(10));
    assert_eq!(atlas.atlas_pixel_channel(1, 1, 1), Some(11));
    assert_eq!(atlas.atlas_pixel_channel(1, 1, 2), Some(12));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 0), Some(13));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 1), Some(14));
    assert_eq!(atlas.atlas_pixel_channel(2, 1, 2), Some(15));
    assert_eq!(atlas.atlas_pixel_channel(3, 1, 0), Some(0));
    assert_eq!(atlas.atlas_pixel_channel(1, 2, 0), Some(20));
    assert_eq!(atlas.atlas_pixel_channel(2, 2, 2), Some(25));
    assert!(atlas.reserve(1, 3).is_some());
    assert!(atlas.reserve(2, 1).is_some());
    assert!(atlas.reserve(1, 1).is_none());
}

#[test]
fn glyph_atlas_ports_ghostty_error_paths_without_partial_mutation() {
    assert!(matches!(
        GlyphAtlas::try_with_format(32, 32, GlyphAtlasFormat::Alpha, 4),
        Err(GlyphAtlasError::CapacityExceeded)
    ));

    let mut atlas = GlyphAtlas::new(4, 4);
    let entry = atlas.reserve(2, 2).expect("2x2 atlas region");
    atlas.set(entry, &[1, 2, 3, 4]);
    let old_modified = atlas.modified_count();
    let old_resized = atlas.resized_count();
    assert!(atlas.reserve(1, 1).is_none());
    assert_eq!(atlas.modified_count(), old_modified);
    assert_eq!(atlas.resized_count(), old_resized);
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);

    assert_eq!(
        atlas.try_grow_with_byte_limit(5, 5, 4),
        Err(GlyphAtlasError::CapacityExceeded)
    );
    assert_eq!(atlas.modified_count(), old_modified);
    assert_eq!(atlas.resized_count(), old_resized);
    assert_eq!(atlas.size(), (4, 4));
    assert_atlas_pixels(&atlas, &[(1, 1, 1), (2, 1, 2), (1, 2, 3), (2, 2, 4)]);
}

#[test]
fn text_atlas_builder_emits_one_textured_quad_per_shaped_cluster() {
    let command = TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 20.0),
        text: "A界".to_owned(),
        attrs: attrs(),
        face: Arc::new(face()),
        font_size: DEFAULT_FONT_SIZE,
    };
    let mut builder = TextAtlasBuilder::new(128, 128);

    let quads = builder.prepare_text_command(&command, 1.0);

    assert_eq!(quads.len(), 2);
    assert!(quads[0].uv.min_x < quads[0].uv.max_x);
    assert_eq!(quads[0].color, attrs().fg);
    assert_eq!(builder.atlas_len(), 2);
}

#[test]
fn text_atlas_builder_appends_textured_quads_without_replacing_existing_batch() {
    let command = TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 20.0, 20.0),
        text: "AB".to_owned(),
        attrs: attrs(),
        face: Arc::new(face()),
        font_size: DEFAULT_FONT_SIZE,
    };
    let sentinel = TexturedGlyphQuad {
        rect: SurfaceRect::from_min_size(99.0, 99.0, 1.0, 1.0),
        uv: SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
        color: attrs().fg,
    };
    let mut builder = TextAtlasBuilder::new(128, 128);
    let mut quads = vec![sentinel];

    builder.prepare_text_command_into(&command, 1.0, &mut quads);

    assert_eq!(quads.len(), 3);
    assert_eq!(quads[0], sentinel);
    assert_eq!(builder.atlas_len(), 2);
}

#[test]
fn atlas_keys_separate_same_glyph_at_different_pixel_scales() {
    let command = TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0),
        text: "A".to_owned(),
        attrs: attrs(),
        face: Arc::new(face()),
        font_size: DEFAULT_FONT_SIZE,
    };
    let mut builder = TextAtlasBuilder::new(128, 128);

    builder.prepare_text_command(&command, 1.0);
    builder.prepare_text_command(&command, 2.0);

    assert_eq!(builder.atlas_len(), 2);
}

#[test]
fn whitespace_clusters_do_not_create_invisible_quads_or_atlas_entries() {
    let command = TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 30.0, 20.0),
        text: "A B".to_owned(),
        attrs: attrs(),
        face: Arc::new(face()),
        font_size: DEFAULT_FONT_SIZE,
    };
    let mut builder = TextAtlasBuilder::new(64, 64);

    let quads = builder.prepare_text_command(&command, 1.0);

    assert_eq!(quads.len(), 2);
    assert_eq!(
        quads[0].rect,
        SurfaceRect::from_min_size(0.0, 0.0, 10.0, 20.0)
    );
    assert_eq!(
        quads[1].rect,
        SurfaceRect::from_min_size(20.0, 0.0, 10.0, 20.0)
    );
    assert_eq!(builder.atlas_len(), 2);
}

fn face() -> ResolvedFontFace {
    ResolvedFontFace {
        family: "Test Mono".to_owned(),
        fallback_families: vec!["Fallback".to_owned()],
        style: FontStyle::Regular,
    }
}

fn attrs() -> bootty_app::paint_plan::TextAttrs {
    bootty_app::paint_plan::TextAttrs {
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

fn assert_atlas_pixels(atlas: &GlyphAtlas, expected: &[(u32, u32, u8)]) {
    for (x, y, value) in expected {
        assert_eq!(atlas.atlas_pixel(*x, *y), Some(*value), "pixel {x},{y}");
    }
}
