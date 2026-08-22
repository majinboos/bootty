use bootty_ui::keycaps;
use bootty_ui::settings::{ComboStyle, IconButtonState, described_combo};
use eframe::egui;

mod model;
mod trigger_edit;

use super::SettingsSurface;
use bootty_config::config::KeybindPreset;
use bootty_winit::direct_input::ModifierSideState;
pub(super) use model::{BindingRow, ChordCapture, KeybindScope};
use model::{action_options, effective_bindings, read_scope_entries, write_scope};
use trigger_edit::{
    MODIFIER_TOKENS, TRIGGER_FLAGS, add_default_modifier_sides, captured_step, join_trigger_flags,
    parse_trigger_flags, prefix_combo, scroll_step, strip_modifier_sides, trigger_step,
    unprefix_combo,
};

/// Seconds to wait for the next chord step before committing the captured trigger.
const CHORD_TIMEOUT: f64 = 0.8;

/// Editor state for the Keys pane. Accepted bindings live in the config; these are the drafts,
/// which may be incomplete and must survive an accepted rebind.
#[derive(Default)]
pub(super) struct EditorState {
    /// Which keybind list is being edited (global, or one of the per-backend lists).
    scope: KeybindScope,
    /// The user layer on top of the built-in defaults, for `loaded_scope`.
    rows: Option<Vec<BindingRow>>,
    /// Whether the loaded scope drops the built-in defaults (the `clear` sentinel).
    clear: bool,
    /// Rows may be incomplete while edited; persistence happens only at explicit boundaries.
    dirty: bool,
    /// The scope `rows`/`clear` were loaded for; reloaded when the scope changes.
    loaded_scope: Option<KeybindScope>,
    capture: Option<ChordCapture>,
    /// Whether the preset-prefix recorder is capturing (one combo, commits on the first step).
    prefix_capture: bool,
    /// Modifier-remap rows (`from`, `to`); loaded lazily so incomplete rows persist.
    modifier_rows: Option<Vec<(String, String)>>,
    /// An action to focus (adding a row if absent) on the next frame, set by the palette's
    /// "configure this command's keybinding".
    pending_focus: Option<String>,
}

impl EditorState {
    /// Whether either recorder is armed. Read before the frame so the host routes direct input to
    /// the recorder instead of egui.
    pub(super) fn is_recording(&self) -> bool {
        self.capture.is_some() || self.prefix_capture
    }

    /// Whether a row's chord recorder is armed; Escape cancels that but not the prefix recorder.
    pub(super) fn capturing_chord(&self) -> bool {
        self.capture.is_some()
    }

    pub(super) fn cancel_capture(&mut self) {
        self.capture = None;
    }

    /// `commit_draft` clears `dirty` when it writes the rows into the draft document, before the
    /// owner has accepted anything. A refused write has to put it back, or Apply disappears while
    /// the failure notice is still on screen and the rows can never be retried.
    pub(super) fn rearm_after_rejected_submission(&mut self) {
        self.dirty = self.rows.is_some();
    }

    /// Drop the loaded rows so the next frame reloads them from the draft document.
    fn reload_scope(&mut self) {
        self.loaded_scope = None;
    }

    /// Jump to `scope` and focus `action`, adding a row for it if the scope has none.
    pub(super) fn focus_action(&mut self, scope: KeybindScope, action: Option<&str>) {
        self.scope = scope;
        self.reload_scope();
        self.cancel_capture();
        self.pending_focus = action.map(str::to_owned);
    }
}

