use std::path::PathBuf;

use bootty_extension::{
    LegacyExtensionModule, ModuleIdentity, ModuleSourceOutcome, ModuleSourceRequest, ModuleSources,
    PublishedSurfaceSnapshot, SurfacePlacement, SurfaceSnapshot, preview_builtin_surfaces,
    preview_module_surfaces,
};
use bootty_mux::controller::{BindingId, MuxScope, SpaceId};
use bootty_ui::code_editor::{CodeEditorSpec, code_editor};
use bootty_ui::icons;
use bootty_ui::status_layout::{Align, ResolvedItem, ResolvedSegment, status_bar_layout};
use bootty_ui::status_strip::{self, StatusStrip};
use eframe::egui::{self, RichText};

use bootty_ui::item_paint::module_color32;

use super::SettingsSurface;

/// Luau keywords the editor's Lua base syntax does not know.
const LUAU_KEYWORDS: &[&str] = &["continue", "export", "type"];

/// Words the completer offers on top of the syntax's own: the bootty host API and the fields an
/// item carries.
const LUAU_COMPLETIONS: &[&str] = &[
    "bootty",
    "register",
    "placement",
    "render",
    "interval",
    "text",
    "icon",
    "fg",
    "bg",
    "action",
    "gauge",
    "progress",
    "session",
    "sessions",
    "windows",
    "metrics",
    "theme",
    "sidebar",
    "visible",
    "run",
    "json",
    "decode",
    "shell",
    "path",
    "ui",
];

/// The editor keeps at least this much height even on a short page, so a module is edited in a
/// real viewport rather than in a few visible lines.
const EDITOR_MIN_HEIGHT: f32 = 640.0;
/// Preview chrome matched to the real thing: one status row's height, and the sidebar mock's box.
const PREVIEW_STATUS_HEIGHT: f32 = 38.0;
const PREVIEW_SIDEBAR_SIZE: egui::Vec2 = egui::vec2(286.0, 190.0);

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
    /// Why the draft produced no surfaces, when it failed to load or run.
    preview_error: Option<String>,
    /// The editor holds changes the extension root has not been told about yet.
    unsaved: bool,
    /// The preview is the module's live render rather than a sandbox one.
    preview_live: bool,
    /// The session rows the previewed module renders among, from the same world as the preview.
    preview_context: Vec<SurfaceSnapshot>,
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
                self.source = displayed_source(&source.source);
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

/// The two `[sidebar]` module lists. Both keys decide what the live sidebar renders and in which
/// order, so this page is the only place they can be edited.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarList {
    Sidebar,
    Session,
}

impl SidebarList {
    /// The placement this list draws candidates from, its drag-list salt, and the key it writes.
    const fn meta(self) -> (SurfacePlacement, &'static str, &'static str) {
        match self {
            Self::Sidebar => (SurfacePlacement::Sidebar, "sidebar_modules", "modules"),
            Self::Session => (
                SurfacePlacement::Session,
                "session_modules",
                "session-modules",
            ),
        }
    }
}

