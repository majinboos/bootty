use crate::{
    geometry::{CellMetrics, DEFAULT_CELL_WIDTH, DEFAULT_FONT_SIZE, DEFAULT_LINE_HEIGHT},
    paint_plan::{TerminalPaintPlan, TextAttrs, TextRun},
    terminal_sprite::{SpriteFamily, SpriteGlyph, SpriteRegistry},
};
use std::{fmt, sync::Arc};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalTextConfig {
    pub families: Vec<String>,
    pub font_features: Vec<FontFeature>,
    pub codepoint_overrides: CodepointFontMap,
    pub font_size: f32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub baseline_adjustment: f32,
    pub underline_position: f32,
    pub underline_thickness: f32,
}

impl Default for TerminalTextConfig {
    fn default() -> Self {
        Self {
            families: vec![
                "Maple Mono".to_owned(),
                "Font Awesome 7 Brands".to_owned(),
                "Maple Mono NF".to_owned(),
                "Symbols Nerd Font Mono".to_owned(),
                "Symbols Nerd Font".to_owned(),
                "Font Awesome 6 Free".to_owned(),
                "Font Awesome 6 Pro".to_owned(),
                "FontAwesome".to_owned(),
                "monospace".to_owned(),
            ],
            font_features: default_font_features(),
            codepoint_overrides: CodepointFontMap::default(),
            font_size: DEFAULT_FONT_SIZE,
            cell_width: DEFAULT_CELL_WIDTH,
            cell_height: DEFAULT_LINE_HEIGHT,
            baseline_adjustment: 3.0,
            underline_position: 2.0,
            underline_thickness: 1.0,
        }
    }
}

