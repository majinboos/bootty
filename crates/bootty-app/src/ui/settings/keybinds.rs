use eframe::egui;

mod model;
mod trigger_edit;

use super::SettingsWindow;
use crate::config::KeybindPreset;
use crate::direct_input::ModifierSideState;
pub(super) use model::{BindingRow, ChordCapture, KeybindScope};
use model::{action_options, effective_bindings, read_scope_entries, split_entry, write_scope};
use trigger_edit::{
    MODIFIER_TOKENS, TRIGGER_FLAGS, add_default_modifier_sides, captured_step, join_trigger_flags,
    parse_trigger_flags, prefix_combo, scroll_step, strip_modifier_sides, trigger_step,
    unprefix_combo,
};

/// Seconds to wait for the next chord step before committing the captured trigger.
const CHORD_TIMEOUT: f64 = 0.8;

pub(super) fn ui(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    let direct_chords = std::mem::take(&mut win.recorder_chords);

    shortcut_options(win, ui);
    preset_options(win, ui, &direct_chords);

    super::section(ui, palette, "KEYBINDINGS");
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Scope").color(palette.subtext));
        let mut scope = win.keybind_scope;
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
        win.keybind_scope = scope;
    });
    ui.add_space(8.0);
    let scope = win.keybind_scope;

    if win.keybind_loaded_scope != Some(scope) {
        let (clear, rows) = read_scope_entries(win, scope);
        win.keybind_clear = clear;
        win.keybind_rows = Some(rows);
        win.keybind_loaded_scope = Some(scope);
        win.keybind_capture = None;
    }

    let mut rows = win.keybind_rows.take().unwrap_or_default();
    let mut clear = win.keybind_clear;
    let mut capture = win.keybind_capture.take();
    let mut changed = false;
    // Prefixed chords are idiomatic in the global and native/rmux scopes; the tmux backend
    // relays raw bytes and the sidebar has no chord support, so those record literally.
    let effective_prefix = match scope {
        KeybindScope::Global | KeybindScope::Native | KeybindScope::Rmux => {
            win.config.input.effective_prefix()
        }
        _ => None,
    };

    // "Configure this command's keybinding" (from the palette): surface the row for
    // the requested action — adding an empty one if absent — and filter the list to
    // it. Recording is left for the user to start; auto-starting it would capture
    // the very chord that opened this view (e.g. `cmd+shift+,`).
    if let Some(target) = win.pending_keybind_focus.take() {
        if !rows.iter().any(|row| row.action.trim() == target.as_str()) {
            rows.push(BindingRow {
                trigger: String::new(),
                action: target.clone(),
                side_sensitive: false,
                prefixed: false,
            });
        }
        let search_id = ui.make_persistent_id(("settings_keybind_search", scope));
        ui.memory_mut(|memory| memory.data.insert_temp(search_id, target));
    }

    if defaults_toggle(ui, palette, &mut clear) {
        changed = true;
    }

    let id = ui.make_persistent_id(("settings_keybind_search", scope));
    let mut search: String = ui.memory(|memory| memory.data.get_temp(id).unwrap_or_default());
    if super::settings_text_edit_width(ui, palette, &mut search, "Search keybindings", 280.0)
        .changed()
    {
        ui.memory_mut(|memory| memory.data.insert_temp(id, search));
    }
    let search_id = ui.make_persistent_id(("settings_keybind_search", scope));
    let search: String = ui.memory(|memory| memory.data.get_temp(search_id).unwrap_or_default());
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

    let invalid_count = rows
        .iter()
        .filter(|row| {
            let trigger = row.trigger.trim();
            let action = row.action.trim();
            !(trigger.is_empty() || action.is_empty() || scope.entry_is_valid(trigger, action))
        })
        .count();
    let complete_count = rows
        .iter()
        .filter(|row| !row.trigger.trim().is_empty() && !row.action.trim().is_empty())
        .count();
    let summary = if invalid_count == 0 {
        format!("{complete_count} complete binding rows; no conflicts or invalid actions detected.")
    } else {
        format!("{invalid_count} invalid binding rows need attention.")
    };
    conflict_banner(ui, palette, invalid_count, &summary);

    let mut remove: Option<usize> = None;
    let mut toggle_capture: Option<usize> = None;
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
        win.prefix_capture = false;
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

    win.keybind_clear = clear;
    if changed {
        write_scope(win, scope, clear, &rows);
        // Keep the resolved-shortcuts panel in sync with what was just written.
        super::reload_settings_config(win);
    }
    win.keybind_rows = Some(rows);
    win.keybind_capture = capture;

    effective_bindings_panel(win, ui, scope);
}