pub(super) fn sidebar_ui(win: &mut SettingsSurface, ui: &mut egui::Ui, sources: ModuleSources<'_>) {
    let palette = win.palette;
    scan_error_notice(ui, palette, &sources);
    let sidebar_available = available_surfaces(win, &sources, SidebarList::Sidebar);
    let session_available = available_surfaces(win, &sources, SidebarList::Session);
    // A module that declares no surface at all renders nowhere and would have no editor on any
    // page, so it gets a section here. One that declares a status or floating surface is edited on
    // the page that owns that placement, not this one.
    let declared = sources
        .declared
        .iter()
        .filter_map(|(_, name)| surface_identity(sources.identities, name))
        .collect::<Vec<_>>();
    let unregistered = sources
        .identities
        .iter()
        .filter(|identity| !declared.contains(identity))
        .cloned()
        .collect::<Vec<_>>();
    // Open on the first configured module, the way the list reads top to bottom, so the page never
    // shows an editor pane with nothing in it.
    let mut selected = win.module_editor.selected.clone().or_else(|| {
        sidebar_available
            .first()
            .or_else(|| session_available.first())
            .and_then(|name| surface_identity(sources.identities, name))
    });

    settings_pane(
        win,
        ui,
        |win, ui| {
            super::section(ui, palette, "SIDEBAR");
            module_list(
                ui,
                win,
                SidebarList::Sidebar,
                sources.identities,
                &sidebar_available,
                &mut selected,
            );

            ui.add_space(10.0);
            super::section(ui, palette, "SESSION");
            module_list(
                ui,
                win,
                SidebarList::Session,
                sources.identities,
                &session_available,
                &mut selected,
            );

            // A new module's starter source registers a sidebar surface, so enable it there and it
            // shows up in the live sidebar immediately.
            if let Some(identity) = new_module_ui(win, ui) {
                let name = identity.namespace();
                let mut modules = configured_modules(win, SidebarList::Sidebar).clone();
                if !modules.contains(&name) {
                    modules.push(name);
                    write_modules(win, SidebarList::Sidebar, modules);
                }
                selected = Some(identity);
            }

            if !unregistered.is_empty() {
                ui.add_space(10.0);
                super::section(ui, palette, "NOT REGISTERED");
                super::settings_notice(
                    ui,
                    palette.warning,
                    "These modules declare no surface, so nothing renders them.",
                );
                for identity in &unregistered {
                    let active = selected.as_ref() == Some(identity);
                    let label = super::module_display_name(&identity.namespace());
                    let response =
                        module_selector_row(ui, palette, &label, active, None, false, |_| {});
                    if response.clicked() {
                        selected = Some(identity.clone());
                    }
                }
            }

            if !sources.legacy.is_empty() {
                ui.add_space(10.0);
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
            Some(identity) => source_editor(win, ui, &identity, &sources),
            None => super::settings_notice(ui, palette.muted, "No sidebar modules found."),
        },
    );
}

/// Every surface name one list can offer: the configured ones in their configured order, then the
/// rest of that placement's declarations. A configured name no module declares any more stays
/// listed, or the only row that could turn it off would disappear.
fn available_surfaces(
    win: &SettingsSurface,
    sources: &ModuleSources<'_>,
    list: SidebarList,
) -> Vec<String> {
    let mut names = configured_modules(win, list).clone();
    for name in sources.declared_for(list.meta().0) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn configured_modules(win: &SettingsSurface, list: SidebarList) -> &Vec<String> {
    match list {
        SidebarList::Sidebar => &win.config.sidebar.modules,
        SidebarList::Session => &win.config.sidebar.session_modules,
    }
}

/// Publish one list's new order or membership: into the working copy so the rows move now, and
/// into the draft document so the sidebar picks it up at the frame boundary.
fn write_modules(win: &mut SettingsSurface, list: SidebarList, names: Vec<String>) {
    match list {
        SidebarList::Sidebar => win.config.sidebar.modules = names.clone(),
        SidebarList::Session => {
            win.config.sidebar.session_modules = names.clone();
            win.config.sidebar.session_modules_configured = true;
        }
    }
    win.writeback
        .set_strings(&["sidebar", list.meta().2], &names);
}

/// Every candidate for one placement, as a row with a toggle: the enabled ones first in their
/// configured order and draggable, the rest after. One control with two states, rather than an `x`
/// on one group and a `plus` on another — an `x` reads as "delete this module", which is not what
/// turning a module off does.
fn module_list(
    ui: &mut egui::Ui,
    win: &mut SettingsSurface,
    list: SidebarList,
    identities: &[ModuleIdentity],
    available: &[String],
    selected: &mut Option<ModuleIdentity>,
) {
    let palette = win.palette;
    let mut modules = configured_modules(win, list).clone();
    let mut toggled = None;
    let reorder = super::reorderable_list(
        ui,
        palette,
        list.meta().1,
        modules.len(),
        |ui, index, handle| {
            // Keep at least one enabled: an empty list means a sidebar with no session rows at all,
            // and the reader treats an empty list as unset, so writing one would misstate intent.
            let last_enabled = modules.len() == 1;
            if module_toggle_row(
                ui,
                palette,
                identities,
                &modules[index],
                selected,
                Some(handle),
                ToggleState {
                    enabled: true,
                    locked: last_enabled,
                },
            ) {
                toggled = Some(modules[index].clone());
            }
        },
    );

    let mut changed = false;
    if let Some((from, slot)) = reorder {
        super::apply_reorder(&mut modules, from, slot);
        changed = true;
    }

    for name in available.iter().filter(|name| !modules.contains(name)) {
        if module_toggle_row(
            ui,
            palette,
            identities,
            name,
            selected,
            None,
            ToggleState {
                enabled: false,
                locked: false,
            },
        ) {
            toggled = Some(name.clone());
        }
    }

    if let Some(name) = toggled {
        match modules.iter().position(|module| *module == name) {
            Some(index) => {
                modules.remove(index);
            }
            // Turning one on puts it last, where the reader will place it until it is dragged.
            None => modules.push(name),
        }
        changed = true;
    }

    if changed {
        write_modules(win, list, modules);
    }
}

/// Whether a module is on, and whether that can be changed.
#[derive(Clone, Copy)]
struct ToggleState {
    enabled: bool,
    locked: bool,
}

/// One module's row. Returns whether its toggle was flipped.
fn module_toggle_row(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    identities: &[ModuleIdentity],
    name: &str,
    selected: &mut Option<ModuleIdentity>,
    handle: Option<&super::DragHandle>,
    state: ToggleState,
) -> bool {
    let identity = surface_identity(identities, name);
    let active = identity.is_some() && identity.as_ref() == selected.as_ref();
    let mut flipped = false;
    let response = module_selector_row(
        ui,
        palette,
        &super::module_display_name(name),
        active,
        handle,
        true,
        |ui| {
            let mut enabled = state.enabled;
            let toggle = ui
                .add_enabled_ui(!state.locked, |ui| {
                    super::settings_toggle(ui, palette, &mut enabled)
                })
                .inner;
            flipped = toggle && !state.locked;
        },
    );
    if state.locked {
        response
            .clone()
            .on_hover_text("The last module in a list stays on");
    }
    if response.clicked() && identity.is_some() {
        selected.clone_from(&identity);
    }
    flipped
}

/// The module file a surface name is edited through. A loaded module claims the name by its
/// namespace (`agents.pi` for `agents/pi.luau`) or by its file stem; with no module set to consult,
/// fall back to the like-named file at the extension root. A surface declared under a name that
/// matches no module file is not reachable this way — open its module under OTHER MODULES.
fn surface_identity(identities: &[ModuleIdentity], name: &str) -> Option<ModuleIdentity> {
    identities
        .iter()
        .find(|identity| identity.namespace() == name || surface_name(identity) == name)
        .cloned()
        .or_else(|| ModuleIdentity::parse(format!("{name}.luau")).ok())
}

/// A module's file stem. Kept as a fallback match beside `namespace()`: the starter template now
/// declares the namespace, but a module created before that change still names its stem.
fn surface_name(identity: &ModuleIdentity) -> &str {
    identity
        .as_ref()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_else(|| identity.as_str())
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
    sources: &ModuleSources<'_>,
) {
    // The surface name is what config records; resolve it against the loaded modules so the live
    // preview can be keyed on the file that publishes it.
    let Some(identity) = surface_identity(sources.identities, surface) else {
        super::settings_notice(
            ui,
            win.palette.destructive,
            "Invalid extension surface name.",
        );
        return;
    };
    source_editor(win, ui, &identity, sources);
}

fn source_editor(
    win: &mut SettingsSurface,
    ui: &mut egui::Ui,
    identity: &ModuleIdentity,
    sources: &ModuleSources<'_>,
) {
    let palette = win.palette;
    win.module_editor.selected = Some(identity.clone());
    if win.module_editor.loaded.as_ref() != Some(identity) {
        // The host owns the extension root; it answers with the source before the next frame.
        win.module_editor
            .request(ModuleSourceRequest::Load(identity.clone()));
        super::settings_notice(ui, palette.muted, "Loading module source…");
        return;
    }
    // An unedited module is already running with real data — its own usage figures, its own agent
    // state — so show that. A sandbox render cannot reach the machine, and inventing answers for
    // whichever commands the built-ins happen to run would never cover a module someone wrote.
    let live = (!win.module_editor.unsaved)
        .then(|| sources.live_for(identity.as_str()))
        .filter(|surfaces| !surfaces.is_empty());
    let variant = win.config.appearance.mode.variant(win.appearance_variant);
    let theme = crate::theme::theme_tokens(&win.config, variant);
    let state = &mut win.module_editor;
    state.preview_live = live.is_some();
    // The surrounding session rows come from the same world as the preview itself: the live ones
    // when showing the live render, the example ones when sandboxing an edit.
    state.preview_context = if live.is_some() {
        sources.live_for(SESSIONS_MODULE)
    } else {
        preview_builtin_surfaces(SESSIONS_MODULE, theme.clone()).unwrap_or_default()
    };
    if let Some(surfaces) = live {
        state.preview = surfaces;
        state.preview_error = None;
        state.preview_source.clone_from(&state.source);
        state.preview_theme = theme.clone();
    } else if state.preview_source != state.source || state.preview_theme != theme {
        match preview_module_surfaces(identity, &state.source, theme.clone()) {
            Ok(surfaces) => {
                state.preview = surfaces;
                state.preview_error = None;
            }
            Err(error) => {
                state.preview.clear();
                state.preview_error = Some(error);
            }
        }
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
                ui.add_space(6.0);
                ui.label(RichText::new(error).color(palette.destructive).size(11.0));
            }
            module_preview(
                ui,
                palette,
                &state.preview,
                state.preview_error.as_deref(),
                &state.preview_context,
                state.preview_live,
            );
            source_edit(ui, palette, state, identity);
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
            "module-name",
            (ui.available_width() - 150.0).max(180.0),
        );
        if super::settings_button(ui, palette, "Create").clicked() {
            // An unusable name is rejected here; only the host can say whether the file exists.
            match new_module_identity(&win.module_editor.new_identity) {
                Ok(identity) => win
                    .module_editor
                    .request(ModuleSourceRequest::Create(identity.as_str().to_owned())),
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
    win.module_editor.created.take()
}

/// The identity a typed name asks for. A bare name gets the `.luau` every module file carries, so
/// the field stays a name rather than a path.
fn new_module_identity(value: &str) -> Result<ModuleIdentity, String> {
    let value = value.trim();
    let value = if std::path::Path::new(value).extension().is_some() {
        value.to_owned()
    } else {
        format!("{value}.luau")
    };
    ModuleIdentity::parse(value)
        .map_err(|_| "Use letters, numbers, hyphens, or underscores.".to_owned())
}

/// Width reserved beside a row for its trailing control, and the gap before it.
const TRAILING_WIDTH: f32 = 54.0;
const TRAILING_GAP: f32 = 8.0;

pub(super) fn module_selector_row(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    label: &str,
    selected: bool,
    handle: Option<&super::DragHandle>,
    has_trailing: bool,
    trailing: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    // The trailing control sits beside the row rather than inside it: a row is selected by clicking
    // it, and a disable button overlapping that target turns a mis-aim into a lost module.
    let trailing_width = if has_trailing { TRAILING_WIDTH } else { 0.0 };
    let (outer, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 36.0),
        egui::Sense::hover(),
    );
    let rect = egui::Rect::from_min_max(
        outer.min,
        egui::Pos2::new(outer.max.x - trailing_width, outer.max.y),
    );
    let response = ui.interact(rect, ui.next_auto_id(), egui::Sense::click());
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
    // The trailing control is a 30px icon button, so the content strip has to be 30px tall or it
    // overflows the row's own frame.
    let content_rect = rect.shrink2(egui::Vec2::new(10.0, (rect.height() - 30.0) / 2.0));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.add_space(22.0);
    content.label(RichText::new(label).color(palette.text).strong());
    if has_trailing {
        let mut slot = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(
                    egui::Pos2::new(rect.max.x + TRAILING_GAP, content_rect.min.y),
                    outer.max,
                ))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        trailing(&mut slot);
    }
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

/// The draft's surfaces, drawn by the chrome that will draw them for real: a status strip for a
/// status surface, and the sidebar for every other placement, which all render as a row list.
fn module_preview(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    surfaces: &[SurfaceSnapshot],
    error: Option<&str>,
    sessions: &[SurfaceSnapshot],
    live: bool,
) {
    ui.label(
        RichText::new(if live {
            "PREVIEW · LIVE"
        } else {
            "PREVIEW · EXAMPLE DATA"
        })
        .color(palette.muted)
        .strong()
        .size(11.0),
    );
    ui.add_space(4.0);
    if let Some(error) = error {
        ui.label(RichText::new(error).color(palette.destructive).size(11.0));
    }
    for surface in surfaces {
        // A module that reads the machine — a usage query, an agent's state — has nothing to draw
        // from example data. Say that, or its preview looks broken rather than empty.
        if surface.items.is_empty() {
            super::settings_notice(
                ui,
                palette.muted,
                &format!("{} is rendering nothing right now.", surface.declaration.id),
            );
        }
        match surface.declaration.placement {
            SurfacePlacement::Status => status_preview(ui, palette, surface),
            // Nothing in a sidebar stands alone: a session module decorates the built-in session
            // rows, and a sidebar module renders beside them. Previewing either by itself shows an
            // empty sidebar, which says nothing about how the module will look.
            placement => sidebar_preview(
                ui,
                palette,
                &in_sidebar_context(surface, placement, sessions),
            ),
        }
    }
    ui.add_space(10.0);
}

/// `surface` placed among the built-in `sessions` rows: composed onto them for a session module,
/// alongside them for anything else. Falls back to the surface alone if the built-in preview fails,
/// so a broken built-in never hides the module being edited — and `sessions` itself is left as-is
/// rather than doubled.
fn in_sidebar_context(
    surface: &SurfaceSnapshot,
    placement: SurfacePlacement,
    sessions: &[SurfaceSnapshot],
) -> SurfaceSnapshot {
    if surface.declaration.id == SESSIONS_MODULE || sessions.is_empty() {
        return surface.clone();
    }
    let published = |module: &str, snapshot: &SurfaceSnapshot| PublishedSurfaceSnapshot {
        module: module.to_owned(),
        generation: 0,
        snapshot: snapshot.clone(),
    };
    let sessions = sessions
        .iter()
        .map(|snapshot| published(SESSIONS_MODULE, snapshot))
        .collect::<Vec<_>>();
    let base = sessions
        .iter()
        .flat_map(PublishedSurfaceSnapshot::items)
        .collect::<Vec<_>>();
    let component_surface = published(&surface.declaration.id, surface);
    let components = component_surface.items();
    let items = if placement == SurfacePlacement::Session {
        crate::ui::chrome::compose_session_module_items(base, components)
    } else {
        base.into_iter().chain(components).collect()
    };
    SurfaceSnapshot {
        declaration: surface.declaration.clone(),
        items: items.into_iter().map(|published| published.item).collect(),
    }
}

/// The built-in module that draws the session rows every other sidebar module sits beside.
const SESSIONS_MODULE: &str = "sessions";

fn status_preview(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, surface: &SurfaceSnapshot) {
    let items = surface
        .items
        .iter()
        .map(|item| ResolvedItem {
            item,
            icon: item.icon.as_deref(),
            fg: item.fg.map(module_color32),
            bg: item.bg.map(module_color32),
            stroke: item.stroke.map(module_color32),
        })
        .collect();
    let segments = [ResolvedSegment {
        align: Align::Left,
        wrappable: false,
        round_run_end: false,
        source_slot: 0,
        module: "preview",
        generation: 0,
        surface: &surface.declaration.id,
        items,
    }];
    egui::Frame::NONE
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .show(ui, |ui| {
            let size = egui::vec2(ui.available_width(), PREVIEW_STATUS_HEIGHT);
            preview_scope(ui, size, |ui| {
                let layout = status_bar_layout(ui, ui.max_rect(), &segments, 8.0, None);
                status_strip::show(
                    ui,
                    palette,
                    StatusStrip {
                        layout: &layout,
                        background: palette.mantle,
                        row_height: PREVIEW_STATUS_HEIGHT,
                        interaction_id: "settings-module-preview",
                        identity_salt: "",
                    },
                    |_, _, _| None::<()>,
                );
            });
        });
}

fn sidebar_preview(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, surface: &SurfaceSnapshot) {
    // The sidebar consumes items the way the host publishes them, so wrap the draft's surface in
    // the same envelope instead of teaching the preview a second item shape.
    let published = PublishedSurfaceSnapshot {
        module: surface.declaration.id.clone(),
        generation: 0,
        snapshot: surface.clone(),
    };
    let (footer, body): (Vec<_>, Vec<_>) = published
        .items()
        .partition(|item| item.item.kind.as_deref() == Some("footer"));
    let scope = MuxScope::new(SpaceId::from_persistence(0), BindingId::from_persistence(0));
    let items = crate::ui::sidebar::build_sidebar_items_from_published_items(
        &body,
        scope,
        Some("$1"),
        false,
    );
    let session_count = body
        .iter()
        .filter(|item| item.item.kind.as_deref() == Some("session"))
        .count();
    let size = egui::vec2(
        ui.available_width().min(PREVIEW_SIDEBAR_SIZE.x),
        PREVIEW_SIDEBAR_SIZE.y,
    );
    preview_scope(ui, size, |ui| {
        egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
            ui.set_width(size.x);
            ui.set_height(size.y);
            crate::ui::chrome::show_sidebar(
                ui,
                palette,
                size.y,
                crate::ui::chrome::SidebarModel {
                    items: &items,
                    footer_items: &footer,
                    session_count,
                    title_visible: false,
                    reserve_titlebar_buttons: false,
                    title_icon: None,
                    top_inset: 0.0,
                    border_visible: true,
                    border_bottom: true,
                    separator_visible: true,
                    focused: false,
                    hovered_session: None,
                    fullscreen: false,
                    hover_override: None,
                    current_override: None,
                    border_override: None,
                },
            );
        });
    });
}

