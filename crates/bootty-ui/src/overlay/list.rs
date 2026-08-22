//! A filterable, keyboard-and-mouse navigable list rendered inside a scroll
//! area. Rows carry a leading icon, a primary label, and an optional trailing
//! label; the framework owns navigation, selection highlight, and scrolling.

use crate::{ThemePalette, icons, keycaps, readable_color};
use eframe::egui::{self, Align, Color32, FontId, Pos2, Rect, Sense};

const ROW_HEIGHT: f32 = 30.0;

/// One row of a [`ListView`].
#[derive(Clone, Debug, Default)]
pub struct ListRow {
    /// Leading icon slug (resolved through `bootty_ui::icons`); omitted if `None`.
    pub icon: Option<String>,
    /// Override the icon tint; defaults to the row's text color.
    pub icon_tint: Option<Color32>,
    /// Primary label.
    pub primary: String,
    /// Character indices in the primary label that matched the active fuzzy query.
    pub primary_matches: Vec<usize>,
    /// Override the primary label color.
    pub primary_tint: Option<Color32>,
    /// Optional dim description rendered under the primary label (needs a taller
    /// [`ListView::row_height`]).
    pub secondary: Option<String>,
    /// Override the secondary label color.
    pub secondary_tint: Option<Color32>,
    /// Character indices in the secondary label that matched the active fuzzy query.
    pub secondary_matches: Vec<usize>,
    /// Optional right-aligned secondary label.
    pub trailing: Option<String>,
    /// Character indices in the trailing label that matched the active fuzzy query.
    pub trailing_matches: Vec<usize>,
    /// Optional keybinding trigger rendered with the shared keycap glyph layout.
    pub trailing_keybind: Option<String>,
    /// Marks the active/current entry: accent bar + primary tint.
    pub current: bool,
    /// Override the selected row's accent bar color.
    pub selection_tint: Option<Color32>,
    /// Non-selectable section heading row.
    pub section: bool,
}

/// What a [`ListView`] produced for one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListOutcome {
    /// Selection index after this frame's keyboard navigation.
    pub selected: usize,
    /// Row activated this frame by Enter or a primary click, if any.
    pub activated: Option<usize>,
    /// Selectable row currently under the pointer, if any.
    pub hovered: Option<usize>,
}

/// A scrollable, selectable list. Construct per frame from the rows the caller
/// already filtered, render with [`ListView::show`], and feed `selected` back.
pub struct ListView<'a> {
    id_salt: egui::Id,
    rows: &'a [ListRow],
    selected: usize,
    max_height: f32,
    scroll_selected: bool,
    row_height: f32,
    empty_text: &'a str,
}

impl<'a> ListView<'a> {
    pub fn new(
        id_salt: impl std::hash::Hash + std::fmt::Debug,
        rows: &'a [ListRow],
        selected: usize,
    ) -> Self {
        Self {
            id_salt: egui::Id::new(id_salt),
            rows,
            selected,
            max_height: f32::INFINITY,
            scroll_selected: false,
            row_height: ROW_HEIGHT,
            empty_text: "no matches",
        }
    }

    /// Cap the scroll viewport so long lists scroll instead of growing the panel.
    #[must_use]
    pub fn max_height(mut self, max_height: f32) -> Self {
        self.max_height = max_height;
        self
    }

    /// Taller rows, e.g. to fit a [`ListRow::secondary`] description line.
    #[must_use]
    pub fn row_height(mut self, row_height: f32) -> Self {
        self.row_height = row_height;
        self
    }

    #[must_use]
    pub fn empty_text(mut self, text: &'a str) -> Self {
        self.empty_text = text;
        self
    }

    /// Scroll the selected row into view this frame even without keyboard navigation.
    #[must_use]
    pub fn scroll_selected(mut self, scroll: bool) -> Self {
        self.scroll_selected = scroll;
        self
    }

