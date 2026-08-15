use smallvec::SmallVec;

use crate::{terminal_font_face::is_symbol_codepoint, terminal_text::terminal_grapheme_cells};

/// One glyph produced by shaping a text run. Glyph ids index the same font face
/// that ab_glyph loads from the identical bytes, so they can be rasterized
/// directly via [`ab_glyph::GlyphId`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ShapedGlyph {
    pub glyph_id: u16,
    /// Cell-relative origin chosen by the Ghostty-compatible shaper. Glyphs
    /// that belong to the same ligature can share this origin even when
    /// HarfBuzz reports different source clusters.
    pub cluster: u32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalTextShaper;

impl TerminalTextShaper {
    pub fn shape(&self, text: &str, start_cell: u16) -> Vec<ShapedCluster> {
        let mut clusters = Vec::with_capacity(text.chars().count().max(1));
        self.shape_into(text, start_cell, &mut clusters);
        clusters
    }

    pub fn shape_into(
        &self,
        text: &str,
        start_cell: u16,
        clusters: &mut Vec<ShapedCluster>,
    ) -> u16 {
        let (total_cells, cluster_len) = self.shape_into_retained(text, start_cell, clusters);
        clusters.truncate(cluster_len);
        total_cells
    }

    pub(super) fn shape_into_retained(
        &self,
        text: &str,
        start_cell: u16,
        clusters: &mut Vec<ShapedCluster>,
    ) -> (u16, usize) {
        if is_printable_ascii(text) {
            return shape_ascii_into_retained(text, start_cell, clusters);
        }

        let mut cell = start_cell;
        let mut total_cells = 0_u16;
        let mut chars = text.chars().peekable();
        let mut cluster_index = 0;
        let mut grapheme = Vec::new();
        while let Some(ch) = chars.next() {
            let cluster = shaped_cluster_slot(clusters, cluster_index);
            cluster.text.clear();
            cluster.glyphs.clear();
            cluster.text.push(ch);
            cluster.cell = cell;
            cluster.is_whitespace = ch.is_whitespace();
            grapheme.clear();
            grapheme.push(ch);
            while let Some(next) = chars.peek().copied() {
                if is_combining_mark(next) || is_variation_selector(next) {
                    cluster.text.push(next);
                    cluster.is_whitespace &= next.is_whitespace();
                    grapheme.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            // Width comes from the whole grapheme, not the base char alone: a VS16 emoji
            // presentation sequence (⚠️) is two cells even though its base measures as one.
            cluster.cells = terminal_grapheme_cells(&grapheme);
            total_cells = total_cells.saturating_add(cluster.cells);
            cell = cell.saturating_add(cluster.cells);
            cluster_index += 1;
        }
        (total_cells.max(1), cluster_index)
    }
}

pub(super) fn is_printable_ascii(text: &str) -> bool {
    text.bytes().all(|byte| matches!(byte, b' '..=b'~'))
}

fn shape_ascii_into_retained(
    text: &str,
    start_cell: u16,
    clusters: &mut Vec<ShapedCluster>,
) -> (u16, usize) {
    let mut cell = start_cell;
    let mut cluster_index = 0;
    for byte in text.bytes() {
        let cluster = shaped_cluster_slot(clusters, cluster_index);
        cluster.text.clear();
        cluster.glyphs.clear();
        cluster.text.push(char::from(byte));
        cluster.cell = cell;
        cluster.cells = 1;
        cluster.is_whitespace = byte == b' ';
        cell = cell.saturating_add(1);
        cluster_index += 1;
    }
    (
        u16::try_from(text.len()).unwrap_or(u16::MAX).max(1),
        cluster_index,
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapedCluster {
    pub text: String,
    pub cell: u16,
    pub cells: u16,
    pub is_whitespace: bool,
    /// Glyphs to rasterize by id when the font shaped this cluster into
    /// ligatures or contextual alternates. Empty means render the cluster
    /// through the per-character path (the common case, and all fallback,
    /// emoji, symbol, and combining-mark handling).
    pub(crate) glyphs: SmallVec<[ShapedGlyph; 2]>,
}

pub(super) fn shaped_cluster_slot(
    clusters: &mut Vec<ShapedCluster>,
    index: usize,
) -> &mut ShapedCluster {
    if index == clusters.len() {
        clusters.push(ShapedCluster {
            text: String::new(),
            cell: 0,
            cells: 0,
            is_whitespace: false,
            glyphs: smallvec::SmallVec::new(),
        });
    }
    &mut clusters[index]
}

pub(super) fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

pub(super) fn is_variation_selector(ch: char) -> bool {
    matches!(ch as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

pub(super) fn is_default_emoji_presentation(ch: char) -> bool {
    matches!(
        ch as u32,
        0x231A..=0x231B | 0x23E9..=0x23EC | 0x23F0 | 0x23F3 | 0x25FD..=0x25FE
            | 0x2614..=0x2615 | 0x2648..=0x2653 | 0x267F | 0x2693 | 0x26A1
            | 0x26AA..=0x26AB | 0x26BD..=0x26BE | 0x26C4..=0x26C5 | 0x26CE
            | 0x26D4 | 0x26EA | 0x26F2..=0x26F3 | 0x26F5 | 0x26FA | 0x26FD
            | 0x2705 | 0x270A..=0x270B | 0x2728 | 0x274C | 0x274E
            | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797 | 0x27B0 | 0x27BF
            | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55 | 0x1F004 | 0x1F0CF | 0x1F18E
            | 0x1F191..=0x1F19A | 0x1F1E6..=0x1F1FF | 0x1F201 | 0x1F21A | 0x1F22F
            | 0x1F232..=0x1F236 | 0x1F238..=0x1F23A | 0x1F250..=0x1F251
            | 0x1F300..=0x1F320 | 0x1F32D..=0x1F335 | 0x1F337..=0x1F37C
            | 0x1F37E..=0x1F393 | 0x1F3A0..=0x1F3CA | 0x1F3CF..=0x1F3D3
            | 0x1F3E0..=0x1F3F0 | 0x1F3F4 | 0x1F3F8..=0x1F43E | 0x1F440
            | 0x1F442..=0x1F4FC | 0x1F4FF..=0x1F53D | 0x1F54B..=0x1F54E
            | 0x1F550..=0x1F567 | 0x1F57A | 0x1F595..=0x1F596 | 0x1F5A4
            | 0x1F5FB..=0x1F64F | 0x1F680..=0x1F6C5 | 0x1F6CC | 0x1F6D0..=0x1F6D2
            | 0x1F6D5..=0x1F6D7 | 0x1F6DC..=0x1F6DF | 0x1F6EB..=0x1F6EC
            | 0x1F6F4..=0x1F6FC | 0x1F7E0..=0x1F7EB | 0x1F7F0 | 0x1F90C..=0x1F93A
            | 0x1F93C..=0x1F945 | 0x1F947..=0x1F9FF | 0x1FA70..=0x1FA7C
            | 0x1FA80..=0x1FA89 | 0x1FA8F..=0x1FAC6 | 0x1FACE..=0x1FADC
            | 0x1FADF..=0x1FAE9 | 0x1FAF0..=0x1FAF8
    )
}

pub(super) fn is_private_use(ch: char) -> bool {
    matches!(
        ch as u32,
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

pub(super) fn is_symbol_like(ch: char) -> bool {
    is_symbol_codepoint(ch as u32)
}

pub(super) fn is_terminal_graphics_symbol(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x259F | 0x1CC00..=0x1CEBF | 0x1FB00..=0x1FBFF | 0xE0B0..=0xE0D7
    )
}

pub(super) fn is_symbol_space(ch: char) -> bool {
    matches!(ch as u32, 0x0020 | 0x2002)
}

pub(super) fn single_ascii_cluster(cluster: &ShapedCluster) -> Option<u8> {
    let bytes = cluster.text.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii()).then_some(bytes[0])
}

#[cfg(windows)]
pub(super) fn windows_gdi_candidate(cluster: &ShapedCluster) -> bool {
    single_ascii_cluster(cluster).is_some_and(|ch| ch.is_ascii_graphic())
}

pub(super) fn is_color_emoji_cluster(cluster: &ShapedCluster) -> bool {
    if cluster.text.contains('\u{fe0e}') {
        return false;
    }
    cluster
        .text
        .chars()
        .any(|ch| ch == '\u{fe0f}' || is_default_emoji_presentation(ch))
}

pub(super) fn cluster_constraint_cells(
    previous: Option<&ShapedCluster>,
    cluster: &ShapedCluster,
    next: Option<&ShapedCluster>,
) -> u16 {
    if cluster.cells > 1 {
        return cluster.cells;
    }
    // A color emoji fills exactly its grid cells. The neighbor-based widening below is for lone
    // monochrome symbols (arrows, shapes) that read better spanning two cells; applied to an emoji
    // it spills the glyph into the next column — eating a following space and making the rendered
    // width flip with whatever character happens to come after it.
    if is_color_emoji_cluster(cluster) {
        return cluster.cells;
    }
    let Some(ch) = cluster.text.chars().next() else {
        return cluster.cells;
    };
    if is_terminal_graphics_symbol(ch) {
        return 1;
    }
    if !is_symbol_like(ch) {
        return cluster.cells;
    }
    if previous
        .and_then(|previous| previous.text.chars().next())
        .is_some_and(|previous| is_symbol_like(previous) && !is_terminal_graphics_symbol(previous))
    {
        return 1;
    }
    if next
        .and_then(|next| next.text.chars().next())
        .is_none_or(is_symbol_space)
    {
        2
    } else {
        cluster.cells
    }
}
