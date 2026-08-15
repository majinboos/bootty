//! Full-app settings surface for editing the user config.
//!
//! Edits are live-applied by writing the changed key straight into `config.toml`; the main
//! window's `ConfigHotReload` watcher then re-reads the file and applies it.

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

use bootty_config::{
    color::Color,
    config::{BoottyConfig, MultiplexerBackendConfig, SidebarPosition},
};
use bootty_ui::settings::{
    ComboStyle, DragHandle, NumberEditSpec, apply_reorder, described_combo, path_row,
    reorderable_list, searchable_combo, section, settings_button, settings_color_picker,
    settings_icon_button, settings_notice, settings_number_edit, settings_page_header,
    settings_row, settings_segmented, settings_segmented_ltr, settings_slider_with_edit,
    settings_text_edit, settings_text_edit_width, settings_toggle, settings_toggle_row,
};
use bootty_ui::{Theme, ThemePalette, icons, readable_color};
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
    icon: &'static str,
    title: &'static str,
    terms: &'static [&'static str],
}

const PAGE_META: [PageMeta; 11] = [
    PageMeta {
        page: SettingsPage::General,
        group: "Core",
        label: "General",
        icon: "sliders-horizontal",
        title: "General",
        terms: &[
            "default profile",
            "multiplexer",
            "backend",
            "sidebar",
            "status bar",
            "new windows",
            "terminal preview",
        ],
    },
    PageMeta {
        page: SettingsPage::Remotes,
        group: "Core",
        label: "Remotes",
        icon: "server",
        title: "Remotes",
        terms: &[
            "ssh",
            "remote",
            "profile",
            "host",
            "port",
            "user",
            "authentication",
            "private key",
            "proxy",
            "test connection",
        ],
    },
    PageMeta {
        page: SettingsPage::Text,
        group: "Core",
        label: "Text",
        icon: "case-sensitive",
        title: "Text",
        terms: &[
            "font",
            "family",
            "fallback",
            "size",
            "cell width",
            "cell height",
            "baseline",
            "underline",
            "glyph",
            "features",
        ],
    },
    PageMeta {
        page: SettingsPage::Appearance,
        group: "Core",
        label: "Appearance",
        icon: "palette",
        title: "Appearance",
        terms: &[
            "theme",
            "colors",
            "background",
            "foreground",
            "cursor",
            "selection",
            "ansi",
            "palette",
            "sidebar colors",
        ],
    },
    PageMeta {
        page: SettingsPage::Window,
        group: "Core",
        label: "Window",
        icon: "panel-top",
        title: "Window",
        terms: &[
            "window",
            "title",
            "titlebar",
            "decoration",
            "fullscreen",
            "size",
            "width",
            "height",
            "sidebar",
            "chrome",
            "dim",
        ],
    },
    PageMeta {
        page: SettingsPage::Sidebar,
        group: "Core",
        label: "Sidebar",
        icon: "panel-left",
        title: "Sidebar",
        terms: &[
            "sidebar",
            "session",
            "navigation",
            "position",
            "width",
            "background",
            "foreground",
            "selected",
            "hover",
            "border",
            "modules",
            "luau",
            "source",
        ],
    },
    PageMeta {
        page: SettingsPage::Status,
        group: "Core",
        label: "Status Bar",
        icon: "activity",
        title: "Status Bar",
        terms: &[
            "status",
            "modules",
            "segments",
            "clock",
            "sysinfo",
            "alignment",
            "icon",
            "foreground",
            "background",
            "luau",
            "source",
        ],
    },
    PageMeta {
        page: SettingsPage::Shell,
        group: "Terminal",
        label: "Shell",
        icon: "terminal",
        title: "Shell",
        terms: &[
            "shell",
            "working directory",
            "environment",
            "env",
            "term",
            "colorterm",
            "scrollback",
            "glyph protocol",
        ],
    },
    PageMeta {
        page: SettingsPage::Keys,
        group: "Terminal",
        label: "Keys",
        icon: "keyboard",
        title: "Keys",
        terms: &[
            "keybindings",
            "shortcuts",
            "scope",
            "global",
            "native",
            "tmux",
            "zellij",
            "sidebar",
            "modifier remap",
            "option as alt",
            "record shortcut",
        ],
    },
    PageMeta {
        page: SettingsPage::Config,
        group: "Advanced",
        label: "Config",
        icon: "file-cog",
        title: "Config",
        terms: &[
            "config",
            "path",
            "directory",
            "themes",
            "status modules",
            "reload",
            "last write error",
        ],
    },
    PageMeta {
        page: SettingsPage::Diagnostics,
        group: "Advanced",
        label: "Diagnostics",
        icon: "bug",
        title: "Diagnostics",
        terms: &[
            "diagnostics",
            "stability trace",
            "trace",
            "reload",
            "errors",
        ],
    },
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
    font_families: Option<Vec<String>>,
    theme_names: Option<Vec<String>>,
    appearance_variant: bootty_config::config::AppearanceVariant,
    remote_editor: remotes::EditorState,
    /// Which keybind list is being edited (global, or one of the per-backend lists).
    keybind_scope: keybinds::KeybindScope,
    /// Editable rows for the loaded scope: the user layer that sits on top of the built-in defaults.
    keybind_rows: Option<Vec<keybinds::BindingRow>>,
    /// Whether the loaded scope drops the built-in defaults (the `clear` sentinel).
    keybind_clear: bool,
    /// The scope `keybind_rows`/`keybind_clear` were loaded for; reloaded when the scope changes.
    keybind_loaded_scope: Option<keybinds::KeybindScope>,
    /// In-progress chord capture, if any.
    keybind_capture: Option<keybinds::ChordCapture>,
    /// Whether the preset-prefix recorder is capturing (single combo, commits on first step).
    prefix_capture: bool,
    /// Editable modifier-remap rows (`from`, `to`); loaded lazily so incomplete rows persist.
    modifier_rows: Option<Vec<(String, String)>>,
    /// Binding-trigger chords captured this frame from the host's direct input path, fed in by the
    /// app so the recorder can capture cmd-modified combos egui drops (⌘V, ⌘⌥X, …).
    recorder_chords: Vec<String>,
    /// Which physical modifier keys are held right now, fed in by the app each frame. Wheel steps
    /// carry no key event, so this is the only source of left/right sides for a scroll recording.
    recorder_modifier_sides: ModifierSideState,
    /// An action the keybind editor should focus (and add a row for) on its next
    /// frame, set by "configure this command's keybinding" from the palette.
    pending_keybind_focus: Option<String>,
    /// The global style captured when settings opened, restored on close so the
    /// settings-only widget overrides don't leak into the main UI's popups.
    base_style: Option<egui::Style>,
    module_editor: modules::EditorState,
}

