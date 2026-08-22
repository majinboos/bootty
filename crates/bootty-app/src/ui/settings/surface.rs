//! Full-app settings surface for editing the user config.
//!
//! The surface edits an in-memory document. The app config owner validates, persists, and
//! publishes each complete draft at the frame boundary.

mod appearance;
mod font;
mod keybinds;
mod modules;
mod remotes;
mod session;
mod status_bar;
mod window;
mod writeback;

use std::path::PathBuf;

use bootty_extension::{ModuleSourceOutcome, ModuleSourceRequest, ModuleSources};

use bootty_config::{
    color::Color,
    config::{BoottyConfig, ConfigDocument, MultiplexerBackendConfig, SidebarPosition},
};
use bootty_ui::settings::{
    DragHandle, NumberEditSpec, apply_reorder, path_row, reorderable_list, searchable_combo,
    section, settings_button, settings_color_picker, settings_icon_button, settings_notice,
    settings_number_edit, settings_page_header, settings_row, settings_segmented,
    settings_segmented_ltr, settings_slider_with_edit, settings_text_edit,
    settings_text_edit_width, settings_toggle, settings_toggle_row,
};
use bootty_ui::{Theme, ThemePalette, readable_color};
use bootty_winit::direct_input::ModifierSideState;
use eframe::egui::{self, Color32, Pos2, Rect, RichText, UiBuilder, Vec2};

const SEARCH_ID: &str = "bootty::settings::search";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
    Remotes,
    Text,
    Appearance,
    Window,
    Sidebar,
    Shell,
    Status,
    Keys,
    Config,
    Diagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PageMeta {
    page: SettingsPage,
    group: &'static str,
    label: &'static str,
    terms: &'static str,
}

macro_rules! page {
    ($page:ident, $group:literal, $label:literal, $terms:literal) => {
        PageMeta {
            page: SettingsPage::$page,
            group: $group,
            label: $label,
            terms: $terms,
        }
    };
}

