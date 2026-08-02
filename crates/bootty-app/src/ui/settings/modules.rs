use std::path::PathBuf;

use eframe::egui::{self, RichText};
use egui_code_editor::{CodeEditor, ColorTheme, Completer, Editor as _, Syntax, Token, TokenType};

use super::SettingsWindow;
use crate::extensions::ModuleKind;

#[derive(Default)]
pub(super) struct EditorState {
    selected_kind: Option<ModuleKind>,
    selected_name: Option<String>,
    creating_kind: Option<ModuleKind>,
    new_name: String,
    create_error: Option<String>,
    loaded_key: Option<String>,
    source: String,
    completer: Option<Completer>,
    path: PathBuf,
    customized: bool,
    has_builtin: bool,
    error: Option<String>,
    preview_key: Option<String>,
    preview_source: String,
    preview_theme: Vec<(String, String)>,
    preview_items: Vec<crate::extensions::ModuleItem>,
}

pub(super) fn sidebar_ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let Some(config_dir) = win.config_path.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let sidebar_dir = config_dir.join(module_dir_name(ModuleKind::Sidebar));
    let session_dir = config_dir.join(module_dir_name(ModuleKind::Session));
    let mut sidebar_names = crate::extensions::module_names(&sidebar_dir, ModuleKind::Sidebar);
    let mut session_names = crate::extensions::module_names(&session_dir, ModuleKind::Session);
    for name in &win.config.sidebar.modules {
        if !sidebar_names.contains(name) {
            sidebar_names.push(name.clone());
        }
    }
    for name in &win.config.sidebar.session_modules {
        if !session_names.contains(name) {
            session_names.push(name.clone());
        }
    }
    sidebar_names.sort();
    session_names.sort();

    let mut selected = win
        .module_editor
        .selected_kind
        .zip(win.module_editor.selected_name.clone())
        .filter(|(kind, name)| match kind {
            ModuleKind::Sidebar => sidebar_names.contains(name),
            ModuleKind::Session => session_names.contains(name),
            ModuleKind::Status => false,
        })
        .or_else(|| {
            win.config
                .sidebar
                .modules
                .first()
                .map(|name| (ModuleKind::Sidebar, name.clone()))
        })
        .or_else(|| {
            win.config
                .sidebar
                .session_modules
                .first()
                .map(|name| (ModuleKind::Session, name.clone()))
        });

    settings_pane(
        win,
        ui,
        |win, ui| {
            super::section(ui, palette, "SIDEBAR");
            module_list(ui, win, ModuleKind::Sidebar, &sidebar_names, &mut selected);
            if let Some(name) = new_module_ui(win, ui, ModuleKind::Sidebar) {
                if !win.config.sidebar.modules.contains(&name) {
                    win.config.sidebar.modules.push(name.clone());
                    win.set_sidebar_modules();
                }
                selected = Some((ModuleKind::Sidebar, name));
            }

            ui.add_space(10.0);
            super::section(ui, palette, "SESSION");
            module_list(ui, win, ModuleKind::Session, &session_names, &mut selected);
            if let Some(name) = new_module_ui(win, ui, ModuleKind::Session) {
                if !win.config.sidebar.session_modules.contains(&name) {
                    win.config.sidebar.session_modules.push(name.clone());
                    win.set_session_modules();
                }
                selected = Some((ModuleKind::Session, name));
            }
            selected
        },
        |win, ui, selected| {
            let Some((kind, name)) = selected else {
                super::settings_notice(ui, palette.muted, "No sidebar modules found.");
                return;
            };
            source_editor(win, ui, kind, &name);
        },
    );
}