impl SettingsSurface {
    #[must_use]
    pub fn new(config: BoottyConfig) -> Self {
        let writeback = writeback::SettingsWriteback::new(config.config_path.clone());
        Self {
            config,
            writeback,
            page: SettingsPage::default(),
            palette: ThemePalette::default(),
            search: String::new(),
            font_families: None,
            theme_names: None,
            appearance_variant: bootty_config::config::AppearanceVariant::Dark,
            remote_editor: remotes::EditorState::default(),
            keybind_scope: keybinds::KeybindScope::Global,
            keybind_rows: None,
            keybind_clear: false,
            keybind_loaded_scope: None,
            keybind_capture: None,
            prefix_capture: false,
            modifier_rows: None,
            recorder_chords: Vec::new(),
            recorder_modifier_sides: ModifierSideState::default(),
            pending_keybind_focus: None,
            base_style: None,
            module_editor: modules::EditorState::default(),
        }
    }

    pub fn is_recording_keybind(&self) -> bool {
        self.keybind_capture.is_some() || self.prefix_capture
    }

    /// Jump to the keybindings page focused on `action` (in the Global list),
    /// adding an editable row for it if none exists yet. Used by the command
    /// palette's "configure this command's keybinding" chord.
    pub fn focus_keybinding(&mut self, action: &str) {
        self.page = SettingsPage::Keys;
        self.keybind_scope = keybinds::KeybindScope::Global;
        // Force a reload so the row set is fresh before we locate/add the row.
        self.keybind_loaded_scope = None;
        self.pending_keybind_focus = Some(action.to_owned());
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        theme: Theme,
        captured_chords: Vec<String>,
        modifier_sides: ModifierSideState,
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
            if self.keybind_capture.is_some() {
                self.keybind_capture = None;
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
                |ui| self.settings_content(ui),
            );
        });
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
                let mut close = false;
                // The UI font has no "←" glyph (it rendered as tofu), so draw the arrow from the
                // icon font and fall back to text-only if the slug is ever missing.
                let mut back = egui::text::LayoutJob::default();
                let back_color = readable_color(self.palette.mantle, self.palette.subtext);
                if let Some((glyph, family)) = icons::icon_glyph("arrow-left") {
                    back.append(
                        &glyph.to_string(),
                        0.0,
                        egui::text::TextFormat {
                            font_id: egui::FontId::new(14.0, egui::FontFamily::Name(family.into())),
                            color: back_color,
                            valign: egui::Align::Center,
                            ..Default::default()
                        },
                    );
                }
                back.append(
                    "  Back to terminal",
                    0.0,
                    egui::text::TextFormat {
                        font_id: egui::FontId::proportional(13.0),
                        color: back_color,
                        valign: egui::Align::Center,
                        ..Default::default()
                    },
                );
                if ui
                    .add(
                        egui::Button::new(back)
                            .fill(self.palette.mantle)
                            .stroke(egui::Stroke::NONE),
                    )
                    .clicked()
                {
                    close = true;
                }

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
                            let visible_pages: Vec<PageMeta> = PAGE_META
                                .iter()
                                .copied()
                                .filter(|meta| meta.group == group)
                                .filter(|meta| query.is_empty() || page_matches(*meta, &query))
                                .collect();
                            if visible_pages.is_empty() {
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
        let row_height = 34.0;
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), row_height),
            egui::Sense::click(),
        );
        let fill = if selected {
            self.palette.surface
        } else if response.hovered() {
            self.palette.hover
        } else {
            self.palette.mantle
        };
        let row_radius = if selected {
            egui::CornerRadius {
                nw: 0,
                ne: self.palette.radius,
                sw: 0,
                se: self.palette.radius,
            }
        } else {
            egui::CornerRadius::same(self.palette.radius)
        };
        ui.painter().rect_filled(rect, row_radius, fill);
        if selected {
            let accent = Rect::from_min_max(
                Pos2::new(rect.min.x, rect.min.y),
                Pos2::new(rect.min.x + 4.0, rect.max.y),
            );
            ui.painter().rect_filled(accent, 0.0, self.palette.accent);
        }
        let tint = readable_color(
            fill,
            if selected {
                self.palette.text
            } else {
                self.palette.subtext
            },
        );
        let icon_center = Pos2::new(rect.min.x + 17.0, rect.center().y);
        icons::paint_icon_slug(ui.painter(), meta.icon, icon_center, 15.0, tint);
        ui.painter().text(
            Pos2::new(rect.min.x + 40.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            meta.label,
            egui::FontId::proportional(13.0),
            tint,
        );
        if response.clicked() {
            self.page = meta.page;
            self.keybind_capture = None;
        }
    }

    fn settings_content(&mut self, ui: &mut egui::Ui) {
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
                            settings_page_header(ui, self.palette, meta.title);
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
                                        SettingsPage::Sidebar => self.sidebar_ui(ui),
                                        SettingsPage::Shell => session::ui(self, ui),
                                        SettingsPage::Status => {
                                            status_preview(ui, self.palette, &self.config);
                                            status_bar::ui(self, ui);
                                        }
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
        path_row(ui, self.palette, "Config file", self.writeback.path());
        if let Some(parent) = self.writeback.path().parent() {
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

    fn sidebar_ui(&mut self, ui: &mut egui::Ui) {
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
        settings_row(
            ui,
            self.palette,
            "Width",
            "Width of the session sidebar.",
            |ui| {
                let mut width = self.config.chrome.sidebar_width;
                if settings_slider_with_edit(
                    ui,
                    self.palette,
                    &mut width,
                    NumberEditSpec {
                        id_salt: &["chrome", "sidebar-width"],
                        range: 120.0..=600.0,
                        suffix: " px",
                        precision: 1,
                        display_scale: 1.0,
                    },
                ) {
                    self.config.chrome.sidebar_width = width;
                    self.writeback.set_f32(&["chrome", "sidebar-width"], width);
                }
            },
        );
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
            self.keybind_scope = keybinds::KeybindScope::Sidebar;
            self.keybind_loaded_scope = None;
            self.keybind_capture = None;
        }
        modules::sidebar_ui(self, ui);
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
                    if trace.trim().is_empty() {
                        self.config.diagnostics.stability_trace = None;
                        self.writeback.remove(&["diagnostics", "stability-trace"]);
                    } else {
                        self.config.diagnostics.stability_trace = Some(PathBuf::from(&trace));
                        self.writeback
                            .set_str(&["diagnostics", "stability-trace"], &trace);
                    }
                }
            },
        );
        section(ui, self.palette, "STATE");
        path_row(ui, self.palette, "Config file", self.writeback.path());
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

