use std::sync::LazyLock;

use bootty_config::config::{MultiplexerBackendConfig, SshProfileConfig};
use bootty_mux::controller::SpaceId;
use bootty_ui::overlay::{FloatingWindow, TextPrompt};
use bootty_ui::{
    Theme, ThemePalette,
    icons::{has_slug, icon_text},
    overlay,
};
use eframe::egui;
use iconflow::{Pack, list};

use crate::remote_catalog::{RemoteCatalogResult, RemoteCatalogTask, RemoteSpaceSummary};
use bootty_workspace::{
    DEFAULT_SPACE_COLOR, RemoteSpaceRef, SpaceMuxOverride, SpaceRemoteOverride,
};

#[derive(Debug)]
pub struct SpaceEditorDialog {
    draft: SpaceDraft,
    profiles: Vec<(String, SshProfileConfig)>,
    catalog: RemoteCatalogState,
    new_remote_space_name: String,
    new_remote_space_backend: MultiplexerBackendConfig,
    focus: bool,
    icon_search: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceDraft {
    pub space_id: Option<SpaceId>,
    pub name: String,
    pub icon: String,
    pub color: [u8; 3],
    pub tint_sidebar: bool,
    pub backend: Option<MultiplexerBackendConfig>,
    pub remote_source: SpaceRemoteOverride,
}

#[derive(Debug, Default)]
enum RemoteCatalogState {
    #[default]
    Idle,
    Running(RemoteCatalogTask),
    Ready(Vec<RemoteSpaceSummary>, Option<String>),
    Failed(String),
}
struct SpacePresentation {
    title: &'static str,
    normalized_name: Option<String>,
    can_save: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpaceEditorIntent {
    Close,
    Save(SpaceDraft),
}

impl SpaceEditorDialog {
    pub fn new_space(icon: String, mux: SpaceMuxOverride) -> Self {
        Self::open(None, String::new(), icon, DEFAULT_SPACE_COLOR, false, mux)
    }

