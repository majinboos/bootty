use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{SystemTime, UNIX_EPOCH},
};

use bootty_ui::{Theme, ThemePalette};
use eframe::egui;
use iconflow::{Pack, list};

use crate::{
    config::MultiplexerBackendConfig,
    mux::controller::SpaceId,
    ui::{
        icons::{has_slug, icon_text},
        overlay::{self, FloatingWindow, TextPrompt},
    },
    workspace::DEFAULT_SPACE_COLOR,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEditorDialog {
    space_id: Option<SpaceId>,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    backend: Option<MultiplexerBackendConfig>,
    focus: bool,
    icon_search: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceEditorEvent {
    None,
    Close,
    Save {
        space_id: Option<SpaceId>,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        backend: Option<MultiplexerBackendConfig>,
    },
}

impl SpaceEditorDialog {
    pub fn new_space(icon: String, backend: Option<MultiplexerBackendConfig>) -> Self {
        Self {
            space_id: None,
            name: String::new(),
            icon,
            color: DEFAULT_SPACE_COLOR,
            tint_sidebar: false,
            backend,
            icon_search: String::new(),
            focus: true,
        }
    }

    pub fn edit_space(
        space_id: SpaceId,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        backend: Option<MultiplexerBackendConfig>,
    ) -> Self {
        Self {
            space_id: Some(space_id),
            name,
            icon,
            color,
            tint_sidebar,
            backend,
            icon_search: String::new(),
            focus: true,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> SpaceEditorEvent {
        let name = normalized_name(&self.name);
        let title = if self.space_id.is_some() {
            "Edit Space"
        } else {
            "New Space"
        };
        let result = FloatingWindow::new("space-editor-dialog", title)
            .icon("shapes")
            .hint("Enter save   Esc close")
            .width(overlay::panel_width(ctx, 620.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let submitted = TextPrompt::new("space-editor-name")
                    .caption("space name")
                    .hint("space name...")
                    .validation(name.is_none().then_some("name cannot be empty"))
                    .submit_disabled(name.is_none())
                    .show(ui, theme, &mut self.name, &mut self.focus)
                    .submitted;
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("color")
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                    ui.add_space(6.0);
                    egui::color_picker::color_edit_button_srgb(ui, &mut self.color);
                    ui.label(
                        egui::RichText::new(color_hex(self.color))
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                });
                ui.checkbox(&mut self.tint_sidebar, "Tint sidebar with Space color");
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("backend")
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                    egui::ComboBox::from_id_salt("space-editor-backend")
                        .selected_text(backend_label(self.backend))
                        .show_ui(ui, |ui| {
                            for backend in [
                                None,
                                Some(MultiplexerBackendConfig::Native),
                                Some(MultiplexerBackendConfig::Rmux),
                                Some(MultiplexerBackendConfig::Tmux),
                                Some(MultiplexerBackendConfig::Zellij),
                            ] {
                                ui.selectable_value(
                                    &mut self.backend,
                                    backend,
                                    backend_label(backend),
                                );
                            }
                        });
                });
                ui.label(
                    egui::RichText::new("icon")
                        .monospace()
                        .size(12.0)
                        .color(palette.muted),
                );
                ui.add_space(4.0);
                show_icon_search_field(ui, palette, &mut self.icon_search)
                    .on_hover_text("Filter Phosphor and Lucide icons");
                ui.add_space(6.0);
                show_icon_picker(ui, palette, &mut self.icon, &self.icon_search);
                ui.add_space(16.0);
                submitted
                    || ui
                        .add_enabled(name.is_some(), egui::Button::new("Save"))
                        .clicked()
            });

        if result.inner
            && let Some(name) = name
        {
            return SpaceEditorEvent::Save {
                space_id: self.space_id,
                name,
                icon: self.icon.clone(),
                color: self.color,
                tint_sidebar: self.tint_sidebar,
                backend: self.backend,
            };
        }
        if result.escaped || result.clicked_outside {
            return SpaceEditorEvent::Close;
        }
        SpaceEditorEvent::None
    }
}

pub(crate) fn default_space_icon(existing: &[String]) -> String {
    let available = space_icon_inventory()
        .into_iter()
        .filter(|icon| !existing.iter().any(|used| used == icon))
        .collect::<Vec<_>>();
    if available.is_empty() {
        return space_icon_inventory()
            .into_iter()
            .next()
            .unwrap_or_else(|| "folder".to_owned());
    }

    let mut hasher = DefaultHasher::new();
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    existing.hash(&mut hasher);
    let rotation = hasher.finish().rotate_left(17) as usize;
    available[rotation % available.len()].clone()
}

pub(crate) fn space_icon_inventory() -> Vec<String> {
    list(Pack::Phosphor)
        .iter()
        .filter_map(|icon| icon.strip_suffix("-duotone"))
        .map(|icon| format!("phosphor:{icon}"))
        .chain(list(Pack::Lucide).iter().map(|icon| (*icon).to_owned()))
        .filter(|icon| has_slug(icon))
        .collect()
}

fn show_icon_search_field(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    search: &mut String,
) -> egui::Response {
    let fill = palette.surface;
    let width = (ui.available_width() - 18.0).max(0.0);
    egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.add_sized(
                [width, 22.0],
                egui::TextEdit::singleline(search)
                    .id(egui::Id::new("space-icon-search"))
                    .hint_text("search icons...")
                    .text_color(palette.text)
                    .vertical_align(egui::Align::Center)
                    .background_color(fill)
                    .frame(egui::Frame::NONE),
            )
        })
        .inner
}

