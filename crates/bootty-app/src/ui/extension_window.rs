//! Renders a module's floating surface through the native overlay framework.
//!
//! A module declares the surface and publishes items; it never touches egui. The filter, keyboard
//! navigation, footer count and dismissal all come from the shared overlay so a Luau window behaves
//! like every other panel in the app. Filter text is per-surface UI state, so it lives here rather
//! than in the module.

use bootty_extension::{ModuleItem, PublishedSurfaceSnapshot};
use bootty_ui::Theme;
use bootty_ui::overlay::{self, FloatingWindow, ListRow, ListView, clamp_selection};
use eframe::egui;

/// Per-surface view state: the filter query and the selected row, keyed by surface id.
#[derive(Default)]
pub struct ExtensionWindows {
    open: std::collections::HashMap<String, WindowState>,
}

#[derive(Default)]
struct WindowState {
    filter: String,
    selected: usize,
    focus: bool,
}

/// What one floating surface's frame produced.
pub enum ExtensionWindowEvent {
    /// The user picked an item; its action goes back to the declaring module.
    Action(String),
    /// Esc or a click outside. The module owns whether the surface still publishes items, so
    /// dismissal is reported rather than applied here.
    Dismissed,
}

impl ExtensionWindows {
    /// Paint `surface` and resolve one frame of interaction.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: Theme,
        surface: &PublishedSurfaceSnapshot,
    ) -> Option<ExtensionWindowEvent> {
        let declaration = &surface.snapshot.declaration;
        let state = self
            .open
            .entry(declaration.id.clone())
            .or_insert(WindowState {
                focus: true,
                ..WindowState::default()
            });

        let matches = filtered(&surface.snapshot.items, &state.filter);
        state.selected = clamp_selection(state.selected, matches.len());
        let rows = matches
            .iter()
            .filter_map(|&index| surface.snapshot.items.get(index))
            .map(|item| ListRow {
                icon: item.icon.clone(),
                primary: item.text.clone(),
                trailing: item.kind.clone(),
                ..ListRow::default()
            })
            .collect::<Vec<_>>();
        let list_max = overlay::list_max_height(ctx, 150.0, 520.0);

        let mut window = FloatingWindow::new(
            ("extension-floating", declaration.id.clone()),
            declaration
                .title
                .clone()
                .unwrap_or_else(|| declaration.id.clone()),
        )
        .hint(
            declaration
                .hint
                .clone()
                .unwrap_or_else(|| "Enter select   Esc close".to_owned()),
        )
        .width(overlay::panel_width(ctx, 720.0, 420.0))
        .footer(format!(
            "{} / {} items",
            matches.len(),
            surface.snapshot.items.len()
        ));
        if let Some(icon) = &declaration.icon {
            window = window.icon(icon.clone());
        }

        let filter_id = egui::Id::new(("extension-floating-filter", declaration.id.clone()));
        let list_id = ("extension-floating-list", declaration.id.clone());
        let result = window.show(ctx, theme, |ui, palette| {
            let filter =
                overlay::filter_field(ui, filter_id, &mut state.filter, theme, "filter...");
            if state.focus {
                filter.request_focus();
                state.focus = false;
            }
            ui.add_space(8.0);
            let outcome = ListView::new(list_id, &rows, state.selected)
                .max_height(list_max)
                .show(ui, palette);
            state.selected = outcome.selected;
            outcome.activated
        });

        if let Some(index) = result.inner
            && let Some(action) = matches
                .get(index)
                .and_then(|&index| surface.snapshot.items.get(index))
                .and_then(|item| item.action.clone())
        {
            return Some(ExtensionWindowEvent::Action(action));
        }
        if result.escaped || result.clicked_outside {
            self.open.remove(&declaration.id);
            return Some(ExtensionWindowEvent::Dismissed);
        }
        None
    }

    /// Drop state for surfaces that are no longer published, so a reloaded module starts clean.
    pub fn retain_open(&mut self, live: impl Fn(&str) -> bool) {
        self.open.retain(|id, _| live(id));
    }
}

/// Items matching `filter`, fuzzily over the label and the item's own key.
fn filtered(items: &[ModuleItem], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            (filter.is_empty()
                || overlay::fuzzy_match(&item.text, filter)
                || item
                    .key
                    .as_deref()
                    .is_some_and(|key| overlay::fuzzy_match(key, filter)))
            .then_some(index)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(key: &str, text: &str) -> ModuleItem {
        ModuleItem {
            text: text.to_owned(),
            key: Some(key.to_owned()),
            ..ModuleItem::default()
        }
    }

    #[test]
    fn filter_matches_item_text_or_key() {
        let items = vec![item("a", "Restart server"), item("b", "Open logs")];
        assert_eq!(filtered(&items, "logs"), vec![1]);
        assert_eq!(filtered(&items, "a"), vec![0]);
        assert_eq!(filtered(&items, ""), vec![0, 1]);
        assert_eq!(filtered(&items, "zzz"), Vec::<usize>::new());
    }
}