    pub fn show(self, ui: &mut egui::Ui, palette: ThemePalette) -> ListOutcome {
        let (next, previous, enter, pointer_pos) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowDown)
                    || (input.key_pressed(egui::Key::N) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::ArrowUp)
                    || (input.key_pressed(egui::Key::P) && input.modifiers.ctrl),
                input.key_pressed(egui::Key::Enter),
                input.pointer.hover_pos(),
            )
        });
        let pointer_moved = pointer_moved_since_last_frame(ui, self.id_salt, pointer_pos);

        if self.rows.is_empty() {
            let (rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_HEIGHT * 2.0),
                Sense::hover(),
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                self.empty_text,
                FontId::monospace(13.0),
                palette.muted,
            );
            return ListOutcome {
                selected: 0,
                activated: None,
                hovered: None,
            };
        }

        let navigated = next || previous;
        let mut selected = selectable_selection_after_nav(self.selected, self.rows, next, previous);
        let mut hovered = None;
        let mut activated = (enter && !self.rows[selected].section).then_some(selected);

        // Reserve a definite height (content, capped at max_height) instead of
        // letting the ScrollArea negotiate with the auto-sizing floating panel —
        // that negotiation collapses the viewport to a couple of rows.
        let view_height = (self.rows.len() as f32 * self.row_height).min(self.max_height);
        ui.allocate_ui(egui::vec2(ui.available_width(), view_height), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(self.id_salt)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let width = ui.available_width();
                    for (index, row) in self.rows.iter().enumerate() {
                        let sense = if row.section {
                            Sense::hover()
                        } else {
                            Sense::click()
                        };
                        let (rect, response) =
                            ui.allocate_exact_size(egui::vec2(width, self.row_height), sense);
                        let row_selected = !row.section && index == selected;
                        paint_row(ui.painter(), rect, palette, row, row_selected);
                        let pointer_over_row = pointer_pos.is_some_and(|pos| rect.contains(pos));
                        let pointer_selected = pointer_over_row
                            && (pointer_moved
                                || response.is_pointer_button_down_on()
                                || response.clicked());
                        if !row.section && pointer_selected {
                            selected = index;
                            hovered = Some(index);
                        }
                        if !row.section && response.clicked() {
                            activated = Some(index);
                        }
                        // Keep the cursor visible only when navigation moved it, so a
                        // resting selection doesn't fight a user's manual scroll.
                        if index == selected && (navigated || self.scroll_selected) {
                            response.scroll_to_me(Some(Align::Center));
                        }
                    }
                });
        });

        ListOutcome {
            selected,
            activated,
            hovered,
        }
    }
}

fn paint_row(
    painter: &egui::Painter,
    rect: Rect,
    palette: ThemePalette,
    row: &ListRow,
    selected: bool,
) {
    if row.section {
        painter.rect_filled(rect, 0.0, palette.surface);
        painter.text(
            Pos2::new(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            row.primary.to_ascii_uppercase(),
            FontId::monospace(11.0),
            palette.muted,
        );
        return;
    }
    let background = if selected {
        Some(palette.hover)
    } else if row.current {
        Some(palette.surface)
    } else {
        None
    };
    if let Some(background) = background {
        painter.rect_filled(rect, 0.0, background);
    }
    let indicator_tint = if selected { row.selection_tint } else { None }
        .or_else(|| row.current.then_some(palette.primary));
    if let Some(tint) = indicator_tint {
        painter.rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            0.0,
            tint,
        );
    }

    let row_background = background.unwrap_or(palette.pane);
    let match_color = readable_color(row_background, palette.warning);
    let text_color = readable_color(
        row_background,
        if row.current {
            palette.primary
        } else {
            palette.text
        },
    );
    let mut x = rect.left() + 14.0;
    if let Some(slug) = &row.icon {
        let tint = row.icon_tint.unwrap_or(if selected || row.current {
            text_color
        } else {
            palette.muted
        });
        if icons::paint_icon_slug(
            painter,
            slug,
            Pos2::new(x + 8.0, rect.center().y),
            15.0,
            tint,
        ) {
            x += 26.0;
        }
    }
    // With a description, stack the primary above it; otherwise center the primary.
    let primary_y = match row.secondary {
        Some(_) => rect.top() + rect.height() * 0.34,
        None => rect.center().y,
    };
    paint_highlighted_text(
        painter,
        HighlightedText {
            pos: Pos2::new(x, primary_y),
            align: egui::Align2::LEFT_CENTER,
            text: &row.primary,
            matches: &row.primary_matches,
            font: FontId::monospace(13.0),
            color: row.primary_tint.unwrap_or(text_color),
            match_color,
        },
    );
    if let Some(secondary) = &row.secondary {
        paint_highlighted_text(
            painter,
            HighlightedText {
                pos: Pos2::new(x, rect.top() + rect.height() * 0.70),
                align: egui::Align2::LEFT_CENTER,
                text: secondary,
                matches: &row.secondary_matches,
                font: FontId::monospace(11.0),
                color: row
                    .secondary_tint
                    .unwrap_or_else(|| readable_color(row_background, palette.muted)),
                match_color,
            },
        );
    }
    if let Some(trigger) = &row.trailing_keybind {
        let color = readable_color(row_background, palette.muted);
        let galley = keycaps::trigger_galley_from_painter(painter, palette, trigger, color, 220.0);
        let pos = Pos2::new(
            rect.right() - 14.0 - galley.size().x,
            rect.center().y - galley.size().y * 0.5,
        );
        painter.galley(pos, galley, color);
    } else if let Some(trailing) = &row.trailing {
        paint_highlighted_text(
            painter,
            HighlightedText {
                pos: Pos2::new(rect.right() - 14.0, rect.center().y),
                align: egui::Align2::RIGHT_CENTER,
                text: trailing,
                matches: &row.trailing_matches,
                font: FontId::monospace(12.0),
                color: readable_color(row_background, palette.muted),
                match_color,
            },
        );
    }
}