pub(super) fn ui(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    let direct_chords = std::mem::take(&mut win.recorder_chords);

    shortcut_options(win, ui);
    preset_options(win, ui, &direct_chords);

    super::section(ui, palette, "KEYBINDINGS");
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Scope").color(palette.subtext));
        let mut scope = win.keybinds.scope;
        if !KeybindScope::ALL
            .iter()
            .any(|(candidate, _)| *candidate == scope)
        {
            scope = KeybindScope::Global;
        }
        let labels: Vec<&str> = KeybindScope::ALL.iter().map(|(_, label)| *label).collect();
        let current = KeybindScope::ALL
            .iter()
            .position(|(candidate, _)| *candidate == scope)
            .unwrap_or(0);
        if let Some(index) = super::settings_segmented_ltr(ui, palette, &labels, current) {
            scope = KeybindScope::ALL[index].0;
        }
        if scope != win.keybinds.scope {
            commit_draft(win);
            win.keybinds.rows = None;
            win.keybinds.reload_scope();
        }
        win.keybinds.scope = scope;
    });
    ui.add_space(8.0);
    let scope = win.keybinds.scope;

    if win.keybinds.loaded_scope != Some(scope) {
        let (clear, rows) = read_scope_entries(&win.writeback, &win.config.input, scope);
        win.keybinds.clear = clear;
        win.keybinds.rows = Some(rows);
        win.keybinds.loaded_scope = Some(scope);
        win.keybinds.cancel_capture();
    }

    let mut rows = win.keybinds.rows.take().unwrap_or_default();
    let mut clear = win.keybinds.clear;
    let mut capture = win.keybinds.capture.take();
    let mut changed = false;
    // Prefixed chords are idiomatic in the global and native/rmux scopes; the tmux backend
    // relays raw bytes and the sidebar has no chord support, so those record literally.
    let effective_prefix = scope.effective_prefix(&win.config.input);

    // "Configure this command's keybinding" (from the palette): surface the row for
    // the requested action — adding an empty one if absent — and filter the list to
    // it. Recording is left for the user to start; auto-starting it would capture
    // the very chord that opened this view (e.g. `cmd+shift+,`).
    if let Some(target) = win.keybinds.pending_focus.take() {
        if !rows.iter().any(|row| row.action.trim() == target.as_str()) {
            rows.push(BindingRow {
                action: target.clone(),
                ..BindingRow::default()
            });
        }
        let search_id = ui.make_persistent_id(("settings_keybind_search", scope));
        ui.memory_mut(|memory| memory.data.insert_temp(search_id, target));
    }

    if defaults_toggle(ui, palette, &mut clear) {
        changed = true;
    }

    let search_id = ui.make_persistent_id(("settings_keybind_search", scope));
    let mut search: String =
        ui.memory(|memory| memory.data.get_temp(search_id).unwrap_or_default());
    if super::settings_text_edit_width(ui, palette, &mut search, "Search keybindings", 280.0)
        .changed()
    {
        ui.memory_mut(|memory| memory.data.insert_temp(search_id, search.clone()));
    }
    let needle = search.trim().to_ascii_lowercase();

    handle_capture(
        ui,
        &mut capture,
        &mut rows,
        &mut changed,
        &direct_chords,
        win.recorder_modifier_sides,
        effective_prefix.as_deref(),
    );

    let (complete_count, invalid_count) = rows
        .iter()
        .filter_map(|row| row.validity(scope))
        .fold((0, 0), |(complete, invalid), valid| {
            (complete + 1, invalid + usize::from(!valid))
        });
    bootty_ui::settings::status_banner(
        ui,
        palette,
        invalid_count == 0,
        if invalid_count == 0 {
            "No conflicts"
        } else {
            "Needs attention"
        },
        &if invalid_count == 0 {
            format!(
                "{complete_count} complete binding rows; no conflicts or invalid actions detected."
            )
        } else {
            format!("{invalid_count} invalid binding rows need attention.")
        },
    );

    let mut remove: Option<usize> = None;
    let mut toggle_capture: Option<usize> = None;
    let action_options = action_options(scope);
    // Zero the inter-row spacing so the striped rows read as one continuous table.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (index, row) in rows.iter_mut().enumerate() {
            let haystack = format!("{} {}", row.trigger, row.action).to_ascii_lowercase();
            if !needle.is_empty() && !haystack.contains(&needle) {
                continue;
            }

            binding_editor_row(
                ui,
                palette,
                row,
                BindingEditorContext {
                    scope,
                    index,
                    action_options: &action_options,
                    prefix: effective_prefix.as_deref(),
                    capture: capture.as_ref(),
                    changed: &mut changed,
                    toggle_capture: &mut toggle_capture,
                    remove: &mut remove,
                },
            );
        }
    });

    ui.add_space(10.0);
    if super::settings_button(ui, palette, "+ Add binding").clicked() {
        rows.push(BindingRow::default());
        changed = true;
    }

    if let Some(index) = toggle_capture {
        win.keybinds.prefix_capture = false;
        capture = match capture {
            Some(cap) if cap.row == index => None,
            _ => Some(ChordCapture {
                row: index,
                steps: Vec::new(),
                deadline: None,
            }),
        };
    }
    if let Some(index) = remove {
        if index < rows.len() {
            rows.remove(index);
            changed = true;
        }
        capture = match capture {
            Some(cap) if cap.row == index => None,
            Some(cap) if cap.row > index => Some(ChordCapture {
                row: cap.row - 1,
                ..cap
            }),
            other => other,
        };
    }

    let apply = (win.keybinds.dirty || changed)
        && super::settings_button(ui, palette, "Apply keybindings").clicked();

    win.keybinds.clear = clear;
    win.keybinds.dirty |= changed;
    win.keybinds.rows = Some(rows);
    win.keybinds.capture = capture;
    if apply {
        commit_draft(win);
    }

    resolved_shortcuts_panel(win, ui, scope);
}

