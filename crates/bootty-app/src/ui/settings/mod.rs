//! Full-app settings surface for editing the user config.
//!
//! Edits are live-applied by writing the changed key straight into `config.toml`; the main
//! window's `ConfigHotReload` watcher then re-reads the file and applies it.

mod appearance;
mod font;
mod keybinds;
mod modules;
mod session;
mod status_bar;
mod window;

use std::path::PathBuf;

use bootty_ui::{Theme, ThemePalette, contrast_ratio, readable_color};
use eframe::egui::{self, Color32, Pos2, Rect, RichText, UiBuilder, Vec2};

use crate::{
    color::Color,
    config::{
        BoottyConfig, ConfigDocument, ConfigResult, MultiplexerBackendConfig, SidebarPosition,
        load_or_create_config_document,
    },
};

const SEARCH_ID: &str = "bootty::settings::search";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPage {
    #[default]
    General,
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

const PAGE_META: [PageMeta; 10] = [
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
    config_path: PathBuf,
    page: SettingsPage,
    palette: ThemePalette,
    search: String,
    font_families: Option<Vec<String>>,
    theme_names: Option<Vec<String>>,
    appearance_variant: crate::config::AppearanceVariant,
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
    last_write_error: Option<String>,
    /// An action the keybind editor should focus (and add a row for) on its next
    /// frame, set by "configure this command's keybinding" from the palette.
    pending_keybind_focus: Option<String>,
    /// The global style captured when settings opened, restored on close so the
    /// settings-only widget overrides don't leak into the main UI's popups.
    base_style: Option<egui::Style>,
    module_editor: modules::EditorState,
}

pub(super) type SettingsWindow = SettingsSurface;

impl SettingsSurface {
    #[must_use]
    pub fn new(config: BoottyConfig) -> Self {
        let config_path = config.config_path.clone();
        Self {
            config,
            config_path,
            page: SettingsPage::default(),
            palette: ThemePalette::default(),
            search: String::new(),
            font_families: None,
            theme_names: None,
            appearance_variant: crate::config::AppearanceVariant::Dark,
            keybind_scope: keybinds::KeybindScope::Global,
            keybind_rows: None,
            keybind_clear: false,
            keybind_loaded_scope: None,
            keybind_capture: None,
            prefix_capture: false,
            modifier_rows: None,
            recorder_chords: Vec::new(),
            last_write_error: None,
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
    ) -> SettingsAction {
        self.recorder_chords = captured_chords;
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
                if let Some((glyph, family)) = crate::ui::icons::icon_glyph("arrow-left") {
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
        crate::ui::icons::paint_icon_slug(ui.painter(), meta.icon, icon_center, 15.0, tint);
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
                            if let Some(error) = self.last_write_error.clone() {
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
                    self.set_str(&["multiplexer", "backend"], backend_token(backend));
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
                    self.set_bool(&["chrome", "sidebar"], sidebar);
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
                    self.set_bool(&["chrome", "bottom-bar"], bottom_bar);
                }
                ui.label(RichText::new("Bottom bar").color(self.palette.subtext));
            },
        );
    }

    fn config_ui(&mut self, ui: &mut egui::Ui) {
        section(ui, self.palette, "LOCATIONS");
        path_row(ui, self.palette, "Config file", &self.config_path);
        if let Some(parent) = self.config_path.parent() {
            path_row(ui, self.palette, "Config directory", parent);
            path_row(ui, self.palette, "Themes directory", &parent.join("themes"));
            path_row(ui, self.palette, "Status modules", &parent.join("status"));
            path_row(ui, self.palette, "Sidebar modules", &parent.join("sidebar"));
        }
        section(ui, self.palette, "RELOAD");
        let status = self
            .last_write_error
            .as_ref()
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
                    self.set_str(&["sidebar", "position"], token);
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
                        path: &["chrome", "sidebar-width"],
                        range: 120.0..=600.0,
                        suffix: " px",
                        precision: 1,
                        display_scale: 1.0,
                    },
                ) {
                    self.config.chrome.sidebar_width = width;
                    self.set_f32(&["chrome", "sidebar-width"], width);
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
                        self.remove(&["diagnostics", "stability-trace"]);
                    } else {
                        self.config.diagnostics.stability_trace = Some(PathBuf::from(&trace));
                        self.set_str(&["diagnostics", "stability-trace"], &trace);
                    }
                }
            },
        );
        section(ui, self.palette, "STATE");
        path_row(ui, self.palette, "Config file", &self.config_path);
        if let Some(error) = self.last_write_error.clone() {
            settings_notice(ui, self.palette.destructive, &error);
        } else {
            settings_notice(ui, self.palette.muted, "No settings write errors recorded.");
        }
    }

    // --- config writeback -------------------------------------------------------------------

    fn write(&mut self, mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>) {
        let result = (|| {
            let mut document = load_or_create_config_document(&self.config_path)?;
            mutate(&mut document)?;
            document.write_to_disk()
        })();
        self.last_write_error = result.err().map(|error| error.to_string());
    }

    fn set_f32(&mut self, path: &[&str], value: f32) {
        self.write(|document| {
            document.set_item(path, bootty_config::toml_edit::value(f64::from(value)))
        });
    }

    fn set_bool(&mut self, path: &[&str], value: bool) {
        self.write(|document| document.set_item(path, bootty_config::toml_edit::value(value)));
    }

    fn set_str(&mut self, path: &[&str], value: &str) {
        self.write(|document| document.set_item(path, bootty_config::toml_edit::value(value)));
    }

    fn set_i64(&mut self, path: &[&str], value: i64) {
        self.write(|document| document.set_item(path, bootty_config::toml_edit::value(value)));
    }

    /// Write an array of `{ name, value }` inline tables (the `[session].env` shape).
    fn set_env(&mut self, path: &[&str], entries: &[(String, String)]) {
        let mut array = bootty_config::toml_edit::Array::new();
        for (name, value) in entries {
            let mut table = bootty_config::toml_edit::InlineTable::new();
            table.insert("name", bootty_config::toml_edit::Value::from(name.as_str()));
            table.insert(
                "value",
                bootty_config::toml_edit::Value::from(value.as_str()),
            );
            array.push(table);
        }
        self.write(move |document| document.set_item(path, bootty_config::toml_edit::value(array)));
    }

    fn set_color(&mut self, path: &[&str], rgb: [u8; 3]) {
        self.set_color_value(
            path,
            Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            },
        );
    }

    fn set_color_value(&mut self, path: &[&str], color: Color) {
        let hex = color_hex(color);
        self.write(move |document| document.set_item(path, bootty_config::toml_edit::value(hex)));
    }

    fn set_strings(&mut self, path: &[&str], values: &[String]) {
        let mut array = bootty_config::toml_edit::Array::new();
        for value in values {
            array.push(value.as_str());
        }
        self.write(move |document| document.set_item(path, bootty_config::toml_edit::value(array)));
    }

    fn set_sidebar_modules(&mut self) {
        let modules = self.config.sidebar.modules.clone();
        self.set_strings(&["sidebar", "modules"], &modules);
    }

    fn contains_config_value(&self, path: &[&str]) -> bool {
        let Ok(Some(document)) = crate::config::load_config_document(&self.config_path) else {
            return false;
        };
        let Some((leaf, parents)) = path.split_last() else {
            return false;
        };
        let mut table = document.document().as_table();
        for key in parents {
            let Some(next) = table
                .get(key)
                .and_then(bootty_config::toml_edit::Item::as_table)
            else {
                return false;
            };
            table = next;
        }
        table.contains_key(leaf)
    }
    fn remove(&mut self, path: &[&str]) {
        self.write(|document| document.remove_item(path));
    }

    fn set_top_bar(&mut self, enabled: bool) {
        self.write(move |document| {
            document.remove_item(&["chrome", "status-bar"])?;
            document.set_item(
                &["chrome", "top-bar"],
                bootty_config::toml_edit::value(enabled),
            )
        });
    }

    /// Write one bar's ordered module segments from the working copy.
    fn set_status_segments(&mut self, position: status_bar::StatusBarPosition) {
        use bootty_config::toml_edit;
        let mut array = toml_edit::Array::new();
        for segment in position.segments(&self.config.chrome) {
            let mut table = toml_edit::InlineTable::new();
            let align = match segment.align {
                crate::config::SegmentAlign::Left => "left",
                crate::config::SegmentAlign::Center => "center",
                crate::config::SegmentAlign::Right => "right",
            };
            table.insert("align", toml_edit::Value::from(align));
            table.insert("module", toml_edit::Value::from(segment.module.as_str()));
            if let Some(color) = segment.fg {
                table.insert("fg", toml_edit::Value::from(color_hex(color)));
            }
            if let Some(color) = segment.bg {
                table.insert("bg", toml_edit::Value::from(color_hex(color)));
            }
            if let Some(icon) = &segment.icon
                && !icon.is_empty()
            {
                table.insert("icon", toml_edit::Value::from(icon.as_str()));
            }
            array.push(table);
        }
        self.write(move |document| {
            if position == status_bar::StatusBarPosition::Top {
                document.remove_item(&["chrome", "status-segment"])?;
            }
            document.set_item(&["chrome", position.segment_key()], toml_edit::value(array))
        });
    }
}

