use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use bootty_config::config::{MultiplexerBackendConfig, SshProfileConfig, SshRemoteConfig};
use bootty_mux::controller::SpaceId;
use bootty_ui::overlay::{FloatingWindow, TextPrompt};
use bootty_ui::{
    Theme, ThemePalette,
    icons::{has_slug, icon_text},
    overlay,
};
use eframe::egui;
use iconflow::{Pack, list};

use crate::remote_catalog::{self, RemoteSpaceSummary};
use bootty_workspace::{
    DEFAULT_SPACE_COLOR, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
};

#[derive(Debug)]
pub struct SpaceEditorDialog {
    space_id: Option<SpaceId>,
    name: String,
    icon: String,
    color: [u8; 3],
    tint_sidebar: bool,
    backend: Option<MultiplexerBackendConfig>,
    remote: RemoteFields,
    remote_source: SpaceRemoteOverride,
    profiles: Vec<(String, SshProfileConfig)>,
    catalog: RemoteCatalogState,
    new_remote_space_name: String,
    new_remote_space_backend: MultiplexerBackendConfig,
    focus: bool,
    icon_search: String,
}

#[derive(Debug)]
struct RemoteCatalogResult {
    spaces: Vec<RemoteSpaceSummary>,
    selected: Option<RemoteSpaceSummary>,
}