/// A bounded, non-interactive stage for real chrome: disabled so no click reaches the live app, but
/// painted at full opacity so the preview shows the module's own colors.
fn preview_scope(ui: &mut egui::Ui, size: egui::Vec2, contents: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.add_enabled_ui(false, |ui| {
            ui.style_mut().visuals.disabled_alpha = 1.0;
            contents(ui);
        });
    });
}

fn module_toolbar(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
    identity: &ModuleIdentity,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(super::module_display_name(&identity.namespace()))
                .color(palette.text)
                .strong()
                .size(15.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if super::settings_icon_button(ui, palette, "copy", "Copy module path").clicked() {
                ui.ctx()
                    .copy_text(state.path.to_string_lossy().into_owned());
            }
            ui.label(
                RichText::new(bootty_extension::display_path(
                    &state.path.to_string_lossy(),
                    crate::strings::home_dir().as_deref(),
                ))
                .color(palette.muted)
                .monospace()
                .size(11.0),
            );
            if state.customized
                && state.has_builtin
                && super::settings_button(ui, palette, "Reset to default").clicked()
            {
                state.request(ModuleSourceRequest::Reset(identity.clone()));
            }
        });
    });
}

fn source_edit(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
    identity: &ModuleIdentity,
) {
    let output = code_editor(
        ui,
        palette,
        CodeEditorSpec {
            id_salt: &format!("module_editor_{identity}"),
            keywords: LUAU_KEYWORDS,
            completions: LUAU_COMPLETIONS,
            min_height: EDITOR_MIN_HEIGHT,
        },
        &mut state.source,
    );
    state.unsaved |= output.changed;
    // Persist when the editor is done being typed into, not per keystroke: a stray keypress while
    // reading a module used to write a permanent override of it before the character was visible.
    if output.lost_focus {
        flush_source(state, identity);
    }
}