/// Re-resolve `win.config` from the config file so read paths (resolved shortcuts, effective
/// prefix, theme previews) reflect what was just written.
fn reload_settings_config(win: &mut SettingsWindow) {
    match crate::config::load_config_from_path(&win.config_path) {
        Ok(config) => win.config = config,
        Err(error) => win.last_write_error = Some(error.to_string()),
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

fn color_hex(color: Color) -> String {
    if color.a == 0xff {
        format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
    } else {
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            color.r, color.g, color.b, color.a
        )
    }
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

/// A combo box whose dropdown has a search filter at the top. Returns the chosen option index.
fn searchable_combo(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id_salt: &str,
    selected_text: &str,
    width: f32,
    options: &[&str],
    current: Option<usize>,
) -> Option<usize> {
    let filter_id = ui.make_persistent_id((id_salt, "filter"));
    let mut chosen = None;
    egui::ComboBox::from_id_salt(id_salt)
        .selected_text(selected_text)
        .width(width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(260.0));
            let mut filter: String =
                ui.memory(|memory| memory.data.get_temp(filter_id).unwrap_or_default());
            let response = settings_text_edit(ui, palette, &mut filter, "Search");
            if !response.has_focus() {
                response.request_focus();
            }
            ui.memory_mut(|memory| memory.data.insert_temp(filter_id, filter.clone()));
            let needle = filter.to_ascii_lowercase();
            ui.separator();
            // Size the list from the full option count, not the filtered subset, so a query that
            // matches nothing can't collapse the popup and leave it stuck small afterward.
            let list_height = (options.len() as f32 * 24.0).clamp(0.0, 300.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_height(list_height);
                    for (index, option) in options.iter().enumerate() {
                        if !needle.is_empty() && !option.to_ascii_lowercase().contains(&needle) {
                            continue;
                        }
                        let is_current = current == Some(index);
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 24.0),
                            egui::Sense::click(),
                        );
                        let fill = if is_current {
                            palette.accent
                        } else if response.hovered() {
                            palette.hover
                        } else {
                            palette.pane
                        };
                        ui.painter().rect_filled(
                            rect,
                            egui::CornerRadius::same(palette.radius),
                            fill,
                        );
                        ui.painter().text(
                            rect.left_center() + Vec2::new(8.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            *option,
                            egui::TextStyle::Button.resolve(ui.style()),
                            readable_color(fill, palette.text),
                        );
                        if response.clicked() {
                            chosen = Some(index);
                            ui.memory_mut(|memory| memory.data.remove_temp::<String>(filter_id));
                            ui.close();
                        }
                    }
                });
        });
    chosen
}