/// Everything actually in force for `scope`, as searchable keycap-and-title pairs. Raw
/// `trigger=action` lines are unreadable at this length — a few hundred entries with the built-in
/// defaults on.
fn resolved_shortcuts_panel(win: &SettingsSurface, ui: &mut egui::Ui, scope: KeybindScope) {
    let palette = win.palette;
    ui.add_space(10.0);
    egui::CollapsingHeader::new(
        egui::RichText::new("Resolved shortcuts")
            .color(palette.subtext)
            .size(12.0),
    )
    .default_open(false)
    .show(ui, |ui| {
        let search_id = ui.make_persistent_id(("settings_resolved_keybind_search", scope));
        let mut search: String =
            ui.memory(|memory| memory.data.get_temp(search_id).unwrap_or_default());
        if super::settings_text_edit_width(
            ui,
            palette,
            &mut search,
            "Search resolved shortcuts",
            320.0,
        )
        .changed()
        {
            ui.memory_mut(|memory| memory.data.insert_temp(search_id, search.clone()));
        }
        let needle = search.trim().to_ascii_lowercase();
        ui.add_space(8.0);
        // Rows render inline, with no nested scroll area: a nested scroll collapsed the panel to a
        // couple of rows instead of letting the page's own scroll take the overflow.
        egui::Frame::NONE
            .fill(palette.pane)
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(egui::CornerRadius::same(palette.radius))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let entries = resolved_entries(&win.config, scope, &needle);
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new("No matching shortcuts.")
                            .color(palette.muted)
                            .size(12.0),
                    );
                    return;
                }
                let cols = ((ui.available_width() / 280.0).floor() as usize).clamp(1, 6);
                // A Grid keeps columns aligned to their widest cell; packing each cell to its own
                // content width staggered the rows.
                egui::Grid::new(("resolved_shortcuts_grid", scope))
                    .num_columns(cols)
                    .spacing([28.0, 12.0])
                    .show(ui, |ui| {
                        for (index, (combo, title, tags)) in entries.iter().enumerate() {
                            ui.horizontal(|ui| {
                                keycaps::chip(ui, palette, combo);
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new(title).color(palette.subtext));
                                if !tags.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!("· {}", tags.join(" · ")))
                                            .color(palette.muted)
                                            .size(11.0),
                                    );
                                }
                            });
                            if (index + 1) % cols == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
    });
}

/// The resolved entries matching `needle`, as (combo, human title, active flag names).
fn resolved_entries(
    config: &bootty_config::config::BoottyConfig,
    scope: KeybindScope,
    needle: &str,
) -> Vec<(String, String, Vec<&'static str>)> {
    effective_bindings(config, scope)
        .iter()
        .filter_map(|entry| {
            let (trigger, action) = bootty_config::config::split_keybind_entry(entry)?;
            if !needle.is_empty()
                && !format!("{trigger} {action}")
                    .to_ascii_lowercase()
                    .contains(needle)
            {
                return None;
            }
            let (flags, combo) = parse_trigger_flags(trigger);
            let tags = TRIGGER_FLAGS
                .iter()
                .zip(flags)
                .filter(|(_, set)| *set)
                .map(|((name, _, _), _)| *name)
                .collect();
            Some((combo, action_title(action), tags))
        })
        .collect()
}

/// The label the command palette would show for `action`, so the same command reads the same in
/// both places. A trailing `:param` is kept.
pub fn action_title(action: &str) -> String {
    if let Some(command) = crate::action_catalog::Command::from_action(action) {
        return command.title().to_owned();
    }
    let (base, param) = match action.split_once(':') {
        Some((base, param)) => (base, Some(param)),
        None => (action, None),
    };
    let mut title = crate::action_catalog::Command::from_action(base)
        .map(|command| command.title().to_owned())
        .unwrap_or_else(|| humanize_action(base));
    if let Some(param) = param {
        title.push_str(": ");
        title.push_str(param);
    }
    title
}