#[rustfmt::skip]
const PAGE_META: [PageMeta; 11] = [
    page!(General, "Core", "General", "default profile|multiplexer|backend|sidebar|status bar|new windows|terminal preview"),
    page!(Remotes, "Core", "Remotes", "ssh|remote|profile|host|port|user|authentication|private key|proxy|test connection"),
    page!(Text, "Core", "Text", "font|family|fallback|size|cell width|cell height|baseline|underline|glyph|features"),
    page!(Appearance, "Core", "Appearance", "theme|colors|background|foreground|cursor|selection|ansi|palette|sidebar colors"),
    page!(Window, "Core", "Window", "window|title|titlebar|decoration|fullscreen|size|width|height|sidebar|chrome|dim"),
    page!(Sidebar, "Core", "Sidebar", "sidebar|session|navigation|position|width|background|foreground|selected|hover|border|modules|luau|source"),
    page!(Status, "Core", "Status Bar", "status|modules|segments|clock|sysinfo|alignment|icon|foreground|background|luau|source"),
    page!(Shell, "Terminal", "Shell", "shell|working directory|environment|env|term|colorterm|scrollback|glyph protocol"),
    page!(Keys, "Terminal", "Keys", "keybindings|shortcuts|scope|global|native|tmux|sidebar|modifier remap|option as alt|record shortcut"),
    page!(Config, "Advanced", "Config", "config|path|directory|themes|status modules|reload|last write error"),
    page!(Diagnostics, "Advanced", "Diagnostics", "diagnostics|stability trace|trace|reload|errors"),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsAction {
    #[default]
    None,
    Close,
}

pub struct SettingsSurface {
    config: BoottyConfig,
    writeback: writeback::SettingsWriteback,
    page: SettingsPage,
    palette: ThemePalette,
    search: String,
    /// Environment catalogs the pages pick from, scanned by the owner when settings opens so no
    /// page reads the font database or the themes directory while painting.
    font_families: Vec<String>,
    theme_names: Vec<String>,
    appearance_variant: bootty_config::config::AppearanceVariant,
    remote_editor: remotes::EditorState,
    keybinds: keybinds::EditorState,
    /// Editable `NAME=value` rows. Loaded lazily and kept until settings reopens, so an
    /// incomplete pair survives an accepted rebind instead of being dropped by the write filter.
    session_env: Option<Vec<(String, String)>>,
    /// Binding-trigger chords captured this frame from the host's direct input path, fed in by the
    /// app so the recorder can capture cmd-modified combos egui drops (⌘V, ⌘⌥X, …).
    recorder_chords: Vec<String>,
    /// Which physical modifier keys are held right now, fed in by the app each frame. Wheel steps
    /// carry no key event, so this is the only source of left/right sides for a scroll recording.
    recorder_modifier_sides: ModifierSideState,
    /// The global style captured when settings opened, restored on close so the
    /// settings-only widget overrides don't leak into the main UI's popups.
    base_style: Option<egui::Style>,
    module_editor: modules::EditorState,
    /// The accepted-config revision `config` and the writeback document were taken from.
    /// `None` until the first sync, so a freshly opened surface always refreshes once.
    synced_revision: Option<u64>,
}

impl SettingsSurface {
    #[must_use]
    pub fn new(config: BoottyConfig, document: ConfigDocument) -> Self {
        let writeback = writeback::SettingsWriteback::new(document);
        Self {
            config,
            writeback,
            page: SettingsPage::default(),
            palette: ThemePalette::default(),
            search: String::new(),
            font_families: Vec::new(),
            theme_names: Vec::new(),
            appearance_variant: bootty_config::config::AppearanceVariant::Dark,
            remote_editor: remotes::EditorState::default(),
            keybinds: keybinds::EditorState::default(),
            session_env: None,
            recorder_chords: Vec::new(),
            recorder_modifier_sides: ModifierSideState::default(),
            base_style: None,
            module_editor: modules::EditorState::default(),
            synced_revision: None,
        }
    }

    /// Install the font and theme catalogs for this settings session.
    pub fn set_catalogs(&mut self, font_families: Vec<String>, theme_names: Vec<String>) {
        self.font_families = font_families;
        self.theme_names = theme_names;
    }

    pub fn is_recording_keybind(&self) -> bool {
        self.keybinds.is_recording()
    }

    /// Refresh accepted settings without discarding explicit editor drafts.
    pub fn rebind_accepted_config(
        &mut self,
        config: BoottyConfig,
        document: ConfigDocument,
        warning: Option<String>,
    ) {
        self.config = config;
        self.writeback.accept(document, warning);
    }

    pub fn reset_accepted_config(&mut self, config: BoottyConfig, document: ConfigDocument) {
        self.config = config;
        self.session_env = None;
        self.writeback.accept(document, None);
        self.module_editor.discard_created();
    }

    pub fn take_document_submission(&mut self) -> Option<ConfigDocument> {
        self.writeback.take_submission()
    }

    pub(crate) fn take_remote_test(&mut self) -> Option<remotes::RemoteTest> {
        self.remote_editor.take_test()
    }

    /// Module-source edits collected while painting, for the extension host to run.
    pub(crate) fn take_module_requests(&mut self) -> Vec<ModuleSourceRequest> {
        self.module_editor.take_requests()
    }

    pub(crate) fn apply_module_outcome(&mut self, outcome: ModuleSourceOutcome) {
        self.module_editor.apply(outcome);
    }

    /// Whether the accepted config at `revision` still has to be copied in. An unchanged
    /// revision spares the caller a whole-config and document clone every frame.
    pub fn needs_accepted_config(&self, revision: u64) -> bool {
        // A dirty draft makes the sync a no-op, so the caller should not build the copy at all.
        !self.writeback.is_dirty() && self.synced_revision != Some(revision)
    }

    pub fn sync_accepted_config(
        &mut self,
        config: BoottyConfig,
        document: ConfigDocument,
        revision: u64,
    ) {
        // An in-progress draft keeps its values; the sync retries once the owner accepts it.
        if !self.writeback.is_dirty() {
            self.config = config;
            self.writeback.sync_accepted(document);
            self.synced_revision = Some(revision);
        }
    }

    /// The owner refused this submission. Keep the exact draft, show why, and re-arm the editors
    /// that clear their dirty flag on submit so their Apply button comes back.
    pub fn reject_submission(&mut self, error: impl ToString) {
        self.writeback.set_error(error);
        self.keybinds.rearm_after_rejected_submission();
    }

    /// Jump to the keybindings page focused on `action` (in the Global list),
    /// adding an editable row for it if none exists yet. Used by the command
    /// palette's "configure this command's keybinding" chord.
    pub fn focus_keybinding(&mut self, action: &str) {
        self.page = SettingsPage::Keys;
        self.keybinds
            .focus_action(keybinds::KeybindScope::Global, Some(action));
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: Theme,
        captured_chords: Vec<String>,
        modifier_sides: ModifierSideState,
        sources: ModuleSources<'_>,
    ) -> SettingsAction {
        self.recorder_chords = captured_chords;
        self.recorder_modifier_sides = modifier_sides;
        self.palette = theme.palette;
        bootty_ui::configure_style(ui.style_mut(), theme);
        // Remember the style as it was before settings overrode it, so closing
        // settings can restore it (the overrides below mutate the shared context
        // style, which popups read globally).
        if self.base_style.is_none() {
            self.base_style = Some((*ui.ctx().global_style()).clone());
        }
        let mut style = (*ui.ctx().global_style()).clone();
        bootty_ui::configure_style(&mut style, theme);
        style.spacing.interact_size.y = 34.0;
        style.spacing.combo_width = 220.0;
        style.visuals.window_fill = self.palette.pane;
        style.visuals.window_stroke = egui::Stroke::new(1.0, self.palette.border);
        style.visuals.popup_shadow = egui::epaint::Shadow::NONE;
        style.visuals.widgets.inactive.bg_fill = self.palette.surface;
        style.visuals.widgets.inactive.weak_bg_fill = self.palette.surface;
        style.visuals.widgets.inactive.fg_stroke =
            egui::Stroke::new(1.0, readable_color(self.palette.surface, self.palette.text));
        style.visuals.widgets.hovered.bg_fill = self.palette.hover;
        style.visuals.widgets.hovered.weak_bg_fill = self.palette.hover;
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, self.palette.accent);
        style.visuals.widgets.hovered.fg_stroke =
            egui::Stroke::new(1.0, readable_color(self.palette.hover, self.palette.text));
        style.visuals.widgets.active.bg_fill = self.palette.accent;
        style.visuals.widgets.active.weak_bg_fill = self.palette.accent;
        style.visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, self.palette.accent);
        style.visuals.widgets.active.fg_stroke =
            egui::Stroke::new(1.0, readable_color(self.palette.accent, self.palette.text));
        ui.ctx().set_global_style(style);

        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let search_focused = ui
            .ctx()
            .memory(|memory| memory.has_focus(egui::Id::new(SEARCH_ID)));
        if escape {
            if self.keybinds.capturing_chord() {
                self.keybinds.cancel_capture();
                return SettingsAction::None;
            }
            if search_focused {
                // Drop the search focus rather than swallowing Escape silently; a
                // second press then closes via the no-focus branch below.
                ui.ctx()
                    .memory_mut(|memory| memory.surrender_focus(egui::Id::new(SEARCH_ID)));
                return SettingsAction::None;
            }
            if ui.ctx().memory(|memory| memory.focused().is_none()) {
                keybinds::commit_draft(self);
                return SettingsAction::Close;
            }
        }

        let mut action = SettingsAction::None;
        egui::Frame::NONE.fill(self.palette.base).show(ui, |ui| {
            let rect = ui.max_rect();
            let sidebar_width = 286.0_f32.min(rect.width() * 0.42);
            let sidebar_rect =
                Rect::from_min_max(rect.min, Pos2::new(rect.min.x + sidebar_width, rect.max.y));
            let content_rect =
                Rect::from_min_max(Pos2::new(sidebar_rect.max.x, rect.min.y), rect.max);

            ui.painter()
                .rect_filled(sidebar_rect, 0.0, self.palette.mantle);
            ui.painter()
                .rect_filled(content_rect, 0.0, self.palette.base);

            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| {
                    if self.settings_sidebar(ui) {
                        action = SettingsAction::Close;
                    }
                },
            );

            ui.scope_builder(
                UiBuilder::new()
                    .max_rect(content_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
                |ui| self.settings_content(ui, sources),
            );
        });
        if action == SettingsAction::Close {
            keybinds::commit_draft(self);
        }
        action
    }

    /// Restore the global style captured when settings opened. The app calls this
    /// once settings closes so the settings-only widget overrides don't persist
    /// into the rest of the UI. Idempotent: a no-op once already restored.
    pub fn restore_global_style(&mut self, ctx: &egui::Context) {
        if let Some(style) = self.base_style.take() {
            ctx.set_global_style(style);
        }
    }

    fn settings_sidebar(&mut self, ui: &mut egui::Ui) -> bool {
        egui::Frame::NONE
            .fill(self.palette.mantle)
            .inner_margin(egui::Margin {
                left: 14,
                right: 0,
                top: 36,
                bottom: 16,
            })
            .show(ui, |ui| {
                let close = settings_button(ui, self.palette, "Back to terminal").clicked();

                ui.add_space(10.0);
                ui.scope(|ui| {
                    ui.set_width((ui.available_width() - 14.0).max(80.0));
                    settings_text_edit(ui, self.palette, &mut self.search, "Search settings...");
                });
                ui.add_space(16.0);

                let query = self.search.trim().to_ascii_lowercase();
                egui::ScrollArea::vertical()
                    .id_salt("settings_sidebar_pages")
                    .max_height(ui.available_height())
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width((ui.available_width() - 14.0).max(0.0));
                        for group in ["Core", "Terminal", "Advanced"] {
                            let visible_pages = PAGE_META
                                .iter()
                                .copied()
                                .filter(|meta| meta.group == group)
                                .filter(|meta| query.is_empty() || page_matches(*meta, &query));
                            if visible_pages.clone().next().is_none() {
                                continue;
                            }
                            ui.label(
                                RichText::new(group)
                                    .color(readable_color(self.palette.mantle, self.palette.muted))
                                    .size(11.0),
                            );
                            ui.add_space(4.0);
                            for meta in visible_pages {
                                self.sidebar_page_button(ui, meta);
                            }
                            ui.add_space(12.0);
                        }
                    });
                close
            })
            .inner
    }

    fn sidebar_page_button(&mut self, ui: &mut egui::Ui, meta: PageMeta) {
        let selected = self.page == meta.page;
        let tint = if selected {
            self.palette.text
        } else {
            self.palette.subtext
        };
        let button = egui::Button::new(RichText::new(meta.label).color(tint))
            .selected(selected)
            .corner_radius(self.palette.radius);
        if ui.add_sized([ui.available_width(), 34.0], button).clicked() {
            self.page = meta.page;
            self.keybinds.cancel_capture();
        }
    }

    fn settings_content(&mut self, ui: &mut egui::Ui, sources: ModuleSources<'_>) {
        egui::Frame::NONE.fill(self.palette.base).show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("settings_content")
                .max_height(ui.available_height())
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin {
                            left: 36,
                            right: 36,
                            top: 30,
                            bottom: 24,
                        })
                        .show(ui, |ui| {
                            let meta = page_meta(self.page);
                            settings_page_header(ui, self.palette, "Bootty Settings", meta.label);
                            if let Some(error) = self.writeback.last_error().map(str::to_owned) {
                                settings_notice(
                                    ui,
                                    self.palette.destructive,
                                    &format!("Write failed: {error}"),
                                );
                            }
                            let max_width = match self.page {
                                SettingsPage::Keys
                                | SettingsPage::Status
                                | SettingsPage::Sidebar => 1040.0,
                                SettingsPage::Appearance => 860.0,
                                _ => 780.0,
                            };
                            let content_width = ui.available_width().min(max_width);
                            let left_pad = ((ui.available_width() - content_width) * 0.5).max(0.0);
                            ui.horizontal(|ui| {
                                ui.add_space(left_pad);
                                ui.allocate_ui_with_layout(
                                    Vec2::new(content_width, ui.available_height()),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| match self.page {
                                        SettingsPage::General => self.general_ui(ui),
                                        SettingsPage::Remotes => remotes::ui(self, ui),
                                        SettingsPage::Text => font::ui(self, ui),
                                        SettingsPage::Appearance => appearance::ui(self, ui),
                                        SettingsPage::Window => window::ui(self, ui),
                                        SettingsPage::Sidebar => self.sidebar_ui(ui, sources),
                                        SettingsPage::Shell => session::ui(self, ui),
                                        SettingsPage::Status => status_bar::ui(self, ui, sources),
                                        SettingsPage::Keys => keybinds::ui(self, ui),
                                        SettingsPage::Config => self.config_ui(ui),
                                        SettingsPage::Diagnostics => self.diagnostics_ui(ui),
                                    },
                                );
                            });
                        });
                });
        });
    }

    fn general_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, self.palette, "DEFAULTS");
        settings_row(
            ui,
            self.palette,
            "Default backend",
            "Switches immediately for new mux actions and refreshes live config.",
            |ui| {
                let mut backend = self.config.multiplexer.backend;
                let options = available_backend_options();
                let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
                let current = options
                    .iter()
                    .position(|(candidate, _)| *candidate == backend)
                    .unwrap_or(0);
                if let Some(index) = settings_segmented(ui, self.palette, &labels, current) {
                    backend = options[index].0;
                    self.config.multiplexer.backend = backend;
                    self.writeback
                        .set_str(&["multiplexer", "backend"], backend_token(backend));
                    // native and rmux keep their terminals in this process, so a remote left
                    // behind here would be a config the next load refuses.
                    if !backend.supports_remote() {
                        self.clear_multiplexer_remote();
                    }
                }
            },
        );
        settings_row(
            ui,
            self.palette,
            "Chrome visibility",
            "Show or hide persistent app chrome.",
            |ui| {
                let mut sidebar = self.config.chrome.sidebar;
                if settings_toggle(ui, self.palette, &mut sidebar) {
                    self.config.chrome.sidebar = sidebar;
                    self.writeback.set_bool(&["chrome", "sidebar"], sidebar);
                }
                ui.label(RichText::new("Sidebar").color(self.palette.subtext));
                ui.add_space(16.0);
                let mut top_bar = self.config.chrome.top_bar;
                if settings_toggle(ui, self.palette, &mut top_bar) {
                    self.config.chrome.top_bar = top_bar;
                    self.set_top_bar(top_bar);
                }
                ui.label(RichText::new("Top bar").color(self.palette.subtext));
                ui.add_space(16.0);
                let mut bottom_bar = self.config.chrome.bottom_bar;
                if settings_toggle(ui, self.palette, &mut bottom_bar) {
                    self.config.chrome.bottom_bar = bottom_bar;
                    self.writeback
                        .set_bool(&["chrome", "bottom-bar"], bottom_bar);
                }
                ui.label(RichText::new("Bottom bar").color(self.palette.subtext));
            },
        );
    }

    fn clear_multiplexer_remote(&mut self) {
        self.config.multiplexer.remote = None;
        self.writeback.remove(&["multiplexer", "remote"]);
    }

    fn config_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, self.palette, "LOCATIONS");
        path_row(ui, self.palette, "Config file", &self.config.config_path);
        if let Some(parent) = self.config.config_path.parent() {
            path_row(ui, self.palette, "Config directory", parent);
            path_row(ui, self.palette, "Themes directory", &parent.join("themes"));
            path_row(
                ui,
                self.palette,
                "Extensions directory",
                &parent.join("extensions"),
            );
        }
        section(ui, self.palette, "RELOAD");
        let status = self
            .writeback
            .last_error()
            .map_or("Last write succeeded", |_| "Last write failed");
        settings_notice(ui, self.palette.muted, status);
    }

    fn sidebar_ui(&mut self, ui: &mut egui::Ui, sources: ModuleSources<'_>) {
        section(ui, self.palette, "NAVIGATION");
        settings_row(
            ui,
            self.palette,
            "Position",
            "Dock the sidebar on the left or right edge.",
            |ui| {
                let mut position = self.config.sidebar.position;
                let options = [
                    (SidebarPosition::Left, "left"),
                    (SidebarPosition::Right, "right"),
                ];
                let labels = ["left", "right"];
                let current = options
                    .iter()
                    .position(|(candidate, _)| *candidate == position)
                    .unwrap_or(0);
                if let Some(index) = settings_segmented(ui, self.palette, &labels, current) {
                    position = options[index].0;
                    self.config.sidebar.position = position;
                    let token = match position {
                        SidebarPosition::Left => "left",
                        SidebarPosition::Right => "right",
                    };
                    self.writeback.set_str(&["sidebar", "position"], token);
                }
            },
        );
        if number_row(
            ui,
            self.palette,
            &mut self.config.chrome.sidebar_width,
            NumberRow {
                label: "Width",
                help: "Width of the session sidebar.",
                path: &["chrome", "sidebar-width"],
                range: 120.0..=600.0,
                suffix: " px",
                scale: 1.0,
                control: NumberControl::Slider,
            },
        ) {
            self.writeback.set_f32(
                &["chrome", "sidebar-width"],
                self.config.chrome.sidebar_width,
            );
        }
        settings_notice(
            ui,
            self.palette.muted,
            "Sidebar colors are edited in the Appearance pane.",
        );
        section(ui, self.palette, "KEYBOARD");
        settings_notice(
            ui,
            self.palette.muted,
            "Sidebar navigation shortcuts are edited in the Keys pane with the Sidebar scope.",
        );
        if settings_button(ui, self.palette, "Edit sidebar shortcuts").clicked() {
            self.page = SettingsPage::Keys;
            self.keybinds
                .focus_action(keybinds::KeybindScope::Sidebar, None);
        }
        modules::sidebar_ui(self, ui, sources);
    }

    fn diagnostics_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, self.palette, "TRACE");
        settings_row(
            ui,
            self.palette,
            "Stability trace",
            "Writes frame-timing diagnostics to this file. Leave empty to disable.",
            |ui| {
                let mut trace = self
                    .config
                    .diagnostics
                    .stability_trace
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if settings_text_edit(ui, self.palette, &mut trace, "path to trace log").changed() {
                    self.config.diagnostics.stability_trace = nonempty(&trace).map(PathBuf::from);
                    write_optional_text(
                        &mut self.writeback,
                        &["diagnostics", "stability-trace"],
                        &trace,
                    );
                }
            },
        );
        section(ui, self.palette, "STATE");
        path_row(ui, self.palette, "Config file", &self.config.config_path);
        if let Some(error) = self.writeback.last_error().map(str::to_owned) {
            settings_notice(ui, self.palette.destructive, &error);
        } else {
            settings_notice(ui, self.palette.muted, "No settings write errors recorded.");
        }
    }

    // --- config writeback -------------------------------------------------------------------

    fn set_top_bar(&mut self, enabled: bool) {
        self.writeback
            .mutate(move |document| document.set_top_bar_enabled(enabled));
    }

    /// Write one bar's ordered module segments from the working copy.
    fn set_status_segments(&mut self, position: status_bar::StatusBarPosition) {
        let segments = position.segments(&self.config.chrome).to_owned();
        self.writeback.mutate(move |document| match position {
            status_bar::StatusBarPosition::Top => document.set_top_status_segments(&segments),
            status_bar::StatusBarPosition::Bottom => document.set_bottom_status_segments(&segments),
        });
    }
}