/// Global input settings, laid out at the top of the page with the same row grammar as the rest of
/// settings so they line up with every other pane.
fn shortcut_options(win: &mut SettingsWindow, ui: &mut egui::Ui) {
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
                win.set_bool(&["input", "hide-mouse-pointer-while-typing"], hide_pointer);
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
                crate::config::MacosOptionAsAltConfig::None => 0,
                crate::config::MacosOptionAsAltConfig::Left => 1,
                crate::config::MacosOptionAsAltConfig::Right => 2,
                crate::config::MacosOptionAsAltConfig::Both => 3,
            };
            if let Some(index) = super::settings_segmented(ui, palette, &tokens, current) {
                win.config.input.macos_option_as_alt = match index {
                    0 => crate::config::MacosOptionAsAltConfig::None,
                    1 => crate::config::MacosOptionAsAltConfig::Left,
                    2 => crate::config::MacosOptionAsAltConfig::Right,
                    _ => crate::config::MacosOptionAsAltConfig::Both,
                };
                win.set_str(&["input", "macos-option-as-alt"], tokens[index]);
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
fn preset_options(win: &mut SettingsWindow, ui: &mut egui::Ui, direct_chords: &[String]) {
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
                win.set_str(&["input", "preset"], preset.as_str());
                // Re-resolve so the built-in default tables (and effective prefix) swap over.
                super::reload_settings_config(win);
                win.keybind_loaded_scope = None;
            }
        },
    );

    let preset = win.config.input.preset;
    if preset.default_prefix().is_none() {
        win.prefix_capture = false;
        return;
    }
    let prefix = win.config.input.effective_prefix().unwrap_or_default();
    let recording = win.prefix_capture;
    let mut toggle_recording = false;
    let mut reset_prefix = false;
    super::settings_row(
        ui,
        palette,
        "Prefix",
        "Leader combo for prefixed shortcuts: press it, then the bound key.",
        |ui| {
            let capture_text = "Press a combo… Esc to cancel";
            if record_cell(ui, palette, &prefix, recording, capture_text).clicked() {
                toggle_recording = true;
            }
            if record_dot_button(ui, palette, recording).clicked() {
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
        win.prefix_capture = !recording;
        win.keybind_capture = None;
    }
    if reset_prefix {
        win.prefix_capture = false;
        win.remove(&["input", "prefix"]);
        super::reload_settings_config(win);
        win.keybind_loaded_scope = None;
    }
    if win.prefix_capture {
        handle_prefix_capture(win, ui, direct_chords);
    }
}

/// Single-combo capture for the prefix recorder: the first step commits immediately (a prefix is
/// one combo, never a chord). Escape cancels.
fn handle_prefix_capture(win: &mut SettingsWindow, ui: &egui::Ui, direct_chords: &[String]) {
    ui.ctx().request_repaint();
    let step = if let Some((key, modifiers)) = drain_first_key_press(ui) {
        if key == egui::Key::Escape {
            win.prefix_capture = false;
            return;
        }
        trigger_step(key, modifiers)
    } else {
        direct_chords.first().map(|step| strip_modifier_sides(step))
    };
    let Some(step) = step else {
        return;
    };
    win.prefix_capture = false;
    win.set_str(&["input", "prefix"], &step);
    // Re-resolve so the prefixed default chords rebuild against the new prefix.
    super::reload_settings_config(win);
    win.keybind_loaded_scope = None;
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

fn modifier_remaps(win: &mut SettingsWindow, ui: &mut egui::Ui) {
    let palette = win.palette;
    if win.modifier_rows.is_none() {
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
        win.modifier_rows = Some(rows);
    }
    let mut rows = win.modifier_rows.take().unwrap_or_default();
    let mut changed = false;
    let mut remove: Option<usize> = None;
    for (index, (from, to)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            let from_index = MODIFIER_TOKENS
                .iter()
                .position(|&token| token == from.as_str());
            let from_label = if from.is_empty() {
                "from"
            } else {
                from.as_str()
            };
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("mod_remap_from_{index}"),
                from_label,
                118.0,
                MODIFIER_TOKENS,
                from_index,
            ) {
                *from = MODIFIER_TOKENS[choice].to_owned();
                changed = true;
            }
            let to_index = MODIFIER_TOKENS
                .iter()
                .position(|&token| token == to.as_str());
            let to_label = if to.is_empty() { "to" } else { to.as_str() };
            if let Some(choice) = super::searchable_combo(
                ui,
                palette,
                &format!("mod_remap_to_{index}"),
                to_label,
                118.0,
                MODIFIER_TOKENS,
                to_index,
            ) {
                *to = MODIFIER_TOKENS[choice].to_owned();
                changed = true;
            }
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
            .collect();
        win.config.input.modifier_remap = entries.clone();
        if entries.is_empty() {
            win.remove(&["input", "modifier-remap"]);
        } else {
            win.set_strings(&["input", "modifier-remap"], &entries);
        }
    }
    win.modifier_rows = Some(rows);
}

fn conflict_banner(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    invalid_count: usize,
    summary: &str,
) {
    let ok = invalid_count == 0;
    egui::Frame::NONE
        .fill(palette.pane)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let color = if ok {
                    palette.success
                } else {
                    palette.destructive
                };
                if let Some(icon) = crate::ui::icons::icon_text(
                    if ok { "check" } else { "circle-alert" },
                    16.0,
                    color,
                ) {
                    ui.label(icon);
                }
                ui.label(
                    egui::RichText::new(if ok {
                        "No conflicts"
                    } else {
                        "Needs attention"
                    })
                    .color(color)
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(summary).color(palette.muted).size(12.0));
                });
            });
        });
    ui.add_space(12.0);
}

