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

/// Shapes `text` with the primary font and emits cell-aligned clusters,
/// attaching shaped glyph ids to ligature/contextual clusters. Returns
/// `None` when the font has no ligature features, so the caller can keep the
/// cheaper per-character paths.
pub(super) fn shape_clusters(
    database: &fontdb::Database,
    id: fontdb::ID,
    font: &FontArc,
    text: &str,
    font_size: f32,
    features: &[FontFeature],
    clusters: &mut Vec<ShapedCluster>,
) -> Option<(u16, usize)> {
    let hb_features = harfbuzz_features(features);
    let glyphs = database
        .with_face_data(id, |data, index| {
            shape_run(data, index, text, font_size, &hb_features)
        })
        .flatten()?;
    let total_cells = text_cell_count(text);
    let mut cluster_index = 0;
    let mut glyph_index = 0;
    while glyph_index < glyphs.len() {
        let group = shaped_glyph_group(text, &glyphs, glyph_index, total_cells, font);
        let mut glyph_end = group.end;
        let mut cells = group.cells;
        let mut source_end = group.source_end;
        let draw_by_glyph = group.draw_by_glyph;

        if draw_by_glyph {
            while glyph_end < glyphs.len() {
                let next_group = shaped_glyph_group(text, &glyphs, glyph_end, total_cells, font);
                if !next_group.draw_by_glyph || next_group.cell != group.cell.saturating_add(cells)
                {
                    break;
                }
                glyph_end = next_group.end;
                cells = cells.saturating_add(next_group.cells);
                source_end = next_group.source_end;
            }
        }

        let slice = &text[group.source_start..source_end];
        let cluster = shaped_cluster_slot(clusters, cluster_index);
        cluster.text.clear();
        cluster.glyphs.clear();
        cluster.text.push_str(slice);
        cluster.cell = group.cell;
        cluster.is_whitespace = slice.chars().all(char::is_whitespace);
        cluster.cells = cells;
        if draw_by_glyph {
            cluster
                .glyphs
                .extend(glyphs[glyph_index..glyph_end].iter().copied());
        }
        cluster_index += 1;
        glyph_index = glyph_end;
    }
    Some((total_cells.max(1), cluster_index))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShapedGlyphGroup {
    cell: u16,
    cells: u16,
    source_start: usize,
    source_end: usize,
    end: usize,
    draw_by_glyph: bool,
}

fn shaped_glyph_group(
    text: &str,
    glyphs: &[ShapedGlyph],
    start: usize,
    total_cells: u16,
    font: &FontArc,
) -> ShapedGlyphGroup {
    let cell = glyphs[start].cluster;
    let mut end = start + 1;
    while end < glyphs.len() && glyphs[end].cluster == cell {
        end += 1;
    }

    let cell = u16::try_from(cell).unwrap_or(u16::MAX);
    let next_cell = glyphs.get(end).map_or(total_cells, |glyph| {
        u16::try_from(glyph.cluster).unwrap_or(u16::MAX)
    });
    let cells = next_cell.saturating_sub(cell).max(1);
    let (source_start, source_end) =
        text_byte_range_for_cells(text, cell, cell.saturating_add(cells));
    let slice = &text[source_start..source_end];

    ShapedGlyphGroup {
        cell,
        cells,
        source_start,
        source_end,
        end,
        draw_by_glyph: draw_span_by_glyph(slice, &glyphs[start..end], font),
    }
}

// Cell counting and cell→byte mapping group base + attached marks into one grapheme and advance
// by its full width, matching `shape_run`. A VS16 emoji presentation sequence spans two cells, so
// per-char counting (which sees the base as one and the selector as zero) would desync the cluster
// slices from the shaped cell positions.
fn text_cell_count(text: &str) -> u16 {
    let mut total = 0_u16;
    let mut chars = text.chars().peekable();
    let mut grapheme = Vec::new();
    while let Some(ch) = chars.next() {
        if is_combining_mark(ch) || is_variation_selector(ch) {
            continue;
        }
        grapheme.clear();
        grapheme.push(ch);
        while let Some(&next) = chars.peek() {
            if is_combining_mark(next) || is_variation_selector(next) {
                grapheme.push(next);
                chars.next();
            } else {
                break;
            }
        }
        total = total.saturating_add(terminal_grapheme_cells(&grapheme));
    }
    total
}

fn text_byte_range_for_cells(text: &str, start: u16, end: u16) -> (usize, usize) {
    let mut cell = 0_u16;
    let mut range_start = None;
    let mut range_end = None;
    let mut chars = text.char_indices().peekable();
    let mut grapheme = Vec::new();

    while let Some((byte_start, ch)) = chars.next() {
        let mut byte_end = byte_start + ch.len_utf8();
        if is_combining_mark(ch) || is_variation_selector(ch) {
            // Standalone mark (run starts mid-grapheme): belongs to the previous cell.
            let mark_cell = cell.saturating_sub(1);
            if mark_cell < end && mark_cell.saturating_add(1) > start {
                range_start.get_or_insert(byte_start);
                range_end = Some(byte_end);
            }
            continue;
        }

        grapheme.clear();
        grapheme.push(ch);
        while let Some(&(next_byte, next)) = chars.peek() {
            if is_combining_mark(next) || is_variation_selector(next) {
                byte_end = next_byte + next.len_utf8();
                grapheme.push(next);
                chars.next();
            } else {
                break;
            }
        }

        let grapheme_end = cell.saturating_add(terminal_grapheme_cells(&grapheme));
        if cell < end && grapheme_end > start {
            range_start.get_or_insert(byte_start);
            range_end = Some(byte_end);
        }
        cell = grapheme_end;
    }

    (range_start.unwrap_or(0), range_end.unwrap_or(text.len()))
}

/// Whether a shaped span should be drawn directly from its glyph ids rather than
/// the per-character path. True only for genuine font output the per-character
/// path cannot reproduce: a ligature (several source cells shaped together) or a
/// single character the font swapped for a contextual alternate. Everything with
/// dedicated handling (whitespace, private-use icons, symbols/box-drawing, emoji,
/// combining marks, or any uncovered `.notdef` glyph) stays on the legacy path.
fn draw_span_by_glyph(slice: &str, glyphs: &[ShapedGlyph], font: &FontArc) -> bool {
    if glyphs.is_empty() || glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
        return false;
    }
    if slice.chars().any(|ch| {
        ch.is_whitespace()
            || is_private_use(ch)
            || is_symbol_like(ch)
            || is_combining_mark(ch)
            || is_variation_selector(ch)
            || ch == '\u{fe0f}'
            || is_default_emoji_presentation(ch)
    }) {
        return false;
    }
    let width_chars = slice
        .chars()
        .filter(|ch| terminal_char_width(*ch) >= 1)
        .count();
    if width_chars >= 2 {
        return true;
    }
    width_chars == 1
        && glyphs.len() == 1
        && slice
            .chars()
            .next()
            .is_some_and(|ch| GlyphId(glyphs[0].glyph_id) != font.glyph_id(ch))
}
