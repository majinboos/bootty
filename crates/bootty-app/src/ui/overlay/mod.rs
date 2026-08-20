//! Shared framework for floating windows (session pickers, prompts, menus).
//!
//! [`FloatingWindow`] draws the normalized chrome — a dimming scrim, a centered
//! rounded panel with an icon+title header and an optional footer — and hands an
//! inner `Ui` to the caller's body, which typically hosts a [`ListView`] (or, in
//! later windows, a text prompt or action menu). The body decides what closes
//! the window via the returned [`OverlayResult`].

pub mod list;
pub mod menu;
pub mod prompt;

use bootty_ui::{Theme, ThemePalette};
use eframe::egui::{self, Color32, CornerRadius, Stroke};

use crate::ui::{icons, keycaps};

pub use list::{ListOutcome, ListRow, ListView};
pub use menu::{ActionItem, ActionMenu, ActionMenuOutcome, ActionRisk, StatusLine};
pub use prompt::{PromptOutcome, TextPrompt};

/// What a [`FloatingWindow`] produced for one frame.
pub struct OverlayResult<R> {
    /// The body closure's return value.
    pub inner: R,
    /// Escape was pressed this frame.
    pub escaped: bool,
    /// The scrim (anywhere outside the panel) was clicked this frame.
    pub clicked_outside: bool,
}

enum HeaderHint {
    Text(String),
    Shortcuts(Vec<(String, String)>),
}

/// A centered modal overlay with normalized chrome. Build it per frame, then
/// [`FloatingWindow::show`] it with a body closure.
pub struct FloatingWindow {
    id: egui::Id,
    title: String,
    icon: Option<String>,
    hint: Option<HeaderHint>,
    footer: Option<String>,
    width: f32,
}

fn floating_window_orders() -> (egui::Order, egui::Order) {
    (egui::Order::Foreground, egui::Order::Foreground)
}

impl FloatingWindow {
    pub fn new(
        id_source: impl std::hash::Hash + std::fmt::Debug,
        title: impl Into<String>,
    ) -> Self {
        Self {
            id: egui::Id::new(id_source),
            title: title.into(),
            icon: None,
            hint: None,
            footer: None,
            width: 720.0,
        }
    }

    /// Leading header icon slug (resolved through `ui::icons`).
    #[must_use]
    pub fn icon(mut self, slug: impl Into<String>) -> Self {
        self.icon = Some(slug.into());
        self
    }

    /// Right-aligned header key hint, e.g. `"Enter select   Esc close"`.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        self.hint = (!hint.is_empty()).then_some(HeaderHint::Text(hint));
        self
    }

    /// Right-aligned header shortcuts rendered with the shared keycap preview style.
    #[must_use]
    pub fn shortcut_hint(
        mut self,
        sections: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> Self {
        let sections = sections
            .into_iter()
            .map(|(trigger, label)| (trigger.to_owned(), label.to_owned()))
            .collect::<Vec<_>>();
        self.hint = (!sections.is_empty()).then_some(HeaderHint::Shortcuts(sections));
        self
    }

    /// Footer status line, drawn under a rule below the body.
    #[must_use]
    pub fn footer(mut self, footer: impl Into<String>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn show<R>(
        self,
        ctx: &egui::Context,
        theme: Theme,
        add_body: impl FnOnce(&mut egui::Ui, ThemePalette) -> R,
    ) -> OverlayResult<R> {
        let palette = theme.palette;
        let screen = ctx.input(|input| input.content_rect());

        let (scrim_order, panel_order) = floating_window_orders();
        // Keep both areas at Foreground and show the panel after the scrim. Widget popups
        // also use Foreground, so they can then render above their parent panel.
        // Order::Tooltip would place the panel above ComboBox popups.
        // A click that reaches the scrim landed outside the panel.
        let clicked_outside = egui::Area::new(self.id.with("scrim"))
            .order(scrim_order)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let response = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(0, 0, 0, 150),
                );
                response.clicked()
            })
            .inner;

        let mut body = None;
        egui::Area::new(self.id)
            .order(panel_order)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                bootty_ui::configure_style(ui.style_mut(), theme);
                egui::Frame::popup(ui.style())
                    .fill(palette.pane)
                    .stroke(Stroke::new(1.0, palette.border))
                    .corner_radius(CornerRadius::same(palette.radius))
                    .inner_margin(egui::Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        ui.set_width(self.width);
                        self.header(ui, palette);
                        rule(ui, palette);
                        ui.add_space(8.0);
                        body = Some(add_body(ui, palette));
                        if let Some(footer) = &self.footer {
                            ui.add_space(6.0);
                            rule(ui, palette);
                            ui.add_space(6.0);
                            ui.label(
                                egui::RichText::new(footer)
                                    .monospace()
                                    .size(12.0)
                                    .color(palette.muted),
                            );
                        }
                    });
            });

        let escaped = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        OverlayResult {
            inner: body.expect("floating-window body always runs"),
            escaped,
            clicked_outside,
        }
    }

    fn header(&self, ui: &mut egui::Ui, palette: ThemePalette) {
        ui.horizontal(|ui| {
            if let Some(slug) = &self.icon
                && let Some(text) = icons::icon_text(slug, 16.0, palette.warning)
            {
                ui.label(text);
                ui.add_space(2.0);
            }
            ui.label(
                egui::RichText::new(&self.title)
                    .monospace()
                    .size(15.0)
                    .color(palette.warning),
            );
            if let Some(hint) = &self.hint {
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| match hint {
                        HeaderHint::Text(hint) => {
                            ui.label(
                                egui::RichText::new(hint)
                                    .monospace()
                                    .size(12.0)
                                    .color(palette.muted),
                            );
                        }
                        HeaderHint::Shortcuts(sections) => {
                            let sections = sections
                                .iter()
                                .map(|(trigger, label)| (trigger.as_str(), label.as_str()))
                                .collect::<Vec<_>>();
                            let color = palette.muted;
                            let galley = keycaps::shortcut_hint_galley_from_painter(
                                ui.painter(),
                                palette,
                                &sections,
                                color,
                                ui.available_width(),
                                12.0,
                            );
                            let (rect, _) =
                                ui.allocate_exact_size(galley.size(), egui::Sense::hover());
                            ui.painter().galley(rect.min, galley, color);
                        }
                    },
                );
            }
        });
    }
}