/// Presentation knobs for [`described_combo`].
pub(super) struct ComboStyle {
    pub width: f32,
    /// Show a search field above the list (for long option sets).
    pub searchable: bool,
    /// Closed-combo text when `current` matches no option.
    pub placeholder: &'static str,
}

/// A combo whose options each render a bold label over a muted one-line description (the look in
/// the fullscreen/decoration pickers). Shared across settings: set [`ComboStyle::searchable`] for
/// long lists (e.g. the keybind action picker). Returns whether the selection changed.
pub(super) fn described_combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id: &str,
    current: &mut T,
    options: &[(T, &str, &str)],
    style: ComboStyle,
) -> bool {
    let ComboStyle {
        width,
        searchable,
        placeholder,
    } = style;
    let selected = options.iter().position(|(value, _, _)| *value == *current);
    let selected_text = selected.map_or(placeholder, |index| options[index].1);
    let filter_id = ui.make_persistent_id((id, "filter"));
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .width(width)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.set_min_width(width.max(300.0));
            let needle = if searchable {
                let mut filter: String =
                    ui.memory(|memory| memory.data.get_temp(filter_id).unwrap_or_default());
                let response = settings_text_edit(ui, palette, &mut filter, "Search");
                if !response.has_focus() {
                    response.request_focus();
                }
                ui.memory_mut(|memory| memory.data.insert_temp(filter_id, filter.clone()));
                ui.separator();
                filter.to_ascii_lowercase()
            } else {
                String::new()
            };
            // Size from the full option count so a query matching nothing can't collapse the popup.
            let list_height = (options.len() as f32 * 54.0).clamp(54.0, 320.0);
            egui::ScrollArea::vertical()
                .max_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_height(list_height);
                    for (value, label, description) in options {
                        if !needle.is_empty()
                            && !label.to_ascii_lowercase().contains(&needle)
                            && !description.to_ascii_lowercase().contains(&needle)
                        {
                            continue;
                        }
                        let is_current = *value == *current;
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 52.0),
                            egui::Sense::click(),
                        );
                        let fill = if is_current {
                            palette.surface
                        } else if response.hovered() {
                            palette.hover
                        } else {
                            palette.pane
                        };
                        ui.painter().rect_filled(rect, palette.radius, fill);
                        ui.painter().text(
                            rect.left_top() + Vec2::new(12.0, 8.0),
                            egui::Align2::LEFT_TOP,
                            *label,
                            egui::TextStyle::Button.resolve(ui.style()),
                            readable_color(fill, palette.text),
                        );
                        if !description.is_empty() {
                            ui.painter().text(
                                rect.left_top() + Vec2::new(12.0, 29.0),
                                egui::Align2::LEFT_TOP,
                                *description,
                                egui::TextStyle::Small.resolve(ui.style()),
                                readable_color(fill, palette.muted),
                            );
                        }
                        if response.clicked() {
                            *current = *value;
                            changed = true;
                            if searchable {
                                ui.memory_mut(|memory| {
                                    memory.data.remove_temp::<String>(filter_id);
                                });
                            }
                            ui.close();
                        }
                        ui.add_space(2.0);
                    }
                });
        });
    changed
}