    pub fn edit_space(
        space_id: SpaceId,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> Self {
        Self::open(Some(space_id), name, icon, color, tint_sidebar, mux)
    }

    fn open(
        space_id: Option<SpaceId>,
        name: String,
        icon: String,
        color: [u8; 3],
        tint_sidebar: bool,
        mux: SpaceMuxOverride,
    ) -> Self {
        Self {
            draft: SpaceDraft {
                space_id,
                name,
                icon,
                color,
                tint_sidebar,
                backend: mux.backend,
                remote_source: mux.remote,
            },
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

    pub fn show(&mut self, ctx: &egui::Context, theme: Theme) -> Option<SpaceEditorIntent> {
        self.poll_catalog();
        let presentation = self.presentation();
        let result = FloatingWindow::new("space-editor-dialog", presentation.title)
            .icon("shapes")
            .hint("Enter save   Esc close")
            .width(overlay::panel_width(ctx, 620.0, 420.0))
            .show(ctx, theme, |ui, palette| {
                let submitted = TextPrompt::new("space-editor-name")
                    .caption("space name")
                    .hint("space name...")
                    .validation(
                        presentation
                            .normalized_name
                            .is_none()
                            .then_some("name cannot be empty"),
                    )
                    .submit_disabled(presentation.normalized_name.is_none())
                    .show(ui, theme, &mut self.draft.name, &mut self.focus)
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
                    egui::color_picker::color_edit_button_srgb(ui, &mut self.draft.color);
                    ui.label(
                        egui::RichText::new(color_hex(self.draft.color))
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                });
                ui.checkbox(
                    &mut self.draft.tint_sidebar,
                    "Tint sidebar with Space color",
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("backend")
                            .monospace()
                            .size(12.0)
                            .color(palette.muted),
                    );
                    ui.add_enabled_ui(
                        !matches!(&self.draft.remote_source, SpaceRemoteOverride::Profile(_)),
                        |ui| {
                            egui::ComboBox::from_id_salt("space-editor-backend")
                                .selected_text(backend_label(self.draft.backend))
                                .show_ui(ui, |ui| {
                                    for backend in [
                                        None,
                                        Some(MultiplexerBackendConfig::Native),
                                        Some(MultiplexerBackendConfig::Rmux),
                                        Some(MultiplexerBackendConfig::Tmux),
                                    ] {
                                        ui.selectable_value(
                                            &mut self.draft.backend,
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
                overlay::filter_field(
                    ui,
                    egui::Id::new("space-icon-search"),
                    &mut self.icon_search,
                    theme,
                    "search icons...",
                )
                .on_hover_text("Filter Phosphor and Lucide icons");
                ui.add_space(6.0);
                show_icon_picker(ui, palette, &mut self.draft.icon, &self.icon_search);
                ui.add_space(16.0);
                submitted
                    || ui
                        .add_enabled(presentation.can_save, egui::Button::new("Save"))
                        .clicked()
            });

        if result.inner
            && presentation.can_save
            && let Some(name) = presentation.normalized_name
        {
            self.draft.name = name;
            return Some(SpaceEditorIntent::Save(self.draft.clone()));
        }
        if result.escaped || result.clicked_outside {
            return Some(SpaceEditorIntent::Close);
        }
        None
    }

    fn presentation(&self) -> SpacePresentation {
        let normalized_name = normalized_name(&self.draft.name);
        SpacePresentation {
            title: if self.draft.space_id.is_some() {
                "Edit Space"
            } else {
                "New Space"
            },
            can_save: normalized_name.is_some() && self.remote_ready(),
            normalized_name,
        }
    }

    /// The host this space's multiplexer runs on. Only for the backends bootty reaches through a
    /// client — the others keep their terminals in this process, with no host to name.
    fn remote_ui(&mut self, ui: &mut egui::Ui, theme: Theme) {
        self.location_ui(ui);
        if let Some(profile_id) = self.selected_profile_id() {
            self.catalog_ui(ui, theme, profile_id);
        } else if matches!(&self.draft.remote_source, SpaceRemoteOverride::Inline(_)) {
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
        match &self.draft.remote_source {
            SpaceRemoteOverride::Profile(remote) => Some(remote.profile_id.clone()),
            _ => None,
        }
    }

    fn location_ui(&mut self, ui: &mut egui::Ui) {
        let mut requested = None;
        ui.horizontal(|ui| {
            ui.label("location");
            egui::ComboBox::from_id_salt("space-editor-location")
                .selected_text(self.remote_source_label())
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            matches!(&self.draft.remote_source, SpaceRemoteOverride::Inherit),
                            "Inherit",
                        )
                        .clicked()
                    {
                        requested = Some(SpaceRemoteOverride::Inherit);
                    }
                    if ui
                        .selectable_label(
                            matches!(&self.draft.remote_source, SpaceRemoteOverride::Local),
                            "This computer",
                        )
                        .clicked()
                    {
                        requested = Some(SpaceRemoteOverride::Local);
                    }
                    for (profile_id, profile) in &self.profiles {
                        let selected = matches!(
                            &self.draft.remote_source,
                            SpaceRemoteOverride::Profile(remote)
                                if remote.profile_id.as_str() == profile_id.as_str()
                        );
                        if ui.selectable_label(selected, &profile.name).clicked() {
                            requested = Some(SpaceRemoteOverride::Profile(RemoteSpaceRef {
                                profile_id: profile_id.clone(),
                                remote_space_id: String::new(),
                                remote_space_name: String::new(),
                                backend: MultiplexerBackendConfig::Tmux,
                            }));
                        }
                    }
                });
        });
        if let Some(remote) = requested {
            self.draft.remote_source = remote;
            self.catalog = RemoteCatalogState::Idle;
        }
    }

    fn catalog_ui(&mut self, ui: &mut egui::Ui, theme: Theme, profile_id: String) {
        match std::mem::take(&mut self.catalog) {
            RemoteCatalogState::Idle => self.start_catalog(&profile_id, None),
            RemoteCatalogState::Running(task) => {
                ui.label("Loading remote Spaces...");
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
                self.catalog = RemoteCatalogState::Running(task);
            }
            RemoteCatalogState::Failed(error) => {
                ui.label(egui::RichText::new(&error).color(theme.palette.destructive));
                self.catalog = RemoteCatalogState::Failed(error);
                if ui.button("Retry").clicked() {
                    self.start_catalog(&profile_id, None);
                }
            }
            RemoteCatalogState::Ready(spaces, warning) => {
                if let Some(warning) = &warning {
                    ui.label(egui::RichText::new(warning).color(theme.palette.destructive));
                }
                let selected = match &self.draft.remote_source {
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
                        for space in &spaces {
                            let current = matches!(
                                &self.draft.remote_source,
                                SpaceRemoteOverride::Profile(remote)
                                    if remote.remote_space_id == space.id
                            );
                            if ui.selectable_label(current, &space.name).clicked() {
                                self.draft.backend = Some(space.backend);
                                self.draft.remote_source =
                                    SpaceRemoteOverride::Profile(RemoteSpaceRef {
                                        profile_id: profile_id.clone(),
                                        remote_space_id: space.id.clone(),
                                        remote_space_name: space.name.clone(),
                                        backend: space.backend,
                                    });
                            }
                        }
                    });
                let create = self.create_remote_space_ui(ui, theme);
                self.catalog = RemoteCatalogState::Ready(spaces, warning);
                if let Some(create) = create {
                    self.start_catalog(&profile_id, Some(create));
                }
            }
        }
    }

    fn create_remote_space_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: Theme,
    ) -> Option<(String, MultiplexerBackendConfig)> {
        ui.horizontal(|ui| {
            bootty_ui::themed_text_edit_singleline(
                ui,
                &mut self.new_remote_space_name,
                theme,
                |edit| edit.hint_text("new remote Space").desired_width(220.0),
            );
            egui::ComboBox::from_id_salt("space-editor-new-remote-backend")
                .selected_text(backend_label(Some(self.new_remote_space_backend)))
                .show_ui(ui, |ui| {
                    for backend in [
                        MultiplexerBackendConfig::Rmux,
                        MultiplexerBackendConfig::Tmux,
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
                return Some((
                    self.new_remote_space_name.trim().to_owned(),
                    self.new_remote_space_backend,
                ));
            }
            None
        })
        .inner
    }

    fn remote_source_label(&self) -> String {
        match &self.draft.remote_source {
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
            &self.draft.remote_source,
            SpaceRemoteOverride::Profile(remote) if remote.remote_space_id.is_empty()
        )
    }

    fn start_catalog(
        &mut self,
        profile_id: &str,
        create: Option<(String, MultiplexerBackendConfig)>,
    ) {
        let Some(profile) = self
            .profiles
            .iter()
            .find(|(id, _)| id == profile_id)
            .map(|(_, profile)| profile.clone())
        else {
            self.catalog =
                RemoteCatalogState::Failed(format!("SSH profile '{profile_id}' is unavailable"));
            return;
        };
        self.catalog = match RemoteCatalogTask::start(profile_id.to_owned(), profile, create) {
            Ok(task) => RemoteCatalogState::Running(task),
            Err(error) => RemoteCatalogState::Failed(error.to_owned()),
        };
    }

    fn poll_catalog(&mut self) {
        let RemoteCatalogState::Running(task) = &self.catalog else {
            return;
        };
        let Some(result) = task.try_recv() else {
            return;
        };
        let profile_id = task.profile_id.clone();
        match result {
            Ok(result) => self.accept_catalog_result(&profile_id, result),
            Err(error) => self.catalog = RemoteCatalogState::Failed(error),
        }
    }

    fn accept_catalog_result(&mut self, profile_id: &str, result: RemoteCatalogResult) {
        if self.selected_profile_id().as_deref() != Some(profile_id) {
            self.catalog = RemoteCatalogState::Idle;
            return;
        }
        let (spaces, warning) = match result {
            RemoteCatalogResult::Listed(spaces) => (spaces, None),
            RemoteCatalogResult::Created {
                selected,
                refreshed,
            } => {
                self.draft.backend = Some(selected.backend);
                self.draft.remote_source = SpaceRemoteOverride::Profile(RemoteSpaceRef {
                    profile_id: profile_id.to_owned(),
                    remote_space_id: selected.id.clone(),
                    remote_space_name: selected.name.clone(),
                    backend: selected.backend,
                });
                self.new_remote_space_name.clear();
                match refreshed {
                    Ok(spaces) => (spaces, None),
                    Err(error) => (
                        vec![selected],
                        Some(format!(
                            "Remote Space was created, but refresh failed: {error}"
                        )),
                    ),
                }
            }
        };
        self.catalog = RemoteCatalogState::Ready(spaces, warning);
    }
}

pub(crate) fn default_space_icon(existing: &[String]) -> String {
    space_icon_inventory()
        .iter()
        .find(|icon| !existing.iter().any(|used| used == *icon))
        .or_else(|| space_icon_inventory().first())
        .cloned()
        .unwrap_or_else(|| "folder".to_owned())
}

pub(crate) fn space_icon_inventory() -> &'static [String] {
    static ICONS: LazyLock<Vec<String>> = LazyLock::new(|| {
        list(Pack::Phosphor)
            .iter()
            .filter_map(|icon| icon.strip_suffix("-duotone"))
            .map(|icon| format!("phosphor:{icon}"))
            .chain(list(Pack::Lucide).iter().map(|icon| (*icon).to_owned()))
            .filter(|icon| has_slug(icon))
            .collect()
    });
    &ICONS
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
                            let current = selected.as_str() == icon.as_str();
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
                                .on_hover_text(icon.as_str())
                                .clicked()
                            {
                                *selected = (**icon).clone();
                            }
                            if (index + 1) % columns == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
    });
}

fn matching_icons(query: &str) -> Vec<&'static String> {
    let query = query.to_ascii_lowercase();
    space_icon_inventory()
        .iter()
        .filter(|icon| icon.to_ascii_lowercase().contains(&query))
        .collect()
}

fn backend_label(backend: Option<MultiplexerBackendConfig>) -> &'static str {
    match backend {
        None => "Inherit",
        Some(MultiplexerBackendConfig::Native) => "Native",
        Some(MultiplexerBackendConfig::Rmux) => "Rmux",
        Some(MultiplexerBackendConfig::Tmux) => "Tmux",
    }
}

fn color_hex([red, green, blue]: [u8; 3]) -> String {
    format!("#{red:02X}{green:02X}{blue:02X}")
}

fn normalized_name(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_owned())
}