/// A flat, borderless single-line filter field. Returns the text-edit response
/// so callers can request focus on the first frame.
pub fn filter_field(
    ui: &mut egui::Ui,
    id: egui::Id,
    buf: &mut String,
    theme: Theme,
    hint: &str,
) -> egui::Response {
    bootty_ui::flat_text_edit_singleline(ui, buf, theme, |edit| {
        edit.id(id).desired_width(f32::INFINITY).hint_text(hint)
    })
}

fn rule(ui: &mut egui::Ui, palette: ThemePalette) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, palette.border);
}

/// Width for a centered overlay panel: `preferred`, shrunk to keep a fixed screen
/// margin, but never below `min`. Shared so every overlay sizes the same way.
#[must_use]
pub fn panel_width(ctx: &egui::Context, preferred: f32, min: f32) -> f32 {
    let available = ctx.input(|input| input.content_rect().width());
    preferred.min((available - 72.0).max(min))
}

/// Height cap for an overlay's scrolling list: the viewport height minus chrome,
/// clamped to `[min, max]`.
#[must_use]
pub fn list_max_height(ctx: &egui::Context, min: f32, max: f32) -> f32 {
    let available = ctx.input(|input| input.content_rect().height());
    (available - 200.0).clamp(min, max)
}

/// Parse a `chord=action` keybind string into `(chord, action)`, dropping any
/// scope/flag prefixes (`all:`, `global:`, `unconsumed:`, `performable:`) from the
/// chord. The action name never contains `=`, so the *last* `=` is the divider —
/// this keeps a literal `=` key (e.g. `cmd+=`) intact.
#[must_use]
pub fn parse_keybind(raw: &str) -> Option<(String, String)> {
    let (mut chord, action) = raw.rsplit_once('=')?;
    chord = chord.trim();
    while let Some(("all" | "global" | "unconsumed" | "performable", rest)) = chord.split_once(':')
    {
        chord = rest;
    }
    let action = action.trim();
    (!chord.is_empty() && !action.is_empty()).then(|| (chord.to_owned(), action.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FuzzyMatch {
    pub score: i32,
    pub indices: Vec<usize>,
}

/// Case-insensitive subsequence match with a score and character indices for highlighting.
#[must_use]
pub fn fuzzy_match_info(candidate: &str, pattern: &str) -> Option<FuzzyMatch> {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            indices: Vec::new(),
        });
    }

    let candidate_chars = candidate.chars().collect::<Vec<_>>();
    let candidate_lower = candidate.to_ascii_lowercase();
    let pattern_lower = pattern.to_ascii_lowercase();
    let pattern_chars = pattern_lower.chars().collect::<Vec<_>>();
    let mut indices = Vec::with_capacity(pattern_chars.len());
    let mut search_from = 0;
    for needle in pattern_chars {
        let found = candidate_chars
            .iter()
            .enumerate()
            .skip(search_from)
            .find_map(|(index, ch)| {
                ch.to_lowercase()
                    .any(|candidate_ch| candidate_ch == needle)
                    .then_some(index)
            })?;
        indices.push(found);
        search_from = found + 1;
    }

    let first = indices[0] as i32;
    let last = *indices.last().unwrap_or(&indices[0]) as i32;
    let gaps = indices
        .windows(2)
        .map(|pair| pair[1] - pair[0] - 1)
        .sum::<usize>() as i32;
    let mut score = 10_000 - first * 25 - gaps * 12 - (last - first) * 4;
    if let Some(byte_index) = candidate_lower.find(&pattern_lower) {
        let char_index = candidate_lower[..byte_index].chars().count() as i32;
        score += 5_000 - char_index * 20;
    }
    if word_boundary_match(&candidate_lower, &pattern_lower) {
        score += 1_500;
    }
    if candidate_lower == pattern_lower {
        score += 10_000;
    }
    Some(FuzzyMatch { score, indices })
}

/// Case-insensitive subsequence match — the picker filter shared across overlays.
#[must_use]
pub fn fuzzy_match(candidate: &str, pattern: &str) -> bool {
    fuzzy_match_info(candidate, pattern).is_some()
}

fn word_boundary_match(candidate: &str, pattern: &str) -> bool {
    candidate
        .match_indices(pattern)
        .any(|(index, _)| index == 0 || !candidate.as_bytes()[index - 1].is_ascii_alphanumeric())
}
