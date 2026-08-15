use super::clusters::{ShapedCluster, is_combining_mark, is_variation_selector};
use super::shaping::font_has_ligature_features;
use super::{coretext, shaping};
use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont, point};
use std::collections::HashMap;

use crate::font_database::system_font_database;
use crate::terminal_font_face::FontFaceMetrics;
use crate::terminal_text::{FontFeature, FontStyle, ResolvedFontFace};

#[derive(Clone, Debug)]
pub(super) struct FontLibrary {
    database: &'static fontdb::Database,
    fonts: HashMap<ResolvedFontFace, Option<FontArc>>,
    fonts_by_id: HashMap<fontdb::ID, Option<FontArc>>,
    fallback_font_ids: HashMap<FallbackFontKey, Option<fontdb::ID>>,
    metrics: HashMap<FontMetricsKey, FontFaceMetrics>,
    shaping_capable: HashMap<fontdb::ID, bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FallbackFontKey {
    face: ResolvedFontFace,
    ch: char,
    physical_font_size_bits: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FontMetricsKey {
    face: ResolvedFontFace,
    scale_x_bits: u32,
    scale_y_bits: u32,
    constraint_cells: u16,
    width: u32,
    height: u32,
}

impl FontLibrary {
    pub(super) fn new() -> Self {
        Self {
            database: system_font_database(),
            fonts: HashMap::new(),
            fonts_by_id: HashMap::new(),
            fallback_font_ids: HashMap::new(),
            metrics: HashMap::new(),
            shaping_capable: HashMap::new(),
        }
    }

    /// Shapes text after FontLibrary resolves the primary face and caches its capability.
    pub(super) fn shape_into_clusters(
        &mut self,
        face: &ResolvedFontFace,
        text: &str,
        font_size: f32,
        features: &[FontFeature],
        clusters: &mut Vec<ShapedCluster>,
    ) -> Option<(u16, usize)> {
        let id = self.primary_font_id(face)?;
        if !self.font_has_shaping_features(id) {
            return None;
        }
        let font = self.font_for_id(id)?;
        shaping::shape_clusters(
            self.database,
            id,
            &font,
            text,
            font_size,
            features,
            clusters,
        )
    }
    fn primary_font_id(&self, face: &ResolvedFontFace) -> Option<fontdb::ID> {
        for family in std::iter::once(&face.family).chain(face.fallback_families.iter()) {
            let query_family = if family == "monospace" {
                fontdb::Family::Monospace
            } else {
                fontdb::Family::Name(family)
            };
            if let Some(id) = query_font_id(self.database, &[query_family], face.style) {
                return Some(id);
            }
        }
        query_font_id(self.database, &[fontdb::Family::Monospace], face.style)
    }

    fn font_has_shaping_features(&mut self, id: fontdb::ID) -> bool {
        if let Some(&capable) = self.shaping_capable.get(&id) {
            return capable;
        }
        let capable = self
            .database
            .with_face_data(id, font_has_ligature_features)
            .unwrap_or(false);
        self.shaping_capable.insert(id, capable);
        capable
    }

    pub(super) fn font_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<FontArc> {
        let ch = cluster
            .text
            .chars()
            .find(|ch| !is_combining_mark(*ch) && !is_variation_selector(*ch))?;
        let font = self.font_for_face(face)?;
        if font_supports_char(&font, ch) {
            return Some(font);
        }

        for family in &face.fallback_families {
            let candidate = ResolvedFontFace {
                family: family.clone(),
                fallback_families: Vec::new(),
                style: face.style,
            };
            let Some(font) = self.font_for_face(&candidate) else {
                continue;
            };
            if font_supports_char(&font, ch) {
                return Some(font);
            }
        }

        let fallback_key = FallbackFontKey {
            face: face.clone(),
            ch,
            physical_font_size_bits: physical_font_size.to_bits(),
        };
        if !self.fallback_font_ids.contains_key(&fallback_key) {
            let fallback_id = font_id_supporting_char(self.database, face, ch, physical_font_size);
            self.fallback_font_ids
                .insert(fallback_key.clone(), fallback_id);
        }
        if let Some(id) = self.fallback_font_ids.get(&fallback_key).copied().flatten()
            && let Some(font) = self.font_for_id(id)
        {
            return Some(font);
        }

        Some(font)
    }

    #[cfg(windows)]
    fn font_family_name_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<String> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size)?;
        self.database
            .face(id)
            .and_then(|info| info.families.first())
            .map(|(family, _)| family.clone())
    }