fn page_meta(page: SettingsPage) -> PageMeta {
    PAGE_META
        .iter()
        .copied()
        .find(|meta| meta.page == page)
        .expect("settings page metadata exists")
}

fn page_matches(meta: PageMeta, query: &str) -> bool {
    meta.group.to_ascii_lowercase().contains(query)
        || meta.label.to_ascii_lowercase().contains(query)
        || meta.terms.split('|').any(|term| term.contains(query))
}

pub(super) fn nonempty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn write_optional_text(writeback: &mut writeback::SettingsWriteback, path: &[&str], value: &str) {
    match nonempty(value) {
        Some(value) => writeback.set_str(path, &value),
        None => writeback.remove(path),
    }
}

#[cfg(windows)]
fn available_backend_options() -> &'static [(MultiplexerBackendConfig, &'static str)] {
    &[
        (MultiplexerBackendConfig::Native, "native"),
        (MultiplexerBackendConfig::Rmux, "rmux"),
    ]
}

#[cfg(not(windows))]
fn available_backend_options() -> &'static [(MultiplexerBackendConfig, &'static str)] {
    &[
        (MultiplexerBackendConfig::Native, "native"),
        (MultiplexerBackendConfig::Rmux, "rmux"),
        (MultiplexerBackendConfig::Tmux, "tmux"),
    ]
}