fn effective_bindings_panel(win: &SettingsWindow, ui: &mut egui::Ui, scope: KeybindScope) {
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
        // Render the rows inline (no nested scroll area) so the panel grows to its full height and
        // the page's own scroll handles overflow — a nested scroll collapsed it to a couple rows.
        egui::Frame::NONE
            .fill(palette.pane)
            .stroke(egui::Stroke::new(1.0, palette.border))
            .corner_radius(egui::CornerRadius::same(palette.radius))
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                // Collect the filtered entries, then lay them out in an aligned multi-column grid.
                let entries: Vec<(String, String, [bool; 4])> = effective_bindings(win, scope)
                    .iter()
                    .filter_map(|entry| {
                        let (trigger, action) = split_entry(entry);
                        let haystack = format!("{trigger} {action}").to_ascii_lowercase();
                        if !needle.is_empty() && !haystack.contains(&needle) {
                            return None;
                        }
                        let (flags, combo) = parse_trigger_flags(&trigger);
                        Some((combo, action_title(&action), flags))
                    })
                    .collect();
                if entries.is_empty() {
                    ui.label(
                        egui::RichText::new("No matching shortcuts.")
                            .color(palette.muted)
                            .size(12.0),
                    );
                    return;
                }
                let cols = ((ui.available_width() / 280.0).floor() as usize).clamp(1, 6);
                // egui::Grid keeps columns aligned (each column sizes to its widest cell) rather than
                // packing each cell to its own content width, which staggered the old layout.
                egui::Grid::new(("resolved_shortcuts_grid", scope))
                    .num_columns(cols)
                    .spacing([28.0, 12.0])
                    .show(ui, |ui| {
                        for (index, (combo, title, flags)) in entries.iter().enumerate() {
                            ui.horizontal(|ui| {
                                keycap_chip(ui, palette, combo);
                                ui.add_space(8.0);
                                ui.label(egui::RichText::new(title).color(palette.subtext));
                                let tags: Vec<&str> = TRIGGER_FLAGS
                                    .iter()
                                    .enumerate()
                                    .filter(|(flag_index, _)| flags[*flag_index])
                                    .map(|(_, (name, _, _))| *name)
                                    .collect();
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

/// Shared control height for the trigger cell and the value field so they line up exactly.
const ROW_CONTROL_HEIGHT: f32 = 36.0;

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

                if record_cell(ui, palette, &combo, recording, &capture_text).clicked() {
                    *ctx.toggle_capture = Some(ctx.index);
                }

                if record_dot_button(ui, palette, recording).clicked() {
                    *ctx.toggle_capture = Some(ctx.index);
                }

                if let Some(icon) = crate::ui::icons::icon_text("arrow-right", 14.0, palette.muted)
                {
                    ui.label(icon);
                }

                // Title + description picker, drawn from the shared action catalog.
                let options = action_options(ctx.scope);
                let (base, params) = split_action_for_editor(&row.action, &options);

                // Spread the action + value across the leftover width, reserving a right cluster for
                // the status, flags, and remove controls so the row uses its full width.
                // Wide enough for the "incomplete" status label plus the flags and remove buttons,
                // so the right-to-left cluster never overlaps the value field.
                let right_cluster = 200.0;
                let fields = (ui.available_width() - right_cluster).max(240.0);
                let action_width = (fields * 0.58 - 8.0).clamp(150.0, 320.0);
                let value_width = (fields - action_width - 8.0).clamp(90.0, 240.0);

                let mut chosen_action: &'static str = options
                    .iter()
                    .find(|(name, _, _)| *name == base)
                    .map_or("", |(name, _, _)| *name);
                if super::described_combo(
                    ui,
                    palette,
                    &format!("kb_action_{}", ctx.index),
                    &mut chosen_action,
                    &options,
                    super::ComboStyle {
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
                    if flags_button(ui, palette, any_flag, flags_open).clicked() {
                        flags_open = !flags_open;
                        ui.memory_mut(|memory| memory.data.insert_temp(flags_open_id, flags_open));
                    }
                    ui.add_space(4.0);
                    binding_status(ui, palette, ctx.scope, &row.trigger, &row.action);
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
    egui::Frame::NONE
        .inner_margin(egui::Margin {
            left: 10,
            right: 10,
            top: 4,
            bottom: 8,
        })
        .show(ui, |ui| {
            for (index, (_, label, help)) in TRIGGER_FLAGS.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let mut on = flags[index];
                    if super::settings_toggle(ui, palette, &mut on) {
                        flags[index] = on;
                        row.trigger = join_trigger_flags(flags, combo);
                        *changed = true;
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(*label)
                                .color(palette.text)
                                .strong()
                                .size(12.0),
                        );
                        ui.label(egui::RichText::new(*help).color(palette.muted).size(11.0));
                    });
                });
                ui.add_space(2.0);
            }

            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                let mut on = row.side_sensitive;
                if super::settings_toggle(ui, palette, &mut on) {
                    row.side_sensitive = on;
                    let combo = if on {
                        add_default_modifier_sides(combo)
                    } else {
                        strip_modifier_sides(combo)
                    };
                    row.trigger = join_trigger_flags(flags, &combo);
                    *changed = true;
                }
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("Modifier side")
                            .color(palette.text)
                            .strong()
                            .size(12.0),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Require the same physical left/right modifier side that was recorded.",
                        )
                        .color(palette.muted)
                        .size(11.0),
                    );
                });
            });
        });
}

