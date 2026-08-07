use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{SystemTime, UNIX_EPOCH},
};

use bootty_ui::{Theme, ThemePalette};
use eframe::egui;
use iconflow::{Pack, list};

use crate::{
    config::{MultiplexerBackendConfig, SshRemoteConfig},
    mux::controller::SpaceId,
    ui::{
        icons::{has_slug, icon_text},
        overlay::{self, FloatingWindow, TextPrompt},
    },
    workspace::{DEFAULT_SPACE_COLOR, SpaceMuxOverride},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEditorDialog {
    space_id: Option<SpaceId>,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    backend: Option<MultiplexerBackendConfig>,
    /// What this space runs when it overrides nothing. The editor needs it to know whether the
    /// space can name a host at all, and to show what it would otherwise inherit — editing a space
    /// that inherits must not turn the inherited value into an override of the same value.
    inherited: SpaceInheritance,
    remote: RemoteFields,
    focus: bool,
    icon_search: String,
}

/// What a space falls back to when it overrides nothing: the backend the config file names, and
/// the host that backend would run on. Shown as placeholders, never written as the space's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceInheritance {
    pub backend: MultiplexerBackendConfig,
    pub host: Option<String>,
}

/// The remote connection as typed. Held as text so a half-written port or host does not have to
/// parse on every keystroke; it becomes an [`SshRemoteConfig`] when the space is saved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RemoteFields {
    host: String,
    user: String,
    port: String,
    program: String,
    flags: String,
}

impl RemoteFields {
    fn from_config(remote: Option<&SshRemoteConfig>) -> Self {
        let Some(remote) = remote else {
            return Self::default();
        };
        Self {
            host: remote.host.clone(),
            user: remote.user.clone().unwrap_or_default(),
            port: remote.port.map(|port| port.to_string()).unwrap_or_default(),
            program: remote.program.clone(),
            flags: remote.args.join(" "),
        }
    }

    /// The remote to save, or `None` when no host is named: a remote without a host reaches
    /// nothing, and the rest of the fields describe how to reach a host that is not there.
    fn to_config(&self) -> Option<SshRemoteConfig> {
        let host = self.host.trim();
        if host.is_empty() {
            return None;
        }
        let mut remote = SshRemoteConfig::for_host(host);
        remote.user = nonempty(&self.user);
        remote.port = self.port.trim().parse().ok();
        if let Some(program) = nonempty(&self.program) {
            remote.program = program;
        }
        remote.args = self
            .flags
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Some(remote)
    }
}

fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
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
        mux: SpaceMuxOverride,
    },
}

impl SpaceEditorDialog {
    pub fn new_space(icon: String, mux: SpaceMuxOverride, inherited: SpaceInheritance) -> Self {
        Self {
            space_id: None,
            name: String::new(),
            icon,
            color: DEFAULT_SPACE_COLOR,
            tint_sidebar: false,
            backend: mux.backend,
            inherited,
            remote: RemoteFields::from_config(mux.remote.as_ref()),
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
        mux: SpaceMuxOverride,
        inherited: SpaceInheritance,
    ) -> Self {
        Self {
            space_id: Some(space_id),
            name,
            icon,
            color,
            tint_sidebar,
            backend: mux.backend,
            inherited,
            remote: RemoteFields::from_config(mux.remote.as_ref()),
            icon_search: String::new(),
            focus: true,
        }
    }

    /// The backend this space will actually run, override or inherited.
    fn resolved_backend(&self) -> MultiplexerBackendConfig {
        self.backend.unwrap_or(self.inherited.backend)
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
                self.remote_ui(ui, palette);
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
                mux: SpaceMuxOverride {
                    backend: self.backend,
                    remote: self.remote.to_config(),
                },
            };
        }
        if result.escaped || result.clicked_outside {
            return SpaceEditorEvent::Close;
        }
        SpaceEditorEvent::None
    }
}

impl SpaceEditorDialog {
    /// The host this space's multiplexer runs on. Only for the backends bootty reaches through a
    /// client — the others keep their terminals in this process, with no host to name.
    fn remote_ui(&mut self, ui: &mut egui::Ui, palette: ThemePalette) {
        // Always shown, so the field is where someone looks for it rather than something they find
        // by changing the backend first. It is only editable for a backend that has a client to run
        // elsewhere, and says so when it does not.
        let remotable = self.resolved_backend().supports_remote();
        let placeholder = match (&self.inherited.host, remotable) {
            (_, false) => "tmux or zellij only".to_owned(),
            (Some(host), true) => format!("{host} (inherited)"),
            (None, true) => "empty keeps this space local".to_owned(),
        };
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("ssh host")
                    .monospace()
                    .size(12.0)
                    .color(palette.muted),
            );
            ui.add_enabled(
                remotable,
                egui::TextEdit::singleline(&mut self.remote.host)
                    .hint_text(placeholder)
                    .desired_width(220.0),
            );
        });
        if !remotable || self.remote.host.trim().is_empty() {
            return;
        }
        egui::CollapsingHeader::new(
            egui::RichText::new("connection details")
                .monospace()
                .size(12.0)
                .color(palette.muted),
        )
        .id_salt("space-editor-remote-details")
        .show(ui, |ui| {
            for (caption, hint, value) in [
                ("user", "from ~/.ssh/config", &mut self.remote.user),
                ("port", "22", &mut self.remote.port),
                ("ssh client", "ssh", &mut self.remote.program),
                ("flags", "-i ~/.ssh/devbox", &mut self.remote.flags),
            ] {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(caption)
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                    ui.add(
                        egui::TextEdit::singleline(value)
                            .hint_text(hint)
                            .desired_width(200.0),
                    );
                });
            }
        });
        ui.add_space(8.0);
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

    /// The editor holds the connection as text while it is being typed, so what comes back out has
    /// to be the same remote that went in — and a space with no host named has to save as local
    /// rather than as a remote pointing nowhere.
    #[test]
    fn editor_fields_round_trip_the_remote_they_were_opened_with() {
        let remote = SshRemoteConfig {
            host: "devbox".to_owned(),
            user: Some("dev".to_owned()),
            port: Some(2222),
            program: "ssh".to_owned(),
            args: vec!["-i".to_owned(), "~/.ssh/devbox".to_owned()],
        };

        let fields = RemoteFields::from_config(Some(&remote));

        assert_eq!(fields.to_config(), Some(remote));
        assert_eq!(RemoteFields::default().to_config(), None);
        assert_eq!(
            RemoteFields {
                host: "  ".to_owned(),
                user: "dev".to_owned(),
                ..RemoteFields::default()
            }
            .to_config(),
            None
        );
        // A port mid-edit is not a port: it drops rather than becoming a different one.
        assert_eq!(
            RemoteFields {
                host: "devbox".to_owned(),
                port: "22x".to_owned(),
                ..RemoteFields::default()
            }
            .to_config()
            .and_then(|remote| remote.port),
            None
        );
    }

    #[test]
    fn space_name_trims_and_rejects_blank() {
        assert_eq!(normalized_name("  Review  "), Some("Review".to_owned()));
        assert_eq!(normalized_name("   "), None);
    }

    #[test]
    fn new_space_editor_starts_with_a_blank_name() {
        let dialog = SpaceEditorDialog::new_space(
            "folder".to_owned(),
            SpaceMuxOverride::default(),
            SpaceInheritance::default(),
        );
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