/// A text button with a constant 1px border in every state, so only its fill changes on hover.
/// egui's default button reads its border in from `hovered`/`active` visuals, which makes the
/// frame appear to grow under the pointer; this keeps the footprint fixed.
fn settings_button(ui: &mut egui::Ui, palette: ThemePalette, label: &str) -> egui::Response {
    let font = egui::FontId::proportional(13.0);
    let text_color = readable_color(palette.surface, palette.text);
    let galley = settings_button_galley(ui, label, font, text_color);
    let padding = Vec2::new(14.0, 8.0);
    let size = Vec2::new(galley.size().x + padding.x * 2.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(palette.radius),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    let text_pos = Pos2::new(rect.center().x - galley.size().x * 0.5, rect.center().y);
    ui.painter().galley(
        Pos2::new(text_pos.x, text_pos.y - galley.size().y * 0.5),
        galley,
        readable_color(fill, text_color),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn settings_button_galley(
    ui: &egui::Ui,
    label: &str,
    font: egui::FontId,
    color: Color32,
) -> std::sync::Arc<egui::Galley> {
    let Some(label) = label.strip_prefix("+ ") else {
        return ui.painter().layout_no_wrap(label.to_owned(), font, color);
    };

    let mut job = egui::text::LayoutJob::default();
    if let Some((glyph, family)) = crate::ui::icons::icon_glyph("plus") {
        job.append(
            &glyph.to_string(),
            0.0,
            egui::text::TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Name(family.into())),
                color,
                ..Default::default()
            },
        );
    }
    job.append(
        label,
        4.0,
        egui::text::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    ui.painter().layout_job(job)
}

/// A drag-and-drop payload identifying which list and row is being dragged. Namespaced by the
/// list id so two reorderable lists on the same page never pick up each other's drags.
#[derive(Clone, Copy)]
struct ReorderPayload {
    list: egui::Id,
    index: usize,
}

/// The grip a reorderable row hands to its renderer; calling `ui` paints the drag handle and makes
/// it the row's only drag source.
struct DragHandle {
    list: egui::Id,
    index: usize,
}

impl DragHandle {
    /// Paint the grip centered in `rect` and make that rect the row's sole drag source. Drawing into
    /// a caller-supplied rect (an overlay) rather than allocating in the layout flow keeps the grip
    /// vertically centered on multi-line rows: egui grows a horizontal layout's cross-axis as items
    /// are added, so an in-flow handle placed first anchors to the first line.
    fn paint_in(&self, ui: &mut egui::Ui, palette: ThemePalette, rect: Rect) {
        let id = self.list.with(("handle", self.index));
        let response = ui.interact(rect, id, egui::Sense::click_and_drag());
        if response.drag_started() || response.dragged() {
            egui::DragAndDrop::set_payload(
                ui.ctx(),
                ReorderPayload {
                    list: self.list,
                    index: self.index,
                },
            );
        }
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        crate::ui::icons::paint_icon_slug(
            ui.painter(),
            "grip-vertical",
            rect.center(),
            16.0,
            palette.muted,
        );
    }
}

/// Render a vertical, drag-reorderable list. `render_row(ui, index, handle)` draws one row and must
/// call `handle.paint_in(rect)` over the gutter where the grip belongs. Returns `Some((from, slot))` on the frame an item
/// is dropped into a new position; pass it to [`apply_reorder`]. Uses a grip handle as the sole
/// drag source — no up/down arrows.
fn reorderable_list(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    id_salt: &str,
    len: usize,
    mut render_row: impl FnMut(&mut egui::Ui, usize, &DragHandle),
) -> Option<(usize, usize)> {
    let list = ui.make_persistent_id(("reorderable_list", id_salt));
    let mut rects = Vec::with_capacity(len);
    for index in 0..len {
        let handle = DragHandle { list, index };
        let inner = ui.scope(|ui| render_row(ui, index, &handle));
        rects.push(inner.response.rect);
    }

    if len == 0 {
        return None;
    }

    // The payload survives until end-of-frame even on release, so reading it here covers both the
    // live drag (draw the insertion line) and the drop frame (commit the move).
    let from = egui::DragAndDrop::payload::<ReorderPayload>(ui.ctx())
        .filter(|payload| payload.list == list)
        .map(|payload| payload.index)?;
    let pointer = ui.input(|input| input.pointer.interact_pos())?;

    let mut slot = len;
    for (index, rect) in rects.iter().enumerate() {
        if pointer.y < rect.center().y {
            slot = index;
            break;
        }
    }

    let left = rects.iter().map(|r| r.left()).fold(f32::INFINITY, f32::min);
    let right = rects
        .iter()
        .map(|r| r.right())
        .fold(f32::NEG_INFINITY, f32::max);
    let line_y = if slot < len {
        rects[slot].top() - 3.0
    } else {
        rects[len - 1].bottom() + 3.0
    };
    ui.painter().line_segment(
        [Pos2::new(left, line_y), Pos2::new(right, line_y)],
        egui::Stroke::new(2.0, palette.accent),
    );

    if ui.input(|input| input.pointer.any_released()) {
        egui::DragAndDrop::clear_payload(ui.ctx());
        // Dropping onto your own edges is a no-op (slot == from keeps position, slot == from+1 lands
        // back in place after removal).
        if slot != from && slot != from + 1 {
            return Some((from, slot));
        }
    }
    None
}

/// Apply a [`reorderable_list`] result: lift item `from` and reinsert it at the `slot` boundary.
fn apply_reorder<T>(items: &mut Vec<T>, from: usize, slot: usize) {
    if from >= items.len() {
        return;
    }
    let item = items.remove(from);
    let to = if slot > from { slot - 1 } else { slot };
    items.insert(to.min(items.len()), item);
}

fn settings_icon_button(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    slug: &str,
    tooltip: &str,
) -> egui::Response {
    let size = Vec2::splat(30.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let fill = if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(palette.radius),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    crate::ui::icons::paint_icon_slug(
        ui.painter(),
        slug,
        rect.center(),
        15.0,
        readable_color(fill, palette.text),
    );
    response.on_hover_text(tooltip)
}

fn settings_page_header(ui: &mut egui::Ui, palette: ThemePalette, title: &str) {
    ui.label(
        RichText::new("Bootty Settings")
            .color(readable_color(palette.base, palette.muted))
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.label(
        RichText::new(title)
            .color(readable_color(palette.base, palette.text))
            .strong()
            .size(24.0),
    );
    ui.add_space(18.0);
}

/// Memory flag: the next `settings_row` is the first in its section, so it draws no separator above
/// it. This makes row dividers act as separators *between* rows — the last row keeps no trailing
/// border, which is what bleeds into a following framed block.
fn section_first_row_id() -> egui::Id {
    egui::Id::new("bootty::settings::section_first_row")
}

/// Section heading inside a page.
fn section(ui: &mut egui::Ui, palette: ThemePalette, title: &str) {
    ui.add_space(12.0);
    ui.label(
        RichText::new(title)
            .color(readable_color(palette.base, palette.subtext))
            .strong()
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.memory_mut(|memory| memory.data.insert_temp(section_first_row_id(), true));
}

fn settings_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    label: &str,
    help: &str,
    add_control: impl FnOnce(&mut egui::Ui),
) {
    let first_in_section = ui.memory(|memory| {
        memory
            .data
            .get_temp::<bool>(section_first_row_id())
            .unwrap_or(false)
    });
    ui.memory_mut(|memory| memory.data.insert_temp(section_first_row_id(), false));

    let top = ui.cursor().top();
    ui.add_space(7.0);
    ui.horizontal(|ui| {
        ui.set_min_width(ui.available_width());
        let full_width = ui.available_width();
        let label_width = full_width.min(300.0);
        ui.allocate_ui_with_layout(
            Vec2::new(label_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(label)
                            .color(readable_color(palette.base, palette.text))
                            .strong(),
                    )
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(help)
                            .color(readable_color(palette.base, palette.muted))
                            .size(11.0),
                    )
                    .wrap(),
                );
            },
        );
        ui.add_space(16.0);
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width(), 34.0),
            egui::Layout::right_to_left(egui::Align::Center),
            add_control,
        );
    });
    ui.add_space(7.0);
    let bottom = ui.cursor().top();
    // Separator above the row (skipped for the first in a section), so no border trails the last row.
    if !first_in_section {
        let rect = Rect::from_min_max(
            Pos2::new(ui.min_rect().left(), top),
            Pos2::new(ui.min_rect().right(), top + 1.0),
        );
        ui.painter().rect_filled(rect, 0.0, palette.border);
    }
    ui.set_min_height((bottom - top).max(54.0));
}