fn backend_token(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
    }
}

#[derive(Clone, Copy)]
pub(super) enum NumberControl {
    Edit,
    Slider,
}

pub(super) struct NumberRow<'a> {
    pub label: &'a str,
    pub help: &'a str,
    pub path: &'a [&'a str],
    pub range: std::ops::RangeInclusive<f32>,
    pub suffix: &'a str,
    pub scale: f32,
    pub control: NumberControl,
}

pub(super) fn number_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    row: NumberRow<'_>,
) -> bool {
    let mut changed = false;
    settings_row(ui, palette, row.label, row.help, |ui| {
        let edit = NumberEditSpec {
            id_salt: row.path,
            range: row.range,
            suffix: row.suffix,
            precision: 1,
            display_scale: row.scale,
        };
        changed = match row.control {
            NumberControl::Edit => settings_number_edit(ui, palette, value, edit),
            NumberControl::Slider => settings_slider_with_edit(ui, palette, value, edit),
        };
    });
    changed
}

pub(super) fn optional_number_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut Option<f32>,
    default: f32,
    row: NumberRow<'_>,
) -> bool {
    let current = *value;
    let mut concrete = current.unwrap_or(default);
    let mut changed = false;
    settings_row(ui, palette, row.label, row.help, |ui| {
        let edit = NumberEditSpec {
            id_salt: row.path,
            range: row.range,
            suffix: row.suffix,
            precision: 1,
            display_scale: row.scale,
        };
        let edited = match row.control {
            NumberControl::Edit => settings_number_edit(ui, palette, &mut concrete, edit),
            NumberControl::Slider => settings_slider_with_edit(ui, palette, &mut concrete, edit),
        };
        if edited {
            *value = Some(concrete);
            changed = true;
        }
        let mut automatic = current.is_none();
        if settings_toggle(ui, palette, &mut automatic) {
            *value = (!automatic).then_some(concrete);
            changed = true;
        }
        ui.label(RichText::new("Auto").color(palette.muted));
    });
    changed
}