pub(super) fn settings_pane<T>(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    selector: impl FnOnce(&mut SettingsWindow, &mut egui::Ui) -> T,
    content: impl FnOnce(&mut SettingsWindow, &mut egui::Ui, T),
) {
    let selector_width = module_selector_width(ui.available_width());
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

fn module_selector_width(available_width: f32) -> f32 {
    (available_width * 0.25).clamp(210.0, 280.0)
}

pub(super) fn source_editor(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    kind: ModuleKind,
    name: &str,
) {
    let palette = win.palette;
    let Some(config_dir) = win.config_path.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let module_dir = config_dir.join(module_dir_name(kind));
    let key = format!("{}:{name}", module_dir_name(kind));
    if win.module_editor.loaded_key.as_deref() != Some(&key) {
        load_editor(&mut win.module_editor, &module_dir, kind, name, key.clone());
    }
    win.module_editor.selected_kind = Some(kind);
    win.module_editor.selected_name = Some(name.to_owned());
    let variant = win.config.appearance.mode.variant(win.appearance_variant);
    let preview_theme = crate::theme::theme_tokens(&win.config, variant);

    let state = &mut win.module_editor;
    if state.preview_key.as_deref() != Some(&key)
        || state.preview_source != state.source
        || state.preview_theme != preview_theme
    {
        let output = crate::extensions::preview_module_source(&state.source, name, &preview_theme);
        state.preview_items = match kind {
            ModuleKind::Status => output,
            ModuleKind::Sidebar if name == "sessions" => output,
            ModuleKind::Sidebar => {
                let mut base = crate::extensions::preview_builtin_module(
                    ModuleKind::Sidebar,
                    "sessions",
                    &preview_theme,
                );
                base.extend(output);
                base
            }
            ModuleKind::Session => crate::app::compose_session_module_items(
                crate::extensions::preview_builtin_module(
                    ModuleKind::Sidebar,
                    "sessions",
                    &preview_theme,
                ),
                output,
            ),
        };
        state.preview_key = Some(key);
        state.preview_source.clone_from(&state.source);
        state.preview_theme = preview_theme;
    }
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin {
            left: 14,
            right: 14,
            top: 12,
            bottom: 12,
        })
        .show(ui, |ui| {
            module_toolbar(ui, palette, state, &module_dir, kind, name);
            if let Some(error) = &state.error {
                ui.add_space(6.0);
                ui.label(RichText::new(error).color(palette.destructive).size(11.0));
            }
            module_preview(ui, palette, kind, name, &state.preview_items);
            code_editor(ui, palette, state, &module_dir, kind, name);
        });
}

pub(super) fn new_module_ui(
    win: &mut SettingsWindow,
    ui: &mut egui::Ui,
    kind: ModuleKind,
) -> Option<String> {
    let palette = win.palette;
    if win.module_editor.creating_kind != Some(kind) {
        if super::settings_button(ui, palette, "+ New module").clicked() {
            win.module_editor.creating_kind = Some(kind);
            win.module_editor.new_name.clear();
            win.module_editor.create_error = None;
        }
        return None;
    }

    let mut created = None;
    ui.horizontal(|ui| {
        super::settings_text_edit_width(
            ui,
            palette,
            &mut win.module_editor.new_name,
            "module-name",
            (ui.available_width() - 150.0).max(180.0),
        );
        if super::settings_button(ui, palette, "Create").clicked() {
            let name = win.module_editor.new_name.trim().to_owned();
            let Some(config_dir) = win.config_path.parent() else {
                return;
            };
            let dir = config_dir.join(module_dir_name(kind));
            if !crate::extensions::valid_module_name(&name) {
                win.module_editor.create_error =
                    Some("Use letters, numbers, hyphens, or underscores.".to_owned());
            } else if crate::extensions::module_source(&dir, kind, &name).is_some() {
                win.module_editor.create_error = Some(format!("Module `{name}` already exists."));
            } else {
                match crate::extensions::save_module(
                    &dir,
                    kind,
                    &name,
                    &module_template(kind, &name),
                ) {
                    Ok(_) => {
                        win.module_editor.creating_kind = None;
                        win.module_editor.loaded_key = None;
                        win.module_editor.create_error = None;
                        created = Some(name);
                    }
                    Err(error) => {
                        win.module_editor.create_error = Some(format!("Create failed: {error}"));
                    }
                }
            }
        }
        if super::settings_button(ui, palette, "Cancel").clicked() {
            win.module_editor.creating_kind = None;
            win.module_editor.create_error = None;
        }
    });
    if let Some(error) = &win.module_editor.create_error {
        ui.label(RichText::new(error).color(palette.destructive).size(11.0));
    }
    created
}

