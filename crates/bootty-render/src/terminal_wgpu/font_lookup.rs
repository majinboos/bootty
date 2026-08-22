use crate::{
    font_database::{load_matching_font, system_font_database},
    geometry::CellMetrics,
    terminal_text::ResolvedFontFace,
};
use ab_glyph::{Font, FontArc, FontVec, PxScale, ScaleFont};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

const GHOSTTY_CONFIG_CELL_HEIGHT_ADJUSTMENT: f32 = 1.45;

pub(super) fn terminal_font(face: &ResolvedFontFace) -> Option<FontArc> {
    static FONT_CACHE: OnceLock<Mutex<TerminalFontCache>> = OnceLock::new();
    let cache = FONT_CACHE.get_or_init(|| Mutex::new(TerminalFontCache::new()));
    cache.lock().ok()?.font_for_face(face)
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
            if let Some(font) =
                load_matching_font(database, &[fontdb::Family::Monospace], face.style)
            {
                return Some(font);
            }
        } else if let Some(font) =
            load_matching_font(database, &[fontdb::Family::Name(&family)], face.style)
        {
            return Some(font);
        }
    }

    load_matching_font(database, &[fontdb::Family::Monospace], face.style)
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

pub(super) const GHOSTTY_FONT_FAMILY_PRIORITY: &[&str] = &[
    "JetBrains Mono",
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "Symbols Nerd Font Mono",
];
