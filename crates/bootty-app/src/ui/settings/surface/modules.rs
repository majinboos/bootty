use std::path::PathBuf;

use bootty_extension::{
    LegacyExtensionModule, ModuleIdentity, ModuleSourceOutcome, ModuleSourceRequest, ModuleSources,
    SurfaceDeclaration, SurfacePlacement, SurfaceSnapshot, error_item, preview_module_surfaces,
};
use bootty_ui::icons;
use eframe::egui::{self, RichText};

use bootty_ui::item_paint::module_color32;

use super::SettingsSurface;

/// Editor-only state. Every filesystem decision belongs to the extension host: this holds the
/// draft being edited plus the requests the host runs once painting is done.
#[derive(Default)]
pub(super) struct EditorState {
    selected: Option<ModuleIdentity>,
    creating: bool,
    new_identity: String,
    create_error: Option<String>,
    /// The identity whose source is loaded in `source`; `None` until the host answers a load.
    loaded: Option<ModuleIdentity>,
    source: String,
    path: PathBuf,
    customized: bool,
    has_builtin: bool,
    error: Option<String>,
    preview_source: String,
    preview_theme: Vec<(String, String)>,
    preview: Vec<SurfaceSnapshot>,
    /// Requests collected this frame, run by the host after painting.
    requests: Vec<ModuleSourceRequest>,
    /// A module the host just created, taken by the page whose button asked for it.
    created: Option<ModuleIdentity>,
}

impl EditorState {
    fn request(&mut self, request: ModuleSourceRequest) {
        self.requests.push(request);
    }

    pub(super) fn take_requests(&mut self) -> Vec<ModuleSourceRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Drop a create nobody consumed. The page that asked takes it on its next paint, so a
    /// leftover means settings closed in between and no page still owes it a row.
    pub(super) fn discard_created(&mut self) {
        self.created = None;
    }

    /// Apply one host outcome. A create/reset/import success drops the loaded draft so the
    /// next frame reloads the module from its owner.
    pub(super) fn apply(&mut self, outcome: ModuleSourceOutcome) {
        match outcome {
            ModuleSourceOutcome::Loaded { source, exists } => {
                self.loaded = Some(source.identity);
                self.source = source.source;
                self.path = source.path;
                self.customized = source.customized;
                self.has_builtin = source.has_builtin;
                self.error =
                    (!exists).then(|| "Module file does not exist; editing creates it.".to_owned());
                self.preview_source.clear();
            }
            ModuleSourceOutcome::Created(Ok(identity)) => {
                self.creating = false;
                self.create_error = None;
                self.loaded = None;
                self.selected = Some(identity.clone());
                self.created = Some(identity);
            }
            ModuleSourceOutcome::Created(Err(error)) => self.create_error = Some(error),
            ModuleSourceOutcome::Saved(Ok(path)) => {
                self.path = path;
                self.customized = true;
                self.error = None;
            }
            ModuleSourceOutcome::Saved(Err(error)) => {
                self.error = Some(format!("Save failed: {error}"));
            }
            ModuleSourceOutcome::Reset(Ok(_)) => self.loaded = None,
            ModuleSourceOutcome::Reset(Err(error)) => self.error = Some(error),
            ModuleSourceOutcome::Imported(Ok(identity)) => {
                self.loaded = None;
                self.error = None;
                self.selected = Some(identity);
            }
            ModuleSourceOutcome::Imported(Err(error)) => {
                self.error = Some(format!("Import failed: {error}"));
            }
        }
    }
}

