use std::path::PathBuf;

use eframe::egui::{self, RichText};
use egui_code_editor::{CodeEditor, ColorTheme, Completer, Syntax};

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
}

pub(super) fn sidebar_ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let Some(config_dir) = win.config_path.parent().map(std::path::Path::to_path_buf) else {
        return;
    };
    let module_dir = config_dir.join(module_dir_name(ModuleKind::Sidebar));
    let mut names = crate::extensions::module_names(&module_dir, ModuleKind::Sidebar);
    for name in &win.config.sidebar.modules {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort();

    let mut selected = win
        .module_editor
        .selected_kind
        .zip(win.module_editor.selected_name.clone())
        .filter(|(kind, name)| *kind == ModuleKind::Sidebar && names.contains(name));

    super::section(ui, palette, "MODULES");
    sidebar_module_list(ui, win, &names, &mut selected);
    if let Some(name) = new_module_ui(win, ui, ModuleKind::Sidebar) {
        if !win.config.sidebar.modules.contains(&name) {
            win.config.sidebar.modules.push(name.clone());
            win.set_sidebar_modules();
        }
        if !names.contains(&name) {
            names.push(name.clone());
            names.sort();
        }
        selected = Some((ModuleKind::Sidebar, name));
    }

    if selected.is_none() {
        selected = win
            .config
            .sidebar
            .modules
            .first()
            .or_else(|| names.first())
            .map(|name| (ModuleKind::Sidebar, name.clone()));
    }
    let Some((_, name)) = selected else {
        super::settings_notice(ui, palette.muted, "No sidebar modules found.");
        return;
    };
    ui.add_space(12.0);
    source_editor(win, ui, ModuleKind::Sidebar, &name);
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
        load_editor(&mut win.module_editor, &module_dir, kind, name, key);
    }
    win.module_editor.selected_kind = Some(kind);
    win.module_editor.selected_name = Some(name.to_owned());

    let state = &mut win.module_editor;
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
            ui.add_space(10.0);
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
                match crate::extensions::save_module(&dir, kind, &name, &module_template(&name)) {
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

fn module_template(name: &str) -> String {
    format!(
        "--!strict\nreturn {{\n\trender = function()\n\t\treturn {{ {{ text = \"{name}\" }} }}\n\tend,\n}}\n"
    )
}

fn sidebar_module_list(
    ui: &mut egui::Ui,
    win: &mut SettingsWindow,
    available: &[String],
    selected: &mut Option<(ModuleKind, String)>,
) {
    let palette = win.palette;
    let modules = win.config.sidebar.modules.clone();
    let mut remove = None;
    let reorder = super::reorderable_list(
        ui,
        palette,
        "sidebar_modules",
        modules.len(),
        |ui, index, handle| {
            let name = &modules[index];
            let active = selected.as_ref() == Some(&(ModuleKind::Sidebar, name.clone()));
            let response = module_row(ui, palette, name, active, Some(handle), |ui| {
                if super::settings_icon_button(ui, palette, "x", "Disable module").clicked() {
                    remove = Some(index);
                }
            });
            if response.clicked() {
                *selected = Some((ModuleKind::Sidebar, name.clone()));
            }
        },
    );

    let mut changed = false;
    if let Some((from, slot)) = reorder {
        super::apply_reorder(&mut win.config.sidebar.modules, from, slot);
        changed = true;
    }
    if let Some(index) = remove {
        win.config.sidebar.modules.remove(index);
        changed = true;
    }

    let inactive = available
        .iter()
        .filter(|name| !win.config.sidebar.modules.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    for name in &inactive {
        let active = selected.as_ref() == Some(&(ModuleKind::Sidebar, name.clone()));
        let mut enable = false;
        let response = module_row(ui, palette, name, active, None, |ui| {
            if super::settings_icon_button(ui, palette, "plus", "Enable module").clicked() {
                enable = true;
            }
        });
        if response.clicked() {
            *selected = Some((ModuleKind::Sidebar, name.clone()));
        }
        if enable {
            win.config.sidebar.modules.push(name.clone());
            *selected = Some((ModuleKind::Sidebar, name.clone()));
            changed = true;
        }
    }

    if changed {
        win.set_sidebar_modules();
    }
}

fn module_row(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    name: &str,
    selected: bool,
    handle: Option<&super::DragHandle>,
    trailing: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 48.0),
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
    let content_rect = rect.shrink2(egui::Vec2::new(12.0, 7.0));
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    content.add_space(22.0);
    content.label(
        RichText::new(module_label(name))
            .color(palette.text)
            .strong(),
    );
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
    ui.add_space(8.0);
    response
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
    let rows = (editor_height / 16.0).floor().max(1.0) as usize;
    let id = format!("module_editor_{}_{}", module_dir_name(kind), name);

    let editor = egui::Frame::NONE
        .fill(palette.base)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .show(ui, |ui| {
            ui.set_min_height(editor_height);
            ui.set_max_height(editor_height);
            CodeEditor::default()
                .id_source(id)
                .with_rows(rows)
                .with_fontsize(12.5)
                .with_theme(theme)
                .with_numlines(true)
                .with_clickable_links(false)
                .desired_width(ui.available_width())
                .vscroll(true)
                .show_with_completer(ui, &mut state.source, &syntax, completer)
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
            state.source =
                "--!strict\nreturn {\n\trender = function()\n\t\treturn {}\n\tend,\n}".to_owned();
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
        ModuleKind::Status => "status",
    }
}

fn module_label(name: &str) -> String {
    match name {
        "sessions" => "Sessions".to_owned(),
        "codexbar" => "Usage footer".to_owned(),
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