/// Sentence-case a snake_case action the catalog does not carry (sidebar actions, `text`, `csi`).
pub fn humanize_action(name: &str) -> String {
    let mut title = String::with_capacity(name.len());
    for (index, word) in name.split('_').filter(|word| !word.is_empty()).enumerate() {
        if index > 0 {
            title.push(' ');
            title.push_str(word);
        } else {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                title.extend(first.to_uppercase());
                title.push_str(chars.as_str());
            }
        }
    }
    if title.is_empty() {
        name.to_owned()
    } else {
        title
    }
}

pub(super) fn commit_draft(win: &mut SettingsSurface) {
    if !win.keybinds.dirty {
        return;
    }
    let Some(rows) = win.keybinds.rows.take() else {
        return;
    };
    write_scope(
        &mut win.writeback,
        win.keybinds.scope,
        win.keybinds.clear,
        &rows,
    );
    win.keybinds.rows = Some(rows);
    win.keybinds.dirty = false;
}

/// Global input settings, laid out at the top of the page with the same row grammar as the rest of
/// settings so they line up with every other pane.
fn shortcut_options(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    super::section(ui, palette, "SHORTCUT OPTIONS");

    let mut hide_pointer = win.config.input.hide_mouse_pointer_while_typing;
    super::settings_row(
        ui,
        palette,
        "Hide pointer while typing",
        "Temporarily hide the mouse pointer while you type.",
        |ui| {
            if super::settings_toggle(ui, palette, &mut hide_pointer) {
                win.config.input.hide_mouse_pointer_while_typing = hide_pointer;
                win.writeback
                    .set_bool(&["input", "hide-mouse-pointer-while-typing"], hide_pointer);
            }
        },
    );

    let mut copy_on_select = win.config.input.copy_on_select;
    super::settings_row(
        ui,
        palette,
        "Copy on select",
        "Copy a completed terminal selection to the system clipboard.",
        |ui| {
            if super::settings_toggle(ui, palette, &mut copy_on_select) {
                win.config.input.copy_on_select = copy_on_select;
                win.writeback
                    .set_bool(&["input", "copy-on-select"], copy_on_select);
            }
        },
    );

    super::settings_row(
        ui,
        palette,
        "Option as Alt",
        "How macOS treats the Option key inside the terminal.",
        |ui| {
            let tokens = ["none", "left", "right", "both"];
            let current = match win.config.input.macos_option_as_alt {
                bootty_config::config::MacosOptionAsAltConfig::None => 0,
                bootty_config::config::MacosOptionAsAltConfig::Left => 1,
                bootty_config::config::MacosOptionAsAltConfig::Right => 2,
                bootty_config::config::MacosOptionAsAltConfig::Both => 3,
            };
            if let Some(index) = super::settings_segmented(ui, palette, &tokens, current) {
                win.config.input.macos_option_as_alt = match index {
                    0 => bootty_config::config::MacosOptionAsAltConfig::None,
                    1 => bootty_config::config::MacosOptionAsAltConfig::Left,
                    2 => bootty_config::config::MacosOptionAsAltConfig::Right,
                    _ => bootty_config::config::MacosOptionAsAltConfig::Both,
                };
                win.writeback
                    .set_str(&["input", "macos-option-as-alt"], tokens[index]);
            }
        },
    );

    super::section(ui, palette, "MODIFIER REMAPS");
    super::settings_notice(
        ui,
        palette.muted,
        "Rewrite one physical modifier to another before shortcuts are matched.",
    );
    ui.add_space(6.0);
    modifier_remaps(win, ui);
}

