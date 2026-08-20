use std::path::{Path, PathBuf};

use bootty_extension::{
    ModuleIdentity, SurfaceDeclaration, SurfacePlacement, SurfaceSnapshot, editable_module_source,
    error_item, import_legacy_extension_module, legacy_extension_modules, module_identities,
    preview_module_surfaces, reset_module_source, save_module_source,
};
use bootty_ui::icons;
use eframe::egui::{self, RichText};

use crate::theme::module_color32;

use super::SettingsWindow;

#[derive(Default)]
pub(super) struct EditorState {
    selected: Option<ModuleIdentity>,
    creating: bool,
    new_identity: String,
    create_error: Option<String>,
    loaded: Option<ModuleIdentity>,
    source: String,
    path: PathBuf,
    customized: bool,
    has_builtin: bool,
    error: Option<String>,
    preview_source: String,
    preview_theme: Vec<(String, String)>,
    preview: Vec<SurfaceSnapshot>,
}

pub(super) fn sidebar_ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let Some(root) = extension_root(win) else {
        return;
    };
    let identities = module_identities(&root).unwrap_or_default();
    let legacy = root
        .parent()
        .and_then(|config_dir| legacy_extension_modules(config_dir).ok())
        .unwrap_or_default();
    let mut selected = win
        .module_editor
        .selected
        .clone()
        .filter(|identity| identities.contains(identity))
        .or_else(|| identities.first().cloned());

    settings_pane(
        win,
        ui,
        |win, ui| {
            super::section(ui, palette, "EXTENSIONS");
            for identity in &identities {
                let active = selected.as_ref() == Some(identity);
                let response =
                    module_selector_row(ui, palette, identity.as_str(), active, None, |_| {});
                if response.clicked() {
                    selected = Some(identity.clone());
                }
            }
            if let Some(identity) = new_module_ui(win, ui) {
                selected = Some(identity);
            }
            if !legacy.is_empty() {
                super::section(ui, palette, "LEGACY MODULES");
                super::settings_notice(
                    ui,
                    palette.warning,
                    "These modules are inactive. Import one to validate and activate it.",
                );
                for module in &legacy {
                    let label = format!(
                        "{} · {}",
                        module.placement.as_str(),
                        module.source_path.display()
                    );
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(label).color(palette.subtext).size(10.0));
                        if super::settings_button(ui, palette, "Import").clicked() {
                            let variant =
                                win.config.appearance.mode.variant(win.appearance_variant);
                            let theme = crate::theme::theme_tokens(&win.config, variant);
                            let config_dir =
                                root.parent().expect("extension root has config parent");
                            match import_legacy_extension_module(config_dir, module, theme) {
                                Ok(identity) => {
                                    win.module_editor.loaded = None;
                                    win.module_editor.error = None;
                                    selected = Some(identity);
                                }
                                Err(error) => {
                                    win.module_editor.error =
                                        Some(format!("Import failed: {error}"));
                                }
                            }
                        }
                    });
                }
            }
            selected
        },
        |win, ui, selected| match selected {
            Some(identity) => source_editor(win, ui, &identity),
            None => super::settings_notice(ui, palette.muted, "No extension modules found."),
        },
    );
}

pub(super) fn settings_pane<T>(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    selector: impl FnOnce(&mut SettingsWindow, &mut egui::Ui) -> T,
    content: impl FnOnce(&mut SettingsWindow, &mut egui::Ui, T),
) {
    let selector_width = (ui.available_width() * 0.25).clamp(210.0, 280.0);
    ui.horizontal_top(|ui| {
        let selected = ui
            .vertical(|ui| {
                ui.set_width(selector_width);
                selector(win, ui)
            })
            .inner;
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.set_min_width(ui.available_width());
            content(win, ui, selected);
        });
    });
}

pub(super) fn source_editor_for_surface(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    surface: &str,
) {
    let Ok(identity) = ModuleIdentity::parse(format!("{surface}.luau")) else {
        super::settings_notice(
            ui,
            win.palette.destructive,
            "Invalid extension surface name.",
        );
        return;
    };
    source_editor(win, ui, &identity);
}