/// Flush whichever module the editor has open. Settings closing is the other end of the blur rule:
/// an edit is never lost, and a keystroke is never a commit.
pub(super) fn flush_selected_source(state: &mut EditorState) {
    if let Some(identity) = state.selected.clone() {
        flush_source(state, &identity);
    }
}

fn flush_source(state: &mut EditorState, identity: &ModuleIdentity) {
    if !std::mem::take(&mut state.unsaved) {
        return;
    }
    let source = saved_source(&state.source);
    state.request(ModuleSourceRequest::Save {
        identity: identity.clone(),
        source,
    });
}

/// The file's terminating newline is hidden while editing — an editor should not open on a blank
/// last line — and [`saved_source`] puts it back, so the file on disk keeps it.
fn displayed_source(source: &str) -> String {
    source.strip_suffix('\n').unwrap_or(source).to_owned()
}

fn saved_source(source: &str) -> String {
    format!("{}\n", source.strip_suffix('\n').unwrap_or(source))
}

/// What the extension root could not load: a scan that failed outright or shed modules past the
/// backstop, then each module that would not load or publish. A skipped module renders nothing and
/// says nothing, so a typo in one file reads as that feature having disappeared.
pub(super) fn scan_error_notice(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    sources: &ModuleSources<'_>,
) {
    if let Some(error) = &sources.scan_error {
        super::settings_notice(
            ui,
            palette.destructive,
            &format!("Extensions did not fully load: {error}"),
        );
    }
    for (identity, error) in &sources.failures {
        super::settings_notice(
            ui,
            palette.destructive,
            &format!("{identity} did not load: {error}"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_hides_only_the_file_terminating_newline() {
        assert_eq!(displayed_source("return {}\n"), "return {}");
        assert_eq!(displayed_source("return {}\n\n"), "return {}\n");
        assert_eq!(saved_source("return {}"), "return {}\n");
        assert_eq!(saved_source("return {}\n"), "return {}\n");
    }

    #[test]
    fn a_new_module_name_gains_the_module_extension() {
        assert_eq!(
            new_module_identity(" my_module ").map(|id| id.as_str().to_owned()),
            Ok("my_module.luau".to_owned())
        );
        assert_eq!(
            new_module_identity("nested/thing.luau").map(|id| id.as_str().to_owned()),
            Ok("nested/thing.luau".to_owned())
        );
        assert!(new_module_identity("bad name!").is_err());
    }

    /// Every text run the preview painted, so a regression back to labelling items — which prints
    /// an icon slug where the icon belongs — fails here.
    fn preview_text(source: &str) -> Vec<String> {
        let identity = ModuleIdentity::parse("preview.luau").expect("identity");
        let surfaces =
            preview_module_surfaces(&identity, source, Vec::new()).expect("preview surfaces");
        let sessions = preview_builtin_surfaces(SESSIONS_MODULE, Vec::new()).expect("session rows");
        let context = egui::Context::default();
        bootty_ui::icons::install_icon_fonts(&context);
        let output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 600.0),
                )),
                ..egui::RawInput::default()
            },
            |ui| {
                module_preview(
                    ui,
                    bootty_ui::ThemePalette::default(),
                    &surfaces,
                    None,
                    &sessions,
                    false,
                )
            },
        );
        let mut runs = Vec::new();
        collect_text(
            &egui::Shape::Vec(
                output
                    .shapes
                    .iter()
                    .map(|shape| shape.shape.clone())
                    .collect(),
            ),
            &mut runs,
        );
        output.drop_without_applying_deltas();
        runs
    }

    fn collect_text(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(text) => {
                let text = text.galley.text().trim();
                if !text.is_empty() {
                    out.push(text.to_owned());
                }
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| collect_text(shape, out)),
            _ => {}
        }
    }

    /// A session module decorates the built-in session rows, so its preview has to show them —
    /// on its own it renders detail rows with nothing to attach to, i.e. "no sessions".
    #[test]
    fn a_session_surface_previews_over_the_builtin_session_rows() {
        let runs = preview_text(
            "bootty.ui.register({ id = \"preview\", placement = \"session\" }, function()\n\
             \treturn bootty.ui.session_components({\n\
             \t\tsessions = bootty.sessions(),\n\
             \t\trender = function()\n\
             \t\t\treturn { summary = { { text = \"+7\" } } }\n\
             \t\tend,\n\
             \t})\n\
             end)\n",
        );
        assert!(
            !runs.iter().any(|run| run == "no sessions"),
            "the built-in session rows are composed in: {runs:?}"
        );
        assert!(
            runs.iter().any(|run| run.contains("api")),
            "an example session is named: {runs:?}"
        );
    }

    /// A sidebar module renders beside the session rows, not instead of them, so its preview shows
    /// them too — and previewing `sessions` itself must not double them.
    #[test]
    fn a_sidebar_surface_previews_beside_the_builtin_session_rows() {
        let runs = preview_text(
            "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
             \treturn { { kind = \"footer\", text = \"usage 42%\" } }\n\
             end)\n",
        );
        assert!(
            !runs.iter().any(|run| run == "no sessions"),
            "the session rows are drawn alongside: {runs:?}"
        );
        assert!(
            runs.iter().any(|run| run.contains("api")),
            "an example session is named: {runs:?}"
        );
        assert!(
            runs.iter().any(|run| run.contains("usage 42%")),
            "the module's own item is drawn: {runs:?}"
        );
    }

    #[test]
    fn a_status_surface_previews_through_the_real_strip() {
        let runs = preview_text(
            "bootty.ui.register({ id = \"preview\", placement = \"status\" }, function()\n\
             \treturn { { text = \"42%\", icon = \"battery-charging\" } }\n\
             end)\n",
        );

        assert!(runs.iter().any(|run| run == "42%"), "{runs:?}");
        // The icon is drawn as a glyph, never spelled out.
        assert!(
            !runs.iter().any(|run| run == "battery-charging"),
            "{runs:?}"
        );
    }

    #[test]
    fn a_sidebar_surface_previews_through_the_real_sidebar() {
        let runs = preview_text(
            "bootty.ui.register({ id = \"preview\", placement = \"sidebar\" }, function()\n\
             \treturn { { text = \"work/api\", kind = \"session\", session_id = \"$1\" } }\n\
             end)\n",
        );

        assert!(runs.iter().any(|run| run == "work/api"), "{runs:?}");
        // A session row means the mock counted it, so the empty-state text stays away.
        assert!(!runs.iter().any(|run| run == "no sessions"), "{runs:?}");
    }
}