fn module_template(kind: ModuleKind, name: &str) -> String {
    if kind == ModuleKind::Session {
        return format!(
            "--!strict\nlocal ui = bootty.ui\n\nreturn function()\n\treturn ui.session_components({{\n\t\tsessions = bootty.sessions(),\n\t\trender = function(ctx, _)\n\t\t\treturn {{\n\t\t\t\tdetails = {{ {{ key = \"{name}\", label = ctx.session.name }} }},\n\t\t\t}}\n\t\tend,\n\t}})\nend\n"
        );
    }
    format!(
        "--!strict\nreturn {{\n\trender = function()\n\t\treturn {{ {{ text = \"{name}\" }} }}\n\tend,\n}}\n"
    )
}

fn module_list(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    kind: ModuleKind,
    available: &[String],
    selected: &mut Option<(ModuleKind, String)>,
) {
    let palette = win.palette;
    let mut modules = match kind {
        ModuleKind::Sidebar => win.config.sidebar.modules.clone(),
        ModuleKind::Session => win.config.sidebar.session_modules.clone(),
        ModuleKind::Status => return,
    };
    let mut remove = None;
    let reorder = super::reorderable_list(
        ui,
        palette,
        module_dir_name(kind),
        modules.len(),
        |ui, index, handle| {
            let name = &modules[index];
            let active = selected.as_ref() == Some(&(kind, name.clone()));
            let label = module_label(name);
            let response = module_selector_row(ui, palette, &label, active, Some(handle), |ui| {
                if super::settings_icon_button(ui, palette, "x", "Disable module").clicked() {
                    remove = Some(index);
                }
            });
            if response.clicked() {
                *selected = Some((kind, name.clone()));
            }
        },
    );

    let mut changed = false;
    if let Some((from, slot)) = reorder {
        super::apply_reorder(&mut modules, from, slot);
        changed = true;
    }
    if let Some(index) = remove {
        modules.remove(index);
        changed = true;
    }

    let inactive = available
        .iter()
        .filter(|name| !modules.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    for name in &inactive {
        let active = selected.as_ref() == Some(&(kind, name.clone()));
        let mut enable = false;
        let label = module_label(name);
        let response = module_selector_row(ui, palette, &label, active, None, |ui| {
            if super::settings_icon_button(ui, palette, "plus", "Enable module").clicked() {
                enable = true;
            }
        });
        if response.clicked() {
            *selected = Some((kind, name.clone()));
        }
        if enable {
            modules.push(name.clone());
            *selected = Some((kind, name.clone()));
            changed = true;
        }
    }

    if changed {
        match kind {
            ModuleKind::Sidebar => {
                win.config.sidebar.modules = modules;
                win.set_sidebar_modules();
            }
            ModuleKind::Session => {
                win.config.sidebar.session_modules = modules;
                win.set_session_modules();
            }
            ModuleKind::Status => {}
        }
    }
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
        crate::ui::icons::paint_icon_slug(
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
    kind: ModuleKind,
    name: &str,
    items: &[crate::extensions::ModuleItem],
) {
    ui.label(
        RichText::new("PREVIEW · EXAMPLE DATA")
            .color(palette.muted)
            .strong()
            .size(11.0),
    );
    ui.add_space(4.0);
    if kind == ModuleKind::Status {
        let segment = crate::ui::chrome::ResolvedSegment {
            align: crate::config::SegmentAlign::Left,
            source_slot: 0,
            items: items
                .iter()
                .map(|item| crate::ui::chrome::ResolvedItem {
                    text: item.text.clone(),
                    icon: item.icon.clone(),
                    fg: item.fg,
                    bg: item.bg,
                    stroke: item.stroke,
                    gauge: item.gauge,
                    primitives: item.primitives.clone(),
                    pad_left: item.pad_left,
                    pad_right: item.pad_right,
                    join: item.join,
                    gap: item.gap,
                    action: None,
                    reorder_anchor: None,
                    module: name.to_owned(),
                })
                .collect(),
        };
        egui::Frame::NONE
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(egui::CornerRadius::same(palette.radius))
            .show(ui, |ui| {
                ui.set_height(38.0);
                ui.add_enabled_ui(false, |ui| {
                    ui.style_mut().visuals.disabled_alpha = 1.0;
                    crate::ui::chrome::show_status_bar(
                        ui,
                        palette,
                        crate::ui::chrome::StatusBarModel {
                            segments: std::slice::from_ref(&segment),
                            tab_context: None,
                            background: palette.mantle,
                            left_padding: 8.0,
                            row_height: 38.0,
                            notch_x: None,
                            tab_rows: 1,
                            interaction_id: "settings-module-preview",
                        },
                    );
                });
            });
    } else {
        let body = items
            .iter()
            .filter(|item| item.kind.as_deref() != Some("footer"))
            .cloned()
            .collect::<Vec<_>>();
        let footer = items
            .iter()
            .filter(|item| item.kind.as_deref() == Some("footer"))
            .cloned()
            .collect::<Vec<_>>();
        let scope = crate::mux::controller::MuxScope::new(
            crate::mux::controller::SpaceId::from_persistence(0),
            crate::mux::controller::BindingId::from_persistence(0),
        );
        let sidebar_items = crate::ui::sidebar::build_sidebar_items_from_module_items(
            &body,
            scope,
            Some("$1"),
            false,
        );
        let session_count = body
            .iter()
            .filter(|item| item.kind.as_deref() == Some("session"))
            .count();
        let width = ui.available_width().min(286.0);
        let height = 190.0;
        ui.allocate_ui_with_layout(
            egui::vec2(width, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Frame::NONE.fill(palette.mantle).show(ui, |ui| {
                    ui.set_width(width);
                    ui.set_height(height);
                    ui.add_enabled_ui(false, |ui| {
                        ui.style_mut().visuals.disabled_alpha = 1.0;
                        crate::ui::chrome::show_sidebar(
                            ui,
                            palette,
                            height,
                            crate::ui::chrome::SidebarModel {
                                items: &sidebar_items,
                                footer_items: &footer,
                                session_count,
                                has_sessions: session_count > 0,
                                title_visible: false,
                                reserve_titlebar_buttons: false,
                                title_icon: None,
                                top_inset: 0.0,
                                border_visible: true,
                                border_bottom: true,
                                separator_visible: true,
                                focused: false,
                                hovered_session: None,
                                unfocused_dim: 1.0,
                                fullscreen: false,
                                hover_override: None,
                                current_override: None,
                                border_override: None,
                            },
                        );
                    });
                });
            },
        );
    }
    ui.add_space(10.0);
}

fn module_toolbar(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    state: &mut EditorState,
    module_dir: &std::path::Path,
    kind: ModuleKind,
    name: &str,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(module_label(name))
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
                RichText::new(crate::strings::display_path(&state.path.to_string_lossy()))
                    .color(palette.muted)
                    .monospace()
                    .size(11.0),
            );
            if state.customized
                && state.has_builtin
                && super::settings_button(ui, palette, "Reset to default").clicked()
            {
                match crate::extensions::reset_module(module_dir, name) {
                    Ok(()) => load_editor(
                        state,
                        module_dir,
                        kind,
                        name,
                        format!("{}:{name}", module_dir_name(kind)),
                    ),
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
    module_dir: &std::path::Path,
    kind: ModuleKind,
    name: &str,
) {
    let editor_height = editor_viewport_height(ui.available_height());
    let syntax = luau_syntax();
    let completer = state.completer.get_or_insert_with(|| {
        let mut completer = Completer::new_with_syntax(&syntax)
            .with_auto_indent()
            .with_user_words();
        for word in LUAU_COMPLETIONS {
            completer.push_word(word);
        }
        completer
    });
    let editor_focused = completer
        .text_edit_id
        .is_some_and(|id| ui.memory(|memory| memory.has_focus(id)));
    let toggle_comments =
        editor_focused && ui.input_mut(|input| take_comment_shortcut(&mut input.events));
    let theme = if is_light_palette(palette) {
        ColorTheme::GITHUB_LIGHT
    } else {
        ColorTheme::GITHUB_DARK
    };
    let id = format!("module_editor_{}_{}", module_dir_name(kind), name);

    let editor = egui::Frame::NONE
        .fill(palette.base)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_height(editor_height);
            code_text_edit(ui, &id, &mut state.source, &syntax, &theme, completer)
        });
    let mut output = editor.inner;

    if toggle_comments && let Some(range) = output.state.cursor.char_range() {
        let range = toggle_luau_comments(&mut state.source, range);
        output.state.cursor.set_char_range(Some(range));
        output.state.store(ui.ctx(), output.response.id);
        output.response.mark_changed();
    }
    if ui.rect_contains_pointer(editor.response.rect) {
        ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
    }

    if output.response.changed() {
        match crate::extensions::save_module(module_dir, kind, name, &state.source) {
            Ok(path) => {
                state.path = path;
                state.customized = true;
                state.error = None;
            }
            Err(error) => state.error = Some(format!("Save failed: {error}")),
        }
    }
}

fn code_text_edit(
    ui: &mut egui::Ui,
    id: &str,
    source: &mut String,
    syntax: &Syntax,
    theme: &ColorTheme,
    completer: &mut Completer,
) -> egui::text_edit::TextEditOutput {
    const FONT_SIZE: f32 = 12.5;

    completer.handle_input(ui.ctx());
    let mut numbers = editor_line_numbers(source);
    let rows = numbers.lines().count().max(1);
    let formatter = CodeEditor::default()
        .with_fontsize(FONT_SIZE)
        .with_theme(*theme);
    let mut output = None;

    egui::ScrollArea::vertical()
        .id_salt(format!("{id}_vertical"))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            theme.modify_style(ui, FONT_SIZE);
            ui.horizontal_top(|ui| {
                let mut number_layouter =
                    |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
                        let job = egui::text::LayoutJob::single_section(
                            text.as_str().to_owned(),
                            egui::TextFormat::simple(
                                egui::FontId::monospace(FONT_SIZE),
                                theme.type_color(TokenType::Comment(true)),
                            ),
                        );
                        ui.fonts_mut(|fonts| fonts.layout_job(job))
                    };
                ui.add(
                    egui::TextEdit::multiline(&mut numbers)
                        .id_salt(format!("{id}_numbers"))
                        .interactive(false)
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::ZERO)
                        .desired_rows(rows)
                        .desired_width((rows.to_string().len() as f32 * FONT_SIZE * 0.6).max(8.0))
                        .layouter(&mut number_layouter),
                );

                egui::ScrollArea::horizontal()
                    .id_salt(format!("{id}_horizontal"))
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let mut layouter =
                            |ui: &egui::Ui, text: &dyn egui::TextBuffer, _wrap_width: f32| {
                                let mut job = egui::text::LayoutJob::default();
                                for token in Token::default().tokens(syntax, text.as_str()) {
                                    formatter.append(&mut job, &token);
                                }
                                ui.fonts_mut(|fonts| fonts.layout_job(job))
                            };
                        output = Some(
                            egui::TextEdit::multiline(source)
                                .id_salt(id)
                                .lock_focus(true)
                                .frame(egui::Frame::NONE)
                                .margin(egui::Margin::ZERO)
                                .desired_rows(rows)
                                .desired_width(ui.available_width())
                                .layouter(&mut layouter)
                                .show(ui),
                        );
                    });
            });
        });

    let mut output = output.expect("code text edit should render");
    completer.show(syntax, theme, FONT_SIZE, &mut output);
    output
}