/// Preset picker + prefix recorder. Switching preset (or prefix) only swaps which built-in
/// defaults the user's override rows layer on top of; the rows themselves are never touched.
fn preset_options(win: &mut SettingsSurface, ui: &mut egui::Ui, direct_chords: &[String]) {
    let palette = win.palette;
    super::section(ui, palette, "PRESET");

    super::settings_row(
        ui,
        palette,
        "Keybinding preset",
        "Built-in defaults to start from. Your own binding rows stay unchanged.",
        |ui| {
            let labels: Vec<&str> = KeybindPreset::ALL
                .iter()
                .map(|preset| preset.label())
                .collect();
            let current = KeybindPreset::ALL
                .iter()
                .position(|preset| *preset == win.config.input.preset)
                .unwrap_or(0);
            if let Some(index) = super::settings_segmented(ui, palette, &labels, current) {
                let preset = KeybindPreset::ALL[index];
                win.writeback.set_str(&["input", "preset"], preset.as_str());
                // Re-resolve so the built-in default tables (and effective prefix) swap over.
                win.keybinds.reload_scope();
            }
        },
    );

    let preset = win.config.input.preset;
    if preset.default_prefix().is_none() {
        win.keybinds.prefix_capture = false;
        return;
    }
    let prefix = win.config.input.effective_prefix().unwrap_or_default();
    let recording = win.keybinds.prefix_capture;
    let mut toggle_recording = false;
    let mut reset_prefix = false;
    super::settings_row(
        ui,
        palette,
        "Prefix",
        "Leader combo for prefixed shortcuts: press it, then the bound key.",
        |ui| {
            let capture_text = "Press a combo… Esc to cancel";
            if keycaps::record_cell(ui, palette, &prefix, recording, capture_text).clicked()
                || keycaps::record_dot(ui, palette, recording).clicked()
            {
                toggle_recording = true;
            }
            if win.config.input.prefix.is_some()
                && super::settings_icon_button(ui, palette, "x", "Reset to the preset default")
                    .clicked()
            {
                reset_prefix = true;
            }
        },
    );
    if toggle_recording {
        win.keybinds.prefix_capture = !recording;
        win.keybinds.cancel_capture();
    }
    if reset_prefix {
        win.keybinds.prefix_capture = false;
        win.writeback.remove(&["input", "prefix"]);
        win.keybinds.reload_scope();
    }
    if win.keybinds.prefix_capture {
        handle_prefix_capture(win, ui, direct_chords);
    }
}

/// Single-combo capture for the prefix recorder: the first step commits immediately (a prefix is
/// one combo, never a chord). Escape cancels.
fn handle_prefix_capture(win: &mut SettingsSurface, ui: &egui::Ui, direct_chords: &[String]) {
    ui.ctx().request_repaint();
    let step = if let Some((key, modifiers)) = drain_first_key_press(ui) {
        if key == egui::Key::Escape {
            win.keybinds.prefix_capture = false;
            return;
        }
        trigger_step(key, modifiers)
    } else {
        direct_chords.first().map(|step| strip_modifier_sides(step))
    };
    let Some(step) = step else {
        return;
    };
    win.keybinds.prefix_capture = false;
    win.writeback.set_str(&["input", "prefix"], &step);
    // Re-resolve so the prefixed default chords rebuild against the new prefix.
    win.keybinds.reload_scope();
}

/// Per-scope toggle for whether Bootty's built-in shortcuts stay active. Stored as a `clear`
/// sentinel (drop defaults), so the visible switch is inverted: on means defaults are kept.
fn defaults_toggle(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, clear: &mut bool) -> bool {
    let mut changed = false;
    super::settings_row(
        ui,
        palette,
        "Use built-in defaults",
        "Keep Bootty's default shortcuts for this scope alongside your own.",
        |ui| {
            let mut use_defaults = !*clear;
            if super::settings_toggle(ui, palette, &mut use_defaults) {
                *clear = !use_defaults;
                changed = true;
            }
        },
    );
    changed
}

fn modifier_remaps(win: &mut SettingsSurface, ui: &mut egui::Ui) {
    let palette = win.palette;
    if win.keybinds.modifier_rows.is_none() {
        let rows = win
            .config
            .input
            .modifier_remap
            .iter()
            .map(|entry| match entry.split_once('=') {
                Some((from, to)) => (from.trim().to_owned(), to.trim().to_owned()),
                None => (entry.clone(), String::new()),
            })
            .collect();
        win.keybinds.modifier_rows = Some(rows);
    }
    let mut rows = win.keybinds.modifier_rows.take().unwrap_or_default();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, (from, to)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            changed |= modifier_combo(ui, palette, index, "from", from);
            changed |= modifier_combo(ui, palette, index, "to", to);
            if super::settings_icon_button(ui, palette, "x", "Remove remap").clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        rows.remove(index);
        changed = true;
    }
    ui.add_space(6.0);
    if super::settings_button(ui, palette, "+ Add remap").clicked() {
        rows.push((String::new(), String::new()));
        changed = true;
    }
    if changed {
        let entries: Vec<String> = rows
            .iter()
            .filter(|(from, to)| remap_is_valid(from, to))
            .map(|(from, to)| format!("{from}={to}"))
            .collect::<Vec<String>>();
        if entries.is_empty() {
            win.writeback.remove(&["input", "modifier-remap"]);
        } else {
            win.writeback
                .set_strings(&["input", "modifier-remap"], &entries);
        }
    }
    win.keybinds.modifier_rows = Some(rows);
}