    #[cfg(windows)]
    fn font_id_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<fontdb::ID> {
        let ch = cluster
            .text
            .chars()
            .find(|ch| !is_combining_mark(*ch) && !is_variation_selector(*ch))?;
        if let Some(id) = self.primary_font_id(face)
            && let Some(font) = self.font_for_id(id)
            && font_supports_char(&font, ch)
        {
            return Some(id);
        }

        for family in &face.fallback_families {
            let candidate = ResolvedFontFace {
                family: family.clone(),
                fallback_families: Vec::new(),
                style: face.style,
            };
            if let Some(id) = self.primary_font_id(&candidate)
                && let Some(font) = self.font_for_id(id)
                && font_supports_char(&font, ch)
            {
                return Some(id);
            }
        }

        let fallback_key = FallbackFontKey {
            face: face.clone(),
            ch,
            physical_font_size_bits: physical_font_size.to_bits(),
        };
        if !self.fallback_font_ids.contains_key(&fallback_key) {
            let fallback_id = font_id_supporting_char(self.database, face, ch, physical_font_size);
            self.fallback_font_ids
                .insert(fallback_key.clone(), fallback_id);
        }
        self.fallback_font_ids.get(&fallback_key).copied().flatten()
    }

    pub(super) fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        if !self.fonts.contains_key(face) {
            let font = load_font(self.database, face);
            self.fonts.insert(face.clone(), font);
        }
        self.fonts.get(face).cloned().flatten()
    }

    fn font_for_id(&mut self, id: fontdb::ID) -> Option<FontArc> {
        if !self.fonts_by_id.contains_key(&id) {
            let font = load_font_id(self.database, id);
            self.fonts_by_id.insert(id, font);
        }
        self.fonts_by_id.get(&id).cloned().flatten()
    }

    pub(super) fn font_face_metrics_for(
        &mut self,
        face: &ResolvedFontFace,
        font: &FontArc,
        scale: PxScale,
        constraint_cells: u16,
        width: u32,
        height: u32,
    ) -> FontFaceMetrics {
        let key = FontMetricsKey {
            face: face.clone(),
            scale_x_bits: scale.x.to_bits(),
            scale_y_bits: scale.y.to_bits(),
            constraint_cells,
            width,
            height,
        };
        if let Some(metrics) = self.metrics.get(&key) {
            return *metrics;
        }
        let metrics = font_face_metrics(font, scale, constraint_cells, width, height);
        self.metrics.insert(key, metrics);
        metrics
    }
}

fn font_supports_char(font: &FontArc, ch: char) -> bool {
    font.glyph_id(ch) != GlyphId(0)
}