const LUAU_COMPLETIONS: &[&str] = &[
    "bootty", "render", "interval", "text", "icon", "fg", "bg", "action", "gauge", "progress",
    "session", "sessions", "windows", "metrics", "theme", "sidebar", "visible", "run", "json",
    "decode", "shell", "path", "ui",
];

fn luau_syntax() -> Syntax {
    let mut syntax = Syntax::lua();
    syntax
        .patch
        .keywords
        .extend(["continue", "export", "type"].map(str::to_owned));
    syntax
}

fn is_light_palette(palette: bootty_ui::ThemePalette) -> bool {
    u16::from(palette.base.r()) + u16::from(palette.base.g()) + u16::from(palette.base.b()) > 384
}

fn take_comment_shortcut(events: &mut Vec<egui::Event>) -> bool {
    let Some(index) = events.iter().position(is_comment_shortcut) else {
        return false;
    };
    events.remove(index);
    true
}

fn is_comment_shortcut(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::Slash | egui::Key::Questionmark,
            physical_key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } if modifiers.matches_logically(egui::Modifiers::COMMAND)
            || (physical_key == &Some(egui::Key::Slash)
                && modifiers.matches_logically(egui::Modifiers::COMMAND))
    )
}

fn toggle_luau_comments(
    source: &mut String,
    range: egui::text::CCursorRange,
) -> egui::text::CCursorRange {
    let selected = range.as_sorted_char_range();
    let start_char: usize = selected.start.into();
    let end_char: usize = selected.end.into();
    let start_byte = char_to_byte(source, start_char);
    let end_byte = char_to_byte(source, end_char);
    let line_start = source[..start_byte]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let end_probe = if end_byte > line_start && source[..end_byte].ends_with('\n') {
        end_byte - 1
    } else {
        end_byte
    };
    let line_end = source[end_probe..]
        .find('\n')
        .map_or(source.len(), |index| end_probe + index);
    let block = &source[line_start..line_end];
    let uncomment = block
        .split('\n')
        .filter(|line| !line.trim().is_empty())
        .all(|line| line.trim_start().starts_with("--"));
    let replacement = block
        .split('\n')
        .map(|line| toggle_luau_line(line, uncomment))
        .collect::<Vec<_>>()
        .join("\n");
    let cursor_was_empty = range.is_empty();
    let old_cursor_in_line = source[line_start..start_byte].chars().count();
    let old_indent = block
        .chars()
        .take_while(|c| matches!(c, ' ' | '\t'))
        .count();
    let removed = if uncomment {
        block[old_indent..]
            .strip_prefix("--")
            .map_or(0, |tail| 2 + usize::from(tail.starts_with(' ')))
    } else {
        0
    };
    source.replace_range(line_start..line_end, &replacement);
    let base = source[..line_start].chars().count();
    if cursor_was_empty {
        let cursor = if uncomment {
            old_cursor_in_line.saturating_sub(
                removed.min(old_cursor_in_line - old_indent.min(old_cursor_in_line)),
            )
        } else if old_cursor_in_line >= old_indent {
            old_cursor_in_line + 3
        } else {
            old_cursor_in_line
        };
        egui::text::CCursorRange::one(egui::text::CCursor::new(base + cursor))
    } else {
        egui::text::CCursorRange::two(
            egui::text::CCursor::new(base),
            egui::text::CCursor::new(base + replacement.chars().count()),
        )
    }
}