fn sidebar_color_row(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    path: &[&str],
    seed: Color32,
    field: fn(&mut bootty_config::config::SidebarConfig) -> &mut Option<Color>,
) {
    let palette = win.palette;
    optional_color_row(
        &mut win.writeback,
        ui,
        palette,
        (label, help),
        (path, seed),
        field(&mut win.config.sidebar),
    );
}

fn chrome_color_row(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    path: &[&str],
    seed: Color32,
    field: fn(&mut bootty_config::config::ChromeConfig) -> &mut Option<Color>,
) {
    let palette = win.palette;
    optional_color_row(
        &mut win.writeback,
        ui,
        palette,
        (label, help),
        (path, seed),
        field(&mut win.config.chrome),
    );
}

fn optional_color_row(
    writeback: &mut writeback::SettingsWriteback,
    ui: &mut egui::Ui,
    palette: ThemePalette,
    description: (&str, &str),
    storage: (&[&str], Color32),
    current: &mut Option<Color>,
) {
    let (label, help) = description;
    let (path, seed) = storage;
    settings_row(ui, palette, label, help, |ui| {
        if optional_color_edit(ui, palette, current, seed, false, path) {
            match *current {
                Some(color) => writeback.set_color_value(path, color),
                None => writeback.remove(path),
            }
        }
    });
}