fn modifier_combo(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    row: usize,
    side: &'static str,
    value: &mut String,
) -> bool {
    let selected = MODIFIER_TOKENS
        .iter()
        .position(|token| *token == value.as_str());
    let label = if value.is_empty() {
        side
    } else {
        value.as_str()
    };
    let Some(choice) = super::searchable_combo(
        ui,
        palette,
        &format!("mod_remap_{side}_{row}"),
        label,
        118.0,
        MODIFIER_TOKENS,
        selected,
    ) else {
        return false;
    };
    *value = MODIFIER_TOKENS[choice].to_owned();
    true
}

/// Shared control height for the trigger cell and the value field so they line up exactly.
const ROW_CONTROL_HEIGHT: f32 = keycaps::RECORD_CELL_HEIGHT;

fn binding_editor_row(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    row: &mut BindingRow,
    ctx: BindingEditorContext<'_>,
) {
    let recording = ctx.capture.is_some_and(|cap| cap.row == ctx.index);
    let (mut flags, combo) = parse_trigger_flags(&row.trigger);
    let flags_open_id = ui.make_persistent_id(("kb_flags_open", ctx.scope, ctx.index));
    let mut flags_open: bool =
        ui.memory(|memory| memory.data.get_temp(flags_open_id).unwrap_or(false));
    let any_flag = flags.iter().any(|on| *on) || row.side_sensitive;

    // No trailing space and alternating fills make the rows read as one continuous striped table.
    egui::Frame::NONE
        .fill(if ctx.index.is_multiple_of(2) {
            palette.pane
        } else {
            palette.surface
        })
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.x = 8.0;

                let capture_text = ctx
                    .capture
                    .filter(|cap| cap.row == ctx.index)
                    .map(|cap| {
                        if cap.steps.is_empty() {
                            "Press keys or scroll… Esc to cancel".to_owned()
                        } else {
                            cap.steps.join(">")
                        }
                    })
                    .unwrap_or_default();
                if let Some(prefix) = ctx.prefix {
                    let mut prefixed = row.prefixed;
                    // Center the checkbox against the taller record cell next to it.
                    let response = ui
                        .allocate_ui_with_layout(
                            egui::vec2(0.0, ROW_CONTROL_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.checkbox(
                                    &mut prefixed,
                                    egui::RichText::new("prefix")
                                        .color(palette.subtext)
                                        .size(11.0),
                                )
                            },
                        )
                        .inner
                        .on_hover_text(format!(
                            "Prefixed: recording captures one key and stores it as {prefix} > key"
                        ));
                    if response.changed() {
                        row.prefixed = prefixed;
                        if !combo.is_empty() {
                            let combo = if prefixed {
                                prefix_combo(&combo, prefix)
                            } else {
                                unprefix_combo(&combo, prefix)
                            };
                            row.trigger = join_trigger_flags(&flags, &combo);
                            *ctx.changed = true;
                        }
                    }
                }

                if keycaps::record_cell(ui, palette, &combo, recording, &capture_text).clicked()
                    || keycaps::record_dot(ui, palette, recording).clicked()
                {
                    *ctx.toggle_capture = Some(ctx.index);
                }

                if let Some(arrow) = bootty_ui::icons::icon_text("arrow-right", 14.0, palette.muted)
                {
                    ui.label(arrow);
                }

                // Title + description picker, drawn from the shared action catalog.
                let (base, params) = split_action_for_editor(&row.action, ctx.action_options);

                // Spread the action + value across the leftover width, reserving a right cluster for
                // the status, flags, and remove controls so the row uses its full width.
                // Wide enough for the "incomplete" status label plus the flags and remove buttons,
                // so the right-to-left cluster never overlaps the value field.
                let right_cluster = 200.0;
                let fields = (ui.available_width() - right_cluster).max(240.0);
                let action_width = (fields * 0.58 - 8.0).clamp(150.0, 320.0);
                let value_width = (fields - action_width - 8.0).clamp(90.0, 240.0);

                let mut chosen_action: &'static str = ctx
                    .action_options
                    .iter()
                    .find(|(name, _, _)| *name == base)
                    .map_or("", |(name, _, _)| *name);
                if described_combo(
                    ui,
                    palette,
                    &format!("kb_action_{}", ctx.index),
                    &mut chosen_action,
                    ctx.action_options,
                    ComboStyle {
                        width: action_width,
                        searchable: true,
                        placeholder: "action",
                    },
                ) {
                    row.action = if params.trim().is_empty() {
                        chosen_action.to_owned()
                    } else {
                        format!("{chosen_action}:{params}")
                    };
                    *ctx.changed = true;
                }

                let mut params_edit = params.clone();
                if super::settings_text_edit_width(
                    ui,
                    palette,
                    &mut params_edit,
                    "value",
                    value_width,
                )
                .changed()
                {
                    row.action = if params_edit.trim().is_empty() {
                        base.clone()
                    } else {
                        format!("{base}:{params_edit}")
                    };
                    *ctx.changed = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if super::settings_icon_button(ui, palette, "x", "Remove binding").clicked() {
                        *ctx.remove = Some(ctx.index);
                    }
                    let options_tip = if any_flag {
                        "Binding options (active)"
                    } else {
                        "Binding options"
                    };
                    if bootty_ui::settings::settings_icon_toggle(
                        ui,
                        palette,
                        "sliders-horizontal",
                        options_tip,
                        IconButtonState {
                            active: any_flag,
                            open: flags_open,
                        },
                    )
                    .clicked()
                    {
                        flags_open = !flags_open;
                        ui.memory_mut(|memory| memory.data.insert_temp(flags_open_id, flags_open));
                    }
                    ui.add_space(4.0);
                    binding_status(ui, palette, row, ctx.scope);
                });
            });

            if flags_open {
                binding_flags_editor(ui, palette, &mut flags, &combo, row, ctx.changed);
            }
        });
}