/// The record indicator: a red ball at rest, a red square while capturing.
fn record_dot_button(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    recording: bool,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(ROW_CONTROL_HEIGHT), egui::Sense::click());
    let center = rect.center();
    let red = palette.destructive;
    if recording {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(center, egui::Vec2::splat(12.0)),
            egui::CornerRadius::same(3),
            red,
        );
    } else {
        ui.painter().circle_filled(center, 7.0, red);
        if response.hovered() {
            ui.painter()
                .circle_stroke(center, 10.0, egui::Stroke::new(1.5, red));
        }
    }
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response.on_hover_text(if recording {
        "Stop recording"
    } else {
        "Record shortcut"
    })
}

/// Small toggle button that opens the per-binding flags editor; tinted when any flag is active.
fn flags_button(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    active: bool,
    open: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(30.0), egui::Sense::click());
    let radius = egui::CornerRadius::same(palette.radius);
    let fill = if open || response.hovered() {
        palette.hover
    } else {
        palette.surface
    };
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, palette.border),
        egui::StrokeKind::Inside,
    );
    let tint = if active {
        palette.primary
    } else {
        palette.muted
    };
    crate::ui::icons::paint_icon_slug(
        ui.painter(),
        "sliders-horizontal",
        rect.center(),
        15.0,
        tint,
    );
    response.on_hover_text("Binding options")
}

/// The clickable shortcut cell: shows the bound combo as keycaps, or a pulsing recording prompt
/// while capturing. Clicking toggles capture, mirroring the record button beside it.
fn record_cell(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    trigger: &str,
    recording: bool,
    capture_text: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::Vec2::new(220.0, ROW_CONTROL_HEIGHT),
        egui::Sense::click(),
    );
    let radius = egui::CornerRadius::same(palette.radius);
    let text_pos = rect.left_center() + egui::vec2(10.0, 0.0);
    if recording {
        ui.ctx().request_repaint();
        let pulse = (ui.input(|input| input.time) * 3.0).sin() * 0.5 + 0.5;
        let alpha = (pulse * 90.0) as u8 + 45;
        let glow = egui::Color32::from_rgba_unmultiplied(
            palette.primary.r(),
            palette.primary.g(),
            palette.primary.b(),
            alpha,
        );
        ui.painter().rect_filled(rect, radius, palette.mantle);
        ui.painter().rect_filled(rect, radius, glow);
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, palette.primary),
            egui::StrokeKind::Inside,
        );
        if capture_text.starts_with("Press keys") {
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                capture_text,
                egui::FontId::proportional(12.0),
                palette.text,
            );
        } else {
            let galley = crate::ui::keycaps::trigger_galley_from_painter(
                ui.painter(),
                palette,
                capture_text,
                palette.text,
                rect.width() - 20.0,
            );
            let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
            ui.painter().galley(pos, galley, palette.text);
        }
    } else {
        let fill = if response.hovered() {
            palette.hover
        } else {
            palette.mantle
        };
        ui.painter().rect_filled(rect, radius, fill);
        ui.painter().rect_stroke(
            rect,
            radius,
            egui::Stroke::new(1.0, palette.border),
            egui::StrokeKind::Inside,
        );
        if trigger.trim().is_empty() {
            ui.painter().text(
                text_pos,
                egui::Align2::LEFT_CENTER,
                "Click to record",
                egui::FontId::proportional(12.0),
                palette.muted,
            );
        } else {
            let galley = crate::ui::keycaps::trigger_galley(
                ui,
                palette,
                trigger,
                palette.text,
                rect.width() - 20.0,
            );
            let pos = egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5);
            ui.painter().galley(pos, galley, palette.text);
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    response.on_hover_text(if recording {
        "Recording — press keys or scroll, Esc cancels"
    } else {
        "Click to record a shortcut"
    })
}