struct HighlightedText<'a> {
    pos: Pos2,
    align: egui::Align2,
    text: &'a str,
    matches: &'a [usize],
    font: FontId,
    color: Color32,
    match_color: Color32,
}

fn paint_highlighted_text(painter: &egui::Painter, text: HighlightedText<'_>) {
    if text.matches.is_empty() {
        painter.text(text.pos, text.align, text.text, text.font, text.color);
        return;
    }
    let mut job = egui::text::LayoutJob::default();
    for (index, ch) in text.text.chars().enumerate() {
        let color = if text.matches.contains(&index) {
            text.match_color
        } else {
            text.color
        };
        job.append(
            &ch.to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: text.font.clone(),
                color,
                ..Default::default()
            },
        );
    }
    let galley = painter.layout_job(job);
    let offset = egui::vec2(
        match text.align.x() {
            egui::Align::LEFT => 0.0,
            egui::Align::Center => -galley.size().x * 0.5,
            egui::Align::RIGHT => -galley.size().x,
        },
        match text.align.y() {
            egui::Align::TOP => 0.0,
            egui::Align::Center => -galley.size().y * 0.5,
            egui::Align::BOTTOM => -galley.size().y,
        },
    );
    painter.galley(text.pos + offset, galley, text.color);
}

fn pointer_moved_since_last_frame(
    ui: &egui::Ui,
    id_salt: egui::Id,
    pointer_pos: Option<Pos2>,
) -> bool {
    let id = id_salt.with("last-pointer-hover-pos");
    let previous = ui.memory(|memory| memory.data.get_temp::<Pos2>(id));
    ui.memory_mut(|memory| {
        if let Some(pos) = pointer_pos {
            memory.data.insert_temp(id, pos);
        } else {
            memory.data.remove_temp::<Pos2>(id);
        }
    });
    previous.is_some_and(|previous| pointer_pos.is_some_and(|current| current != previous))
}

fn selectable_selection_after_nav(
    mut selected: usize,
    rows: &[ListRow],
    next: bool,
    previous: bool,
) -> usize {
    let Some(_) = rows.get(selected).filter(|row| !row.section) else {
        return rows.iter().position(|row| !row.section).unwrap_or(0);
    };
    if next {
        selected = rows
            .iter()
            .enumerate()
            .skip(selected + 1)
            .find(|(_, row)| !row.section)
            .map_or(selected, |(index, _)| index);
    }
    if previous {
        selected = rows[..selected]
            .iter()
            .rposition(|row| !row.section)
            .unwrap_or(selected);
    }
    selected
}

/// Keep a stored selection in range after the row set shrinks.
#[must_use]
pub fn clamp_selection(selected: usize, len: usize) -> usize {
    if len == 0 { 0 } else { selected.min(len - 1) }
}
