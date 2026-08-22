//! A source editor for Lua-family text: syntax highlighting, a line-number gutter, a completion
//! popup, and the `Cmd+/` comment toggle, inside its own scroll viewport.
//!
//! Product-free. The caller names the dialect's extra keywords and the words its completer should
//! offer; nothing here knows what the source is for.

use eframe::egui;
use egui_code_editor::{CodeEditor, ColorTheme, Completer, Editor as _, Syntax, Token, TokenType};

use crate::ThemePalette;

const FONT_SIZE: f32 = 12.5;
/// Line-comment token of the Lua family, used by the toggle chord.
const COMMENT: &str = "--";

/// What one editor instance highlights and completes.
#[derive(Clone, Copy)]
pub struct CodeEditorSpec<'a> {
    /// Distinguishes this editor from every other one; changing it starts a fresh completer.
    pub id_salt: &'a str,
    /// Dialect keywords to add to the Lua base syntax.
    pub keywords: &'a [&'a str],
    /// Words the completer offers beyond the ones the syntax already knows.
    pub completions: &'a [&'a str],
    /// Floor for the scroll viewport, so a short file still gets a usable editing area.
    pub min_height: f32,
}

/// One frame of the editor.
pub struct CodeEditorOutput {
    /// The text changed this frame.
    pub changed: bool,
    /// The editor just lost keyboard focus — the point at which a caller should persist, rather
    /// than on every keystroke.
    pub lost_focus: bool,
}

/// Paints the editor over `source`.
pub fn code_editor(
    ui: &mut egui::Ui,
    palette: ThemePalette,
    spec: CodeEditorSpec<'_>,
    source: &mut String,
) -> CodeEditorOutput {
    let syntax = dialect_syntax(spec.keywords);
    // The completer carries the popup's selection and the file's own words across frames. Parking
    // it in egui memory under `id_salt` keeps it out of the caller's state and gives a different
    // editor — or the same one on a different file — a fresh one for free.
    let completer_id = egui::Id::new(("bootty::code_editor::completer", spec.id_salt));
    let mut completer = ui
        .data(|data| data.get_temp::<Completer>(completer_id))
        .unwrap_or_else(|| new_completer(&syntax, spec.completions));

    // Only the focused editor claims the chord, and it must be taken before the text edit sees it.
    let focused = completer
        .text_edit_id
        .is_some_and(|id| ui.memory(|memory| memory.has_focus(id)));
    let toggle_comments = focused && ui.input_mut(|input| take_comment_shortcut(&mut input.events));
    let theme = if is_light_palette(palette) {
        ColorTheme::GITHUB_LIGHT
    } else {
        ColorTheme::GITHUB_DARK
    };

    let editor = egui::Frame::NONE
        .fill(palette.base)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_height(ui.available_height().max(spec.min_height));
            code_text_edit(ui, spec.id_salt, source, &syntax, &theme, &mut completer)
        });
    let mut output = editor.inner;

    if toggle_comments && let Some(range) = output.state.cursor.char_range() {
        let range = toggle_comments_in(source, range);
        output.state.cursor.set_char_range(Some(range));
        output.state.store(ui.ctx(), output.response.id);
        output.response.mark_changed();
    }
    // The editor owns the wheel while the pointer is over it, so the page beneath cannot scroll
    // out from under the line the user is reading.
    if ui.rect_contains_pointer(editor.response.rect) {
        ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
    }
    ui.data_mut(|data| data.insert_temp(completer_id, completer));
    CodeEditorOutput {
        changed: output.response.changed(),
        lost_focus: output.response.lost_focus(),
    }
}

fn new_completer(syntax: &Syntax, completions: &[&str]) -> Completer {
    let mut completer = Completer::new_with_syntax(syntax)
        .with_auto_indent()
        .with_user_words();
    for word in completions {
        completer.push_word(word);
    }
    completer
}

/// The text edit itself: a non-interactive number gutter beside the source, both inside one
/// vertical viewport, with the source additionally scrolling horizontally so a long line does not
/// force the gutter off-screen.
fn code_text_edit(
    ui: &mut egui::Ui,
    id: &str,
    source: &mut String,
    syntax: &Syntax,
    theme: &ColorTheme,
    completer: &mut Completer,
) -> egui::text_edit::TextEditOutput {
    completer.handle_input(ui.ctx());
    let mut numbers = line_numbers(source);
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

/// The base syntax with a dialect's extra keywords folded in.
pub fn dialect_syntax(keywords: &[&str]) -> Syntax {
    let mut syntax = Syntax::lua();
    syntax
        .patch
        .keywords
        .extend(keywords.iter().copied().map(str::to_owned));
    syntax
}

fn is_light_palette(palette: ThemePalette) -> bool {
    u16::from(palette.base.r()) + u16::from(palette.base.g()) + u16::from(palette.base.b()) > 384
}

fn take_comment_shortcut(events: &mut Vec<egui::Event>) -> bool {
    let Some(index) = events.iter().position(is_comment_shortcut) else {
        return false;
    };
    events.remove(index);
    true
}

/// `Cmd+/` — matched on the logical key (which a layout may report as `?`) or on the physical slash.
/// Whether an event is the comment-toggle shortcut, by logical key or physical slash.
pub fn is_comment_shortcut(event: &egui::Event) -> bool {
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

/// Comment or uncomment every line the cursor or selection touches, preserving indentation. The
/// block is uncommented only when all of its non-blank lines are already comments.
/// Comments or uncomments the lines the cursor or selection covers, returning the new range.
pub fn toggle_comments_in(
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
        .all(|line| line.trim_start().starts_with(COMMENT));
    let replacement = block
        .split('\n')
        .map(|line| toggle_comment_line(line, uncomment))
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
            .strip_prefix(COMMENT)
            .map_or(0, |tail| COMMENT.len() + usize::from(tail.starts_with(' ')))
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
            old_cursor_in_line + COMMENT.chars().count() + 1
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

fn toggle_comment_line(line: &str, uncomment: bool) -> String {
    if line.trim().is_empty() {
        return line.to_owned();
    }
    let indent_len = line.len() - line.trim_start_matches([' ', '\t']).len();
    let (indent, content) = line.split_at(indent_len);
    if uncomment {
        let content = content.strip_prefix(COMMENT).unwrap_or(content);
        format!("{indent}{}", content.strip_prefix(' ').unwrap_or(content))
    } else {
        format!("{indent}{COMMENT} {content}")
    }
}

fn char_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices()
        .nth(char_index)
        .map_or(text.len(), |(index, _)| index)
}

/// One number per line the source actually has, so the gutter never numbers past the text.
/// The gutter text for `source`: one number per line the editor can put a cursor on.
pub fn line_numbers(source: &str) -> String {
    let lines = source.lines().count() + usize::from(source.ends_with('\n'));
    (1..=lines.max(1))
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