fn binding_status(
    ui: &mut egui::Ui,
    palette: bootty_ui::ThemePalette,
    scope: KeybindScope,
    trigger: &str,
    action: &str,
) {
    let trigger = trigger.trim();
    let action = action.trim();
    if trigger.is_empty() || action.is_empty() {
        ui.label(
            egui::RichText::new("incomplete")
                .color(palette.muted)
                .size(11.0),
        );
    } else if scope.entry_is_valid(trigger, action) {
        if let Some(icon) = crate::ui::icons::icon_text("check", 16.0, palette.success) {
            ui.label(icon);
        }
    } else {
        ui.label(
            egui::RichText::new("invalid")
                .color(palette.destructive)
                .size(11.0),
        );
    }
}

/// A framed keycap rendering of a resolved trigger for the read-only list.
fn keycap_chip(ui: &mut egui::Ui, palette: bootty_ui::ThemePalette, trigger: &str) {
    egui::Frame::NONE
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(egui::CornerRadius::same(palette.radius))
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            let galley =
                crate::ui::keycaps::trigger_galley(ui, palette, trigger, palette.text, 320.0);
            ui.add(egui::Label::new(galley).selectable(false));
        });
}

struct BindingEditorContext<'a> {
    scope: KeybindScope,
    index: usize,
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
    let mut set = crate::modifier_remap::ModifierRemapSet::default();
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

    // egui events first, for Escape (cancel) and any non-cmd chord egui still delivers as a key.
    if let Some((key, modifiers)) = drain_first_key_press(ui) {
        if key == egui::Key::Escape {
            *capture = None;
            return;
        }
        if let Some(cap) = capture.as_mut()
            && let Some(step) = captured_step(
                rows.get(cap.row).is_some_and(|row| row.side_sensitive),
                direct_chords,
                key,
                modifiers,
            )
        {
            cap.steps.push(step);
            cap.deadline = Some(now + CHORD_TIMEOUT);
        }
        return;
    }

    // Wheel steps: `scroll_up` / `scroll_down` are bindable triggers (the default font-size zoom
    // rides on `alt+shift+scroll`), so the recorder captures the wheel the same way it captures keys.
    if let Some((up, modifiers)) = drain_first_scroll(ui)
        && let Some(cap) = capture.as_mut()
    {
        let side_sensitive = rows.get(cap.row).is_some_and(|row| row.side_sensitive);
        cap.steps
            .push(scroll_step(up, modifiers, modifier_sides, side_sensitive));
        cap.deadline = Some(now + CHORD_TIMEOUT);
        return;
    }

    // Direct-input chords: cmd-modified combos (incl. Cmd+C/Cmd+X/Cmd+V and their +alt/+shift
    // variants) that egui turns into copy/cut/paste events with no recordable key event.
    if let Some(step) = direct_chords.first()
        && let Some(cap) = capture.as_mut()
    {
        let step = if rows.get(cap.row).is_some_and(|row| row.side_sensitive) {
            step.clone()
        } else {
            strip_modifier_sides(step)
        };
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

/// A human-readable label for an action string, preferring the shared action catalog's title (the
/// same titles the command palette shows) and falling back to sentence-casing the name for actions
/// the catalog doesn't know (sidebar actions, `text`/`csi`/…). Keeps any trailing `:param` suffix.
fn action_title(action: &str) -> String {
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

/// Turn a snake_case action name into a sentence-cased label.
fn humanize_action(name: &str) -> String {
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
