use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    sync::Arc,
};

use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont, point};
use smallvec::SmallVec;

mod coretext;
mod shaping;
#[cfg(windows)]
mod windows_gdi;

use shaping::{ShapedGlyph, font_has_ligature_features, harfbuzz_features, shape_run};

use crate::{
    font_database::system_font_database,
    geometry::SurfaceRect,
    paint_plan::PlanColor,
    terminal_font_face::{
        FontFaceMetrics, GlyphSize, is_symbol_codepoint, terminal_glyph_constraint,
    },
    terminal_render::{SpriteCommandBatch, TextCommand},
    terminal_sprite::SpriteCommand,
    terminal_text::{
        FontFeature, FontStyle, ResolvedFontFace, default_font_features, terminal_char_width,
        terminal_grapheme_cells,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalTextShaper {
    font_features: Vec<FontFeature>,
}

impl Default for TerminalTextShaper {
    fn default() -> Self {
        Self {
            font_features: default_font_features(),
        }
    }
}

impl TerminalTextShaper {
    pub fn with_features(font_features: Vec<FontFeature>) -> Self {
        Self { font_features }
    }

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

    fn shape_into_retained(
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

fn is_printable_ascii(text: &str) -> bool {
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

fn shaped_cluster_slot(clusters: &mut Vec<ShapedCluster>, index: usize) -> &mut ShapedCluster {
    if index == clusters.len() {
        clusters.push(ShapedCluster {
            text: String::new(),
            cell: 0,
            cells: 0,
            is_whitespace: false,
            glyphs: SmallVec::new(),
        });
    }
    &mut clusters[index]
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphAtlasKey {
    pub face: GlyphAtlasFaceKey,
    pub text: GlyphAtlasTextKey,
    pub font_size_bits: u32,
    pub pixels_per_point_bits: u32,
    pub width: u32,
    pub height: u32,
}

impl Hash for GlyphAtlasKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut hash = self.face.hash ^ self.text.hash.rotate_left(13);
        hash ^= u64::from(self.font_size_bits).rotate_left(29);
        hash ^= u64::from(self.pixels_per_point_bits).rotate_left(43);
        hash ^= u64::from(self.width) << 32 | u64::from(self.height);
        state.write_u64(hash);
    }
}

#[derive(Clone, Debug)]
pub struct GlyphAtlasTextKey {
    text: Arc<str>,
    hash: u64,
}

impl GlyphAtlasTextKey {
    pub fn new(text: impl AsRef<str>) -> Self {
        let text = text.as_ref();
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Self {
            text: Arc::from(text),
            hash: hasher.finish(),
        }
    }

    fn for_char(ch: char) -> Self {
        let mut buffer = [0_u8; 4];
        Self::new(ch.encode_utf8(&mut buffer))
    }
}

impl PartialEq for GlyphAtlasTextKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && self.text == other.text
    }
}

impl Eq for GlyphAtlasTextKey {}

impl Hash for GlyphAtlasTextKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

#[derive(Clone, Debug)]
pub struct GlyphAtlasFaceKey {
    face: Arc<ResolvedFontFace>,
    hash: u64,
}

impl GlyphAtlasFaceKey {
    pub fn new(face: ResolvedFontFace) -> Self {
        let mut hasher = DefaultHasher::new();
        face.hash(&mut hasher);
        Self {
            face: Arc::new(face),
            hash: hasher.finish(),
        }
    }
}

impl PartialEq for GlyphAtlasFaceKey {
    fn eq(&self, other: &Self) -> bool {
        self.hash == other.hash && (Arc::ptr_eq(&self.face, &other.face) || self.face == other.face)
    }
}

impl Eq for GlyphAtlasFaceKey {}

impl Hash for GlyphAtlasFaceKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphAtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct GlyphAtlasRecord {
    entry: GlyphAtlasEntry,
    is_color_glyph: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphAtlasFormat {
    Alpha,
    Bgr,
    Rgba,
}

impl GlyphAtlasFormat {
    pub fn depth(self) -> u32 {
        match self {
            Self::Alpha => 1,
            Self::Bgr => 3,
            Self::Rgba => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphAtlasError {
    CapacityExceeded,
}

#[derive(Clone, Debug)]
pub struct GlyphAtlas {
    width: u32,
    height: u32,
    format: GlyphAtlasFormat,
    allocations: Vec<GlyphAtlasEntry>,
    entries: HashMap<GlyphAtlasKey, GlyphAtlasRecord>,
    pixels: Vec<u8>,
    modified: u64,
    dirty_regions: Vec<(u64, GlyphAtlasEntry)>,
    next_x: u32,
    next_y: u32,
    row_height: u32,
    resized: u64,
    // Smallest footprint that the gap scan most recently failed to place. Since the atlas only
    // gains space by growing (it never evicts), any later request at least this large is doomed
    // too, so we skip the scan until the next grow clears this. Without it, a saturated atlas
    // re-scans the whole surface for every new glyph and freezes the render thread.
    no_fit_at_least: Option<(u32, u32)>,
}

// Upper bound on atlas growth, well under every backend's max texture dimension.
const MAX_ATLAS_DIM: u32 = 4096;

impl GlyphAtlas {
    pub fn new(width: u32, height: u32) -> Self {
        Self::with_format(width, height, GlyphAtlasFormat::Alpha)
    }

    pub fn with_format(width: u32, height: u32, format: GlyphAtlasFormat) -> Self {
        Self::try_with_format(width, height, format, usize::MAX).expect("unlimited atlas")
    }

    pub fn try_with_format(
        width: u32,
        height: u32,
        format: GlyphAtlasFormat,
        byte_limit: usize,
    ) -> Result<Self, GlyphAtlasError> {
        let width = width.max(1);
        let height = height.max(1);
        let depth = format.depth();
        let byte_len = atlas_byte_len(width, height, depth)?;
        if byte_len > byte_limit {
            return Err(GlyphAtlasError::CapacityExceeded);
        }
        Ok(Self {
            width,
            height,
            format,
            allocations: Vec::new(),
            entries: HashMap::new(),
            pixels: vec![0; byte_len],
            next_x: 1,
            next_y: 1,
            row_height: 0,
            modified: 0,
            dirty_regions: Vec::new(),
            resized: 0,
            no_fit_at_least: None,
        })
    }

    pub fn insert_or_get(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        alpha: Vec<u8>,
    ) -> GlyphAtlasEntry {
        self.insert_or_get_with(key, width, height, || alpha)
    }

    pub fn insert_or_get_with(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        pixels: impl FnOnce() -> Vec<u8>,
    ) -> GlyphAtlasEntry {
        self.insert_or_get_with_color(key, width, height, || (pixels(), false))
            .0
    }

    fn insert_or_get_with_color(
        &mut self,
        key: GlyphAtlasKey,
        width: u32,
        height: u32,
        pixels: impl FnOnce() -> (Vec<u8>, bool),
    ) -> (GlyphAtlasEntry, bool) {
        if let Some(record) = self.entries.get(&key) {
            return (record.entry, record.is_color_glyph);
        }

        let width = width.max(1);
        let height = height.max(1);
        // Grow rather than drop: the atlas never evicts, so a glyph that no longer fits (common once
        // zoomed glyphs are supersampled) would otherwise be lost to the 1x1 fallback below.
        let mut reserved = self.reserve(width, height);
        while reserved.is_none() && self.grow_for_glyph(width, height) {
            reserved = self.reserve(width, height);
        }
        if reserved.is_none() && width + 2 <= self.width && height + 2 <= self.height {
            self.recycle();
            reserved = self.reserve(width, height);
        }
        let mut entry = reserved.unwrap_or(GlyphAtlasEntry {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });
        if entry.width != width || entry.height != height {
            entry.width = entry.width.min(width);
            entry.height = entry.height.min(height);
        }
        let (pixels, is_color_glyph) = pixels();
        self.set(entry, &pixels);
        self.entries.insert(
            key,
            GlyphAtlasRecord {
                entry,
                is_color_glyph,
            },
        );
        (entry, is_color_glyph)
    }

    pub fn get(&self, key: &GlyphAtlasKey) -> Option<GlyphAtlasEntry> {
        self.entries.get(key).map(|record| record.entry)
    }

    // Enlarge the atlas to make room for a glyph that did not fit. Returns false at the size cap.
    fn grow_for_glyph(&mut self, width: u32, height: u32) -> bool {
        let target_width = if width + 2 > self.width {
            (width + 2).max(self.width)
        } else {
            self.width
        }
        .min(MAX_ATLAS_DIM);
        // A glyph that fits dimensionally but still failed to reserve means the shelves are full,
        // so add height; otherwise grow to fit the oversized glyph itself.
        let target_height = if height + 2 > self.height {
            (height + 2).max(self.height)
        } else {
            self.height.saturating_mul(2)
        }
        .min(MAX_ATLAS_DIM);
        if target_width == self.width && target_height == self.height {
            return false;
        }
        self.grow(target_width, target_height);
        true
    }

    pub fn reserve(&mut self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let width = width.max(1);
        let height = height.max(1);
        if width + 2 > self.width || height + 2 > self.height {
            return None;
        }
        if let Some((fw, fh)) = self.no_fit_at_least
            && width >= fw
            && height >= fh
        {
            return None;
        }

        if let Some(entry) = self.reserve_next_shelf_slot(width, height) {
            return Some(entry);
        }

        match self.first_fit_in_gaps(width, height) {
            Some(entry) => {
                self.allocations.push(entry);
                Some(entry)
            }
            None => {
                self.no_fit_at_least = Some((width, height));
                None
            }
        }
    }

    // Top-left first-fit over the surface, identical in result to scanning every pixel but
    // visiting only positions flush against the surface edges or the edges of existing
    // allocations. The optimal first-fit corner always lands on one of these, so the candidate
    // grid is O(allocations) per axis instead of O(width * height).
    fn first_fit_in_gaps(&self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let max_x = (self.width - 1).saturating_sub(width);
        let max_y = (self.height - 1).saturating_sub(height);

        let mut candidate_xs: Vec<u32> = core::iter::once(1)
            .chain(self.allocations.iter().map(|used| used.x + used.width))
            .filter(|&x| (1..=max_x).contains(&x))
            .collect();
        candidate_xs.sort_unstable();
        candidate_xs.dedup();

        let mut candidate_ys: Vec<u32> = core::iter::once(1)
            .chain(self.allocations.iter().map(|used| used.y + used.height))
            .filter(|&y| (1..=max_y).contains(&y))
            .collect();
        candidate_ys.sort_unstable();
        candidate_ys.dedup();

        for &y in &candidate_ys {
            for &x in &candidate_xs {
                let entry = GlyphAtlasEntry {
                    x,
                    y,
                    width,
                    height,
                };
                if self
                    .allocations
                    .iter()
                    .all(|used| !rects_overlap(*used, entry))
                {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn reserve_next_shelf_slot(&mut self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let usable_right = self.width - 1;
        let usable_bottom = self.height - 1;
        let max_x = usable_right.saturating_sub(width);
        let max_y = usable_bottom.saturating_sub(height);

        if self.next_x > max_x {
            self.next_x = 1;
            self.next_y = self.next_y.saturating_add(self.row_height.max(1));
            self.row_height = 0;
        }
        if self.next_y > max_y {
            return None;
        }

        let entry = GlyphAtlasEntry {
            x: self.next_x,
            y: self.next_y,
            width,
            height,
        };
        if self
            .allocations
            .iter()
            .any(|used| rects_overlap(*used, entry))
        {
            return None;
        }

        self.allocations.push(entry);
        self.next_x = self.next_x.saturating_add(width).saturating_add(1);
        self.row_height = self.row_height.max(height.saturating_add(1));
        Some(entry)
    }

    pub fn set(&mut self, entry: GlyphAtlasEntry, alpha: &[u8]) {
        blit_pixels(
            &mut self.pixels,
            self.width,
            self.format.depth(),
            entry,
            alpha,
        );
        self.modified = self.modified.saturating_add(1);
        self.dirty_regions.push((self.modified, entry));
    }

    pub fn set_from_larger(
        &mut self,
        entry: GlyphAtlasEntry,
        alpha: &[u8],
        source_width: u32,
        source_x: u32,
        source_y: u32,
    ) {
        blit_pixels_from_source(
            &mut self.pixels,
            BlitTarget {
                atlas_width: self.width,
                depth: self.format.depth(),
                entry,
            },
            BlitSource {
                pixels: alpha,
                width: source_width,
                x: source_x,
                y: source_y,
            },
        );
        self.modified = self.modified.saturating_add(1);
        self.dirty_regions.push((self.modified, entry));
    }

    pub fn grow(&mut self, width: u32, height: u32) {
        self.try_grow_with_byte_limit(width, height, usize::MAX)
            .expect("unlimited atlas grow");
    }

    pub fn try_grow_with_byte_limit(
        &mut self,
        width: u32,
        height: u32,
        byte_limit: usize,
    ) -> Result<(), GlyphAtlasError> {
        let width = width.max(self.width);
        let height = height.max(self.height);
        if width == self.width && height == self.height {
            return Ok(());
        }

        let depth = self.format.depth();
        let byte_len = atlas_byte_len(width, height, depth)?;
        if byte_len > byte_limit {
            return Err(GlyphAtlasError::CapacityExceeded);
        }

        let mut pixels = vec![0; byte_len];
        for y in 0..self.height {
            let old_start = (y * self.width * depth) as usize;
            let old_end = old_start + (self.width * depth) as usize;
            let new_start = (y * width * depth) as usize;
            let new_end = new_start + (self.width * depth) as usize;
            pixels[new_start..new_end].copy_from_slice(&self.pixels[old_start..old_end]);
        }
        self.width = width;
        self.height = height;
        self.pixels = pixels;
        self.modified = self.modified.saturating_add(1);
        self.resized = self.resized.saturating_add(1);
        // The larger surface may now hold rects that previously failed the gap scan.
        self.no_fit_at_least = None;
        Ok(())
    }

    fn recycle(&mut self) {
        self.allocations.clear();
        self.entries.clear();
        self.pixels.fill(0);
        self.dirty_regions.clear();
        self.next_x = 1;
        self.next_y = 1;
        self.row_height = 0;
        self.no_fit_at_least = None;
        self.modified = self.modified.saturating_add(1);
        // Same-size recycle still invalidates every cached UV and GPU texture.
        self.resized = self.resized.saturating_add(1);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn atlas_pixel(&self, x: u32, y: u32) -> Option<u8> {
        self.atlas_pixel_channel(x, y, 0)
    }

    pub fn atlas_pixel_channel(&self, x: u32, y: u32, channel: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let depth = self.format.depth();
        if channel >= depth {
            return None;
        }
        self.pixels
            .get(((y * self.width + x) * depth + channel) as usize)
            .copied()
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn modified_count(&self) -> u64 {
        self.modified
    }
    pub fn dirty_rect_since(&self, modified: u64) -> Option<GlyphAtlasEntry> {
        let start = self
            .dirty_regions
            .partition_point(|(version, _)| *version <= modified);
        self.dirty_regions[start..]
            .iter()
            .map(|(_, entry)| *entry)
            .reduce(union_atlas_entries)
    }

    pub fn resized_count(&self) -> u64 {
        self.resized
    }

    pub fn format(&self) -> GlyphAtlasFormat {
        self.format
    }
}

fn union_atlas_entries(left: GlyphAtlasEntry, right: GlyphAtlasEntry) -> GlyphAtlasEntry {
    let x = left.x.min(right.x);
    let y = left.y.min(right.y);
    let max_x = left
        .x
        .saturating_add(left.width)
        .max(right.x.saturating_add(right.width));
    let max_y = left
        .y
        .saturating_add(left.height)
        .max(right.y.saturating_add(right.height));
    GlyphAtlasEntry {
        x,
        y,
        width: max_x - x,
        height: max_y - y,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexturedGlyphQuad {
    pub rect: SurfaceRect,
    pub uv: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Debug)]
struct AsciiGlyphAtlasRecord {
    face: GlyphAtlasFaceKey,
    font_size_bits: u32,
    pixels_per_point_bits: u32,
    width: u32,
    height: u32,
    atlas_resized_count: u64,
    record: GlyphAtlasRecord,
}

struct ClusterGlyphRequest<'a> {
    command: &'a TextCommand,
    cluster: &'a ShapedCluster,
    face_key: GlyphAtlasFaceKey,
    pixels_per_point: f32,
    constraint_cells: u16,
    glyph_width: u32,
    glyph_height: u32,
}

#[derive(Clone, Debug)]
struct PreparedTextCommandCacheEntry {
    command: TextCommand,
    pixels_per_point_bits: u32,
    atlas_resized_count: u64,
    quads: Vec<TexturedGlyphQuad>,
}

/// Identity of a shaped run: shaping output depends only on the text, the resolved
/// face, and the font size, so a run that merely moved (scrolling) or reappeared keys
/// to the same entry. Position, color, and atlas state are deliberately excluded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapedRunCacheKey {
    text: String,
    face: GlyphAtlasFaceKey,
    font_size_bits: u32,
}

#[derive(Clone, Debug)]
struct ShapedRunCacheEntry {
    total_cells: u16,
    clusters: Vec<ShapedCluster>,
}

/// Bounds the shaped-run cache so unbounded unique output (e.g. streaming a huge log)
/// can't grow it without limit; the cache clears wholesale when the cap is hit. The
/// working set of interactive use and scrollback stays well under this.
const SHAPED_RUN_CACHE_CAP: usize = 1024;

#[derive(Clone, Debug)]
pub struct TextAtlasBuilder {
    shaper: TerminalTextShaper,
    atlas: GlyphAtlas,
    fonts: FontLibrary,
    face_cache: HashMap<ResolvedFontFace, GlyphAtlasFaceKey>,
    text_cache: HashMap<String, GlyphAtlasTextKey>,
    ascii_char_cache: [Option<GlyphAtlasTextKey>; 128],
    ascii_glyph_cache: [Option<AsciiGlyphAtlasRecord>; 128],
    char_cache: HashMap<char, GlyphAtlasTextKey>,
    sprite_face_key: GlyphAtlasFaceKey,
    clusters: Vec<ShapedCluster>,
    shaped_run_cache: HashMap<ShapedRunCacheKey, ShapedRunCacheEntry>,
    prepared_text_cache: Vec<PreparedTextCommandCacheEntry>,
    prepared_text_cache_cursor: usize,
    prepared_text_frame_active: bool,
}

impl TextAtlasBuilder {
    pub fn new(width: u32, height: u32) -> Self {
        Self::with_format(width, height, GlyphAtlasFormat::Alpha)
    }

    pub fn new_rgba(width: u32, height: u32) -> Self {
        Self::with_format(width, height, GlyphAtlasFormat::Rgba)
    }

    pub fn with_format(width: u32, height: u32, format: GlyphAtlasFormat) -> Self {
        Self {
            shaper: TerminalTextShaper::default(),
            atlas: GlyphAtlas::with_format(width, height, format),
            fonts: FontLibrary::new(),
            face_cache: HashMap::new(),
            text_cache: HashMap::new(),
            ascii_char_cache: std::array::from_fn(|_| None),
            ascii_glyph_cache: std::array::from_fn(|_| None),
            char_cache: HashMap::new(),
            sprite_face_key: GlyphAtlasFaceKey::new(ResolvedFontFace {
                family: "Ghostty Sprite".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Regular,
            }),
            clusters: Vec::new(),
            shaped_run_cache: HashMap::new(),
            prepared_text_cache: Vec::new(),
            prepared_text_cache_cursor: 0,
            prepared_text_frame_active: false,
        }
    }

    pub(crate) fn begin_text_frame(&mut self) {
        self.prepared_text_frame_active = true;
        self.prepared_text_cache_cursor = 0;
    }

    pub(crate) fn finish_text_frame(&mut self) {
        if self.prepared_text_frame_active {
            self.prepared_text_cache
                .truncate(self.prepared_text_cache_cursor);
            self.prepared_text_frame_active = false;
        }
    }
    pub(crate) fn reset_atlas_for_frame_rebuild(&mut self) {
        self.atlas.recycle();
    }

    pub fn prepare_text_command(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
    ) -> Vec<TexturedGlyphQuad> {
        let mut quads = Vec::new();
        self.prepare_text_command_into(command, pixels_per_point, &mut quads);
        quads
    }

    pub fn prepare_text_command_into(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) {
        self.prepare_text_command_into_frame(command, pixels_per_point, quads);
    }

    pub(crate) fn prepare_text_command_into_frame(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) -> bool {
        if self.prepared_text_frame_active {
            return self.prepare_text_command_into_cached(command, pixels_per_point, quads);
        }
        self.prepare_text_command_into_uncached(command, pixels_per_point, quads);
        true
    }

    fn prepare_text_command_into_cached(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) -> bool {
        let cache_index = self.prepared_text_cache_cursor;
        self.prepared_text_cache_cursor += 1;
        let pixels_per_point_bits = pixels_per_point.to_bits();
        let atlas_resized_count = self.atlas.resized_count();

        if let Some(cached) = self.prepared_text_cache.get(cache_index)
            && cached.atlas_resized_count == atlas_resized_count
            && cached.pixels_per_point_bits == pixels_per_point_bits
            && cached.command == *command
        {
            quads.extend_from_slice(&cached.quads);
            return false;
        }

        let start = quads.len();
        self.prepare_text_command_into_uncached(command, pixels_per_point, quads);
        let cached = PreparedTextCommandCacheEntry {
            command: command.clone(),
            pixels_per_point_bits,
            atlas_resized_count: self.atlas.resized_count(),
            quads: quads[start..].to_vec(),
        };
        if cache_index == self.prepared_text_cache.len() {
            self.prepared_text_cache.push(cached);
        } else {
            self.prepared_text_cache[cache_index] = cached;
        }
        true
    }

    fn prepare_text_command_into_uncached(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) {
        let face_key = self.intern_face(&command.face);
        self.prepare_text_command_into_uncached_with_face(
            command,
            pixels_per_point,
            face_key,
            quads,
        );
    }
    fn prepare_ascii_text_command_into_uncached_with_face(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        face_key: GlyphAtlasFaceKey,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) {
        let total_cells = u16::try_from(command.text.len()).unwrap_or(u16::MAX).max(1);
        let cell_width = command.rect.width() / f32::from(total_cells);
        quads.reserve(command.text.len());
        let mut cluster = ShapedCluster {
            text: String::new(),
            cell: 0,
            cells: 1,
            is_whitespace: false,
            glyphs: SmallVec::new(),
        };

        for (cell, ch) in command.text.bytes().enumerate() {
            if ch == b' ' {
                continue;
            }
            let cell = u16::try_from(cell).unwrap_or(u16::MAX);
            cluster.text.clear();
            cluster.text.push(char::from(ch));
            cluster.cell = cell;
            cluster.is_whitespace = false;
            let rect = SurfaceRect::from_min_size(
                command.rect.min_x + f32::from(cell) * cell_width,
                command.rect.min_y,
                cell_width,
                command.rect.height(),
            );
            let glyph_width = (rect.width() * pixels_per_point).ceil().max(1.0) as u32;
            let glyph_height = (rect.height() * pixels_per_point).ceil().max(1.0) as u32;
            let request = ClusterGlyphRequest {
                command,
                cluster: &cluster,
                face_key: face_key.clone(),
                pixels_per_point,
                constraint_cells: 1,
                glyph_width,
                glyph_height,
            };
            let (entry, is_color_glyph) = self.prepare_ascii_cluster(ch, request);
            let color = if is_color_glyph {
                PlanColor {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: command.attrs.fg.a,
                }
            } else {
                command.attrs.fg
            };
            quads.push(TexturedGlyphQuad {
                rect,
                uv: atlas_uv(self.atlas.size(), entry),
                color,
            });
        }
    }

    fn prepare_text_command_into_uncached_with_face(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
        face_key: GlyphAtlasFaceKey,
        quads: &mut Vec<TexturedGlyphQuad>,
    ) {
        let mut clusters = std::mem::take(&mut self.clusters);
        // Shaping depends only on (text, face, font_size), so memoize it: scrolled or
        // repeated text reuses the shape and skips rustybuzz. Positioning and
        // (atlas-cached) rasterization still run per command, so output is identical.
        let cache_key = ShapedRunCacheKey {
            text: command.text.clone(),
            face: face_key.clone(),
            font_size_bits: command.font_size.to_bits(),
        };
        let (total_cells, cluster_len) = if let Some(entry) = self.shaped_run_cache.get(&cache_key)
        {
            clusters.clear();
            clusters.extend_from_slice(&entry.clusters);
            (entry.total_cells, entry.clusters.len())
        } else {
            let features = self.shaper.font_features.clone();
            let shaped = self.fonts.shape_into_clusters(
                &command.face,
                &command.text,
                command.font_size,
                &features,
                &mut clusters,
            );
            match shaped {
                Some((total_cells, cluster_len)) => {
                    self.insert_shaped_run(cache_key, total_cells, &clusters[..cluster_len]);
                    (total_cells, cluster_len)
                }
                None => {
                    // The font carries no ligature/contextual features (or shaping is
                    // unavailable): keep the per-character fast paths unchanged.
                    if is_printable_ascii(&command.text) {
                        self.clusters = clusters;
                        self.prepare_ascii_text_command_into_uncached_with_face(
                            command,
                            pixels_per_point,
                            face_key,
                            quads,
                        );
                        return;
                    }
                    self.shaper
                        .shape_into_retained(&command.text, 0, &mut clusters)
                }
            }
        };
        let active_clusters = &clusters[..cluster_len];
        let cell_width = command.rect.width() / f32::from(total_cells);
        quads.reserve(active_clusters.len());

        for (index, cluster) in active_clusters.iter().enumerate() {
            if cluster.is_whitespace {
                continue;
            }
            let constraint_cells = cluster_constraint_cells(
                index
                    .checked_sub(1)
                    .and_then(|index| active_clusters.get(index)),
                cluster,
                active_clusters.get(index + 1),
            );
            let mut rect = SurfaceRect::from_min_size(
                command.rect.min_x + f32::from(cluster.cell) * cell_width,
                command.rect.min_y,
                f32::from(constraint_cells) * cell_width,
                command.rect.height(),
            );
            // Size a color emoji to the text em (font size), the way Ghostty draws it — matching the
            // visual weight of surrounding glyphs — rather than the width of its grid cells (which is
            // far thinner than the line in a narrow font, rendering the glyph tiny) or the full padded
            // cell height (which overshoots the text). Center the square over the grid span both ways;
            // the grid still reserves the cells for layout.
            if is_color_emoji_cluster(cluster) {
                let side = command.font_size.min(rect.height());
                let center_x = rect.min_x + rect.width() * 0.5;
                // Center on the text's cap band (top-of-ascent to baseline), not the padded cell
                // center — text sits low in the cell, so cell-centering reads slightly high. Derive
                // the baseline exactly as the text glyphs do, then take the midpoint of the ascent.
                let center_y = self
                    .fonts
                    .font_for_face(&command.face)
                    .map(|font| {
                        let scaled = font.as_scaled(PxScale::from(command.font_size.max(1.0)));
                        let baseline = (rect.height() - scaled.height()) * 0.5 + scaled.ascent();
                        rect.min_y + baseline - scaled.ascent() * 0.5
                    })
                    .unwrap_or(rect.min_y + rect.height() * 0.5);
                rect = SurfaceRect::from_min_size(
                    center_x - side * 0.5,
                    center_y - side * 0.5,
                    side,
                    side,
                );
            }
            let glyph_width = (rect.width() * pixels_per_point).ceil().max(1.0) as u32;
            let glyph_height = (rect.height() * pixels_per_point).ceil().max(1.0) as u32;
            let request = ClusterGlyphRequest {
                command,
                cluster,
                face_key: face_key.clone(),
                pixels_per_point,
                constraint_cells,
                glyph_width,
                glyph_height,
            };
            let (entry, is_color_glyph) = if let Some(ch) = single_ascii_cluster(cluster) {
                self.prepare_ascii_cluster(ch, request)
            } else {
                self.prepare_cluster(request)
            };
            let color = if is_color_glyph {
                PlanColor {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: command.attrs.fg.a,
                }
            } else {
                command.attrs.fg
            };
            quads.push(TexturedGlyphQuad {
                rect,
                uv: atlas_uv(self.atlas.size(), entry),
                color,
            });
        }
        self.clusters = clusters;
    }

    fn insert_shaped_run(
        &mut self,
        key: ShapedRunCacheKey,
        total_cells: u16,
        clusters: &[ShapedCluster],
    ) {
        if self.shaped_run_cache.len() >= SHAPED_RUN_CACHE_CAP {
            self.shaped_run_cache.clear();
        }
        self.shaped_run_cache.insert(
            key,
            ShapedRunCacheEntry {
                total_cells,
                clusters: clusters.to_vec(),
            },
        );
    }

    fn prepare_ascii_cluster(
        &mut self,
        ch: u8,
        request: ClusterGlyphRequest<'_>,
    ) -> (GlyphAtlasEntry, bool) {
        let font_size_bits = request.command.font_size.to_bits();
        let pixels_per_point_bits = request.pixels_per_point.to_bits();
        let cache_index = usize::from(ch);
        let atlas_resized_count = self.atlas.resized_count();
        if let Some(cached) = &self.ascii_glyph_cache[cache_index]
            && cached.face == request.face_key
            && cached.font_size_bits == font_size_bits
            && cached.pixels_per_point_bits == pixels_per_point_bits
            && cached.width == request.glyph_width
            && cached.height == request.glyph_height
            && cached.atlas_resized_count == atlas_resized_count
        {
            return (cached.record.entry, cached.record.is_color_glyph);
        }

        let face_key = request.face_key.clone();
        let width = request.glyph_width;
        let height = request.glyph_height;
        let (entry, is_color_glyph) = self.prepare_cluster(request);
        self.ascii_glyph_cache[cache_index] = Some(AsciiGlyphAtlasRecord {
            face: face_key,
            font_size_bits,
            pixels_per_point_bits,
            width,
            height,
            atlas_resized_count: self.atlas.resized_count(),
            record: GlyphAtlasRecord {
                entry,
                is_color_glyph,
            },
        });
        (entry, is_color_glyph)
    }

    fn prepare_cluster(&mut self, request: ClusterGlyphRequest<'_>) -> (GlyphAtlasEntry, bool) {
        let key = GlyphAtlasKey {
            face: request.face_key,
            text: self.intern_cluster_key(request.cluster),
            font_size_bits: request.command.font_size.to_bits(),
            pixels_per_point_bits: request.pixels_per_point.to_bits(),
            width: request.glyph_width,
            height: request.glyph_height,
        };
        let format = self.atlas.format();
        self.atlas
            .insert_or_get_with_color(key, request.glyph_width, request.glyph_height, || {
                let rasterized = self.fonts.rasterize_cluster(RasterizeClusterRequest {
                    face: &request.command.face,
                    cluster: request.cluster,
                    font_size: request.command.font_size,
                    pixels_per_point: request.pixels_per_point,
                    constraint_cells: request.constraint_cells,
                    tile: (request.glyph_width, request.glyph_height),
                    format,
                });
                (rasterized.pixels, rasterized.color)
            })
    }

    fn intern_text(&mut self, text: &str) -> GlyphAtlasTextKey {
        if let Some(cached) = self.text_cache.get(text) {
            return cached.clone();
        }
        let cached = GlyphAtlasTextKey::new(text);
        self.text_cache.insert(text.to_owned(), cached.clone());
        cached
    }

    fn intern_cluster_text(&mut self, text: &str) -> GlyphAtlasTextKey {
        let mut chars = text.chars();
        if let Some(ch) = chars.next()
            && chars.next().is_none()
        {
            return self.intern_char(ch);
        }
        self.intern_text(text)
    }

    /// Atlas key for a cluster. Shaped (ligature/contextual) clusters key on
    /// their glyph ids rather than source text, because the same characters can
    /// shape to different glyphs depending on run context.
    fn intern_cluster_key(&mut self, cluster: &ShapedCluster) -> GlyphAtlasTextKey {
        if cluster.glyphs.is_empty() {
            return self.intern_cluster_text(&cluster.text);
        }
        let mut signature = String::with_capacity(cluster.glyphs.len() * 22 + 1);
        signature.push('\u{1}');
        for glyph in &cluster.glyphs {
            signature.push_str(&glyph.glyph_id.to_string());
            signature.push('@');
            signature.push_str(&glyph.x_offset.to_bits().to_string());
            signature.push(':');
            signature.push_str(&glyph.y_offset.to_bits().to_string());
            signature.push(',');
        }
        self.intern_text(&signature)
    }

    fn intern_char(&mut self, ch: char) -> GlyphAtlasTextKey {
        if ch.is_ascii() {
            let index = ch as usize;
            if let Some(cached) = &self.ascii_char_cache[index] {
                return cached.clone();
            }
            let cached = GlyphAtlasTextKey::for_char(ch);
            self.ascii_char_cache[index] = Some(cached.clone());
            return cached;
        }
        if let Some(cached) = self.char_cache.get(&ch) {
            return cached.clone();
        }
        let cached = GlyphAtlasTextKey::for_char(ch);
        self.char_cache.insert(ch, cached.clone());
        cached
    }

    fn intern_face(&mut self, face: &ResolvedFontFace) -> GlyphAtlasFaceKey {
        if let Some(cached) = self.face_cache.get(face) {
            return cached.clone();
        }
        let cached = GlyphAtlasFaceKey::new(face.clone());
        self.face_cache.insert(face.clone(), cached.clone());
        cached
    }

    pub fn prepare_sprite_command(
        &mut self,
        command: &SpriteCommandBatch,
        pixels_per_point: f32,
    ) -> TexturedGlyphQuad {
        let width = (command.rect.width() * pixels_per_point).ceil().max(1.0) as u32;
        let height = (command.rect.height() * pixels_per_point).ceil().max(1.0) as u32;
        let key = GlyphAtlasKey {
            face: self.sprite_face_key.clone(),
            text: self.intern_char(command.ch),
            font_size_bits: command.rect.height().to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
            width,
            height,
        };
        let format = self.atlas.format();
        let entry = self.atlas.insert_or_get_with(key, width, height, || {
            let alpha = rasterize_sprite_commands(&command.commands, command.rect, width, height);
            alpha_to_atlas_pixels(format, alpha)
        });
        TexturedGlyphQuad {
            rect: command.rect,
            uv: atlas_uv(self.atlas.size(), entry),
            color: command.color,
        }
    }

    pub fn atlas_len(&self) -> usize {
        self.atlas.len()
    }

    pub fn atlas_pixels(&self) -> &[u8] {
        self.atlas.pixels()
    }

    pub fn atlas_size(&self) -> (u32, u32) {
        self.atlas.size()
    }

    pub fn atlas_modified_count(&self) -> u64 {
        self.atlas.modified_count()
    }
    pub fn atlas_dirty_rect_since(&self, modified: u64) -> Option<GlyphAtlasEntry> {
        self.atlas.dirty_rect_since(modified)
    }

    pub fn atlas_resized_count(&self) -> u64 {
        self.atlas.resized_count()
    }

    pub fn atlas_format(&self) -> GlyphAtlasFormat {
        self.atlas.format()
    }
}

fn single_ascii_cluster(cluster: &ShapedCluster) -> Option<u8> {
    let bytes = cluster.text.as_bytes();
    (bytes.len() == 1 && bytes[0].is_ascii()).then_some(bytes[0])
}

#[cfg(windows)]
fn windows_gdi_candidate(cluster: &ShapedCluster) -> bool {
    single_ascii_cluster(cluster).is_some_and(|ch| ch.is_ascii_graphic())
}

fn cluster_constraint_cells(
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

#[derive(Clone, Debug)]
struct FontLibrary {
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

struct RasterizeClusterRequest<'a> {
    face: &'a ResolvedFontFace,
    cluster: &'a ShapedCluster,
    font_size: f32,
    pixels_per_point: f32,
    constraint_cells: u16,
    tile: (u32, u32),
    format: GlyphAtlasFormat,
}

struct RasterizedCluster {
    pixels: Vec<u8>,
    color: bool,
}

struct PositionedClusterGlyphRequest {
    ch: char,
    glyph_id: GlyphId,
    scale: PxScale,
    position: ab_glyph::Point,
    metrics: FontFaceMetrics,
    constraint_cells: u16,
    tile: (u32, u32),
}

impl FontLibrary {
    fn new() -> Self {
        Self {
            database: system_font_database(),
            fonts: HashMap::new(),
            fonts_by_id: HashMap::new(),
            fallback_font_ids: HashMap::new(),
            metrics: HashMap::new(),
            shaping_capable: HashMap::new(),
        }
    }

    /// Shapes `text` with the primary font and emits cell-aligned clusters,
    /// attaching shaped glyph ids to ligature/contextual clusters. Returns
    /// `None` when the font has no ligature features, so the caller can keep the
    /// cheaper per-character paths.
    fn shape_into_clusters(
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
        let hb_features = harfbuzz_features(features);
        let glyphs = self
            .database
            .with_face_data(id, |data, index| {
                shape_run(data, index, text, font_size, &hb_features)
            })
            .flatten()?;
        let total_cells = text_cell_count(text);
        let mut cluster_index = 0;
        let mut glyph_index = 0;
        while glyph_index < glyphs.len() {
            let group = shaped_glyph_group(text, &glyphs, glyph_index, total_cells, &font);
            let mut glyph_end = group.end;
            let mut cells = group.cells;
            let mut source_end = group.source_end;
            let draw_by_glyph = group.draw_by_glyph;

            if draw_by_glyph {
                while glyph_end < glyphs.len() {
                    let next_group =
                        shaped_glyph_group(text, &glyphs, glyph_end, total_cells, &font);
                    if !next_group.draw_by_glyph
                        || next_group.cell != group.cell.saturating_add(cells)
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

    fn rasterize_cluster(&mut self, request: RasterizeClusterRequest<'_>) -> RasterizedCluster {
        let RasterizeClusterRequest {
            face,
            cluster,
            font_size,
            pixels_per_point,
            constraint_cells,
            tile: (width, height),
            format,
        } = request;
        if cluster.is_whitespace {
            return RasterizedCluster {
                pixels: vec![0; (width * height * format.depth()) as usize],
                color: false,
            };
        }
        #[cfg(windows)]
        if windows_gdi_candidate(cluster)
            && let Some(family) =
                self.font_family_name_for_cluster(face, cluster, font_size * pixels_per_point)
            && let Some(alpha) = windows_gdi::rasterize_text_cluster(
                &family,
                face.style,
                cluster,
                font_size * pixels_per_point,
                width,
                height,
            )
        {
            return RasterizedCluster {
                pixels: alpha_to_atlas_pixels(format, alpha),
                color: false,
            };
        }
        // An explicit emoji-presentation cluster (VS16, or a default-emoji codepoint) renders as a
        // color emoji, even when the primary font carries a monochrome glyph for the base symbol —
        // otherwise ⚠️/❤️ draw as a theme-tinted text glyph, and the rendering flips with whatever
        // the shaper happened to produce. Skip the by-glyph path so it reaches the color path below.
        let prefer_color_emoji =
            format == GlyphAtlasFormat::Rgba && is_color_emoji_cluster(cluster);
        if !cluster.glyphs.is_empty()
            && !prefer_color_emoji
            && let Some(font) = self.font_for_face(face)
            && let Some(alpha) = rasterize_glyph_cluster(RasterizeGlyphClusterRequest {
                font: &font,
                glyphs: &cluster.glyphs,
                physical_font_size: font_size * pixels_per_point,
                pixels_per_point,
                style: face.style,
                constraint_cells,
                tile: (width, height),
            })
        {
            return RasterizedCluster {
                pixels: alpha_to_atlas_pixels(format, alpha),
                color: false,
            };
        }
        let scale = PxScale::from((font_size * pixels_per_point).max(1.0));
        let primary_metrics = self.font_for_face(face).map(|font| {
            self.font_face_metrics_for(face, &font, scale, constraint_cells, width, height)
        });
        if let Some(metrics) = primary_metrics {
            if format == GlyphAtlasFormat::Rgba
                && is_color_emoji_cluster(cluster)
                && let Some(pixels) = coretext::rasterize_color_cluster(
                    face,
                    cluster,
                    font_size * pixels_per_point,
                    metrics,
                    constraint_cells,
                    width,
                    height,
                )
            {
                return RasterizedCluster {
                    pixels,
                    color: true,
                };
            }
            let cluster_uses_private_codepoint = cluster.text.chars().any(is_private_use);
            if !cluster_uses_private_codepoint
                && let Some(alpha) = coretext::rasterize_symbol_cluster(
                    face,
                    cluster,
                    font_size * pixels_per_point,
                    metrics,
                    constraint_cells,
                    width,
                    height,
                )
            {
                return RasterizedCluster {
                    pixels: alpha_to_atlas_pixels(format, alpha),
                    color: false,
                };
            }
        }
        let Some(font) = self.font_for_cluster(face, cluster, font_size * pixels_per_point) else {
            return RasterizedCluster {
                pixels: alpha_to_atlas_pixels(
                    format,
                    fallback_cluster_mask(cluster, width, height),
                ),
                color: false,
            };
        };
        let scaled = font.as_scaled(scale);
        let metrics = primary_metrics
            .unwrap_or_else(|| font_face_metrics(&font, scale, constraint_cells, width, height));
        let baseline = ((height as f32 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
        let mut pen_x = 0.0_f32;
        let mut alpha = vec![0; (width * height) as usize];

        for ch in cluster.text.chars() {
            if is_combining_mark(ch) || is_variation_selector(ch) {
                continue;
            }
            let glyph_id = scaled.glyph_id(ch);
            if glyph_id.0 == 0 {
                continue;
            }
            let glyph = positioned_cluster_glyph(
                &font,
                PositionedClusterGlyphRequest {
                    ch,
                    glyph_id,
                    scale,
                    position: point(pen_x, baseline),
                    metrics,
                    constraint_cells,
                    tile: (width, height),
                },
            );
            let glyph_scaled = font.as_scaled(glyph.scale);
            draw_outline_glyph(&mut alpha, &glyph_scaled, glyph.clone(), width, height);
            if matches!(face.style, FontStyle::Bold | FontStyle::BoldItalic) {
                let glyph = glyph_id.with_scale_and_position(
                    scale,
                    point(
                        glyph.position.x + (pixels_per_point * 0.45).max(1.0),
                        glyph.position.y,
                    ),
                );
                draw_outline_glyph(&mut alpha, &glyph_scaled, glyph, width, height);
            }
            pen_x += scaled.h_advance(glyph_id);
        }

        if alpha.iter().any(|value| *value > 0) {
            RasterizedCluster {
                pixels: alpha_to_atlas_pixels(format, alpha),
                color: false,
            }
        } else {
            RasterizedCluster {
                pixels: alpha_to_atlas_pixels(
                    format,
                    fallback_cluster_mask(cluster, width, height),
                ),
                color: false,
            }
        }
    }

    fn font_for_cluster(
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

    fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
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

    fn font_face_metrics_for(
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

fn draw_outline_glyph<F: Font>(
    alpha: &mut [u8],
    font: &impl ScaleFont<F>,
    glyph: ab_glyph::Glyph,
    width: u32,
    height: u32,
) {
    if let Some(outlined) = font.outline_glyph(glyph) {
        let bounds = outlined.px_bounds();
        outlined.draw(|x, y, coverage| {
            let px = bounds.min.x + x as f32;
            let py = bounds.min.y + y as f32;
            if px < 0.0 || py < 0.0 || px >= width as f32 || py >= height as f32 {
                return;
            }
            let index = py as u32 * width + px as u32;
            if let Some(dst) = alpha.get_mut(index as usize) {
                *dst = (*dst).max((coverage * 255.0).round() as u8);
            }
        });
    }
}

fn font_supports_char(font: &FontArc, ch: char) -> bool {
    font.glyph_id(ch) != GlyphId(0)
}

struct RasterizeGlyphClusterRequest<'a> {
    font: &'a FontArc,
    glyphs: &'a [ShapedGlyph],
    physical_font_size: f32,
    pixels_per_point: f32,
    style: FontStyle,
    constraint_cells: u16,
    tile: (u32, u32),
}

/// Draws a shaped cluster from its glyph ids into an alpha mask. Glyph offsets
/// arrive in logical pixels (shaped at the logical font size) and scale up by
/// `pixels_per_point` to device pixels. Bold faces get the same synthetic-weight
/// double strike as the per-character path.
fn rasterize_glyph_cluster(request: RasterizeGlyphClusterRequest<'_>) -> Option<Vec<u8>> {
    let RasterizeGlyphClusterRequest {
        font,
        glyphs,
        physical_font_size,
        pixels_per_point,
        style,
        constraint_cells,
        tile: (width, height),
    } = request;
    let scale = PxScale::from(physical_font_size.max(1.0));
    let scaled = font.as_scaled(scale);
    let baseline = ((height as f32 - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let synthesize_bold = matches!(style, FontStyle::Bold | FontStyle::BoldItalic);
    let bold_offset = (pixels_per_point * 0.45).max(1.0);

    let cluster_start = glyphs.iter().map(|glyph| glyph.cluster).min().unwrap_or(0);
    let cell_width = width as f32 / f32::from(constraint_cells.max(1));
    let mut alpha = vec![0_u8; (width * height) as usize];

    // A single substituted glyph (e.g. a font's GSUB stylistic-alternate circle) can be drawn
    // wider than its cell. Fit it to the tile and center it so it doesn't hard-clip; this only
    // diverts when the ink actually overflows, leaving normal glyphs and multi-glyph ligatures on
    // the shaped-position path below.
    if let [only] = glyphs
        && let Some(outlined) = scaled
            .outline_glyph(GlyphId(only.glyph_id).with_scale_and_position(scale, point(0.0, 0.0)))
    {
        let bounds = outlined.px_bounds();
        let fit = (width as f32 / bounds.width())
            .min(height as f32 / bounds.height())
            .min(1.0);
        if fit < 1.0 {
            let fitted = PxScale {
                x: scale.x * fit,
                y: scale.y * fit,
            };
            let fitted_scaled = font.as_scaled(fitted);
            let glyph_id = GlyphId(only.glyph_id);
            if let Some(outlined) = fitted_scaled
                .outline_glyph(glyph_id.with_scale_and_position(fitted, point(0.0, 0.0)))
            {
                let bounds = outlined.px_bounds();
                let dx = ((width as f32 - bounds.width()) * 0.5) - bounds.min.x;
                let dy = ((height as f32 - bounds.height()) * 0.5) - bounds.min.y;
                draw_outline_glyph(
                    &mut alpha,
                    &fitted_scaled,
                    glyph_id.with_scale_and_position(fitted, point(dx, dy)),
                    width,
                    height,
                );
                if synthesize_bold {
                    draw_outline_glyph(
                        &mut alpha,
                        &fitted_scaled,
                        glyph_id.with_scale_and_position(fitted, point(dx + bold_offset, dy)),
                        width,
                        height,
                    );
                }
                return alpha.iter().any(|value| *value > 0).then_some(alpha);
            }
        }
    }

    for glyph in glyphs {
        let glyph_id = GlyphId(glyph.glyph_id);
        let cell_offset = glyph.cluster.saturating_sub(cluster_start) as f32 * cell_width;
        let x = cell_offset + glyph.x_offset * pixels_per_point;
        let y = baseline - glyph.y_offset * pixels_per_point;
        draw_outline_glyph(
            &mut alpha,
            &scaled,
            glyph_id.with_scale_and_position(scale, point(x, y)),
            width,
            height,
        );
        if synthesize_bold {
            draw_outline_glyph(
                &mut alpha,
                &scaled,
                glyph_id.with_scale_and_position(scale, point(x + bold_offset, y)),
                width,
                height,
            );
        }
    }
    alpha.iter().any(|value| *value > 0).then_some(alpha)
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

fn positioned_cluster_glyph(
    font: &FontArc,
    request: PositionedClusterGlyphRequest,
) -> ab_glyph::Glyph {
    let PositionedClusterGlyphRequest {
        ch,
        glyph_id,
        scale,
        position,
        metrics,
        constraint_cells,
        tile,
    } = request;
    let glyph = glyph_id.with_scale_and_position(scale, position);
    let scaled = font.as_scaled(scale);
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let tile_width = tile.0 as f32;
    let tile_height = tile.1 as f32;

    let constraint = terminal_glyph_constraint(ch as u32);
    if constraint.does_anything() {
        let constrained = constraint.constrain(
            GlyphSize {
                width: f64::from(bounds.width()),
                height: f64::from(bounds.height()),
                x: f64::from(bounds.min.x),
                y: f64::from(bounds.min.y),
            },
            metrics,
            constraint_cells.min(u16::from(u8::MAX)) as u8,
        );
        let scale_factor = (constrained.width as f32 / bounds.width()).max(0.01);
        let scale = PxScale {
            x: scale.x * scale_factor,
            y: scale.y * scale_factor,
        };
        let scaled = font.as_scaled(scale);
        let glyph = glyph_id.with_scale_and_position(scale, point(0.0, 0.0));
        let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
            return glyph;
        };
        let bounds = outlined.px_bounds();
        return glyph_id.with_scale_and_position(
            scale,
            point(
                constrained.x as f32 - bounds.min.x,
                constrained.y as f32 - bounds.min.y,
            ),
        );
    }

    // No symbol constraint: keep the glyph at its natural size and baseline, EXCEPT when its ink
    // overflows the cell. Oversized symbols a font draws wider than one cell (circles, bullets,
    // shapes outside the symbol-fit ranges) would otherwise be hard-clipped by draw_outline_glyph.
    // Private-use icons always fit-and-center. The fit caps at 1.0, so well-behaved text glyphs
    // (ink within the cell) are returned untouched.
    let fit = (tile_width / bounds.width())
        .min(tile_height / bounds.height())
        .min(1.0);
    if fit >= 1.0 && !is_private_use(ch) {
        return glyph;
    }
    let scale = PxScale {
        x: scale.x * fit,
        y: scale.y * fit,
    };
    let scaled = font.as_scaled(scale);
    let baseline = ((tile_height - scaled.height()) * 0.5).max(0.0) + scaled.ascent();
    let glyph = glyph_id.with_scale_and_position(scale, point(position.x, baseline));
    let Some(outlined) = scaled.outline_glyph(glyph.clone()) else {
        return glyph;
    };
    let bounds = outlined.px_bounds();
    let dx = ((tile_width - bounds.width()) * 0.5) - bounds.min.x;
    let dy = ((tile_height - bounds.height()) * 0.5) - bounds.min.y;
    glyph_id.with_scale_and_position(scale, point(position.x + dx, baseline + dy))
}

fn font_face_metrics(
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

fn is_private_use(ch: char) -> bool {
    matches!(
        ch as u32,
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD
    )
}

// Routing to the per-character (constraint-applying) path must track the constraint classifier
// exactly: a glyph that fit-scales to the cell but takes the direct glyph-id path never gets the
// scale applied and `draw_outline_glyph` clips it. Delegating keeps the two in lockstep — a drift
// here is what let Geometric Shapes circles clip after they gained a Fit constraint.
fn is_symbol_like(ch: char) -> bool {
    is_symbol_codepoint(ch as u32)
}

fn is_terminal_graphics_symbol(ch: char) -> bool {
    matches!(
        ch as u32,
        0x2500..=0x259F | 0x1CC00..=0x1CEBF | 0x1FB00..=0x1FBFF | 0xE0B0..=0xE0D7
    )
}

fn is_symbol_space(ch: char) -> bool {
    matches!(ch as u32, 0x0020 | 0x2002)
}

fn is_color_emoji_cluster(cluster: &ShapedCluster) -> bool {
    if cluster.text.contains('\u{fe0e}') {
        return false;
    }
    cluster
        .text
        .chars()
        .any(|ch| ch == '\u{fe0f}' || is_default_emoji_presentation(ch))
}

// Emoji_Presentation=Yes ranges from Unicode 16.0 emoji-data.txt. Symbols outside
// these ranges (⚠ ✔ ❤ …) default to text presentation and must take the alpha
// path so the theme foreground tints them.
fn is_default_emoji_presentation(ch: char) -> bool {
    matches!(
        ch as u32,
        0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            | 0x1F004
            | 0x1F0CF
            | 0x1F18E
            | 0x1F191..=0x1F19A
            | 0x1F1E6..=0x1F1FF
            | 0x1F201
            | 0x1F21A
            | 0x1F22F
            | 0x1F232..=0x1F236
            | 0x1F238..=0x1F23A
            | 0x1F250..=0x1F251
            | 0x1F300..=0x1F320
            | 0x1F32D..=0x1F335
            | 0x1F337..=0x1F37C
            | 0x1F37E..=0x1F393
            | 0x1F3A0..=0x1F3CA
            | 0x1F3CF..=0x1F3D3
            | 0x1F3E0..=0x1F3F0
            | 0x1F3F4
            | 0x1F3F8..=0x1F43E
            | 0x1F440
            | 0x1F442..=0x1F4FC
            | 0x1F4FF..=0x1F53D
            | 0x1F54B..=0x1F54E
            | 0x1F550..=0x1F567
            | 0x1F57A
            | 0x1F595..=0x1F596
            | 0x1F5A4
            | 0x1F5FB..=0x1F64F
            | 0x1F680..=0x1F6C5
            | 0x1F6CC
            | 0x1F6D0..=0x1F6D2
            | 0x1F6D5..=0x1F6D7
            | 0x1F6DC..=0x1F6DF
            | 0x1F6EB..=0x1F6EC
            | 0x1F6F4..=0x1F6FC
            | 0x1F7E0..=0x1F7EB
            | 0x1F7F0
            | 0x1F90C..=0x1F93A
            | 0x1F93C..=0x1F945
            | 0x1F947..=0x1F9FF
            | 0x1FA70..=0x1FA7C
            | 0x1FA80..=0x1FA89
            | 0x1FA8F..=0x1FAC6
            | 0x1FACE..=0x1FADC
            | 0x1FADF..=0x1FAE9
            | 0x1FAF0..=0x1FAF8
    )
}

fn alpha_to_atlas_pixels(format: GlyphAtlasFormat, alpha: Vec<u8>) -> Vec<u8> {
    match format {
        GlyphAtlasFormat::Alpha => alpha,
        GlyphAtlasFormat::Bgr => alpha
            .into_iter()
            .flat_map(|alpha| [alpha, alpha, alpha])
            .collect(),
        GlyphAtlasFormat::Rgba => alpha
            .into_iter()
            .flat_map(|alpha| [255, 255, 255, alpha])
            .collect(),
    }
}

#[cfg(target_os = "macos")]
fn unpremultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * 255) / alpha).min(255) as u8;
        }
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

fn is_combining_mark(ch: char) -> bool {
    matches!(
        ch as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

fn is_variation_selector(ch: char) -> bool {
    matches!(ch as u32, 0xFE00..=0xFE0F | 0xE0100..=0xE01EF)
}

fn fallback_cluster_mask(cluster: &ShapedCluster, width: u32, height: u32) -> Vec<u8> {
    let mut alpha = vec![0; (width * height) as usize];
    if cluster.is_whitespace {
        return alpha;
    }
    if let Some(ch) = cluster.text.chars().next()
        && draw_fallback_arrow(&mut alpha, ch, width, height)
    {
        return alpha;
    }
    let seed = cluster.text.chars().next().unwrap_or(' ') as u32;
    let margin_x = (width / 6).min(width.saturating_sub(1));
    let margin_y = (height / 6).min(height.saturating_sub(1));
    for y in margin_y..height.saturating_sub(margin_y) {
        for x in margin_x..width.saturating_sub(margin_x) {
            let pattern = (x + y + seed).is_multiple_of(3);
            if pattern || cluster.text != " " {
                alpha[(y * width + x) as usize] = 220;
            }
        }
    }
    alpha
}

fn draw_fallback_arrow(alpha: &mut [u8], ch: char, width: u32, height: u32) -> bool {
    let direction = match ch {
        '\u{21e1}' | '\u{2191}' | '\u{21e7}' => ArrowDirection::Up,
        '\u{21e3}' | '\u{2193}' | '\u{21e9}' => ArrowDirection::Down,
        _ => return false,
    };
    let stroke = (width / 6).max(1);
    let center_x = width / 2;
    let top = height / 4;
    let bottom = height - height / 4;

    match direction {
        ArrowDirection::Up => {
            fill_pixel_rect(
                alpha,
                width,
                center_x.saturating_sub(stroke / 2),
                top + height / 8,
                stroke,
                bottom.saturating_sub(top + height / 8),
            );
            for offset in 0..=(width / 4).max(1) {
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x.saturating_sub(offset),
                    top + offset,
                    stroke,
                    stroke,
                );
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x + offset,
                    top + offset,
                    stroke,
                    stroke,
                );
            }
        }
        ArrowDirection::Down => {
            fill_pixel_rect(
                alpha,
                width,
                center_x.saturating_sub(stroke / 2),
                top,
                stroke,
                bottom.saturating_sub(top + height / 8),
            );
            for offset in 0..=(width / 4).max(1) {
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x.saturating_sub(offset),
                    bottom.saturating_sub(offset),
                    stroke,
                    stroke,
                );
                fill_pixel_rect(
                    alpha,
                    width,
                    center_x + offset,
                    bottom.saturating_sub(offset),
                    stroke,
                    stroke,
                );
            }
        }
    }
    true
}

#[derive(Clone, Copy)]
enum ArrowDirection {
    Up,
    Down,
}

fn fill_pixel_rect(alpha: &mut [u8], width: u32, x: u32, y: u32, rect_width: u32, height: u32) {
    let total_height = alpha.len() as u32 / width.max(1);
    for py in y..(y + height).min(total_height) {
        for px in x..(x + rect_width).min(width) {
            alpha[(py * width + px) as usize] = 220;
        }
    }
}

fn rasterize_sprite_commands(
    commands: &[SpriteCommand],
    rect: SurfaceRect,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut alpha = vec![0; (width * height) as usize];
    for command in commands {
        match command {
            SpriteCommand::FillRect {
                rect: fill,
                alpha: coverage,
            } => {
                fill_mask_rect(&mut alpha, rect, *fill, width, height, *coverage);
            }
            SpriteCommand::FillPolygon {
                points,
                alpha: coverage,
                ..
            } => {
                fill_mask_polygon(&mut alpha, rect, points, width, height, *coverage);
            }
            SpriteCommand::StrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for pair in points.windows(2) {
                    fill_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        pair[0],
                        pair[1],
                        *stroke_width,
                        (width, height),
                        *coverage,
                    );
                }
            }
            SpriteCommand::ClearStrokePolyline {
                points,
                width: stroke_width,
                alpha: coverage,
            } => {
                for pair in points.windows(2) {
                    clear_mask_stroke_segment(
                        &mut alpha,
                        rect,
                        pair[0],
                        pair[1],
                        *stroke_width,
                        (width, height),
                        *coverage,
                    );
                }
            }
        }
    }
    alpha
}

fn fill_mask_rect(
    pixels: &mut [u8],
    cell: SurfaceRect,
    fill: SurfaceRect,
    width: u32,
    height: u32,
    coverage: f32,
) {
    let min_x = (((fill.min_x - cell.min_x) / cell.width().max(1.0)) * width as f32)
        .floor()
        .clamp(0.0, width as f32) as u32;
    let max_x = (((fill.max_x - cell.min_x) / cell.width().max(1.0)) * width as f32)
        .ceil()
        .clamp(0.0, width as f32) as u32;
    let min_y = (((fill.min_y - cell.min_y) / cell.height().max(1.0)) * height as f32)
        .floor()
        .clamp(0.0, height as f32) as u32;
    let max_y = (((fill.max_y - cell.min_y) / cell.height().max(1.0)) * height as f32)
        .ceil()
        .clamp(0.0, height as f32) as u32;
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in min_y..max_y {
        for x in min_x..max_x {
            if let Some(dst) = pixels.get_mut((y * width + x) as usize) {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn fill_mask_polygon(
    pixels: &mut [u8],
    cell: SurfaceRect,
    points: &[crate::terminal_sprite::SpritePoint],
    width: u32,
    height: u32,
    coverage: f32,
) {
    if points.len() < 3 {
        return;
    }
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if point_in_polygon(px, py, points)
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn fill_mask_stroke_segment(
    pixels: &mut [u8],
    cell: SurfaceRect,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
    stroke_width: f32,
    size: (u32, u32),
    coverage: f32,
) {
    let (width, height) = size;
    let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if distance_to_segment(px, py, start, end) <= stroke_width * 0.5
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).max(value);
            }
        }
    }
}

fn clear_mask_stroke_segment(
    pixels: &mut [u8],
    cell: SurfaceRect,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
    stroke_width: f32,
    size: (u32, u32),
    coverage: f32,
) {
    let (width, height) = size;
    let value = ((1.0 - coverage.clamp(0.0, 1.0)) * 255.0).round() as u8;
    for y in 0..height {
        for x in 0..width {
            let px = cell.min_x + ((x as f32 + 0.5) / width as f32) * cell.width();
            let py = cell.min_y + ((y as f32 + 0.5) / height as f32) * cell.height();
            if distance_to_segment(px, py, start, end) <= stroke_width * 0.5
                && let Some(dst) = pixels.get_mut((y * width + x) as usize)
            {
                *dst = (*dst).min(value);
            }
        }
    }
}

fn point_in_polygon(x: f32, y: f32, points: &[crate::terminal_sprite::SpritePoint]) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let current_point = points[current];
        let previous_point = points[previous];
        if ((current_point.y > y) != (previous_point.y > y))
            && (x
                < (previous_point.x - current_point.x) * (y - current_point.y)
                    / (previous_point.y - current_point.y)
                    + current_point.x)
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn distance_to_segment(
    x: f32,
    y: f32,
    start: crate::terminal_sprite::SpritePoint,
    end: crate::terminal_sprite::SpritePoint,
) -> f32 {
    let vx = end.x - start.x;
    let vy = end.y - start.y;
    let wx = x - start.x;
    let wy = y - start.y;
    let len_squared = vx * vx + vy * vy;
    if len_squared <= f32::EPSILON {
        return ((x - start.x).powi(2) + (y - start.y).powi(2)).sqrt();
    }
    let t = ((wx * vx + wy * vy) / len_squared).clamp(0.0, 1.0);
    let proj_x = start.x + t * vx;
    let proj_y = start.y + t * vy;
    ((x - proj_x).powi(2) + (y - proj_y).powi(2)).sqrt()
}

fn rects_overlap(a: GlyphAtlasEntry, b: GlyphAtlasEntry) -> bool {
    a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y
}

fn atlas_byte_len(width: u32, height: u32, depth: u32) -> Result<usize, GlyphAtlasError> {
    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(depth))
        .map(|bytes| bytes as usize)
        .ok_or(GlyphAtlasError::CapacityExceeded)
}

fn blit_pixels(
    pixels: &mut [u8],
    atlas_width: u32,
    depth: u32,
    entry: GlyphAtlasEntry,
    alpha: &[u8],
) {
    let row_bytes = (entry.width * depth) as usize;
    for y in 0..entry.height {
        let dst_start = (((entry.y + y) * atlas_width + entry.x) * depth) as usize;
        let src_start = (y * entry.width * depth) as usize;
        let Some(dst_row) = pixels.get_mut(dst_start..dst_start.saturating_add(row_bytes)) else {
            continue;
        };
        let Some(src_row) = alpha.get(src_start..src_start.saturating_add(row_bytes)) else {
            continue;
        };
        dst_row.copy_from_slice(src_row);
    }
}

#[derive(Clone, Copy)]
struct BlitTarget {
    atlas_width: u32,
    depth: u32,
    entry: GlyphAtlasEntry,
}

#[derive(Clone, Copy)]
struct BlitSource<'a> {
    pixels: &'a [u8],
    width: u32,
    x: u32,
    y: u32,
}

fn blit_pixels_from_source(pixels: &mut [u8], target: BlitTarget, source: BlitSource<'_>) {
    let row_bytes = (target.entry.width * target.depth) as usize;
    for y in 0..target.entry.height {
        let dst_start =
            (((target.entry.y + y) * target.atlas_width + target.entry.x) * target.depth) as usize;
        let Some(dst_row) = pixels.get_mut(dst_start..dst_start.saturating_add(row_bytes)) else {
            continue;
        };
        dst_row.fill(0);

        let src_start = (((source.y + y) * source.width + source.x) * target.depth) as usize;
        let Some(src_row) = source
            .pixels
            .get(src_start..src_start.saturating_add(row_bytes))
        else {
            continue;
        };
        dst_row.copy_from_slice(src_row);
    }
}

fn atlas_uv((atlas_width, atlas_height): (u32, u32), entry: GlyphAtlasEntry) -> SurfaceRect {
    SurfaceRect {
        min_x: (entry.x as f32 + 0.5) / atlas_width as f32,
        min_y: (entry.y as f32 + 0.5) / atlas_height as f32,
        max_x: (entry.x + entry.width) as f32 / atlas_width as f32 - 0.5 / atlas_width as f32,
        max_y: (entry.y + entry.height) as f32 / atlas_height as f32 - 0.5 / atlas_height as f32,
    }
}

#[cfg(test)]
mod tests;