fn toggle_luau_line(line: &str, uncomment: bool) -> String {
    if line.trim().is_empty() {
        return line.to_owned();
    }
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, content) = line.split_at(indent_len);
    if uncomment {
        let content = content.strip_prefix("--").unwrap_or(content);
        format!("{indent}{}", content.strip_prefix(' ').unwrap_or(content))
    } else {
        format!("{indent}-- {content}")
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}
fn editor_line_numbers(source: &str) -> String {
    let lines = source.lines().count() + usize::from(source.ends_with('\n'));
    (1..=lines.max(1))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn editor_viewport_height(available_height: f32) -> f32 {
    available_height.max(640.0)
}

fn displayed_source(source: &str) -> String {
    source.strip_suffix('\n').unwrap_or(source).to_owned()
}

fn load_editor(
    state: &mut EditorState,
    dir: &std::path::Path,
    kind: ModuleKind,
    name: &str,
    key: String,
) {
    state.completer = None;
    match crate::extensions::module_source(dir, kind, name) {
        Some(module) => {
            state.loaded_key = Some(key);
            state.source = displayed_source(&module.source);
            state.path = module.path;
            state.customized = module.customized;
            state.has_builtin = module.has_builtin;
            state.error = None;
        }
        None => {
            state.loaded_key = Some(key);
            state.source = displayed_source(&module_template(kind, name));
            state.path = dir.join(format!("{name}.luau"));
            state.customized = false;
            state.has_builtin = false;
            state.error = Some("Module file does not exist; editing creates it.".to_owned());
        }
    }
}

fn module_dir_name(kind: ModuleKind) -> &'static str {
    match kind {
        ModuleKind::Sidebar => "sidebar",
        ModuleKind::Session => "session",
        ModuleKind::Status => "status",
    }
}