fn source_editor(win: &mut SettingsWindow, ui: &mut egui::Ui, identity: &ModuleIdentity) {
    let palette = win.palette;
    let Some(root) = extension_root(win) else {
        return;
    };
    if win.module_editor.loaded.as_ref() != Some(identity) {
        load_editor(&mut win.module_editor, &root, identity);
    }
    win.module_editor.selected = Some(identity.clone());
    let variant = win.config.appearance.mode.variant(win.appearance_variant);
    let theme = crate::theme::theme_tokens(&win.config, variant);
    let state = &mut win.module_editor;
    if state.preview_source != state.source || state.preview_theme != theme {
        state.preview = preview_module_surfaces(identity, &state.source, theme.clone())
            .unwrap_or_else(|error| vec![error_surface(error)]);
        state.preview_source.clone_from(&state.source);
        state.preview_theme = theme;
    }

    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            module_toolbar(ui, palette, state, &root, identity);
            if let Some(error) = &state.error {
                ui.label(RichText::new(error).color(palette.destructive).size(11.0));
            }
            module_preview(ui, palette, &state.preview);
            code_editor(ui, palette, state, &root, identity);
        });
}

pub(super) fn new_module_ui(win: &mut SettingsWindow, ui: &mut egui::Ui) -> Option<ModuleIdentity> {
    let palette = win.palette;
    if !win.module_editor.creating {
        if super::settings_button(ui, palette, "+ New module").clicked() {
            win.module_editor.creating = true;
            win.module_editor.new_identity.clear();
            win.module_editor.create_error = None;
        }
        return None;
    }
    let mut created = None;
    ui.horizontal(|ui| {
        super::settings_text_edit_width(
            ui,
            palette,
            &mut win.module_editor.new_identity,
            "nested/module.luau",
            (ui.available_width() - 150.0).max(180.0),
        );
        if super::settings_button(ui, palette, "Create").clicked() {
            let value = win.module_editor.new_identity.trim().to_owned();
            match ModuleIdentity::parse(value) {
                Ok(identity) => {
                    let Some(root) = extension_root(win) else {
                        return;
                    };
                    if root.join(identity.as_ref()).exists() {
                        win.module_editor.create_error =
                            Some(format!("Module `{identity}` already exists."));
                    } else {
                        match save_module_source(&root, &identity, &module_template(&identity)) {
                            Ok(_) => {
                                win.module_editor.creating = false;
                                win.module_editor.loaded = None;
                                win.module_editor.create_error = None;
                                created = Some(identity);
                            }
                            Err(error) => {
                                win.module_editor.create_error =
                                    Some(format!("Create failed: {error}"));
                            }
                        }
                    }
                }
                Err(error) => win.module_editor.create_error = Some(error),
            }
        }
        if super::settings_button(ui, palette, "Cancel").clicked() {
            win.module_editor.creating = false;
            win.module_editor.create_error = None;
        }
    });
    if let Some(error) = &win.module_editor.create_error {
        ui.label(RichText::new(error).color(palette.destructive).size(11.0));
    }
    created
}

pub(super) fn module_selector_row(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    label: &str,
    selected: bool,
    handle: Option<&super::DragHandle>,
    trailing: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 36.0),
        egui::Sense::click(),
    );
    let fill = if selected {
        palette.surface
    } else if response.hovered() {
        palette.hover
    } else {
        palette.pane
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(palette.radius), fill);
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(palette.radius),
        egui::Stroke::new(
            1.0,
            if selected {
                palette.primary
            } else {
                palette.border
            },
        ),
        egui::StrokeKind::Inside,
    );
    let content_rect = rect.shrink2(egui::Vec2::new(10.0, 5.0));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.add_space(22.0);
    content.label(RichText::new(label).color(palette.text).strong());
    content.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    let gutter = egui::Rect::from_min_max(
        content_rect.left_top(),
        egui::Pos2::new(content_rect.left() + 22.0, content_rect.bottom()),
    );
    if let Some(handle) = handle {
        handle.paint_in(ui, palette, gutter);
    } else {
        icons::paint_icon_slug(
            ui.painter(),
            "file-code",
            gutter.center(),
            14.0,
            palette.muted,
        );
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.add_space(4.0);
    response
}