fn settings_toggle_row(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    label: &str,
    help: &str,
    mut value: bool,
    on_change: impl FnOnce(bool),
) {
    let mut changed = false;
    settings_row(ui, palette, label, help, |ui| {
        changed = settings_toggle(ui, palette, &mut value);
    });
    if changed {
        on_change(value);
    }
}

fn settings_toggle(ui: &mut egui::Ui, palette: ThemePalette, value: &mut bool) -> bool {
    let size = Vec2::new(46.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let changed = response.clicked();
    if changed {
        *value = !*value;
    }
    let fill = if *value {
        palette.accent
    } else if response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(13), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(13),
        egui::Stroke::new(
            1.0,
            if *value {
                palette.accent
            } else {
                palette.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let knob_x = if *value {
        rect.right() - 13.0
    } else {
        rect.left() + 13.0
    };
    ui.painter().circle_filled(
        Pos2::new(knob_x, rect.center().y),
        9.0,
        readable_color(
            fill,
            if *value {
                palette.base
            } else {
                palette.subtext
            },
        ),
    );
    changed
}

fn settings_notice(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.label(
        RichText::new(text)
            .color(readable_color(ui.visuals().panel_fill, color))
            .size(12.0),
    );
}

fn settings_text_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut String,
    hint: &str,
) -> egui::Response {
    let inner_width = (ui.available_width().min(360.0) - 22.0).max(80.0);
    settings_text_edit_width(ui, palette, value, hint, inner_width)
}

fn settings_text_edit_width(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    let fill = palette.surface;
    egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.add_sized(
                [width, 22.0],
                egui::TextEdit::singleline(value)
                    .hint_text(hint)
                    .text_color(readable_color(fill, palette.text))
                    .vertical_align(egui::Align::Center)
                    .background_color(fill)
                    .frame(egui::Frame::NONE),
            )
        })
        .inner
}