fn show_icon_picker(ui: &mut egui::Ui, palette: ThemePalette, selected: &mut String, search: &str) {
    let icons = matching_icons(search);
    if icons.is_empty() {
        ui.label(
            egui::RichText::new("No matching icons.")
                .size(12.0)
                .color(palette.muted),
        );
        return;
    }

    let button_size = egui::vec2(42.0, 36.0);
    let columns = ((ui.available_width() / 50.0).floor() as usize).clamp(1, 12);
    let rows = icons.len().div_ceil(columns);
    let height = (rows as f32 * 44.0).min(overlay::list_max_height(ui.ctx(), 180.0, 320.0));
    ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
        egui::ScrollArea::vertical()
            .id_salt("space-icon-grid-scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("space-icon-grid")
                    .num_columns(columns)
                    .spacing(egui::vec2(8.0, 8.0))
                    .show(ui, |ui| {
                        for (index, icon) in icons.iter().enumerate() {
                            let current = *selected == *icon;
                            let button = egui::Button::new(
                                icon_text(
                                    icon,
                                    18.0,
                                    if current { palette.base } else { palette.text },
                                )
                                .unwrap_or_else(|| egui::RichText::new("?")),
                            )
                            .fill(if current {
                                palette.primary
                            } else {
                                palette.surface
                            });
                            if ui
                                .add_sized(button_size, button)
                                .on_hover_text(icon)
                                .clicked()
                            {
                                *selected = icon.clone();
                            }
                            if (index + 1) % columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
    });
}

fn matching_icons(query: &str) -> Vec<String> {
    let query = query.to_ascii_lowercase();
    space_icon_inventory()
        .into_iter()
        .filter(|icon| icon.to_ascii_lowercase().contains(&query))
        .collect()
}

fn backend_label(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "Inherit",
        Some(MultiplexerBackendConfig::Native) => "Native",
        Some(MultiplexerBackendConfig::Rmux) => "Rmux",
        Some(MultiplexerBackendConfig::Tmux) => "Tmux",
        Some(MultiplexerBackendConfig::Zellij) => "Zellij",
    }
}

fn color_hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn normalized_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_name_trims_and_rejects_blank() {
        assert_eq!(normalized_name("  Review  "), Some("Review".to_owned()));
        assert_eq!(normalized_name("   "), None);
    }

    #[test]
    fn new_space_editor_starts_with_a_blank_name() {
        let dialog = SpaceEditorDialog::new_space("folder".to_owned(), None);
        assert!(dialog.name.is_empty());
    }

    #[test]
    fn default_icon_avoids_existing_icons_when_inventory_has_an_unused_icon() {
        let inventory = space_icon_inventory();
        let existing = inventory.iter().take(3).cloned().collect::<Vec<_>>();
        let icon = default_space_icon(&existing);

        assert!(inventory.contains(&icon));
        assert!(!existing.contains(&icon));
    }

    #[test]
    fn phosphor_inventory_precedes_lucide_and_icons_render() {
        let icons = space_icon_inventory();
        let first_lucide = icons
            .iter()
            .position(|icon| !icon.starts_with("phosphor:"))
            .expect("Lucide icon");
        assert!(
            icons[..first_lucide]
                .iter()
                .all(|icon| icon.starts_with("phosphor:"))
        );
        assert!(icons.iter().any(|icon| icon == "phosphor:alarm"));
    }

    #[test]
    fn icon_search_filters_pack_prefixed_icons_case_insensitively() {
        let lowercase = matching_icons("phosphor:alarm");
        let uppercase = matching_icons("PHOSPHOR:ALARM");
        assert_eq!(uppercase, lowercase);
        assert!(lowercase.contains(&"phosphor:alarm".to_owned()));
        assert!(
            lowercase
                .iter()
                .all(|icon| icon.starts_with("phosphor:alarm"))
        );
    }

    #[test]
    fn icon_search_box_accepts_text_and_filters_icons() {
        let context = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(240.0, 44.0));
        let palette = ThemePalette::default();
        let mut search = String::new();
        let show = |events: Vec<egui::Event>, search: &mut String| {
            let _ = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            show_icon_search_field(ui, palette, search);
                        });
                },
            );
        };
        let field = egui::Pos2::new(20.0, 16.0);

        show(vec![egui::Event::PointerMoved(field)], &mut search);
        show(
            vec![egui::Event::PointerButton {
                pos: field,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut search,
        );
        show(
            vec![egui::Event::PointerButton {
                pos: field,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            &mut search,
        );
        show(
            vec![egui::Event::Text("phosphor:alarm".to_owned())],
            &mut search,
        );

        assert_eq!(search, "phosphor:alarm");
        assert!(matching_icons(&search).contains(&"phosphor:alarm".to_owned()));
    }
}