impl TerminalTextConfig {
    pub fn with_cell_metrics(cell: CellMetrics) -> Self {
        Self {
            cell_width: cell.width,
            cell_height: cell.height,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TerminalFontMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    pub cell_baseline: u32,
    pub underline_position: u32,
    pub underline_thickness: u32,
    pub strikethrough_position: u32,
    pub strikethrough_thickness: u32,
    pub overline_position: i32,
    pub overline_thickness: u32,
    pub box_thickness: u32,
    pub cursor_height: u32,
    pub icon_height: f64,
    pub icon_height_single: f64,
    pub face_width: f64,
    pub face_height: f64,
    pub face_y: f64,
}

impl TerminalFontMetrics {
    pub fn apply(&mut self, modifiers: &TerminalMetricModifiers) {
        for (key, modifier) in &modifiers.entries {
            match key {
                MetricKey::CellWidth => {
                    self.cell_width = modifier.apply_u32(self.cell_width).max(1)
                }
                MetricKey::CellHeight => {
                    let original = self.cell_height;
                    let new = modifier.apply_u32(original).max(1);
                    if new != original {
                        self.cell_height = new;
                        self.adjust_height_dependent_metrics(original, new);
                    }
                }
                MetricKey::UnderlineThickness => {
                    self.underline_thickness = modifier.apply_u32(self.underline_thickness).max(1);
                }
                MetricKey::StrikethroughThickness => {
                    self.strikethrough_thickness =
                        modifier.apply_u32(self.strikethrough_thickness).max(1);
                }
                MetricKey::OverlineThickness => {
                    self.overline_thickness = modifier.apply_u32(self.overline_thickness).max(1);
                }
                MetricKey::BoxThickness => {
                    self.box_thickness = modifier.apply_u32(self.box_thickness).max(1);
                }
                MetricKey::CursorHeight => {
                    self.cursor_height = modifier.apply_u32(self.cursor_height).max(1);
                }
                MetricKey::IconHeight => {
                    self.icon_height = modifier.apply_f64(self.icon_height).max(1.0);
                    self.icon_height_single = modifier.apply_f64(self.icon_height_single).max(1.0);
                }
                MetricKey::FaceWidth => {
                    self.face_width = modifier.apply_f64(self.face_width).max(1.0)
                }
                MetricKey::FaceHeight => {
                    self.face_height = modifier.apply_f64(self.face_height).max(1.0);
                }
                MetricKey::CellBaseline => {
                    self.cell_baseline = modifier.apply_u32(self.cell_baseline)
                }
                MetricKey::UnderlinePosition => {
                    self.underline_position = modifier.apply_u32(self.underline_position);
                }
                MetricKey::StrikethroughPosition => {
                    self.strikethrough_position = modifier.apply_u32(self.strikethrough_position);
                }
                MetricKey::OverlinePosition => {
                    self.overline_position = modifier.apply_i32(self.overline_position);
                }
                MetricKey::FaceY => self.face_y = modifier.apply_f64(self.face_y),
            }
        }
    }

    fn adjust_height_dependent_metrics(&mut self, original: u32, new: u32) {
        let original_f64 = f64::from(original);
        let new_f64 = f64::from(new);
        let diff = new_f64 - original_f64;
        let half_diff = diff / 2.0;
        let centered_face_y = (original_f64 - self.face_height) / 2.0;
        let position_with_respect_to_center = self.face_y - centered_face_y;
        let (diff_top, diff_bottom) = if position_with_respect_to_center > 0.0 {
            (half_diff.ceil(), half_diff.floor())
        } else {
            (half_diff.floor(), half_diff.ceil())
        };

        self.cell_baseline = add_rounded_delta_to_u32(self.cell_baseline, diff_bottom);
        self.face_y += diff_bottom;
        self.underline_position = add_rounded_delta_to_u32(self.underline_position, diff_top);
        self.strikethrough_position =
            add_rounded_delta_to_u32(self.strikethrough_position, diff_top);
        self.overline_position = self.overline_position.saturating_add(diff_top as i32);
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalMetricModifiers {
    entries: Vec<(MetricKey, MetricModifier)>,
}

impl TerminalMetricModifiers {
    pub fn put(&mut self, key: MetricKey, modifier: MetricModifier) {
        self.entries.push((key, modifier));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetricKey {
    CellWidth,
    CellHeight,
    CellBaseline,
    UnderlinePosition,
    UnderlineThickness,
    StrikethroughPosition,
    StrikethroughThickness,
    OverlinePosition,
    OverlineThickness,
    BoxThickness,
    CursorHeight,
    IconHeight,
    FaceWidth,
    FaceHeight,
    FaceY,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MetricModifier {
    Percent(f64),
    Absolute(i32),
}

impl MetricModifier {
    pub fn parse(input: &str) -> Option<Self> {
        if input.is_empty() {
            return None;
        }
        if let Some(percent) = input.strip_suffix('%') {
            let value = percent.parse::<f64>().ok()? / 100.0;
            return Some(Self::Percent((1.0 + value).max(0.0)));
        }
        Some(Self::Absolute(input.parse::<i32>().ok()?))
    }

    pub fn format_config(self) -> String {
        match self {
            Self::Percent(value) => format!("{}%", (value - 1.0) * 100.0),
            Self::Absolute(value) => value.to_string(),
        }
    }

    fn apply_u32(self, value: u32) -> u32 {
        match self {
            Self::Percent(percent) => (f64::from(value) * percent.max(0.0)).round() as u32,
            Self::Absolute(delta) => {
                if delta >= 0 {
                    value.saturating_add(delta as u32)
                } else {
                    value.saturating_sub(delta.unsigned_abs())
                }
            }
        }
    }

    fn apply_i32(self, value: i32) -> i32 {
        match self {
            Self::Percent(percent) => (f64::from(value) * percent.max(0.0)).round() as i32,
            Self::Absolute(delta) => value.saturating_add(delta),
        }
    }

    fn apply_f64(self, value: f64) -> f64 {
        match self {
            Self::Percent(percent) => value * percent.max(0.0),
            Self::Absolute(delta) => value + f64::from(delta),
        }
    }
}

fn add_rounded_delta_to_u32(value: u32, delta: f64) -> u32 {
    if delta >= 0.0 {
        value.saturating_add(delta as u32)
    } else {
        value.saturating_sub((-delta) as u32)
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontFeature {
    tag: [u8; 4],
    value: u32,
}

impl FontFeature {
    pub const fn new(tag: [u8; 4], value: u32) -> Self {
        Self { tag, value }
    }

    pub fn parse(setting: &str) -> Option<Self> {
        let setting = setting.split_once(',').map_or(setting, |(head, _)| head);
        parse_font_feature_setting(setting)
    }

    pub fn tag(self) -> [u8; 4] {
        self.tag
    }

    pub fn value(self) -> u32 {
        self.value
    }

    fn tag_str(self) -> String {
        String::from_utf8_lossy(&self.tag).into_owned()
    }
}

impl fmt::Display for FontFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.value <= 1 {
            f.write_str(if self.value == 0 { "-" } else { "+" })?;
            f.write_str(&self.tag_str())
        } else {
            write!(f, "{}={}", self.tag_str(), self.value)
        }
    }
}

pub fn default_font_features() -> Vec<FontFeature> {
    vec![FontFeature::new(*b"liga", 1)]
}

pub fn parse_font_features(settings: &str) -> Vec<FontFeature> {
    settings
        .split(',')
        .filter_map(parse_font_feature_setting)
        .collect()
}

fn parse_font_feature_setting(setting: &str) -> Option<FontFeature> {
    let bytes = setting.as_bytes();
    let mut index = skip_space(bytes, 0);
    let mut prefixed_value = None;
    match bytes.get(index).copied() {
        Some(b'+') => {
            prefixed_value = Some(1);
            index += 1;
        }
        Some(b'-') => {
            prefixed_value = Some(0);
            index += 1;
        }
        _ => {}
    }

    let mut tag = [0_u8; 4];
    let mut len = 0_usize;
    while let Some(byte) = bytes.get(index).copied() {
        if byte == b'\'' || byte == b'"' {
            index += 1;
            continue;
        }
        if len == 4 || byte == b' ' || byte == b'\t' || byte == b'=' || byte == b',' {
            break;
        }
        tag[len] = byte;
        len += 1;
        index += 1;
    }
    if len != 4 {
        return None;
    }

    let mut rest = &setting[index..];
    loop {
        let trimmed = rest.trim_start_matches([' ', '\t', '\'', '"']);
        if trimmed.len() == rest.len() {
            break;
        }
        rest = trimmed;
    }

    let value = if let Some(value) = prefixed_value {
        if rest.trim_matches([' ', '\t']).is_empty() {
            value
        } else {
            return None;
        }
    } else if rest.trim_matches([' ', '\t']).is_empty() {
        1
    } else {
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.strip_prefix('=').map_or(rest, |value| value);
        parse_font_feature_value(rest.trim_matches([' ', '\t']))?
    };

    Some(FontFeature { tag, value })
}

fn parse_font_feature_value(value: &str) -> Option<u32> {
    match value {
        "on" | "ON" | "On" => Some(1),
        "off" | "OFF" | "Off" => Some(0),
        _ if value.bytes().all(|byte| byte.is_ascii_digit()) => value.parse().ok(),
        _ => None,
    }
}

fn skip_space(bytes: &[u8], mut index: usize) -> usize {
    while matches!(bytes.get(index), Some(b' ' | b'\t')) {
        index += 1;
    }
    index
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

    pub fn resolve_face_for_text(&self, attrs: &TextAttrs, text: &str) -> ResolvedFontFace {
        self.resolve_face_handle_for_text(attrs, text)
            .as_ref()
            .clone()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodepointPresentation {
    Any,
    Text,
    Emoji,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodepointResolution {
    Font(ResolvedFontFace),
    Sprite(SpriteFamily),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerminalCodepointResolver {
    font_resolver: FontResolver,
    enabled_styles: Vec<FontStyle>,
    sprite_registry: Option<SpriteRegistry>,
}

impl TerminalCodepointResolver {
    pub fn new(config: TerminalTextConfig) -> Self {
        Self {
            font_resolver: FontResolver::new(config),
            enabled_styles: vec![FontStyle::Regular, FontStyle::Bold, FontStyle::Italic],
            sprite_registry: None,
        }
    }

    pub fn with_sprite_registry(mut self, sprite_registry: SpriteRegistry) -> Self {
        self.sprite_registry = Some(sprite_registry);
        self
    }

    pub fn disable_style(&mut self, style: FontStyle) {
        self.enabled_styles.retain(|enabled| *enabled != style);
    }

    pub fn resolve(
        &self,
        ch: char,
        requested_style: FontStyle,
        presentation: CodepointPresentation,
    ) -> Option<CodepointResolution> {
        if let Some(registry) = self.sprite_registry
            && presentation != CodepointPresentation::Emoji
            && let Some(glyph) = registry.glyph_for(ch)
        {
            return Some(CodepointResolution::Sprite(glyph.family));
        }

        let style = if self.enabled_styles.contains(&requested_style) {
            requested_style
        } else {
            FontStyle::Regular
        };
        Some(CodepointResolution::Font(resolve_face_for_char_and_style(
            &self.font_resolver.config,
            Some(ch),
            style,
        )))
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
    pub config: TerminalTextConfig,
    pub resolver: FontResolver,
    pub native_symbol_policy: NativeSymbolPolicy,
    pub sprite_registry: SpriteRegistry,
}

impl TerminalTextContract {
    pub fn new(config: TerminalTextConfig, native_symbol_policy: NativeSymbolPolicy) -> Self {
        let resolver = FontResolver::new(config.clone());
        Self {
            config,
            resolver,
            native_symbol_policy,
            sprite_registry: SpriteRegistry::prompt_graphics(),
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

    pub fn shape_run(&self, run: &TextRun) -> ShapedTerminalText {
        let face = self.resolve_face_for_run(run);
        let fragments = self.shape_fragments(run);

        ShapedTerminalText { face, fragments }
    }

    pub fn resolve_face_for_run(&self, run: &TextRun) -> ResolvedFontFace {
        self.resolver.resolve_face_for_text(&run.attrs, &run.text)
    }

    pub fn resolve_face_handle_for_run(&self, run: &TextRun) -> Arc<ResolvedFontFace> {
        self.resolver
            .resolve_face_handle_for_text(&run.attrs, &run.text)
    }

    pub fn has_native_symbol_fragments(&self, text: &str) -> bool {
        if text.is_ascii() {
            return false;
        }
        text.chars().any(|ch| self.sprite_class(ch).is_some())
    }

    pub fn native_symbol_glyph(&self, ch: char) -> Option<SpriteGlyph> {
        self.native_symbol_policy.classify(ch)?;
        self.sprite_registry.glyph_for(ch)
    }

    pub fn shape_fragments(&self, run: &TextRun) -> Vec<TerminalTextFragment> {
        let mut fragments = Vec::new();
        let mut text = String::new();
        let mut text_start = 0_u16;

        let mut cell = 0_u16;
        for ch in run.text.chars() {
            if let Some(class) = self.sprite_class(ch) {
                if !text.is_empty() {
                    fragments.push(TerminalTextFragment::Text {
                        cell: text_start,
                        text: std::mem::take(&mut text),
                    });
                }
                fragments.push(TerminalTextFragment::NativeSymbol { cell, ch, class });
                cell = cell.saturating_add(terminal_char_width(ch));
                text_start = cell;
            } else {
                if text.is_empty() {
                    text_start = cell;
                }
                text.push(ch);
                cell = cell.saturating_add(terminal_char_width(ch));
            }
        }

        if !text.is_empty() {
            fragments.push(TerminalTextFragment::Text {
                cell: text_start,
                text,
            });
        }

        fragments
    }

    fn sprite_class(&self, ch: char) -> Option<NativeSymbolClass> {
        self.native_symbol_glyph(ch)
            .map(|glyph| native_symbol_class_for_family(glyph.family))
    }
}

pub fn terminal_text_config_for_plan(
    plan: &TerminalPaintPlan,
    base_config: &TerminalTextConfig,
) -> TerminalTextConfig {
    plan.text_runs
        .first()
        .map(|run| TerminalTextConfig {
            cell_width: run.rect.width() / f32::from(run.cells.max(1)),
            cell_height: run.rect.height(),
            ..base_config.clone()
        })
        .unwrap_or_else(|| base_config.clone())
}

fn native_symbol_class_for_family(family: SpriteFamily) -> NativeSymbolClass {
    match family {
        SpriteFamily::Powerline => NativeSymbolClass::Powerline,
        SpriteFamily::Separator => NativeSymbolClass::Separator,
        SpriteFamily::ProgressIndicator => NativeSymbolClass::ProgressIndicator,
        SpriteFamily::Block => NativeSymbolClass::Block,
        SpriteFamily::Shade => NativeSymbolClass::Shade,
        SpriteFamily::BoxDrawing => NativeSymbolClass::BoxDrawing,
        SpriteFamily::Braille => NativeSymbolClass::Braille,
        SpriteFamily::LegacyComputing => NativeSymbolClass::LegacyComputing,
        SpriteFamily::LegacyComputingSupplement => NativeSymbolClass::LegacyComputingSupplement,
        SpriteFamily::Special => NativeSymbolClass::Special,
    }
}

pub fn terminal_char_width(ch: char) -> u16 {
    UnicodeWidthChar::width(ch).unwrap_or(0) as u16
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapedTerminalText {
    pub face: ResolvedFontFace,
    pub fragments: Vec<TerminalTextFragment>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalTextFragment {
    Text {
        cell: u16,
        text: String,
    },
    NativeSymbol {
        cell: u16,
        ch: char,
        class: NativeSymbolClass,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geometry::SurfaceRect, paint_plan::PlanColor};

    fn attrs() -> TextAttrs {
        TextAttrs {
            fg: PlanColor {
                r: 220,
                g: 221,
                b: 222,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        }
    }

    fn run(text: &str) -> TextRun {
        TextRun {
            rect: SurfaceRect::from_min_size(0.0, 0.0, 18.0, 16.0),
            cells: 3,
            text: text.to_owned(),
            attrs: attrs(),
        }
    }

    #[test]
    fn default_terminal_text_config_matches_comparison_ghostty_font_stack() {
        let config = TerminalTextConfig::default();

        assert_eq!(config.families[0], "Maple Mono");
        assert_eq!(config.families[1], "Font Awesome 7 Brands");
        assert_eq!(config.families[2], "Maple Mono NF");
        assert_eq!(config.font_size, DEFAULT_FONT_SIZE);
        assert_eq!(config.baseline_adjustment, 3.0);
        assert!(
            config
                .families
                .iter()
                .any(|family| family == "Symbols Nerd Font Mono")
        );
        assert!(
            config
                .families
                .iter()
                .any(|family| family == "Font Awesome 6 Free")
        );
        assert_eq!(
            config.families.last().map(String::as_str),
            Some("monospace")
        );
        assert_eq!(config.font_features, vec![FontFeature::new(*b"liga", 1)]);
    }

    #[test]
    fn font_feature_settings_match_upstream_harfbuzz_syntax() {
        let kern_on = FontFeature::new(*b"kern", 1);
        for setting in [
            "kern",
            "kern, ",
            "kern on",
            "kern on, ",
            "+kern",
            "+kern, ",
            "\"kern\" = 1",
            "\"kern\" = 1, ",
        ] {
            assert_eq!(FontFeature::parse(setting), Some(kern_on), "{setting}");
        }

        let kern_off = FontFeature::new(*b"kern", 0);
        for setting in [
            "kern off",
            "kern off, ",
            "-'kern'",
            "-'kern', ",
            "\"kern\" = 0",
            "\"kern\" = 0, ",
        ] {
            assert_eq!(FontFeature::parse(setting), Some(kern_off), "{setting}");
        }

        let aalt_2 = FontFeature::new(*b"aalt", 2);
        for setting in ["aalt=2", "aalt=2, ", "'aalt' 2", "'aalt'\t2, "] {
            assert_eq!(FontFeature::parse(setting), Some(aalt_2), "{setting}");
        }

        for invalid in [
            "aalt=2x",
            "toolong",
            "sht",
            "-kern 1",
            "-kern on",
            "aalt=o,",
            "aalt=ofn,",
        ] {
            assert_eq!(FontFeature::parse(invalid), None, "{invalid}");
        }

        let features = parse_font_features(
            "  kern, kern on , +kern, \"kern\"  = 1,\
             kern    off, -'kern' , \"kern\"=0,\
             aalt=2,  'aalt'\t2,\
             aalt=2x, toolong, sht, -kern 1, -kern on, aalt=o, aalt=ofn,\
             last",
        );
        let expected = [
            vec![kern_on; 4],
            vec![kern_off; 3],
            vec![aalt_2; 2],
            vec![FontFeature::new(*b"last", 1)],
        ]
        .concat();
        assert_eq!(features, expected);
        assert_eq!(kern_on.to_string(), "+kern");
        assert_eq!(kern_off.to_string(), "-kern");
        assert_eq!(aalt_2.to_string(), "aalt=2");
    }

    #[test]
    fn font_metric_modifier_ports_parse_apply_and_format_cases() {
        for (input, expected) in [
            ("100", MetricModifier::Absolute(100)),
            ("-100", MetricModifier::Absolute(-100)),
            ("20%", MetricModifier::Percent(1.2)),
            ("-20%", MetricModifier::Percent(0.8)),
            ("0%", MetricModifier::Percent(1.0)),
        ] {
            assert_eq!(MetricModifier::parse(input), Some(expected), "{input}");
        }

        for (input, expected) in [("24%", "24%"), ("-30", "-30")] {
            assert_eq!(
                MetricModifier::parse(input).unwrap().format_config(),
                expected
            );
        }

        for (modifier, expected) in [
            (MetricModifier::Percent(0.8), 80),
            (MetricModifier::Percent(1.8), 180),
            (MetricModifier::Absolute(-100), 0),
            (MetricModifier::Absolute(-120), 0),
            (MetricModifier::Absolute(100), 200),
        ] {
            assert_eq!(modifier.apply_u32(100), expected);
        }
    }

    #[test]
    fn font_metrics_apply_modifiers_ports_width_case() {
        let mut modifiers = TerminalMetricModifiers::default();
        modifiers.put(MetricKey::CellWidth, MetricModifier::Percent(1.2));
        let mut metrics = test_metrics();

        metrics.apply(&modifiers);

        assert_eq!(metrics.cell_width, 120);
    }

    #[test]
    fn font_metrics_apply_modifiers_ports_cell_height_adjustments() {
        let mut smaller = TerminalMetricModifiers::default();
        smaller.put(MetricKey::CellHeight, MetricModifier::Percent(0.75));
        let mut metrics = test_metrics();
        metrics.apply(&smaller);

        assert_f64_eq(metrics.face_y, -12.67);
        assert_eq!(metrics.cell_height, 75);
        assert_eq!(metrics.cell_baseline, 37);
        assert_eq!(metrics.underline_position, 43);
        assert_eq!(metrics.strikethrough_position, 18);
        assert_eq!(metrics.overline_position, -12);
        assert_eq!(metrics.cursor_height, 100);

        let mut larger = TerminalMetricModifiers::default();
        larger.put(MetricKey::CellHeight, MetricModifier::Percent(1.75));
        let mut metrics = test_metrics();
        metrics.apply(&larger);

        assert_f64_eq(metrics.face_y, 37.33);
        assert_eq!(metrics.cell_height, 175);
        assert_eq!(metrics.cell_baseline, 87);
        assert_eq!(metrics.underline_position, 93);
        assert_eq!(metrics.strikethrough_position, 68);
        assert_eq!(metrics.overline_position, 38);
        assert_eq!(metrics.cursor_height, 100);
    }

    #[test]
    fn font_metrics_apply_modifiers_ports_icon_height_adjustments() {
        let mut percent = TerminalMetricModifiers::default();
        percent.put(MetricKey::IconHeight, MetricModifier::Percent(0.75));
        let mut metrics = test_metrics();
        metrics.apply(&percent);

        assert_f64_eq(metrics.icon_height, 75.0);
        assert_f64_eq(metrics.icon_height_single, 60.0);
        assert_f64_eq(metrics.face_height, 99.67);
        assert_f64_eq(metrics.face_y, 0.33);

        let mut absolute = TerminalMetricModifiers::default();
        absolute.put(MetricKey::IconHeight, MetricModifier::Absolute(-5));
        let mut metrics = test_metrics();
        metrics.apply(&absolute);

        assert_f64_eq(metrics.icon_height, 95.0);
        assert_f64_eq(metrics.icon_height_single, 75.0);
        assert_f64_eq(metrics.face_height, 99.67);
        assert_f64_eq(metrics.face_y, 0.33);
    }

    #[test]
    fn codepoint_font_map_matches_upstream_priority_semantics() {
        let mut map = CodepointFontMap::default();
        assert_eq!(map.family_for('A'), None);

        map.add('A'..='A', "A");
        assert_eq!(map.family_for('A'), Some("A"));
        assert_eq!(map.family_for('B'), None);

        map.add('A'..='B', "B");
        assert_eq!(map.family_for('A'), Some("B"));
        assert_eq!(map.family_for('B'), Some("B"));
        assert_eq!(map.family_for('@'), None);

        map.add('C'..='D', "C");
        map.add('E'..='F', "D");
        assert_eq!(map.family_for('C'), Some("C"));
        assert_eq!(map.family_for('D'), Some("C"));
        assert_eq!(map.family_for('E'), Some("D"));
        assert_eq!(map.family_for('F'), Some("D"));
        assert_eq!(map.family_for('G'), None);
    }

    #[test]
    fn codepoint_resolver_ports_visible_ascii_presentation_and_sprite_cases() {
        let mut config = TerminalTextConfig {
            families: vec!["Regular Mono".to_owned()],
            ..TerminalTextConfig::default()
        };
        config.codepoint_overrides.add('🥸'..='🥸', "Emoji Color");
        config.codepoint_overrides.add('✌'..='✌', "Emoji Text");
        let resolver = TerminalCodepointResolver::new(config);

        for ch in ' '..='~' {
            assert_eq!(
                resolver.resolve(ch, FontStyle::Regular, CodepointPresentation::Any),
                Some(CodepointResolution::Font(ResolvedFontFace {
                    family: "Regular Mono".to_owned(),
                    fallback_families: Vec::new(),
                    style: FontStyle::Regular,
                })),
                "{ch}"
            );
        }
        assert_eq!(
            resolver.resolve('🥸', FontStyle::Regular, CodepointPresentation::Any),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Emoji Color".to_owned(),
                fallback_families: vec!["Regular Mono".to_owned()],
                style: FontStyle::Regular,
            }))
        );
        assert_eq!(
            resolver.resolve('✌', FontStyle::Regular, CodepointPresentation::Text),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Emoji Text".to_owned(),
                fallback_families: vec!["Regular Mono".to_owned()],
                style: FontStyle::Regular,
            }))
        );
        assert_eq!(
            resolver.resolve('─', FontStyle::Regular, CodepointPresentation::Any),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Regular Mono".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Regular,
            }))
        );

        let sprite_resolver = TerminalCodepointResolver::new(TerminalTextConfig {
            families: vec!["Regular Mono".to_owned()],
            ..TerminalTextConfig::default()
        })
        .with_sprite_registry(SpriteRegistry::prompt_graphics());

        assert_eq!(
            sprite_resolver.resolve('─', FontStyle::Regular, CodepointPresentation::Any),
            Some(CodepointResolution::Sprite(SpriteFamily::BoxDrawing))
        );
    }

    #[test]
    fn text_contract_detects_when_native_symbol_shaping_is_needed() {
        let contract = TerminalTextContract::new(
            TerminalTextConfig {
                families: vec!["Regular Mono".to_owned()],
                ..TerminalTextConfig::default()
            },
            NativeSymbolPolicy::terminal_glyph_primitives(),
        );

        assert!(!contract.has_native_symbol_fragments("ordinary ascii"));
        assert!(!contract.has_native_symbol_fragments("ASCII !@#$%^&*() 0123456789"));
        assert!(contract.has_native_symbol_fragments("box ─ line"));
    }

    #[test]
    fn codepoint_resolver_ports_disabled_style_fallback() {
        let mut resolver = TerminalCodepointResolver::new(TerminalTextConfig {
            families: vec!["Regular Mono".to_owned()],
            ..TerminalTextConfig::default()
        });
        resolver.disable_style(FontStyle::Bold);

        assert_eq!(
            resolver.resolve('A', FontStyle::Regular, CodepointPresentation::Any),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Regular Mono".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Regular,
            }))
        );
        assert_eq!(
            resolver.resolve('A', FontStyle::Bold, CodepointPresentation::Any),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Regular Mono".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Regular,
            }))
        );
        assert_eq!(
            resolver.resolve('A', FontStyle::Italic, CodepointPresentation::Any),
            Some(CodepointResolution::Font(ResolvedFontFace {
                family: "Regular Mono".to_owned(),
                fallback_families: Vec::new(),
                style: FontStyle::Italic,
            }))
        );
    }

    #[test]
    fn font_resolver_selects_ordered_family_fallback_and_style_face() {
        let resolver = FontResolver::new(TerminalTextConfig {
            families: vec!["Maple Mono NF".to_owned(), "Symbols Nerd Font".to_owned()],
            ..TerminalTextConfig::default()
        });

        let mut bold_italic = attrs();
        bold_italic.bold = true;
        bold_italic.italic = true;

        let face = resolver.resolve_face(&bold_italic);

        assert_eq!(face.family, "Maple Mono NF");
        assert_eq!(face.fallback_families, vec!["Symbols Nerd Font"]);
        assert_eq!(face.style, FontStyle::BoldItalic);
    }

    #[test]
    fn font_resolver_applies_codepoint_overrides_before_fallbacks() {
        let mut config = TerminalTextConfig {
            families: vec!["Default Mono".to_owned(), "Fallback Mono".to_owned()],
            ..TerminalTextConfig::default()
        };
        config.codepoint_overrides.add('界'..='界', "CJK Override");
        let resolver = FontResolver::new(config);

        let face = resolver.resolve_face_for_text(&attrs(), "界");
        assert_eq!(face.family, "CJK Override");
        assert_eq!(
            face.fallback_families,
            vec!["Default Mono".to_owned(), "Fallback Mono".to_owned()]
        );

        let default_face = resolver.resolve_face_for_text(&attrs(), "A");
        assert_eq!(default_face.family, "Default Mono");
        assert_eq!(default_face.fallback_families, vec!["Fallback Mono"]);
    }

    #[test]
    fn shaping_contract_uses_codepoint_override_for_text_face() {
        let mut config = TerminalTextConfig {
            families: vec!["Default Mono".to_owned()],
            ..TerminalTextConfig::default()
        };
        config.codepoint_overrides.add('界'..='界', "CJK Override");
        let contract = TerminalTextContract::new(config, NativeSymbolPolicy::font_only());

        let shaped = contract.shape_run(&run("界"));

        assert_eq!(shaped.face.family, "CJK Override");
    }

    #[test]
    fn native_symbol_policy_classifies_terminal_symbols_explicitly() {
        let policy = NativeSymbolPolicy::default();

        for (ch, class) in [
            ('█', NativeSymbolClass::Block),
            ('▒', NativeSymbolClass::Shade),
            ('▘', NativeSymbolClass::Quadrant),
            ('─', NativeSymbolClass::BoxDrawing),
            ('\u{E0B0}', NativeSymbolClass::Powerline),
            ('❯', NativeSymbolClass::Separator),
        ] {
            assert_eq!(policy.classify(ch), Some(class));
        }
        assert_eq!(policy.classify('A'), None);
    }

    #[test]
    fn shaping_contract_splits_text_from_native_symbols_by_cell() {
        let config = TerminalTextConfig::default();
        let contract = TerminalTextContract::new(config, NativeSymbolPolicy::default());

        let shaped = contract.shape_run(&run("a█b"));

        assert_eq!(
            shaped.fragments,
            vec![
                TerminalTextFragment::Text {
                    cell: 0,
                    text: "a".to_owned()
                },
                TerminalTextFragment::NativeSymbol {
                    cell: 1,
                    ch: '█',
                    class: NativeSymbolClass::Block
                },
                TerminalTextFragment::Text {
                    cell: 2,
                    text: "b".to_owned()
                }
            ]
        );
    }

    #[test]
    fn shaping_keeps_native_symbol_cell_after_combining_mark() {
        let config = TerminalTextConfig::default();
        let contract = TerminalTextContract::new(config, NativeSymbolPolicy::default());

        let shaped = contract.shape_run(&run("e\u{301}█"));

        assert_eq!(
            shaped.fragments,
            vec![
                TerminalTextFragment::Text {
                    cell: 0,
                    text: "e\u{301}".to_owned()
                },
                TerminalTextFragment::NativeSymbol {
                    cell: 1,
                    ch: '█',
                    class: NativeSymbolClass::Block
                }
            ]
        );
    }

    #[test]
    fn terminal_text_cells_stream_positioned_borrowed_clusters() {
        assert_terminal_text_cells(
            "ab好e\u{301}",
            &[(0, "a"), (1, "b"), (2, "好"), (4, "e\u{301}")],
        );
    }

    #[test]
    fn terminal_text_cells_treat_ambiguous_width_symbols_as_single_cells() {
        assert_terminal_text_cells("Ω·a", &[(0, "Ω"), (1, "·"), (2, "a")]);
    }

    #[test]
    fn nerd_status_glyphs_stay_font_rendered_for_fallback_resolution() {
        let contract = TerminalTextContract::new(
            TerminalTextConfig::default(),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        );

        let shaped = contract.shape_run(&run("main \u{F126}"));

        assert_eq!(
            shaped.fragments,
            vec![TerminalTextFragment::Text {
                cell: 0,
                text: "main \u{F126}".to_owned()
            }]
        );
    }

    #[test]
    fn geometric_shape_symbols_stay_font_rendered() {
        let contract = TerminalTextContract::new(
            TerminalTextConfig::default(),
            NativeSymbolPolicy::terminal_glyph_primitives(),
        );

        let shaped = contract.shape_run(&run("▼ ○"));

        assert_eq!(
            shaped.fragments,
            vec![TerminalTextFragment::Text {
                cell: 0,
                text: "▼ ○".to_owned()
            }]
        );
    }

    #[test]
    fn terminal_text_cells_attach_leading_combining_marks_forward() {
        assert_terminal_text_cells("\u{301}a", &[(0, "\u{301}a")]);
    }

    fn assert_terminal_text_cells(input: &str, expected: &[(u16, &str)]) {
        let mut cells = Vec::new();

        for_terminal_text_cells(input, |cell, text| {
            cells.push((cell, text.to_owned()));
        });

        assert_eq!(
            cells,
            expected
                .iter()
                .map(|(cell, text)| (*cell, (*text).to_owned()))
                .collect::<Vec<_>>()
        );
    }

    fn test_metrics() -> TerminalFontMetrics {
        TerminalFontMetrics {
            cell_width: 100,
            cell_height: 100,
            cell_baseline: 50,
            underline_position: 55,
            underline_thickness: 1,
            strikethrough_position: 30,
            strikethrough_thickness: 1,
            overline_position: 0,
            overline_thickness: 1,
            box_thickness: 1,
            cursor_height: 100,
            icon_height: 100.0,
            icon_height_single: 80.0,
            face_width: 100.0,
            face_height: 99.67,
            face_y: 0.33,
        }
    }

    fn assert_f64_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "{actual} != {expected}"
        );
    }
}
