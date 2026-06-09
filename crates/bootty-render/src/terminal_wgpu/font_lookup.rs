use crate::{
    font_database::system_font_database,
    geometry::{CellMetrics, GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT},
    terminal_text::{FontStyle, ResolvedFontFace},
};
use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

pub(super) fn terminal_font(face: &ResolvedFontFace) -> Option<FontArc> {
    static FONT_CACHE: OnceLock<Mutex<TerminalFontCache>> = OnceLock::new();
    let cache = FONT_CACHE.get_or_init(|| Mutex::new(TerminalFontCache::new()));
    cache.lock().ok()?.font_for_face(face)
}

pub(super) fn terminal_font_for_char(face: &ResolvedFontFace, ch: char) -> Option<FontArc> {
    let font = terminal_font(face)?;
    if font_supports_char(&font, ch) {
        return Some(font);
    }

    for family in terminal_font_family_priority(face) {
        let candidate = ResolvedFontFace {
            family,
            fallback_families: Vec::new(),
            style: face.style,
        };
        let Some(font) = terminal_font(&candidate) else {
            continue;
        };
        if font_supports_char(&font, ch) {
            return Some(font);
        }
    }

    Some(font)
}

fn font_supports_char(font: &FontArc, ch: char) -> bool {
    font.glyph_id(ch) != GlyphId(0)
}

pub(super) fn ghostty_cell_metrics_from_font(font: &FontArc, font_size: f32) -> CellMetrics {
    let scale = PxScale::from(font_size.max(1.0));
    let scaled = font.as_scaled(scale);
    let face_width = (' '..='~')
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .fold(0.0_f32, f32::max);
    let face_height = scaled.height() + scaled.line_gap();

    CellMetrics::new(
        face_width.round().max(1.0),
        (face_height.round() * GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT)
            .round()
            .max(1.0),
    )
}

struct TerminalFontCache {
    database: &'static fontdb::Database,
    fonts: HashMap<ResolvedFontFace, Option<FontArc>>,
}

impl TerminalFontCache {
    fn new() -> Self {
        Self {
            database: system_font_database(),
            fonts: HashMap::new(),
        }
    }

    fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        let database = self.database;
        self.fonts
            .entry(face.clone())
            .or_insert_with(|| load_terminal_font(database, face))
            .clone()
    }
}

fn load_terminal_font(database: &fontdb::Database, face: &ResolvedFontFace) -> Option<FontArc> {
    for family in terminal_font_family_priority(face) {
        if family == "monospace" {
            if let Some(font) = load_matching_font(database, &[fontdb::Family::Monospace], face) {
                return Some(font);
            }
        } else if let Some(font) =
            load_matching_font(database, &[fontdb::Family::Name(&family)], face)
        {
            return Some(font);
        }
    }

    load_matching_font(database, &[fontdb::Family::Monospace], face)
}

pub(super) fn terminal_font_family_priority(face: &ResolvedFontFace) -> Vec<String> {
    let mut families = Vec::new();
    push_family(&mut families, &face.family);
    for family in &face.fallback_families {
        push_family(&mut families, family);
    }
    for family in GHOSTTY_FONT_FAMILY_PRIORITY {
        push_family(&mut families, family);
    }
    push_family(&mut families, "monospace");
    families
}

fn push_family(families: &mut Vec<String>, family: &str) {
    if !families.iter().any(|existing| existing == family) {
        families.push(family.to_owned());
    }
}

fn load_matching_font(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    face: &ResolvedFontFace,
) -> Option<FontArc> {
    let id = database.query(&fontdb::Query {
        families,
        weight: font_weight(face.style),
        style: font_style(face.style),
        ..fontdb::Query::default()
    })?;

    database
        .with_face_data(id, |data, face_index| {
            FontVec::try_from_vec_and_index(data.to_vec(), face_index)
                .ok()
                .map(FontArc::new)
        })
        .flatten()
}

fn font_weight(style: FontStyle) -> fontdb::Weight {
    match style {
        FontStyle::Bold | FontStyle::BoldItalic => fontdb::Weight::BOLD,
        FontStyle::Regular | FontStyle::Italic => fontdb::Weight::NORMAL,
    }
}

fn font_style(style: FontStyle) -> fontdb::Style {
    match style {
        FontStyle::Italic | FontStyle::BoldItalic => fontdb::Style::Italic,
        FontStyle::Regular | FontStyle::Bold => fontdb::Style::Normal,
    }
}

pub(super) const GHOSTTY_FONT_FAMILY_PRIORITY: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "Symbols Nerd Font Mono",
];