fn settings_segmented(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    settings_segmented_unit(ui, palette, labels, selected, 82.0)
}

fn settings_segmented_ltr(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
) -> Option<usize> {
    settings_segmented_unit(ui, palette, labels, selected, 68.0)
}

fn settings_segmented_unit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    labels: &[&str],
    selected: usize,
    min_item_width: f32,
) -> Option<usize> {
    if labels.is_empty() {
        return None;
    }
    let mut changed = None;
    let natural_item_width = labels
        .iter()
        .map(|label| (label.len() as f32 * 8.5 + 24.0).max(min_item_width))
        .fold(min_item_width, f32::max);
    // Never exceed the column we were handed: a control wider than the available width spills
    // leftward (the control column lays out right-to-left) and paints over the row's label/help.
    let max_item_width = (ui.available_width() / labels.len() as f32).max(1.0);
    let item_width = natural_item_width.min(max_item_width);
    let size = Vec2::new(item_width * labels.len() as f32, 34.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let radius = egui::CornerRadius::same(palette.radius);
    ui.painter().rect_filled(rect, radius, palette.surface);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    for (index, label) in labels.iter().enumerate() {
        let min = Pos2::new(rect.left() + item_width * index as f32, rect.top());
        let item = Rect::from_min_size(min, Vec2::new(item_width, rect.height()));
        if index > 0 {
            ui.painter().line_segment(
                [item.left_top(), item.left_bottom()],
                egui::Stroke::new(1.0, palette.border),
            );
        }
        let pointer_hovered = response.hover_pos().is_some_and(|pos| item.contains(pos));
        if pointer_hovered && index != selected {
            ui.painter()
                .rect_filled(item.shrink(3.0), egui::CornerRadius::same(5), palette.hover);
        }
        if index == selected {
            let selected_rect = item.shrink(3.0);
            ui.painter()
                .rect_filled(selected_rect, egui::CornerRadius::same(5), palette.accent);
        }
        let fill = if index == selected {
            palette.accent
        } else if pointer_hovered {
            palette.hover
        } else {
            palette.surface
        };
        let color = if index == selected {
            readable_color(fill, palette.text)
        } else {
            readable_color(fill, palette.subtext)
        };
        // Shrink the label to match a clamped item so long labels (e.g. "Drawn by system") stay
        // inside their cell instead of bleeding into the neighbour.
        let font_size = (12.5 * (item_width / natural_item_width)).clamp(9.5, 12.5);
        ui.painter().text(
            item.center(),
            egui::Align2::CENTER_CENTER,
            *label,
            egui::FontId::proportional(font_size),
            color,
        );
    }
    if response.clicked()
        && let Some(pos) = response.interact_pointer_pos()
    {
        let index = ((pos.x - rect.left()) / item_width).floor() as usize;
        if index < labels.len() && index != selected {
            changed = Some(index);
        }
    }
    changed
}

fn settings_color_picker(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    rgb: &mut [u8; 3],
) -> egui::Response {
    let mut style = (*ui.ctx().global_style()).clone();
    style.spacing.interact_size = Vec2::splat(30.0);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
        &mut style.visuals.widgets.open,
    ] {
        widget.bg_fill = palette.border;
        widget.weak_bg_fill = palette.border;
        widget.expansion = 0.0;
    }
    ui.scope(|ui| {
        ui.set_style(style);
        let response = egui::color_picker::color_edit_button_srgb(ui, rgb);
        let swatch = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let border = swatch_border_color(palette, swatch);
        let hover = response.hovered();
        let stroke = if hover {
            egui::Stroke::new(2.0, readable_color(swatch, palette.accent))
        } else {
            egui::Stroke::new(1.5, border)
        };
        ui.painter().rect_stroke(
            response.rect,
            egui::CornerRadius::same(4),
            stroke,
            egui::StrokeKind::Inside,
        );
        if hover {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    })
    .inner
}

fn swatch_border_color(palette: ThemePalette, swatch: Color32) -> Color32 {
    if contrast_ratio(swatch, palette.border) >= 3.0 {
        return palette.border;
    }
    [palette.text, palette.muted, Color32::BLACK, Color32::WHITE]
        .into_iter()
        .max_by(|a, b| {
            contrast_ratio(swatch, *a)
                .partial_cmp(&contrast_ratio(swatch, *b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(Color32::BLACK)
}

pub(super) struct NumberEditSpec<'a> {
    pub path: &'a [&'a str],
    pub range: std::ops::RangeInclusive<f32>,
    pub suffix: &'a str,
    pub precision: usize,
    pub display_scale: f32,
}

pub(super) fn settings_slider_with_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: NumberEditSpec<'_>,
) -> bool {
    let edit_id = number_edit_id(ui, spec.path);
    let group_width = 190.0 + 8.0 + number_edit_outer_width(&spec);
    ui.allocate_ui_with_layout(
        Vec2::new(group_width, 34.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let mut changed = settings_number_edit_with_id(ui, palette, value, &spec, edit_id);
            ui.add_space(8.0);
            if settings_slider(ui, palette, value, spec.range.clone()) {
                ui.memory_mut(|memory| {
                    memory
                        .data
                        .insert_temp(edit_id, format_number_value(*value, &spec));
                });
                changed = true;
            }
            changed
        },
    )
    .inner
}

pub(super) fn settings_number_edit(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: NumberEditSpec<'_>,
) -> bool {
    let edit_id = number_edit_id(ui, spec.path);
    settings_number_edit_with_id(ui, palette, value, &spec, edit_id)
}

fn number_edit_id(ui: &mut egui::Ui, path: &[&str]) -> egui::Id {
    ui.make_persistent_id(("settings-number-edit", path.join(".")))
}

fn settings_number_edit_with_id(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    spec: &NumberEditSpec<'_>,
    edit_id: egui::Id,
) -> bool {
    let focused = ui.memory(|memory| memory.has_focus(edit_id));
    let mut text = ui
        .memory(|memory| memory.data.get_temp::<String>(edit_id))
        .unwrap_or_else(|| format_number_value(*value, spec));
    if !focused {
        text = format_number_value(*value, spec);
    }

    let fill = palette.surface;
    let response = egui::Frame::NONE
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.add_sized(
                [number_edit_inner_width(spec), 22.0],
                egui::TextEdit::singleline(&mut text)
                    .id(edit_id)
                    .text_color(readable_color(fill, palette.text))
                    .horizontal_align(egui::Align::RIGHT)
                    .vertical_align(egui::Align::Center)
                    .background_color(fill)
                    .frame(egui::Frame::NONE),
            )
        })
        .inner;

    ui.memory_mut(|memory| memory.data.insert_temp(edit_id, text.clone()));
    if response.changed()
        && let Some(parsed) = parse_number_value(&text, spec)
    {
        *value = parsed;
        return true;
    }
    if response.lost_focus() {
        ui.memory_mut(|memory| {
            memory
                .data
                .insert_temp(edit_id, format_number_value(*value, spec));
        });
    }
    false
}

