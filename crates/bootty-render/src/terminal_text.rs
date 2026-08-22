use crate::{
    geometry::{CellMetrics, DEFAULT_FONT_SIZE},
    paint_plan::{TerminalPaintPlan, TextAttrs, TextRun},
    terminal_sprite::SpriteGlyph,
};
pub use bootty_font::FontFeature;
use std::sync::Arc;
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTextConfig {
    pub families: Vec<String>,
    pub font_features: Vec<FontFeature>,
    pub codepoint_overrides: CodepointFontMap,
    pub font_size: f32,
    pub cell_width: Option<f32>,
    pub cell_height: Option<f32>,
    pub fit_cell_height: bool,
    pub fit_cell_width: bool,
    pub baseline_adjustment: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
}

impl Default for TerminalTextConfig {
    fn default() -> Self {
        Self {
            families: vec!["monospace".to_owned()],
            font_features: default_font_features(),
            codepoint_overrides: CodepointFontMap::default(),
            font_size: DEFAULT_FONT_SIZE,
            cell_width: None,
            cell_height: None,
            fit_cell_height: true,
            fit_cell_width: false,
            baseline_adjustment: 3.0,
            underline_position: 2.0,
            underline_thickness: 1.0,
        }
    }
}

impl TerminalTextConfig {
    pub fn with_cell_metrics(cell: CellMetrics) -> Self {
        Self {
            cell_width: Some(cell.width),
            cell_height: Some(cell.height),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodepointFontMap {
    entries: Vec<CodepointFontEntry>,
}

impl CodepointFontMap {
    pub fn add(&mut self, range: std::ops::RangeInclusive<char>, family: impl Into<String>) {
        let start = u32::from(*range.start());
        let end = u32::from(*range.end());
        assert!(start <= end, "codepoint override range must be ordered");
        self.entries.push(CodepointFontEntry {
            start,
            end,
            family: family.into(),
        });
    }

    pub fn family_for(&self, ch: char) -> Option<&str> {
        let codepoint = u32::from(ch);
        self.entries
            .iter()
            .rev()
            .find(|entry| entry.start <= codepoint && codepoint <= entry.end)
            .map(|entry| entry.family.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodepointFontEntry {
    start: u32,
    end: u32,
    family: String,
}

pub fn default_font_features() -> Vec<FontFeature> {
    vec![FontFeature::new(*b"liga", 1)]
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontResolver {
    config: TerminalTextConfig,
    default_faces: [Arc<ResolvedFontFace>; 4],
}

impl FontResolver {
    pub fn new(config: TerminalTextConfig) -> Self {
        let default_faces = std::array::from_fn(|index| {
            Arc::new(resolve_face_for_char_and_style(
                &config,
                None,
                FontStyle::from_index(index),
            ))
        });
        Self {
            config,
            default_faces,
        }
    }

    pub fn resolve_face(&self, attrs: &TextAttrs) -> ResolvedFontFace {
        self.resolve_face_handle(attrs, None).as_ref().clone()
    }

    pub fn resolve_face_handle_for_text(
        &self,
        attrs: &TextAttrs,
        text: &str,
    ) -> Arc<ResolvedFontFace> {
        self.resolve_face_handle(attrs, text.chars().find(|ch| terminal_char_width(*ch) > 0))
    }

    fn resolve_face_handle(&self, attrs: &TextAttrs, ch: Option<char>) -> Arc<ResolvedFontFace> {
        let style = FontStyle::from_attrs(attrs);
        if ch
            .and_then(|ch| self.config.codepoint_overrides.family_for(ch))
            .is_none()
        {
            return Arc::clone(&self.default_faces[style.index()]);
        }
        Arc::new(resolve_face_for_char_and_style(&self.config, ch, style))
    }
}

fn resolve_face_for_char_and_style(
    config: &TerminalTextConfig,
    ch: Option<char>,
    style: FontStyle,
) -> ResolvedFontFace {
    let mut families = config.families.iter();
    let default_family = families
        .next()
        .cloned()
        .unwrap_or_else(|| "monospace".to_owned());
    let override_family = ch.and_then(|ch| config.codepoint_overrides.family_for(ch));
    let family = override_family
        .map(str::to_owned)
        .unwrap_or_else(|| default_family.clone());
    let fallback_families = if override_family.is_some() {
        std::iter::once(default_family)
            .chain(families.cloned())
            .collect()
    } else {
        families.cloned().collect()
    };
    ResolvedFontFace {
        family,
        fallback_families,
        style,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResolvedFontFace {
    pub family: String,
    pub fallback_families: Vec<String>,
    pub style: FontStyle,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FontStyle {
    #[default]
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl FontStyle {
    fn from_index(index: usize) -> Self {
        match index {
            0 => Self::Regular,
            1 => Self::Bold,
            2 => Self::Italic,
            _ => Self::BoldItalic,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Regular => 0,
            Self::Bold => 1,
            Self::Italic => 2,
            Self::BoldItalic => 3,
        }
    }

    fn from_attrs(attrs: &TextAttrs) -> Self {
        match (attrs.bold, attrs.italic) {
            (true, true) => Self::BoldItalic,
            (true, false) => Self::Bold,
            (false, true) => Self::Italic,
            (false, false) => Self::Regular,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NativeSymbolPolicy {
    blocks: bool,
    shades: bool,
    quadrants: bool,
    box_drawing: bool,
    powerline: bool,
    progress_indicators: bool,
    separators: bool,
    braille: bool,
    legacy: bool,
    special: bool,
}

impl NativeSymbolPolicy {
    pub fn font_only() -> Self {
        Self {
            blocks: false,
            shades: false,
            quadrants: false,
            box_drawing: false,
            powerline: false,
            progress_indicators: false,
            separators: false,
            braille: false,
            legacy: false,
            special: false,
        }
    }
    pub fn terminal_glyph_primitives() -> Self {
        Self {
            blocks: true,
            shades: true,
            quadrants: true,
            box_drawing: true,
            powerline: true,
            progress_indicators: true,
            separators: true,
            braille: true,
            legacy: true,
            special: true,
        }
    }
    pub fn classify(self, ch: char) -> Option<NativeSymbolClass> {
        let class = match ch {
            '▀'..='▐' | '▔' | '▕' if self.blocks => NativeSymbolClass::Block,
            '░' | '▒' | '▓' if self.shades => NativeSymbolClass::Shade,
            '▖'..='▟' if self.quadrants => NativeSymbolClass::Quadrant,
            '─'..='╿' if self.box_drawing => NativeSymbolClass::BoxDrawing,
            '\u{E0B0}'..='\u{E0D7}' if self.powerline => NativeSymbolClass::Powerline,
            '\u{EE00}'..='\u{EE0B}' if self.progress_indicators => {
                NativeSymbolClass::ProgressIndicator
            }
            '❯' | '❮' | '' | '' if self.separators => NativeSymbolClass::Separator,
            '\u{2800}'..='\u{28FF}' if self.braille => NativeSymbolClass::Braille,
            '\u{1FB00}'..='\u{1FBFF}' if self.legacy => NativeSymbolClass::LegacyComputing,
            '\u{1CC00}'..='\u{1CEBF}' if self.legacy => {
                NativeSymbolClass::LegacyComputingSupplement
            }
            '\u{F5D0}'..='\u{F60D}' if self.special => NativeSymbolClass::Special,
            _ => return None,
        };
        Some(class)
    }
}

impl Default for NativeSymbolPolicy {
    fn default() -> Self {
        Self {
            blocks: true,
            shades: true,
            quadrants: true,
            box_drawing: true,
            powerline: true,
            progress_indicators: true,
            separators: true,
            braille: true,
            legacy: true,
            special: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeSymbolClass {
    Block,
    Shade,
    Quadrant,
    BoxDrawing,
    Powerline,
    ProgressIndicator,
    Separator,
    Braille,
    LegacyComputing,
    LegacyComputingSupplement,
    Special,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTextContract {
    pub(crate) config: TerminalTextConfig,
    pub(crate) font_features: Arc<[FontFeature]>,
    pub(crate) resolver: FontResolver,
    pub(crate) native_symbol_policy: NativeSymbolPolicy,
}

impl TerminalTextContract {
    pub fn new(config: TerminalTextConfig, native_symbol_policy: NativeSymbolPolicy) -> Self {
        let resolver = FontResolver::new(config.clone());
        let font_features = Arc::from(config.font_features.clone());
        Self {
            config,
            font_features,
            resolver,
            native_symbol_policy,
        }
    }

    pub fn for_terminal_paint_plan(
        plan: &TerminalPaintPlan,
        base_config: &TerminalTextConfig,
    ) -> Self {
        Self::new(
            terminal_text_config_for_plan(plan, base_config),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        )
    }

    pub fn resolve_face_handle_for_run(&self, run: &TextRun) -> Arc<ResolvedFontFace> {
        self.resolver
            .resolve_face_handle_for_text(&run.attrs, &run.text)
    }

    pub fn has_native_symbol_fragments(&self, text: &str) -> bool {
        if text.is_ascii() {
            return false;
        }
        text.chars()
            .any(|ch| self.native_symbol_glyph(ch).is_some())
    }

    pub fn native_symbol_glyph(&self, ch: char) -> Option<SpriteGlyph> {
        self.native_symbol_policy.classify(ch)?;
        SpriteGlyph::from_char(ch)
    }
}

pub fn terminal_text_config_for_plan(
    plan: &TerminalPaintPlan,
    base_config: &TerminalTextConfig,
) -> TerminalTextConfig {
    plan.text_runs
        .first()
        .map(|run| TerminalTextConfig {
            cell_width: Some(run.rect.width() / f32::from(run.cells.max(1))),
            cell_height: Some(run.rect.height()),
            ..base_config.clone()
        })
        .unwrap_or_else(|| base_config.clone())
}

pub fn terminal_char_width(ch: char) -> u16 {
    UnicodeWidthChar::width(ch).unwrap_or(0) as u16
}

/// Per-character contribution to a running, grapheme-cluster-aware cell total. A VS16 (U+FE0F)
/// makes its preceding character's cluster two cells instead of one; summing `terminal_char_width`
/// per character alone undercounts it (the base contributes 1, the selector itself measures 0),
/// desyncing the running total from `terminal_grapheme_cells` and squeezing everything after the
/// emoji into space reserved for one cell instead of two. FE0F contributes the missing cell here.
pub fn terminal_char_cell_delta(ch: char) -> u16 {
    if ch == '\u{FE0F}' {
        1
    } else {
        terminal_char_width(ch)
    }
}

/// Cells occupied by one grapheme cluster, matching libghostty's grid under grapheme-cluster mode
/// (DEC 2027, which bootty enables by default). A U+FE0F (VS16) emoji-presentation sequence
/// (⚠️ ❤️ ☺️) is two cells even though its base symbol measures one — `UnicodeWidthStr::width`
/// reports that whole-cluster width, while a per-char sum (base + zero-width selector) would not.
/// Every cell/run width site must use this one measure or the planner and shaper disagree and the
/// emoji flickers between one and two cells frame to frame.
pub fn terminal_grapheme_cells(chars: &[char]) -> u16 {
    if chars.contains(&'\u{FE0F}') {
        return 2;
    }
    match chars {
        [] => 1,
        [ch] => terminal_char_width(*ch).max(1),
        _ => chars
            .iter()
            .copied()
            .map(terminal_char_width)
            .sum::<u16>()
            .max(1),
    }
}

pub fn for_terminal_text_cells(text: &str, mut emit: impl FnMut(u16, &str)) {
    let mut current_start = None;
    let mut current_cell = 0_u16;
    let mut current_has_advance = false;
    let mut cursor = 0_u16;

    for (index, ch) in text.char_indices() {
        let width = terminal_char_width(ch);
        if width == 0 {
            if current_start.is_none() {
                current_start = Some(index);
                current_cell = cursor;
                current_has_advance = false;
            }
            // A zero-width char (e.g. a combining mark) stays in the current group, but a VS16
            // still consumes an extra cell (its cluster is two cells, not the base's one) — advance
            // the cursor by that delta so the next group starts at the right column.
            cursor = cursor.saturating_add(terminal_char_cell_delta(ch));
            continue;
        }

        match current_start {
            Some(start) if current_has_advance => {
                emit(current_cell, &text[start..index]);
                current_start = Some(index);
                current_cell = cursor;
            }
            Some(_) => {
                current_cell = cursor;
            }
            None => {
                current_start = Some(index);
                current_cell = cursor;
            }
        }
        current_has_advance = true;
        cursor = cursor.saturating_add(width);
    }

    if let Some(start) = current_start {
        emit(current_cell, &text[start..]);
    }
}
