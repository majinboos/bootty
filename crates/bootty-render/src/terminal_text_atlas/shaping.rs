use rustybuzz::ttf_parser::Tag;
use rustybuzz::{BufferClusterLevel, Direction, Face, Feature, UnicodeBuffer};

use ab_glyph::{Font, FontArc, GlyphId};

use super::clusters::{
    ShapedCluster, ShapedGlyph, is_combining_mark, is_default_emoji_presentation, is_private_use,
    is_symbol_like, is_variation_selector, shaped_cluster_slot,
};
use crate::terminal_text::{FontFeature, terminal_char_width, terminal_grapheme_cells};

/// Shapes a run of text against a font's GSUB/GPOS tables via HarfBuzz
/// (`rustybuzz`). Ligatures and contextual alternates only form when the font
/// actually contains them, so a font without an "fi" ligature yields two
/// separate glyphs rather than a forced merge.
///
/// Returns `None` when the bytes do not parse as a usable face.
pub(super) fn shape_run(
    font_data: &[u8],
    face_index: u32,
    text: &str,
    font_size: f32,
    features: &[Feature],
) -> Option<Vec<ShapedGlyph>> {
    let face = Face::from_slice(font_data, face_index)?;
    let units_per_em = face.units_per_em() as f32;
    if units_per_em <= 0.0 {
        return None;
    }
    let scale = font_size / units_per_em;

    let mut source = Vec::new();
    let mut buffer = UnicodeBuffer::new();
    buffer.set_cluster_level(BufferClusterLevel::Characters);
    buffer.set_direction(Direction::LeftToRight);
    let mut cell = 0_u16;
    let mut grapheme = Vec::new();
    let mut chars = text.chars().enumerate().peekable();
    while let Some((index, ch)) = chars.next() {
        buffer.add(ch, u32::try_from(index).ok()?);
        if is_attached_codepoint(ch) {
            // A mark with no preceding base (the run starts mid-grapheme): attach to the
            // previous cell rather than starting a new one.
            source.push(SourceCodepoint {
                cell: cell.saturating_sub(1),
                starts_cell: false,
            });
            continue;
        }
        let base_cell = cell;
        source.push(SourceCodepoint {
            cell: base_cell,
            starts_cell: true,
        });
        grapheme.clear();
        grapheme.push(ch);
        while let Some(&(next_index, next)) = chars.peek() {
            if !is_attached_codepoint(next) {
                break;
            }
            buffer.add(next, u32::try_from(next_index).ok()?);
            source.push(SourceCodepoint {
                cell: base_cell,
                starts_cell: false,
            });
            grapheme.push(next);
            chars.next();
        }
        // Advance by the whole grapheme's grid width (base + attached marks), matching
        // libghostty: a VS16 emoji presentation sequence (⚠️) is one cell, not two.
        cell = cell.saturating_add(crate::terminal_text::terminal_grapheme_cells(&grapheme));
    }
    buffer.guess_segment_properties();

    let shaped = rustybuzz::shape(&face, features, buffer);
    let infos = shaped.glyph_infos();
    let positions = shaped.glyph_positions();
    if infos.len() != positions.len() {
        return None;
    }

    let mut run_offset_x = 0.0_f32;
    let mut run_offset_y = 0.0_f32;
    let mut run_offset_cell = 0_u16;
    let mut cell_offset_cell = 0_u16;
    let mut cell_offset_x = 0.0_f32;
    let mut glyphs = Vec::with_capacity(infos.len());

    for (info, position) in infos.iter().zip(positions) {
        let source_index = usize::try_from(info.cluster).ok()?;
        let codepoint = source.get(source_index)?;
        let glyph_cell = codepoint.cell;
        if cell_offset_cell != glyph_cell {
            let is_after_glyph_from_current_or_next_clusters = glyph_cell <= run_offset_cell;
            if codepoint.starts_cell && !is_after_glyph_from_current_or_next_clusters {
                cell_offset_cell = glyph_cell;
                cell_offset_x = run_offset_x;
            }
        }

        glyphs.push(ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            cluster: u32::from(cell_offset_cell),
            x_offset: run_offset_x - cell_offset_x + position.x_offset as f32 * scale,
            y_offset: run_offset_y + position.y_offset as f32 * scale,
        });

        run_offset_x += position.x_advance as f32 * scale;
        run_offset_y += position.y_advance as f32 * scale;
        run_offset_cell = run_offset_cell.max(glyph_cell);
    }

    Some(glyphs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SourceCodepoint {
    cell: u16,
    starts_cell: bool,
}

fn is_attached_codepoint(ch: char) -> bool {
    is_combining_mark(ch) || is_variation_selector(ch)
}

/// Translates the user's [`FontFeature`] list into HarfBuzz features. The
/// `liga` setting acts as the single "ligatures" knob: disabling it also
/// disables the contextual/common ligature features that HarfBuzz would
/// otherwise apply by default, so `liga=0` turns ligatures off as a user
/// expects.
pub(super) fn harfbuzz_features(features: &[FontFeature]) -> Vec<Feature> {
    let mut out: Vec<Feature> = features
        .iter()
        .map(|feature| Feature::new(Tag::from_bytes(&feature.tag()), feature.value(), ..))
        .collect();
    if !ligatures_enabled(features) {
        for tag in [b"calt", b"clig", b"liga", b"rlig", b"dlig"] {
            out.push(Feature::new(Tag::from_bytes(tag), 0, ..));
        }
    }
    out
}

/// Whether the font's GSUB table advertises any feature that can substitute or
/// merge glyphs in horizontal text. Fonts without these (e.g. Menlo, SF Mono)
/// keep the cheaper per-character render paths.
pub(super) fn font_has_ligature_features(font_data: &[u8], face_index: u32) -> bool {
    let Some(face) = Face::from_slice(font_data, face_index) else {
        return false;
    };
    let Some(gsub) = face.tables().gsub else {
        return false;
    };
    gsub.features.into_iter().any(|feature| {
        matches!(
            &feature.tag.to_bytes(),
            b"liga" | b"clig" | b"calt" | b"rlig" | b"dlig"
        )
    })
}

fn ligatures_enabled(features: &[FontFeature]) -> bool {
    features
        .iter()
        .rev()
        .find(|feature| feature.tag() == *b"liga")
        .is_none_or(|feature| feature.value() != 0)
}
