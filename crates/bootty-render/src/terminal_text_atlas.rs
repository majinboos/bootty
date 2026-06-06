use std::collections::HashMap;

use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont, point};

mod coretext;

use crate::{
    geometry::SurfaceRect,
    paint_plan::PlanColor,
    terminal_font_face::{FontFaceMetrics, GlyphSize, terminal_glyph_constraint},
    terminal_render::{SpriteCommandBatch, TextCommand},
    terminal_sprite::SpriteCommand,
    terminal_text::{
        FontFeature, FontStyle, ResolvedFontFace, default_font_features, terminal_char_width,
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
        let mut clusters = Vec::new();
        let mut cell = start_cell;
        let mut chars = text.chars().peekable();
        let liga_enabled = self.feature_enabled(*b"liga");
        while let Some(ch) = chars.next() {
            let mut cluster = String::from(ch);
            while let Some(next) = chars.peek().copied() {
                if is_combining_mark(next) || is_variation_selector(next) {
                    cluster.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            if liga_enabled && cluster == "f" && chars.peek() == Some(&'i') {
                cluster.push('i');
                chars.next();
            }
            let cells = cluster
                .chars()
                .next()
                .map(terminal_char_width)
                .unwrap_or(1)
                .max(1);
            clusters.push(ShapedCluster {
                text: cluster,
                cell,
                cells,
            });
            cell = cell.saturating_add(cells);
        }
        clusters
    }

    fn feature_enabled(&self, tag: [u8; 4]) -> bool {
        self.font_features
            .iter()
            .rev()
            .find(|feature| feature.tag() == tag)
            .is_none_or(|feature| feature.value() != 0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapedCluster {
    pub text: String,
    pub cell: u16,
    pub cells: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GlyphAtlasKey {
    pub face: ResolvedFontFace,
    pub text: String,
    pub font_size_bits: u32,
    pub pixels_per_point_bits: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphAtlasEntry {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
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
    entries: HashMap<GlyphAtlasKey, GlyphAtlasEntry>,
    pixels: Vec<u8>,
    modified: u64,
    resized: u64,
}

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
            modified: 0,
            resized: 0,
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
        if let Some(entry) = self.entries.get(&key) {
            return *entry;
        }

        let width = width.max(1);
        let height = height.max(1);
        let mut entry = self.reserve(width, height).unwrap_or(GlyphAtlasEntry {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        });
        if entry.width != width || entry.height != height {
            entry.width = entry.width.min(width);
            entry.height = entry.height.min(height);
        }
        let pixels = pixels();
        self.set(entry, &pixels);
        self.entries.insert(key, entry);
        entry
    }

    pub fn get(&self, key: &GlyphAtlasKey) -> Option<GlyphAtlasEntry> {
        self.entries.get(key).copied()
    }

    pub fn reserve(&mut self, width: u32, height: u32) -> Option<GlyphAtlasEntry> {
        let width = width.max(1);
        let height = height.max(1);
        if width + 2 > self.width || height + 2 > self.height {
            return None;
        }

        let usable_right = self.width - 1;
        let usable_bottom = self.height - 1;
        for y in 1..=usable_bottom.saturating_sub(height) {
            for x in 1..=usable_right.saturating_sub(width) {
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
                    self.allocations.push(entry);
                    return Some(entry);
                }
            }
        }
        None
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
        Ok(())
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

    pub fn resized_count(&self) -> u64 {
        self.resized
    }

    pub fn format(&self) -> GlyphAtlasFormat {
        self.format
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TexturedGlyphQuad {
    pub rect: SurfaceRect,
    pub uv: SurfaceRect,
    pub color: PlanColor,
}

#[derive(Clone, Debug)]
pub struct TextAtlasBuilder {
    shaper: TerminalTextShaper,
    atlas: GlyphAtlas,
    fonts: FontLibrary,
    color_glyphs: HashMap<GlyphAtlasKey, bool>,
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
            color_glyphs: HashMap::new(),
        }
    }

    pub fn prepare_text_command(
        &mut self,
        command: &TextCommand,
        pixels_per_point: f32,
    ) -> Vec<TexturedGlyphQuad> {
        let total_cells = command
            .text
            .chars()
            .map(terminal_char_width)
            .sum::<u16>()
            .max(1);
        let cell_width = command.rect.width() / f32::from(total_cells);
        let clusters = self.shaper.shape(&command.text, 0);
        let mut quads = Vec::with_capacity(clusters.len());

        for (index, cluster) in clusters.iter().enumerate() {
            let constraint_cells = cluster_constraint_cells(
                index.checked_sub(1).and_then(|index| clusters.get(index)),
                cluster,
                clusters.get(index + 1),
            );
            let rect = SurfaceRect::from_min_size(
                command.rect.min_x + f32::from(cluster.cell) * cell_width,
                command.rect.min_y,
                f32::from(constraint_cells) * cell_width,
                command.rect.height(),
            );
            let glyph_width = (rect.width() * pixels_per_point).ceil().max(1.0) as u32;
            let glyph_height = (rect.height() * pixels_per_point).ceil().max(1.0) as u32;
            let key = GlyphAtlasKey {
                face: command.face.clone(),
                text: cluster.text.clone(),
                font_size_bits: command.font_size.to_bits(),
                pixels_per_point_bits: pixels_per_point.to_bits(),
                width: glyph_width,
                height: glyph_height,
            };
            let (entry, is_color_glyph) = if let Some(entry) = self.atlas.get(&key) {
                (entry, self.color_glyphs.get(&key).copied().unwrap_or(false))
            } else {
                let rasterized = self.fonts.rasterize_cluster(RasterizeClusterRequest {
                    face: &command.face,
                    cluster,
                    font_size: command.font_size,
                    pixels_per_point,
                    constraint_cells,
                    tile: (glyph_width, glyph_height),
                    format: self.atlas.format(),
                });
                let color = rasterized.color;
                let entry =
                    self.atlas
                        .insert_or_get_with(key.clone(), glyph_width, glyph_height, || {
                            rasterized.pixels
                        });
                self.color_glyphs.insert(key, color);
                (entry, color)
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

        quads
    }

    pub fn prepare_sprite_command(
        &mut self,
        command: &SpriteCommandBatch,
        pixels_per_point: f32,
    ) -> Vec<TexturedGlyphQuad> {
        let width = (command.rect.width() * pixels_per_point).ceil().max(1.0) as u32;
        let height = (command.rect.height() * pixels_per_point).ceil().max(1.0) as u32;
        let alpha = rasterize_sprite_commands(&command.commands, command.rect, width, height);
        let pixels = alpha_to_atlas_pixels(self.atlas.format(), alpha);
        let entry = self.atlas.insert_or_get(
            GlyphAtlasKey {
                face: ResolvedFontFace {
                    family: "Ghostty Sprite".to_owned(),
                    fallback_families: Vec::new(),
                    style: FontStyle::Regular,
                },
                text: command.ch.to_string(),
                font_size_bits: command.rect.height().to_bits(),
                pixels_per_point_bits: pixels_per_point.to_bits(),
                width,
                height,
            },
            width,
            height,
            pixels,
        );
        vec![TexturedGlyphQuad {
            rect: command.rect,
            uv: atlas_uv(self.atlas.size(), entry),
            color: command.color,
        }]
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

    pub fn atlas_resized_count(&self) -> u64 {
        self.atlas.resized_count()
    }

    pub fn atlas_format(&self) -> GlyphAtlasFormat {
        self.atlas.format()
    }
}

fn cluster_constraint_cells(
    previous: Option<&ShapedCluster>,
    cluster: &ShapedCluster,
    next: Option<&ShapedCluster>,
) -> u16 {
    if cluster.cells > 1 {
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
    database: fontdb::Database,
    fonts: HashMap<ResolvedFontFace, Option<FontArc>>,
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
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        Self {
            database,
            fonts: HashMap::new(),
        }
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
        if cluster.text.trim().is_empty() {
            return RasterizedCluster {
                pixels: vec![0; (width * height * format.depth()) as usize],
                color: false,
            };
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
        let metrics_font = self.font_for_face(face).unwrap_or_else(|| font.clone());

        let scale = PxScale::from((font_size * pixels_per_point).max(1.0));
        let scaled = font.as_scaled(scale);
        let metrics = font_face_metrics(&metrics_font, scale, constraint_cells, width, height);
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
        if let Some(alpha) = coretext::rasterize_symbol_cluster(
            face,
            cluster,
            font_size * pixels_per_point,
            metrics,
            constraint_cells,
            width,
            height,
        ) {
            return RasterizedCluster {
                pixels: alpha_to_atlas_pixels(format, alpha),
                color: false,
            };
        }
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

        if let Some(font) = load_font_supporting_char(&self.database, face, ch, physical_font_size)
        {
            return Some(font);
        }

        Some(font)
    }

    fn font_for_face(&mut self, face: &ResolvedFontFace) -> Option<FontArc> {
        if !self.fonts.contains_key(face) {
            let font = load_font(&self.database, face);
            self.fonts.insert(face.clone(), font);
        }
        self.fonts.get(face).cloned().flatten()
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

    if !is_private_use(ch) {
        return glyph;
    }

    let fit = (tile_width / bounds.width())
        .min(tile_height / bounds.height())
        .min(1.0);
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

fn is_symbol_like(ch: char) -> bool {
    is_private_use(ch)
        || matches!(
            ch as u32,
            0x2190..=0x21FF
                | 0x2460..=0x24FF
                | 0x2500..=0x259F
                | 0x2600..=0x27BF
                | 0x1F000..=0x1FAFF
        )
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
    cluster
        .text
        .chars()
        .any(|ch| ch == '\u{fe0f}' || matches!(ch as u32, 0x1F000..=0x1FAFF | 0x2600..=0x27BF))
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
    let id = database.query(&fontdb::Query {
        families,
        weight: match face.style {
            FontStyle::Bold | FontStyle::BoldItalic => fontdb::Weight::BOLD,
            FontStyle::Regular | FontStyle::Italic => fontdb::Weight::NORMAL,
        },
        style: match face.style {
            FontStyle::Italic | FontStyle::BoldItalic => fontdb::Style::Italic,
            FontStyle::Regular | FontStyle::Bold => fontdb::Style::Normal,
        },
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

fn load_font_supporting_char(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    ch: char,
    physical_font_size: f32,
) -> Option<FontArc> {
    if let Some(font) = load_coretext_fallback_font(database, face, ch, physical_font_size) {
        return Some(font);
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
        let Some(font) = database
            .with_face_data(face.id, |data, face_index| {
                FontVec::try_from_vec_and_index(data.to_vec(), face_index)
                    .ok()
                    .map(FontArc::new)
            })
            .flatten()
        else {
            continue;
        };
        if font_supports_char(&font, ch) {
            return Some(font);
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn load_coretext_fallback_font(
    database: &fontdb::Database,
    face: &ResolvedFontFace,
    ch: char,
    physical_font_size: f32,
) -> Option<FontArc> {
    let names = coretext::fallback_names(&face.family, ch, physical_font_size)?;
    let id = font_id_for_postscript_or_family(database, &names.postscript, &names.family, face)?;
    load_font_id(database, id)
}

#[cfg(not(target_os = "macos"))]
fn load_coretext_fallback_font(
    _database: &fontdb::Database,
    _face: &ResolvedFontFace,
    _ch: char,
    _physical_font_size: f32,
) -> Option<FontArc> {
    None
}

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
    if cluster.text.trim().is_empty() {
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
        min_x: entry.x as f32 / atlas_width as f32,
        min_y: entry.y as f32 / atlas_height as f32,
        max_x: (entry.x + entry.width) as f32 / atlas_width as f32,
        max_y: (entry.y + entry.height) as f32 / atlas_height as f32,
    }
}

#[cfg(test)]
mod tests;