#[derive(Debug, Default)]
enum RemoteCatalogState {
    #[default]
    Idle,
    Running {
        profile_id: String,
        receiver: mpsc::Receiver<Result<RemoteCatalogResult, String>>,
    },
    Ready {
        profile_id: String,
        spaces: Vec<RemoteSpaceSummary>,
    },
    Failed {
        profile_id: String,
        error: String,
    },
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

fn remote_text_edit(
    ui: &mut egui::Ui,
    value: &mut String,
    theme: Theme,
    hint: &str,
    width: f32,
) -> egui::Response {
    let response = ui.allocate_ui_with_layout(
        egui::vec2(width, 34.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            bootty_ui::themed_text_edit_singleline(ui, value, theme, |edit| {
                edit.hint_text(hint).desired_width(width)
            })
        },
    );
    response.inner.union(response.response)
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
    pub fn new_space(icon: String, mux: SpaceMuxOverride) -> Self {
        Self {
            space_id: None,
            name: String::new(),
            icon,
            color: DEFAULT_SPACE_COLOR,
            tint_sidebar: false,
            backend: mux.backend,
            remote: RemoteFields::from_config(match &mux.remote {
                SpaceRemoteOverride::Inline(remote) => Some(remote),
                _ => None,
            }),
            remote_source: mux.remote,
            profiles: Vec::new(),
            catalog: RemoteCatalogState::Idle,
            new_remote_space_name: String::new(),
            new_remote_space_backend: MultiplexerBackendConfig::Tmux,
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
    ) -> Self {
        Self {
            space_id: Some(space_id),
            name,
            icon,
            color,
            tint_sidebar,
            backend: mux.backend,
            remote: RemoteFields::from_config(match &mux.remote {
                SpaceRemoteOverride::Inline(remote) => Some(remote),
                _ => None,
            }),
            remote_source: mux.remote,
            profiles: Vec::new(),
            catalog: RemoteCatalogState::Idle,
            new_remote_space_name: String::new(),
            new_remote_space_backend: MultiplexerBackendConfig::Tmux,
            icon_search: String::new(),
            focus: true,
        }
    }

    pub fn with_profiles(
        mut self,
        profiles: impl Iterator<Item = (String, SshProfileConfig)>,
    ) -> Self {
        self.profiles = profiles.collect();
        self
    }

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> SpaceEditorEvent {
        self.poll_catalog();
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
                    ui.add_enabled_ui(
                        !matches!(&self.remote_source, SpaceRemoteOverride::Profile(_)),
                        |ui| {
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
                        },
                    );
                });
                self.remote_ui(ui, theme);
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
                        .add_enabled(
                            name.is_some() && self.remote_ready(),
                            egui::Button::new("Save"),
                        )
                        .clicked()
            });

        if result.inner
            && self.remote_ready()
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
                    remote: match &self.remote_source {
                        SpaceRemoteOverride::Inherit => self
                            .remote
                            .to_config()
                            .map(SpaceRemoteOverride::Inline)
                            .unwrap_or_default(),
                        SpaceRemoteOverride::Inline(_) => self
                            .remote
                            .to_config()
                            .map(SpaceRemoteOverride::Inline)
                            .unwrap_or(SpaceRemoteOverride::Local),
                        remote => remote.clone(),
                    },
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
    fn remote_ui(&mut self, ui: &mut egui::Ui, theme: Theme) {
        self.location_ui(ui);
        if let Some(profile_id) = self.selected_profile_id() {
            self.catalog_ui(ui, theme, profile_id);
        } else if matches!(&self.remote_source, SpaceRemoteOverride::Inline(_)) {
            ui.label(
                egui::RichText::new(
                    "Legacy inline SSH settings are preserved. Select an SSH profile to migrate.",
                )
                .size(12.0)
                .color(theme.palette.muted),
            );
        }
    }

    fn selected_profile_id(&self) -> Option<String> {
        match &self.remote_source {
            SpaceRemoteOverride::Profile(remote) => Some(remote.profile_id.clone()),
            _ => None,
        }
    }

    fn location_ui(&mut self, ui: &mut egui::Ui) {
        let profiles = self
            .profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.name.clone()))
            .collect::<Vec<_>>();
        let mut requested = None;
        ui.horizontal(|ui| {
            ui.label("location");
            egui::ComboBox::from_id_salt("space-editor-location")
                .selected_text(self.remote_source_label())
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            matches!(&self.remote_source, SpaceRemoteOverride::Inherit),
                            "Inherit",
                        )
                        .clicked()
                    {
                        requested = Some(SpaceRemoteOverride::Inherit);
                    }
                    if ui
                        .selectable_label(
                            matches!(&self.remote_source, SpaceRemoteOverride::Local),
                            "This computer",
                        )
                        .clicked()
                    {
                        requested = Some(SpaceRemoteOverride::Local);
                    }
                    for (profile_id, name) in profiles {
                        let selected = matches!(
                            &self.remote_source,
                            SpaceRemoteOverride::Profile(remote)
                                if remote.profile_id == profile_id
                        );
                        if ui.selectable_label(selected, name).clicked() {
                            requested = Some(SpaceRemoteOverride::Profile(RemoteSpaceRef {
                                profile_id,
                                remote_space_id: String::new(),
                                remote_space_name: String::new(),
                                backend: MultiplexerBackendConfig::Tmux,
                            }));
                        }
                    }
                });
        });
        if let Some(remote) = requested {
            self.remote_source = remote;
            self.catalog = RemoteCatalogState::Idle;
        }
    }

    fn catalog_ui(&mut self, ui: &mut egui::Ui, theme: Theme, profile_id: String) {
        enum View {
            Idle,
            Running,
            Ready(Vec<RemoteSpaceSummary>),
            Failed(String),
        }
        let view = match &self.catalog {
            RemoteCatalogState::Idle => View::Idle,
            RemoteCatalogState::Running {
                profile_id: current,
                ..
            } if current == &profile_id => View::Running,
            RemoteCatalogState::Ready {
                profile_id: current,
                spaces,
            } if current == &profile_id => View::Ready(spaces.clone()),
            RemoteCatalogState::Failed {
                profile_id: current,
                error,
            } if current == &profile_id => View::Failed(error.clone()),
            _ => View::Idle,
        };
        match view {
            View::Idle => self.start_catalog(ui.ctx(), &profile_id, None),
            View::Running => {
                ui.label("Loading remote Spaces...");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
            View::Failed(error) => {
                ui.label(egui::RichText::new(error).color(theme.palette.destructive));
                if ui.button("Retry").clicked() {
                    self.start_catalog(ui.ctx(), &profile_id, None);
                }
            }
            View::Ready(spaces) => {
                let selected = match &self.remote_source {
                    SpaceRemoteOverride::Profile(remote)
                        if !remote.remote_space_name.is_empty() =>
                    {
                        remote.remote_space_name.clone()
                    }
                    _ => "Select a remote Space".to_owned(),
                };
                egui::ComboBox::from_id_salt("space-editor-remote-space")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for space in spaces {
                            let current = matches!(
                                &self.remote_source,
                                SpaceRemoteOverride::Profile(remote)
                                    if remote.remote_space_id == space.id
                            );
                            if ui.selectable_label(current, &space.name).clicked() {
                                self.backend = Some(space.backend);
                                self.remote_source = SpaceRemoteOverride::Profile(RemoteSpaceRef {
                                    profile_id: profile_id.clone(),
                                    remote_space_id: space.id,
                                    remote_space_name: space.name,
                                    backend: space.backend,
                                });
                            }
                        }
                    });
                self.create_remote_space_ui(ui, theme, &profile_id);
            }
        }
    }

    fn create_remote_space_ui(&mut self, ui: &mut egui::Ui, theme: Theme, profile_id: &str) {
        ui.horizontal(|ui| {
            remote_text_edit(
                ui,
                &mut self.new_remote_space_name,
                theme,
                "new remote Space",
                220.0,
            );
            egui::ComboBox::from_id_salt("space-editor-new-remote-backend")
                .selected_text(backend_label(Some(self.new_remote_space_backend)))
                .show_ui(ui, |ui| {
                    for backend in [
                        MultiplexerBackendConfig::Rmux,
                        MultiplexerBackendConfig::Tmux,
                        MultiplexerBackendConfig::Zellij,
                    ] {
                        ui.selectable_value(
                            &mut self.new_remote_space_backend,
                            backend,
                            backend_label(Some(backend)),
                        );
                    }
                });
            if ui
                .add_enabled(
                    !self.new_remote_space_name.trim().is_empty(),
                    egui::Button::new("Create"),
                )
                .clicked()
            {
                let name = self.new_remote_space_name.trim().to_owned();
                self.start_catalog(
                    ui.ctx(),
                    profile_id,
                    Some((name, self.new_remote_space_backend)),
                );
            }
        });
    }

    fn remote_source_label(&self) -> String {
        match &self.remote_source {
            SpaceRemoteOverride::Inherit => "Inherit".to_owned(),
            SpaceRemoteOverride::Local => "This computer".to_owned(),
            SpaceRemoteOverride::Profile(remote) => self
                .profiles
                .iter()
                .find(|(id, _)| id == &remote.profile_id)
                .map(|(_, profile)| profile.name.clone())
                .unwrap_or_else(|| remote.profile_id.clone()),
            SpaceRemoteOverride::Inline(remote) => format!("Legacy: {}", remote.host),
        }
    }

    fn remote_ready(&self) -> bool {
        !matches!(
            &self.remote_source,
            SpaceRemoteOverride::Profile(remote) if remote.remote_space_id.is_empty()
        )
    }

    fn start_catalog(
        &mut self,
        ctx: &egui::Context,
        profile_id: &str,
        create: Option<(String, MultiplexerBackendConfig)>,
    ) {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|(id, _)| id == profile_id)
            .map(|(_, profile)| profile.clone())
        else {
            self.catalog = RemoteCatalogState::Failed {
                profile_id: profile_id.to_owned(),
                error: format!("SSH profile '{profile_id}' is unavailable"),
            };
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = if let Some((name, backend)) = create {
                remote_catalog::create_remote(&profile, &name, backend).and_then(|selected| {
                    remote_catalog::list_remote(&profile).map(|spaces| RemoteCatalogResult {
                        spaces,
                        selected: Some(selected),
                    })
                })
            } else {
                remote_catalog::list_remote(&profile).map(|spaces| RemoteCatalogResult {
                    spaces,
                    selected: None,
                })
            }
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
            ctx.request_repaint();
        });
        self.catalog = RemoteCatalogState::Running {
            profile_id: profile_id.to_owned(),
            receiver,
        };
    }

    fn poll_catalog(&mut self) {
        let RemoteCatalogState::Running {
            profile_id,
            receiver,
        } = &self.catalog
        else {
            return;
        };
        let Ok(result) = receiver.try_recv() else {
            return;
        };
        let profile_id = profile_id.clone();
        match result {
            Ok(result) => self.accept_catalog_result(profile_id, result),
            Err(error) => {
                self.catalog = RemoteCatalogState::Failed { profile_id, error };
            }
        }
    }

    fn accept_catalog_result(&mut self, profile_id: String, result: RemoteCatalogResult) {
        if let Some(selected) = result.selected {
            self.backend = Some(selected.backend);
            self.remote_source = SpaceRemoteOverride::Profile(RemoteSpaceRef {
                profile_id: profile_id.clone(),
                remote_space_id: selected.id,
                remote_space_name: selected.name,
                backend: selected.backend,
            });
            self.new_remote_space_name.clear();
        }
        self.catalog = RemoteCatalogState::Ready {
            profile_id,
            spaces: result.spaces,
        };
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