/// Inline expander with one toggle per trigger flag. Rewrites the row's trigger string from the
/// toggles so users never type `performable:` etc. by hand.
fn binding_flags_editor(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    flags: &mut [bool; 4],
    combo: &str,
    row: &mut BindingRow,
    changed: &mut bool,
) {
    // Inset from the striped row it expands out of, so the panel reads as belonging to that row.
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 4,
            bottom: 8,
        })
        .show(ui, |ui| {
            for (index, (_, label, help)) in TRIGGER_FLAGS.iter().enumerate() {
                if binding_option(ui, palette, &mut flags[index], label, help) {
                    row.trigger = join_trigger_flags(flags, combo);
                    *changed = true;
                }
                ui.add_space(2.0);
            }
            if binding_option(
                ui,
                palette,
                &mut row.side_sensitive,
                "Modifier side",
                "Require the same physical left/right modifier side that was recorded.",
            ) {
                let combo = if row.side_sensitive {
                    add_default_modifier_sides(combo)
                } else {
                    strip_modifier_sides(combo)
                };
                row.trigger = join_trigger_flags(flags, &combo);
                *changed = true;
            }
        });
}

fn binding_option(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    value: &mut bool,
    label: &str,
    help: &str,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = super::settings_toggle(ui, palette, value);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(label)
                    .color(palette.text)
                    .strong()
                    .size(12.0),
            );
            ui.label(egui::RichText::new(help).color(palette.muted).size(11.0));
        });
    });
    changed
}

fn binding_status(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    row: &BindingRow,
    scope: KeybindScope,
) {
    // A healthy row should read as a tick, not as another word competing with the action text.
    if let Some(true) = row.validity(scope) {
        if let Some(icon) = bootty_ui::icons::icon_text("check", 16.0, palette.success) {
            ui.label(icon);
        }
        return;
    }
    let (status, color) = match row.validity(scope) {
        Some(false) => ("invalid", palette.destructive),
        _ => ("incomplete", palette.muted),
    };
    ui.label(egui::RichText::new(status).color(color).small());
}

struct BindingEditorContext<'a> {
    scope: KeybindScope,
    index: usize,
    action_options: &'a [(&'static str, &'static str, &'static str)],
    /// The active preset's prefix, when this scope supports prefixed chords.
    prefix: Option<&'a str>,
    capture: Option<&'a ChordCapture>,
    changed: &'a mut bool,
    toggle_capture: &'a mut Option<usize>,
    remove: &'a mut Option<usize>,
}

fn remap_is_valid(from: &str, to: &str) -> bool {
    if from.is_empty() || to.is_empty() {
        return false;
    }
    let mut set = bootty_winit::modifier_remap::ModifierRemapSet::default();
    set.parse(&format!("{from}={to}")).is_ok()
}