/// Re-resolve `win.config` from the config file so read paths (resolved shortcuts, effective
/// prefix, theme previews) reflect what was just written.
fn reload_settings_config(win: &mut SettingsSurface) {
    if let Some(config) = win.writeback.reload() {
        win.config = config;
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
        || meta.title.to_ascii_lowercase().contains(query)
        || meta
            .terms
            .iter()
            .any(|term| term.to_ascii_lowercase().contains(query))
}

#[cfg(windows)]
fn available_backend_options() -> &'static [(MultiplexerBackendConfig, &'static str)] {
    &[
        (MultiplexerBackendConfig::Native, "native"),
        (MultiplexerBackendConfig::Rmux, "rmux"),
        (MultiplexerBackendConfig::Zellij, "zellij"),
    ]
}

#[cfg(not(windows))]
fn available_backend_options() -> &'static [(MultiplexerBackendConfig, &'static str)] {
    &[
        (MultiplexerBackendConfig::Native, "native"),
        (MultiplexerBackendConfig::Rmux, "rmux"),
        (MultiplexerBackendConfig::Tmux, "tmux"),
        (MultiplexerBackendConfig::Zellij, "zellij"),
    ]
}

fn backend_token(backend: MultiplexerBackendConfig) -> &'static str {
    match backend {
        MultiplexerBackendConfig::Native => "native",
        MultiplexerBackendConfig::Rmux => "rmux",
        MultiplexerBackendConfig::Tmux => "tmux",
        MultiplexerBackendConfig::Zellij => "zellij",
    }
}

