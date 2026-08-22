use super::clusters::{ShapedCluster, is_combining_mark, is_variation_selector};
#[cfg(target_os = "macos")]
use super::coretext;
use super::shaping;
use super::shaping::font_has_ligature_features;
use ab_glyph::{Font, FontArc, GlyphId, PxScale, ScaleFont, point};
use std::collections::HashMap;

use crate::font_database::{
    font_style, font_weight, load_font_id, query_font_id, system_font_database,
};
use crate::terminal_font_face::FontFaceMetrics;
use crate::terminal_text::{FontFeature, ResolvedFontFace};

#[derive(Clone, Debug)]
pub(super) struct FontLibrary {
    database: &'static fontdb::Database,
    font_ids: HashMap<ResolvedFontFace, Option<fontdb::ID>>,
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
            font_ids: HashMap::new(),
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
    fn primary_font_id(&mut self, face: &ResolvedFontFace) -> Option<fontdb::ID> {
        if !self.font_ids.contains_key(face) {
            let mut id = None;
            for family in std::iter::once(&face.family).chain(face.fallback_families.iter()) {
                let query_family = if family == "monospace" {
                    fontdb::Family::Monospace
                } else {
                    fontdb::Family::Name(family)
                };
                if let Some(found) = query_font_id(self.database, &[query_family], face.style) {
                    id = Some(found);
                    break;
                }
            }
            let id = id
                .or_else(|| query_font_id(self.database, &[fontdb::Family::Monospace], face.style));
            self.font_ids.insert(face.clone(), id);
        }
        self.font_ids.get(face).copied().flatten()
    }

    fn font_has_shaping_features(&mut self, id: fontdb::ID) -> bool {
        let database = self.database;
        *self.shaping_capable.entry(id).or_insert_with(|| {
            database
                .with_face_data(id, font_has_ligature_features)
                .unwrap_or(false)
        })
    }

    pub(super) fn font_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<FontArc> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size, true)?;
        self.font_for_id(id)
    }

    #[cfg(windows)]
    fn font_family_name_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
    ) -> Option<String> {
        let id = self.font_id_for_cluster(face, cluster, physical_font_size, false)?;
        self.database
            .face(id)
            .and_then(|info| info.families.first())
            .map(|(family, _)| family.clone())
    }

    fn font_id_for_cluster(
        &mut self,
        face: &ResolvedFontFace,
        cluster: &ShapedCluster,
        physical_font_size: f32,
        fallback_to_primary: bool,
    ) -> Option<fontdb::ID> {
        let ch = cluster
            .text
            .chars()
            .find(|ch| !is_combining_mark(*ch) && !is_variation_selector(*ch))?;
        let primary_id = self.primary_font_id(face);
        let primary_font = primary_id.and_then(|id| self.font_for_id(id));
        if fallback_to_primary && primary_font.is_none() {
            return None;
        }
        if primary_font
            .as_ref()
            .is_some_and(|font| font_supports_char(font, ch))
        {
            return primary_id;
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
        let fallback_id = self.fallback_font_ids.get(&fallback_key).copied().flatten();
        if fallback_to_primary {
            fallback_id
                .filter(|id| self.font_for_id(*id).is_some())
                .or(primary_id)
        } else {
            fallback_id
        }
    }

    pub(super) fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        let id = self.primary_font_id(face)?;
        self.font_for_id(id)
    }

    fn font_for_id(&mut self, id: fontdb::ID) -> Option<FontArc> {
        let database = self.database;
        self.fonts_by_id
            .entry(id)
            .or_insert_with(|| load_font_id(database, id))
            .clone()
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
        *self
            .metrics
            .entry(key)
            .or_insert_with(|| font_face_metrics(font, scale, constraint_cells, width, height))
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
