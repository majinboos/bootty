//! Context menus described as a table of entries.
//!
//! The caller owns the vocabulary: it hands over labels, enabled flags and its own action values,
//! and gets back the one that was chosen. Nothing about what the entries *mean* lives here, which
//! is what lets two different menus — and eventually an extension-contributed one — share this.

use eframe::egui;

/// One line in a context menu.
pub enum MenuEntry<'a, T> {
    Item {
        label: &'a str,
        enabled: bool,
        value: T,
    },
    Separator,
    /// A nested menu. Its own entries are only built when the submenu opens.
    Submenu {
        label: &'a str,
        entries: Vec<MenuEntry<'a, T>>,
    },
}

impl<'a, T> MenuEntry<'a, T> {
    /// An always-enabled item.
    pub fn item(label: &'a str, value: T) -> Self {
        Self::Item {
            label,
            enabled: true,
            value,
        }
    }

    /// An item the caller may disable, keeping it visible so the menu shape stays stable.
    pub fn enabled_item(enabled: bool, label: &'a str, value: T) -> Self {
        Self::Item {
            label,
            enabled,
            value,
        }
    }

    pub fn submenu(label: &'a str, entries: Vec<MenuEntry<'a, T>>) -> Self {
        Self::Submenu { label, entries }
    }
}

/// Attach a context menu to `response` and return the chosen value.
///
/// The first click wins: once something is chosen the remaining entries stop accepting clicks and
/// the menu closes, so one gesture cannot produce two actions.
#[must_use]
pub fn context_menu<T: Copy>(response: &egui::Response, entries: &[MenuEntry<'_, T>]) -> Option<T> {
    let mut chosen = None;
    response.context_menu(|ui| {
        show_entries(ui, entries, &mut chosen);
        if chosen.is_some() {
            ui.close();
        }
    });
    chosen
}

fn show_entries<T: Copy>(ui: &mut egui::Ui, entries: &[MenuEntry<'_, T>], chosen: &mut Option<T>) {
    for entry in entries {
        match entry {
            MenuEntry::Separator => {
                ui.separator();
            }
            MenuEntry::Item {
                label,
                enabled,
                value,
            } => {
                if chosen.is_none()
                    && ui
                        .add_enabled(*enabled, egui::Button::new(*label))
                        .clicked()
                {
                    *chosen = Some(*value);
                }
            }
            MenuEntry::Submenu { label, entries } => {
                if chosen.is_none() {
                    ui.menu_button(*label, |ui| show_entries(ui, entries, chosen));
                }
            }
        }
    }
}