fn module_preview(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    surfaces: &[SurfaceSnapshot],
) {
    ui.label(
        RichText::new("PREVIEW · EXAMPLE DATA")
            .color(palette.muted)
            .strong()
            .size(11.0),
    );
    for surface in surfaces {
        ui.label(
            RichText::new(format!(
                "{} · {:?}",
                surface.declaration.id, surface.declaration.placement
            ))
            .color(palette.subtext)
            .size(10.0),
        );
        for item in &surface.items {
            ui.horizontal_wrapped(|ui| {
                if let Some(icon) = &item.icon {
                    ui.label(
                        RichText::new(icon)
                            .color(item.fg.map(module_color32).unwrap_or(palette.text)),
                    );
                }
                ui.label(
                    RichText::new(&item.text)
                        .color(item.fg.map(module_color32).unwrap_or(palette.text)),
                );
            });
        }
    }
    ui.add_space(10.0);
}

fn module_toolbar(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
    root: &Path,
    identity: &ModuleIdentity,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(identity.as_str())
                .color(palette.text)
                .strong()
                .size(15.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if super::settings_icon_button(ui, palette, "copy", "Copy module path").clicked() {
                ui.ctx()
                    .copy_text(state.path.to_string_lossy().into_owned());
            }
            if state.customized
                && state.has_builtin
                && super::settings_button(ui, palette, "Reset to default").clicked()
            {
                match reset_module_source(root, identity) {
                    Ok(()) => load_editor(state, root, identity),
                    Err(error) => state.error = Some(error.to_string()),
                }
            }
        });
    });
}

fn code_editor(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
    root: &Path,
    identity: &ModuleIdentity,
) {
    let response = egui::Frame::NONE
        .fill(palette.base)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut state.source)
                    .code_editor()
                    .desired_rows(30)
                    .desired_width(f32::INFINITY),
            )
        })
        .inner;
    if response.changed() {
        match save_module_source(root, identity, &state.source) {
            Ok(path) => {
                state.path = path;
                state.customized = true;
                state.error = None;
            }
            Err(error) => state.error = Some(format!("Save failed: {error}")),
        }
    }
}

fn load_editor(state: &mut EditorState, root: &Path, identity: &ModuleIdentity) {
    match editable_module_source(root, identity) {
        Some(module) => {
            state.loaded = Some(identity.clone());
            state.source = module.source;
            state.path = module.path;
            state.customized = module.customized;
            state.has_builtin = module.has_builtin;
            state.error = None;
        }
        None => {
            state.loaded = Some(identity.clone());
            state.source = module_template(identity);
            state.path = root.join(identity.as_ref());
            state.customized = false;
            state.has_builtin = false;
            state.error = Some("Module file does not exist; editing creates it.".to_owned());
        }
    }
    state.preview_source.clear();
}

fn extension_root(win: &SettingsWindow) -> Option<PathBuf> {
    win.writeback
        .path()
        .parent()
        .map(|path| path.join("extensions"))
}

fn module_template(identity: &ModuleIdentity) -> String {
    let id = identity
        .as_ref()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("extension");
    format!(
        "--!strict\nbootty.ui.register({{ id = \"{id}\", placement = \"sidebar\" }}, function()\n\treturn {{ {{ text = \"{id}\" }} }}\nend)\n"
    )
}

fn error_surface(error: String) -> SurfaceSnapshot {
    SurfaceSnapshot {
        declaration: SurfaceDeclaration {
            id: "preview-error".to_owned(),
            placement: SurfacePlacement::Floating,
            order: 0,
            interval: std::time::Duration::from_secs(1),
        },
        items: vec![error_item(&error)],
    }
}