fn status_preview(ui: &mut egui::Ui, palette: ThemePalette, config: &BoottyConfig) {
    egui::Frame::NONE
        .fill(palette.mantle)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            let height = config.chrome.status_height.clamp(24.0, 40.0);
            let status_background = config
                .chrome
                .status_background
                .map_or(palette.mantle, color_to_egui);
            let mut visible = false;
            if config.chrome.top_bar {
                status_preview_bar(
                    ui,
                    palette,
                    height,
                    status_background,
                    &config.chrome.top_segments,
                );
                visible = true;
            }
            if config.chrome.bottom_bar {
                if visible {
                    ui.add_space(6.0);
                }
                status_preview_bar(
                    ui,
                    palette,
                    height,
                    status_background,
                    &config.chrome.bottom_segments,
                );
                visible = true;
            }
            if !visible {
                ui.label(RichText::new("Both module bars are hidden.").color(palette.muted));
            }
        });
    ui.add_space(10.0);
}

fn status_preview_bar(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    height: f32,
    status_background: Color32,
    segments: &[bootty_config::config::StatusSegment],
) {
    let (bar, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), height),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(
        bar,
        egui::CornerRadius::same(palette.radius),
        status_background,
    );

    for (align, x_anchor) in [
        (bootty_config::config::SegmentAlign::Left, bar.left() + 10.0),
        (bootty_config::config::SegmentAlign::Center, bar.center().x),
        (
            bootty_config::config::SegmentAlign::Right,
            bar.right() - 10.0,
        ),
    ] {
        let modules: Vec<_> = segments
            .iter()
            .filter(|segment| segment.align == align)
            .collect();
        let width = modules.len() as f32 * 92.0;
        let mut x = match align {
            bootty_config::config::SegmentAlign::Left => x_anchor,
            bootty_config::config::SegmentAlign::Center => x_anchor - width * 0.5,
            bootty_config::config::SegmentAlign::Right => x_anchor - width,
        };
        for segment in modules {
            let bg = segment.bg.map_or(palette.hover, color_to_egui);
            let fg = readable_color(bg, segment.fg.map_or(palette.text, color_to_egui));
            let chip =
                Rect::from_min_size(Pos2::new(x, bar.center().y - 12.0), Vec2::new(84.0, 24.0));
            ui.painter()
                .rect_filled(chip, egui::CornerRadius::same(5), bg);
            ui.painter().rect_stroke(
                chip,
                egui::CornerRadius::same(5),
                egui::Stroke::new(1.0, palette.border),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                segment
                    .icon
                    .as_ref()
                    .map_or(segment.module.as_str(), String::as_str),
                egui::FontId::monospace(12.0),
                fg,
            );
            x += 92.0;
        }
    }
}

