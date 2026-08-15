use bootty_ui::{Theme, overlay};
use eframe::egui;

use crate::ui::keybind_source::parse_keybind;
use bootty_ui::overlay::{FloatingWindow, ListRow, ListView};

/// A read-only, filterable cheatsheet of the currently active keybindings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindHelpDialog {
    filter: String,
    selected: usize,
    /// `(chord, action)` pairs, sorted by action then chord.
    bindings: Vec<(String, String)>,
    focus_filter: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeybindHelpEvent {
    None,
    Close,
}

impl KeybindHelpDialog {
    /// Build from the raw `chord=action` binding strings the config resolves for
    /// the active backend.
    pub fn open(raw_bindings: &[String]) -> Self {
        let mut bindings: Vec<(String, String)> = raw_bindings
            .iter()
            .filter_map(|raw| parse_keybind(raw))
            .collect();
        bindings.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Self {
            filter: String::new(),
            selected: 0,
            bindings,
            focus_filter: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> KeybindHelpEvent {
        let matches = filtered(&self.bindings, &self.filter);
        self.selected = overlay::clamp_selection(self.selected, matches.len());
        let rows: Vec<ListRow> = matches
            .iter()
            .filter_map(|&index| self.bindings.get(index))
            .map(|(chord, action)| ListRow {
                primary: action.clone(),
                trailing: Some(chord.clone()),
                ..ListRow::default()
            })
            .collect();
        let list_max = overlay::list_max_height(ctx, 150.0, 560.0);

        let result = FloatingWindow::new("keybind-help-dialog", "Keybindings")
            .icon("keyboard")
            .hint("Esc close")
            .footer(format!(
                "{} / {} bindings",
                matches.len(),
                self.bindings.len()
            ))
            .width(overlay::panel_width(ctx, 720.0, 480.0))
            .show(ctx, theme, |ui, palette| {
                let filter = overlay::filter_field(
                    ui,
                    egui::Id::new("keybind-help-filter"),
                    &mut self.filter,
                    theme,
                    "filter keybindings...",
                );
                if self.focus_filter {
                    filter.request_focus();
                    self.focus_filter = false;
                }
                ui.add_space(8.0);
                let outcome = ListView::new("keybind-help-list", &rows, self.selected)
                    .max_height(list_max)
                    .empty_text("no matching keybindings")
                    .show(ui, palette);
                self.selected = outcome.selected;
            });

        if result.escaped || result.clicked_outside {
            KeybindHelpEvent::Close
        } else {
            KeybindHelpEvent::None
        }
    }
}

fn filtered(bindings: &[(String, String)], filter: &str) -> Vec<usize> {
    let filter = filter.trim();
    bindings
        .iter()
        .enumerate()
        .filter_map(|(index, (chord, action))| {
            (filter.is_empty()
                || overlay::fuzzy_match(action, filter)
                || overlay::fuzzy_match(chord, filter))
            .then_some(index)
        })
        .collect()
}