pub(super) fn sidebar_ui(win: &mut SettingsSurface, ui: &mut egui::Ui, sources: ModuleSources<'_>) {
    let palette = win.palette;
    let mut selected = win
        .module_editor
        .selected
        .clone()
        .filter(|identity| sources.identities.contains(identity))
        .or_else(|| sources.identities.first().cloned());

    settings_pane(
        win,
        ui,
        |win, ui| {
            super::section(ui, palette, "EXTENSIONS");
            for identity in sources.identities {
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
            if !sources.legacy.is_empty() {
                super::section(ui, palette, "LEGACY MODULES");
                super::settings_notice(
                    ui,
                    palette.warning,
                    "These modules are inactive. Import one to validate and activate it.",
                );
                for module in sources.legacy {
                    legacy_row(win, ui, module);
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

fn legacy_row(win: &mut SettingsSurface, ui: &mut egui::Ui, module: &LegacyExtensionModule) {
    let palette = win.palette;
    let label = format!(
        "{} · {}",
        module.placement.as_str(),
        module.source_path.display()
    );
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(label).color(palette.subtext).size(10.0));
        if super::settings_button(ui, palette, "Import").clicked() {
            win.module_editor
                .request(ModuleSourceRequest::ImportLegacy(module.clone()));
        }
    });
}

pub(super) fn settings_pane<T>(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    selector: impl FnOnce(&mut SettingsSurface, &mut egui::Ui) -> T,
    content: impl FnOnce(&mut SettingsSurface, &mut egui::Ui, T),
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
    win: &mut SettingsSurface,
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

fn source_editor(win: &mut SettingsSurface, ui: &mut egui::Ui, identity: &ModuleIdentity) {
    let palette = win.palette;
    win.module_editor.selected = Some(identity.clone());
    if win.module_editor.loaded.as_ref() != Some(identity) {
        // The host owns the extension root; it answers with the source before the next frame.
        win.module_editor
            .request(ModuleSourceRequest::Load(identity.clone()));
        super::settings_notice(ui, palette.muted, "Loading module source…");
        return;
    }
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
            module_toolbar(ui, palette, state, identity);
            if let Some(error) = &state.error {
                ui.label(RichText::new(error).color(palette.destructive).size(11.0));
            }
            module_preview(ui, palette, &state.preview);
            code_editor(ui, palette, state, identity);
        });
}

pub(super) fn new_module_ui(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
) -> Option<ModuleIdentity> {
    let palette = win.palette;
    if !win.module_editor.creating {
        if super::settings_button(ui, palette, "+ New module").clicked() {
            win.module_editor.creating = true;
            win.module_editor.new_identity.clear();
            win.module_editor.create_error = None;
        }
        return win.module_editor.created.take();
    }
    ui.horizontal(|ui| {
        super::settings_text_edit_width(
            ui,
            palette,
            &mut win.module_editor.new_identity,
            "nested/module.luau",
            (ui.available_width() - 150.0).max(180.0),
        );
        if super::settings_button(ui, palette, "Create").clicked() {
            let value = win.module_editor.new_identity.clone();
            win.module_editor
                .request(ModuleSourceRequest::Create(value));
        }
        if super::settings_button(ui, palette, "Cancel").clicked() {
            win.module_editor.creating = false;
            win.module_editor.create_error = None;
        }
    });
    if let Some(error) = &win.module_editor.create_error {
        ui.label(RichText::new(error).color(palette.destructive).size(11.0));
    }
    win.module_editor.created.take()
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
    ui.colored_label(palette.muted, "PREVIEW · EXAMPLE DATA");
    for surface in surfaces {
        ui.colored_label(
            palette.subtext,
            format!(
                "{} · {:?}",
                surface.declaration.id, surface.declaration.placement
            ),
        );
        for item in &surface.items {
            ui.horizontal_wrapped(|ui| {
                if let Some(icon) = &item.icon {
                    ui.colored_label(item.fg.map(module_color32).unwrap_or(palette.text), icon);
                }
                ui.colored_label(
                    item.fg.map(module_color32).unwrap_or(palette.text),
                    &item.text,
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
    identity: &ModuleIdentity,
) {
    ui.horizontal(|ui| {
        ui.colored_label(palette.text, identity.as_str());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if super::settings_icon_button(ui, palette, "copy", "Copy module path").clicked() {
                ui.ctx()
                    .copy_text(state.path.to_string_lossy().into_owned());
            }
            if state.customized
                && state.has_builtin
                && super::settings_button(ui, palette, "Reset to default").clicked()
            {
                state.request(ModuleSourceRequest::Reset(identity.clone()));
            }
        });
    });
}

fn code_editor(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
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
        let source = state.source.clone();
        state.request(ModuleSourceRequest::Save {
            identity: identity.clone(),
            source,
        });
    }
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