fn number_edit_outer_width(spec: &NumberEditSpec<'_>) -> f32 {
    number_edit_inner_width(spec) + 16.0
}

fn number_edit_inner_width(spec: &NumberEditSpec<'_>) -> f32 {
    let start = format_number_value(*spec.range.start(), spec);
    let end = format_number_value(*spec.range.end(), spec);
    let widest = start.len().max(end.len()).max(6) as f32;
    (widest * 8.0 + 8.0).clamp(74.0, 112.0)
}

fn parse_number_value(text: &str, spec: &NumberEditSpec<'_>) -> Option<f32> {
    let trimmed = text.trim();
    let without_suffix = if spec.suffix.trim().is_empty() {
        trimmed
    } else {
        trimmed
            .strip_suffix(spec.suffix.trim())
            .unwrap_or(trimmed)
            .trim()
    };
    let number = without_suffix.parse::<f32>().ok()? / spec.display_scale;
    Some(number.clamp(*spec.range.start(), *spec.range.end()))
}

fn format_number_value(value: f32, spec: &NumberEditSpec<'_>) -> String {
    let displayed = value * spec.display_scale;
    format!("{:.*}{}", spec.precision, displayed, spec.suffix)
}

fn settings_slider(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> bool {
    let size = Vec2::new(190.0, 26.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
    let start = *range.start();
    let end = *range.end();
    let mut normalized = ((*value - start) / (end - start)).clamp(0.0, 1.0);
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        normalized = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        *value = start + (end - start) * normalized;
    }
    let rail = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 9.0));
    ui.painter()
        .rect_filled(rail, egui::CornerRadius::same(5), palette.surface);
    ui.painter().rect_stroke(
        rail,
        egui::CornerRadius::same(5),
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    let active = Rect::from_min_max(
        rail.min,
        Pos2::new(rail.left() + rail.width() * normalized, rail.bottom()),
    );
    ui.painter()
        .rect_filled(active, egui::CornerRadius::same(4), palette.accent);
    let thumb = Pos2::new(rail.left() + rail.width() * normalized, rail.center().y);
    ui.painter().circle_filled(
        thumb,
        10.0,
        readable_color(palette.surface, palette.subtext),
    );
    ui.painter()
        .circle_stroke(thumb, 10.0, egui::Stroke::new(2.0, palette.accent));
    response.changed() || response.dragged() || response.clicked()
}

