use std::{collections::HashMap, fmt::Write as _, sync::Arc};

use ab_glyph::{Font, PxScale, ScaleFont};
use smallvec::SmallVec;

mod atlas;
mod clusters;

pub use atlas::{
    GlyphAtlas, GlyphAtlasEntry, GlyphAtlasError, GlyphAtlasFaceKey, GlyphAtlasFormat,
    GlyphAtlasKey, GlyphAtlasTextKey,
};
pub use clusters::{ShapedCluster, TerminalTextShaper};
mod coretext;
mod font_library;
mod font_raster;
mod shaping;
mod sprite_raster;
#[cfg(windows)]
mod windows_gdi;

use atlas::{GlyphAtlasRecord, alpha_to_atlas_pixels, atlas_uv};
use clusters::{
    cluster_constraint_cells, is_color_emoji_cluster, is_printable_ascii, single_ascii_cluster,
};
use font_library::FontLibrary;
use font_raster::{RasterizeClusterRequest, rasterize_cluster};
use sprite_raster::rasterize_sprite_commands;

use crate::{
    geometry::SurfaceRect,
    paint_plan::PlanColor,
    terminal_render::{SpriteCommandBatch, TextCommand},
    terminal_text::{FontFeature, FontStyle, ResolvedFontFace},
};

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

/// Identity of a shaped run: shaping output depends on the text, the resolved
/// face, the font size, and the ordered font-feature policy. A run that merely
/// moved or reappeared keys to the same entry. Position, color, and atlas state
/// are deliberately excluded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapedRunCacheKey {
    text: String,
    face: GlyphAtlasFaceKey,
    font_size_bits: u32,
    font_features: Arc<[FontFeature]>,
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
            shaper: TerminalTextShaper,
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
            font_features: Arc::clone(&command.font_features),
        };
        let (total_cells, cluster_len) = if let Some(entry) = self.shaped_run_cache.get(&cache_key)
        {
            clusters.clear();
            clusters.extend_from_slice(&entry.clusters);
            (entry.total_cells, entry.clusters.len())
        } else {
            let shaped = self.fonts.shape_into_clusters(
                &command.face,
                &command.text,
                command.font_size,
                &command.font_features,
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
            text: self.intern_cluster_key(request.cluster, &request.command.font_features),
            font_size_bits: request.command.font_size.to_bits(),
            pixels_per_point_bits: request.pixels_per_point.to_bits(),
            width: request.glyph_width,
            height: request.glyph_height,
        };
        let format = self.atlas.format();
        self.atlas
            .insert_or_get_with_color(key, request.glyph_width, request.glyph_height, || {
                let rasterized = rasterize_cluster(
                    &mut self.fonts,
                    RasterizeClusterRequest {
                        face: &request.command.face,
                        cluster: request.cluster,
                        font_size: request.command.font_size,
                        pixels_per_point: request.pixels_per_point,
                        constraint_cells: request.constraint_cells,
                        tile: (request.glyph_width, request.glyph_height),
                        format,
                    },
                );
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
    fn intern_cluster_key(
        &mut self,
        cluster: &ShapedCluster,
        font_features: &[FontFeature],
    ) -> GlyphAtlasTextKey {
        if cluster.glyphs.is_empty() {
            return self.intern_cluster_text(&cluster.text);
        }
        let mut signature = String::with_capacity(cluster.glyphs.len() * 22 + 1);
        signature.push('\u{1}');
        for feature in font_features {
            let tag = feature.tag();
            write!(
                signature,
                "{:02x}{:02x}{:02x}{:02x}{:08x}",
                tag[0],
                tag[1],
                tag[2],
                tag[3],
                feature.value()
            )
            .expect("writing to String is infallible");
        }
        signature.push('\u{2}');
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
            text: self.intern_char(command.glyph.ch),
            font_size_bits: command.rect.height().to_bits(),
            pixels_per_point_bits: pixels_per_point.to_bits(),
            width,
            height,
        };
        let format = self.atlas.format();
        let entry = self.atlas.insert_or_get_with(key, width, height, || {
            let commands = command.glyph.commands_for(command.rect);
            let alpha = rasterize_sprite_commands(&commands, command.rect, width, height);
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