fn module_label(name: &str) -> String {
    match name {
        "sessions" => "Sessions".to_owned(),
        "codexbar" => "Usage footer".to_owned(),
        "diffs" => "Diffs".to_owned(),
        "process" => "Process".to_owned(),
        "agent" => "Agent".to_owned(),
        "directory" => "Directory".to_owned(),
        "branch" => "Git branch".to_owned(),
        "ports" => "Ports".to_owned(),
        "progress" => "Progress".to_owned(),
        "session" => "Session".to_owned(),
        "windows" => "Windows".to_owned(),
        "sysinfo" => "System info".to_owned(),
        "clock" => "Clock".to_owned(),
        other => other.replace(['-', '_'], " "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_selector_stays_narrow_enough_for_a_side_by_side_editor() {
        assert_eq!(module_selector_width(600.0), 210.0);
        assert_eq!(module_selector_width(1_000.0), 250.0);
        assert_eq!(module_selector_width(2_000.0), 280.0);
    }

    #[test]
    fn editor_numbers_only_existing_source_lines() {
        assert_eq!(editor_line_numbers("first\nsecond"), "1\n2");
        assert_eq!(editor_line_numbers("first\n"), "1\n2");
    }

    #[test]
    fn editor_uses_the_available_page_height() {
        assert_eq!(editor_viewport_height(720.0), 720.0);
    }

    #[test]
    fn editor_hides_only_the_file_terminating_newline() {
        assert_eq!(displayed_source("return {}\n"), "return {}");
        assert_eq!(displayed_source("return {}\n\n"), "return {}\n");
    }

    #[test]
    fn luau_syntax_includes_luau_keywords() {
        assert!(luau_syntax().is_keyword("continue"));
        assert!(luau_syntax().is_keyword("export"));
        assert!(luau_syntax().is_keyword("type"));
    }

    #[test]
    fn comment_shortcut_accepts_logical_questionmark_and_physical_slash() {
        let event = egui::Event::Key {
            key: egui::Key::Questionmark,
            physical_key: Some(egui::Key::Slash),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };

        assert!(is_comment_shortcut(&event));
    }

    #[test]
    fn comment_toggle_handles_the_current_line() {
        let mut source = "local value = 1".to_owned();
        let cursor = egui::text::CCursorRange::one(egui::text::CCursor::new(6));

        let cursor = toggle_luau_comments(&mut source, cursor);
        assert_eq!(source, "-- local value = 1");
        assert_eq!(usize::from(cursor.primary.index), 9);

        let cursor = toggle_luau_comments(&mut source, cursor);
        assert_eq!(source, "local value = 1");
        assert_eq!(usize::from(cursor.primary.index), 6);
    }

    #[test]
    fn comment_toggle_handles_selected_lines() {
        let original = "  local a\n  local b\nnext";
        let mut source = original.to_owned();
        let selection = egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(19),
        );

        let selection = toggle_luau_comments(&mut source, selection);
        assert_eq!(source, "  -- local a\n  -- local b\nnext");

        toggle_luau_comments(&mut source, selection);
        assert_eq!(source, original);
    }
}