fn path_row(ui: &mut egui::Ui, palette: ThemePalette, label: &str, path: &std::path::Path) {
    settings_row(ui, palette, label, "Read-only location.", |ui| {
        settings_value_chip(ui, palette, &path.display().to_string());
    });
}

fn settings_value_chip(ui: &mut egui::Ui, palette: ThemePalette, text: &str) {
    egui::Frame::NONE
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(palette.text).monospace());
        });
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
    segments: &[crate::config::StatusSegment],
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
        (crate::config::SegmentAlign::Left, bar.left() + 10.0),
        (crate::config::SegmentAlign::Center, bar.center().x),
        (crate::config::SegmentAlign::Right, bar.right() - 10.0),
    ] {
        let modules: Vec<_> = segments
            .iter()
            .filter(|segment| segment.align == align)
            .collect();
        let width = modules.len() as f32 * 92.0;
        let mut x = match align {
            crate::config::SegmentAlign::Left => x_anchor,
            crate::config::SegmentAlign::Center => x_anchor - width * 0.5,
            crate::config::SegmentAlign::Right => x_anchor - width,
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
    field: fn(&mut crate::config::SidebarConfig) -> &mut Option<Color>,
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
            win.set_color(path, rgb);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.sidebar) = None;
            win.remove(path);
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
    field: fn(&mut crate::config::ChromeConfig) -> &mut Option<Color>,
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
            win.set_color(path, rgb);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.chrome) = None;
            win.remove(path);
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
    field: fn(&mut crate::config::ChromeConfig) -> &mut Option<Color>,
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
                path,
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
            win.set_color_value(path, next);
        }
        if current.is_some() && settings_button(ui, win.palette, "Reset").clicked() {
            *field(&mut win.config.chrome) = None;
            win.remove(path);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_page_metadata_uses_expected_group_order() {
        let pages: Vec<SettingsPage> = PAGE_META.iter().map(|meta| meta.page).collect();
        assert_eq!(
            pages,
            [
                SettingsPage::General,
                SettingsPage::Text,
                SettingsPage::Appearance,
                SettingsPage::Window,
                SettingsPage::Sidebar,
                SettingsPage::Status,
                SettingsPage::Shell,
                SettingsPage::Keys,
                SettingsPage::Config,
                SettingsPage::Diagnostics,
            ]
        );
        assert_eq!(PAGE_META[0].group, "Core");
        assert_eq!(PAGE_META[6].group, "Terminal");
        assert_eq!(PAGE_META[8].group, "Advanced");
    }

    #[test]
    fn settings_search_matches_page_labels_and_row_terms() {
        assert!(page_matches(page_meta(SettingsPage::Text), "font"));
        assert!(page_matches(page_meta(SettingsPage::Status), "modules"));
        assert!(page_matches(
            page_meta(SettingsPage::Keys),
            "record shortcut"
        ));
        assert!(page_matches(
            page_meta(SettingsPage::Config),
            "status modules"
        ));
        assert!(!page_matches(
            page_meta(SettingsPage::Window),
            "record shortcut"
        ));
    }

    #[test]
    fn number_edit_parser_handles_scaled_suffix_values() {
        let percent = NumberEditSpec {
            path: &["chrome", "unfocused-terminal-dim"],
            range: 0.0..=1.0,
            suffix: "%",
            precision: 1,
            display_scale: 100.0,
        };
        assert_eq!(parse_number_value("12.5%", &percent), Some(0.125));
        assert_eq!(parse_number_value("250%", &percent), Some(1.0));

        let pixels = NumberEditSpec {
            path: &["chrome", "pane-divider-width"],
            range: 0.0..=16.0,
            suffix: " px",
            precision: 1,
            display_scale: 1.0,
        };
        assert_eq!(parse_number_value("3.5 px", &pixels), Some(3.5));
        assert_eq!(format_number_value(3.5, &pixels), "3.5 px");
    }
}