/// Edit an inherited color in place. `alpha` adds the opacity control used by pane focus borders.
pub(super) fn optional_color_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    color: &mut Option<Color>,
    seed: Color32,
    alpha: bool,
    id_salt: &[&str],
) -> bool {
    let mut next = color.unwrap_or(Color {
        r: seed.r(),
        g: seed.g(),
        b: seed.b(),
        a: seed.a(),
    });
    let mut rgb = [next.r, next.g, next.b];
    let mut changed = settings_color_picker(ui, palette, &mut rgb).changed();
    next.r = rgb[0];
    next.g = rgb[1];
    next.b = rgb[2];
    if alpha {
        ui.label(RichText::new("Opacity").color(palette.muted));
        let mut opacity = f32::from(next.a) / 255.0;
        changed |= settings_number_edit(
            ui,
            palette,
            &mut opacity,
            NumberEditSpec {
                id_salt,
                range: 0.0..=1.0,
                suffix: "%",
                precision: 0,
                display_scale: 100.0,
            },
        );
        next.a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    if changed {
        *color = Some(next);
    }
    if color.is_some() && settings_button(ui, palette, "Reset").clicked() {
        *color = None;
        changed = true;
    }
    changed
}

fn chrome_color_row_with_alpha(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    label: &str,
    help: &str,
    path: &[&str],
    seed: Color32,
    field: fn(&mut bootty_config::config::ChromeConfig) -> &mut Option<Color>,
) {
    settings_row(ui, win.palette, label, help, |ui| {
        let color = field(&mut win.config.chrome);
        if optional_color_edit(ui, win.palette, color, seed, true, path) {
            match *color {
                Some(color) => win.writeback.set_color_value(path, color),
                None => win.writeback.remove(path),
            }
        }
    });
}