fn handle_capture(
    ui: &egui::Ui,
    capture: &mut Option<ChordCapture>,
    rows: &mut [BindingRow],
    changed: &mut bool,
    direct_chords: &[String],
    modifier_sides: ModifierSideState,
    prefix: Option<&str>,
) {
    if capture.is_none() {
        return;
    }
    let now = ui.input(|input| input.time);
    // Keep repainting so the chord-timeout commit fires even without further input.
    ui.ctx().request_repaint();
    let row = capture.as_ref().expect("capture checked above").row;
    let side_sensitive = rows.get(row).is_some_and(|row| row.side_sensitive);

    // Input sources are ordered: egui keys, wheel, then chords intercepted by direct input.
    let step = if let Some((key, modifiers)) = drain_first_key_press(ui) {
        if key == egui::Key::Escape {
            *capture = None;
            return;
        }
        let step = captured_step(side_sensitive, direct_chords, key, modifiers);
        if step.is_none() {
            return;
        }
        step
    } else if let Some((up, modifiers)) = drain_first_scroll(ui) {
        Some(scroll_step(up, modifiers, modifier_sides, side_sensitive))
    } else {
        direct_chords.first().map(|step| {
            if side_sensitive {
                step.clone()
            } else {
                strip_modifier_sides(step)
            }
        })
    };
    if let Some(step) = step {
        let cap = capture.as_mut().expect("capture checked above");
        cap.steps.push(step);
        cap.deadline = Some(now + CHORD_TIMEOUT);
        return;
    }

    let commit = capture.as_ref().and_then(|cap| {
        // A prefixed row records exactly one key: commit on the first step, composed with the
        // prefix, instead of waiting out the chord timeout.
        let row_prefix = prefix.filter(|_| rows.get(cap.row).is_some_and(|row| row.prefixed));
        let ready = if row_prefix.is_some() {
            !cap.steps.is_empty()
        } else {
            cap.deadline.is_some_and(|deadline| now >= deadline) && !cap.steps.is_empty()
        };
        ready.then(|| {
            let combo = match row_prefix {
                Some(prefix) => format!("{prefix}>{}", cap.steps[0]),
                None => cap.steps.join(">"),
            };
            (cap.row, combo)
        })
    });
    if let Some((row, combo)) = commit {
        if let Some(entry) = rows.get_mut(row) {
            // Recording only captures the key combo; keep any flag prefixes the row already carries.
            let (flags, _) = parse_trigger_flags(&entry.trigger);
            entry.trigger = join_trigger_flags(&flags, &combo);
        }
        *capture = None;
        *changed = true;
    }
}

/// Remove and return the first key-press event this frame. Also drops the text/clipboard events the
/// same keystroke produces (⌘V emits `Paste`, ⌘C/⌘X emit `Copy`/`Cut`) so a captured shortcut never
/// types into a focused field or runs a clipboard action behind the settings overlay.
fn drain_first_key_press(ui: &egui::Ui) -> Option<(egui::Key, egui::Modifiers)> {
    ui.input_mut(|input| {
        let mut first = None;
        input.events.retain(|event| match event {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if first.is_none() {
                    first = Some((*key, *modifiers));
                }
                false
            }
            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut => {
                false
            }
            _ => true,
        });
        first
    })
}

/// Remove and return the first wheel event this frame, as `(scrolled_up, modifiers)`. The frame's
/// accumulated scroll is zeroed too, since the deltas are summed before events reach us and would
/// otherwise scroll the settings page out from under the row being recorded.
fn drain_first_scroll(ui: &egui::Ui) -> Option<(bool, egui::Modifiers)> {
    ui.input_mut(|input| {
        let mut first = None;
        input.events.retain(|event| match event {
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } if delta.y != 0.0 => {
                if first.is_none() {
                    first = Some((delta.y > 0.0, *modifiers));
                }
                false
            }
            _ => true,
        });
        if first.is_some() {
            input.smooth_scroll_delta = egui::Vec2::ZERO;
        }
        first
    })
}

fn split_action_for_editor(
    action: &str,
    options: &[(&'static str, &'static str, &'static str)],
) -> (String, String) {
    if options.iter().any(|(name, _, _)| *name == action) {
        return (action.to_owned(), String::new());
    }
    match action.split_once(':') {
        Some((base, params)) => (base.to_owned(), params.to_owned()),
        None => (action.to_owned(), String::new()),
    }
}