fn color_to_egui(color: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r, color.g, color.b, color.a)
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
    settings_row(ui, win.palette, label, help, |ui| {
        let current = *field(&mut win.config.sidebar);
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if settings_color_picker(ui, win.palette, &mut rgb).changed() {
            *field(&mut win.config.sidebar) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            });
            win.writeback.set_color(path, rgb);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.sidebar) = None;
            win.writeback.remove(path);
        }
    });
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
    settings_row(ui, win.palette, label, help, |ui| {
        let current = *field(&mut win.config.chrome);
        let mut rgb = current.map_or([seed.r(), seed.g(), seed.b()], |color| {
            [color.r, color.g, color.b]
        });
        if settings_color_picker(ui, win.palette, &mut rgb).changed() {
            *field(&mut win.config.chrome) = Some(Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            });
            win.writeback.set_color(path, rgb);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.chrome) = None;
            win.writeback.remove(path);
        }
    });
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
        let current = *field(&mut win.config.chrome);
        let mut next = current.unwrap_or(Color {
            r: seed.r(),
            g: seed.g(),
            b: seed.b(),
            a: seed.a(),
        });
        let mut rgb = [next.r, next.g, next.b];
        let mut changed = false;
        if settings_color_picker(ui, win.palette, &mut rgb).changed() {
            next.r = rgb[0];
            next.g = rgb[1];
            next.b = rgb[2];
            changed = true;
        }
        ui.label(RichText::new("Opacity").color(win.palette.muted));
        let mut opacity = f32::from(next.a) / 255.0;
        if settings_number_edit(
            ui,
            win.palette,
            &mut opacity,
            NumberEditSpec {
                id_salt: path,
                range: 0.0..=1.0,
                suffix: "%",
                precision: 0,
                display_scale: 100.0,
            },
        ) {
            next.a = (opacity.clamp(0.0, 1.0) * 255.0).round() as u8;
            changed = true;
        }
        if changed {
            *field(&mut win.config.chrome) = Some(next);
            win.writeback.set_color_value(path, next);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.chrome) = None;
            win.writeback.remove(path);
        }
    });
}