pub(super) fn font_face_metrics(
    font: &FontArc,
    scale: PxScale,
    constraint_cells: u16,
    width: u32,
    height: u32,
) -> FontFaceMetrics {
    let scaled = font.as_scaled(scale);
    let cell_width = width as f32 / f32::from(constraint_cells.max(1));
    let baseline = ((height as f32 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let face_width = (' '..='~')
        .map(|ch| scaled.h_advance(scaled.glyph_id(ch)))
        .fold(0.0_f32, f32::max)
        .min(cell_width)
        .max(1.0);
    let face_height = scaled.height();
    let cap_height = scaled
        .outline_glyph(
            scaled
                .glyph_id('H')
                .with_scale_and_position(scale, point(0.0, 0.0)),
        )
        .map(|glyph| glyph.px_bounds().height())
        .unwrap_or(face_height);

    FontFaceMetrics {
        cell_width: cell_width.round().max(1.0) as u16,
        cell_height: height.max(1) as u16,
        cell_baseline: ((height as f32 - baseline).round().max(0.0) as u32).min(u32::from(u16::MAX))
            as u16,
        icon_height: f64::from(face_height),
        icon_height_single: f64::from((2.0 * cap_height + face_height) / 3.0),
        face_width: f64::from(face_width),
        face_height: f64::from(face_height),
        face_y: f64::from(((height as f32 - face_height) * 0.5).max(0.0)),
    }
}

fn load_font(database: &fontdb::Database, face: &ResolvedFontFace) -> Option<FontArc> {
    for family in std::iter::once(&face.family).chain(face.fallback_families.iter()) {
        let query_family = if family == "monospace" {
            fontdb::Family::Monospace
        } else {
            fontdb::Family::Name(family)
        };
        if let Some(font) = load_matching_font(database, &[query_family], face) {
            return Some(font);
        }
    }
    load_matching_font(database, &[fontdb::Family::Monospace], face)
}

fn load_matching_font(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    face: &ResolvedFontFace,
) -> Option<FontArc> {
    let id = query_font_id(database, families, face.style)?;
    database
        .with_face_data(id, |data, face_index| {
            FontVec::try_from_vec_and_index(data.to_vec(), face_index)
                .ok()
                .map(FontArc::new)
        })
        .flatten()
}

fn query_font_id(
    database: &fontdb::Database,
    families: &[fontdb::Family<'_>],
    style: FontStyle,
) -> Option<fontdb::ID> {
    database.query(&fontdb::Query {
        families,
        weight: font_weight(style),
        style: font_style(style),
        ..fontdb::Query::default()
    })
}

fn font_id_supporting_char(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    ch: char,
    physical_font_size: f32,
) -> Option<fontdb::ID> {
    if let Some(id) = coretext_fallback_font_id(database, face, ch, physical_font_size) {
        return Some(id);
    }

    let style = face.style;
    let wanted_style = font_style(style);
    let wanted_weight = font_weight(style);
    let faces = database
        .faces()
        .filter(|face| face.style == wanted_style && face.weight == wanted_weight)
        .chain(database.faces().filter(|face| face.style == wanted_style))
        .chain(database.faces());

    for face in faces {
        let Some(font) = load_font_id(database, face.id) else {
            continue;
        };
        if font_supports_char(&font, ch) {
            return Some(face.id);
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn coretext_fallback_font_id(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    ch: char,
    physical_font_size: f32,
) -> Option<fontdb::ID> {
    let names = coretext::fallback_names(&face.family, ch, physical_font_size)?;
    font_id_for_postscript_or_family(database, &names.postscript, &names.family, face)
}

#[cfg(not(target_os = "macos"))]
fn coretext_fallback_font_id(
    _database: &fontdb::Database,
    _face: &ResolvedFontFace,
    _ch: char,
    _physical_font_size: f32,
) -> Option<fontdb::ID> {
    None
}

#[cfg(target_os = "macos")]
fn font_id_for_postscript_or_family(
    database: &fontdb::Database,
    postscript: &str,
    family: &str,
    face: &ResolvedFontFace,
) -> Option<fontdb::ID> {
    let wanted_style = font_style(face.style);
    let wanted_weight = font_weight(face.style);
    database
        .faces()
        .find(|candidate| {
            candidate.post_script_name == postscript
                && candidate.style == wanted_style
                && candidate.weight == wanted_weight
        })
        .or_else(|| {
            database.faces().find(|candidate| {
                candidate
                    .families
                    .iter()
                    .any(|(candidate_family, _)| candidate_family == family)
                    && candidate.style == wanted_style
                    && candidate.weight == wanted_weight
            })
        })
        .or_else(|| {
            database
                .faces()
                .find(|candidate| candidate.post_script_name == postscript)
        })
        .or_else(|| {
            database.faces().find(|candidate| {
                candidate
                    .families
                    .iter()
                    .any(|(candidate_family, _)| candidate_family == family)
            })
        })
        .map(|candidate| candidate.id)
}

fn load_font_id(database: &fontdb::Database, id: fontdb::ID) -> Option<FontArc> {
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
