use crate::direct_input::ModifierSideState;
use crate::input_binding::BindingTrigger;
use eframe::egui;

/// Trigger flag prefixes from the binding grammar (`performable:`, `global:`, …). Surfaced as
/// per-row toggles so the trigger cell only ever holds a recordable key combo. Display order is
/// independent of how the parser accepts them.
pub(super) const TRIGGER_FLAGS: [(&str, &str, &str); 4] = [
    (
        "performable",
        "Performable",
        "Only fire when the action can run now; otherwise the keys pass through.",
    ),
    (
        "global",
        "Global",
        "Match even when Bootty is not the focused app.",
    ),
    (
        "all",
        "All surfaces",
        "Apply on every surface, not just the active one.",
    ),
    (
        "unconsumed",
        "Pass-through",
        "Run the action but still deliver the keys to the terminal.",
    ),
];

/// Split a stored trigger into its flag prefixes and the bare key combo. Mirrors the parser, which
/// strips known `prefix:` tokens off the front before reading the combo.
pub(super) fn parse_trigger_flags(trigger: &str) -> ([bool; 4], String) {
    let mut flags = [false; 4];
    let mut rest = trigger.trim();
    while let Some((prefix, tail)) = rest.split_once(':') {
        match TRIGGER_FLAGS
            .iter()
            .position(|(name, _, _)| *name == prefix)
        {
            Some(index) if !flags[index] => {
                flags[index] = true;
                rest = tail.trim_start();
            }
            _ => break,
        }
    }
    (flags, rest.to_owned())
}

/// Reassemble a trigger string from flag toggles and a key combo.
pub(super) fn join_trigger_flags(flags: &[bool; 4], combo: &str) -> String {
    let mut out = String::new();
    for (index, (name, _, _)) in TRIGGER_FLAGS.iter().enumerate() {
        if flags[index] {
            out.push_str(name);
            out.push(':');
        }
    }
    out.push_str(combo.trim());
    out
}

/// Modifier tokens accepted by the modifier-remap parser, both unsided and per-side.
pub(super) const MODIFIER_TOKENS: &[&str] = &[
    "ctrl",
    "alt",
    "shift",
    "super",
    "left_ctrl",
    "left_alt",
    "left_shift",
    "left_super",
    "right_ctrl",
    "right_alt",
    "right_shift",
    "right_super",
];

pub(super) fn captured_step(
    side_sensitive: bool,
    direct_chords: &[String],
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<String> {
    if side_sensitive && let Some(step) = direct_chords.first() {
        return Some(step.clone());
    }
    trigger_step(key, modifiers)
}

/// Whether a combo is exactly `{prefix}>{one step}` — the shape the Prefixed checkbox produces.
pub(super) fn combo_is_prefixed(combo: &str, prefix: &str) -> bool {
    combo
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('>'))
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('>'))
}

pub(super) fn prefix_combo(combo: &str, prefix: &str) -> String {
    if combo_is_prefixed(combo, prefix) {
        combo.to_owned()
    } else {
        format!("{prefix}>{combo}")
    }
}

pub(super) fn unprefix_combo(combo: &str, prefix: &str) -> String {
    combo
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('>'))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(combo)
        .to_owned()
}

pub(super) fn combo_has_modifier_sides(combo: &str) -> bool {
    combo
        .split('>')
        .flat_map(|step| step.split('+'))
        .any(is_sided_modifier_token)
}

pub(super) fn strip_modifier_sides(combo: &str) -> String {
    rewrite_modifier_tokens(combo, strip_modifier_side_token)
}

pub(super) fn add_default_modifier_sides(combo: &str) -> String {
    rewrite_modifier_tokens(combo, add_default_modifier_side_token)
}

fn rewrite_modifier_tokens(combo: &str, rewrite: fn(&str) -> &str) -> String {
    combo
        .split('>')
        .map(|step| step.split('+').map(rewrite).collect::<Vec<_>>().join("+"))
        .collect::<Vec<_>>()
        .join(">")
}

fn strip_modifier_side_token(token: &str) -> &str {
    match token {
        "left_shift" | "right_shift" => "shift",
        "left_ctrl" | "left_control" | "right_ctrl" | "right_control" => "ctrl",
        "left_alt" | "left_opt" | "left_option" | "right_alt" | "right_opt" | "right_option" => {
            "alt"
        }
        "left_cmd" | "left_command" | "left_super" | "right_cmd" | "right_command"
        | "right_super" => "cmd",
        other => other,
    }
}

fn add_default_modifier_side_token(token: &str) -> &str {
    match token {
        "shift" => "left_shift",
        "ctrl" | "control" => "left_ctrl",
        "alt" | "opt" | "option" => "left_alt",
        "cmd" | "command" | "super" => "left_cmd",
        other => other,
    }
}

fn is_sided_modifier_token(token: &str) -> bool {
    matches!(
        token,
        "left_shift"
            | "right_shift"
            | "left_ctrl"
            | "left_control"
            | "right_ctrl"
            | "right_control"
            | "left_alt"
            | "left_opt"
            | "left_option"
            | "right_alt"
            | "right_opt"
            | "right_option"
            | "left_cmd"
            | "left_command"
            | "left_super"
            | "right_cmd"
            | "right_command"
            | "right_super"
    )
}

pub(super) fn trigger_step(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    Some(modified_step(&key_token(key)?, modifiers))
}

/// A wheel step (`alt+scroll_up`), formatted by the binding grammar's own writer so the tokens
/// match what the parser accepts. A side-sensitive row keeps the left/right side of every held
/// modifier, taken from the same live side state the runtime matches wheel bindings against —
/// egui's `Modifiers` alone cannot tell the sides apart.
pub(super) fn scroll_step(
    up: bool,
    modifiers: egui::Modifiers,
    modifier_sides: ModifierSideState,
    side_sensitive: bool,
) -> String {
    let mods = crate::app_actions::key_mods_for_egui_binding(modifiers, modifier_sides);
    let mut trigger = BindingTrigger::from_scroll_with_modifier_sides(up, mods);
    if !side_sensitive {
        trigger.mods = trigger.mods.without_side_constraints();
    }
    trigger.format_entry()
}

fn modified_step(token: &str, modifiers: egui::Modifiers) -> String {
    let mut parts: Vec<&str> = Vec::new();
    // egui aliases `command` to `ctrl` off macOS, so only treat the real Cmd key as cmd.
    if cfg!(target_os = "macos") && (modifiers.mac_cmd || modifiers.command) {
        parts.push("cmd");
    }
    if modifiers.ctrl {
        parts.push("ctrl");
    }
    if modifiers.alt {
        parts.push("alt");
    }
    if modifiers.shift {
        parts.push("shift");
    }
    let mut step = parts.join("+");
    if !step.is_empty() {
        step.push('+');
    }
    step.push_str(token);
    step
}

fn key_token(key: egui::Key) -> Option<String> {
    use egui::Key;
    let token = match key {
        Key::A => "a",
        Key::B => "b",
        Key::C => "c",
        Key::D => "d",
        Key::E => "e",
        Key::F => "f",
        Key::G => "g",
        Key::H => "h",
        Key::I => "i",
        Key::J => "j",
        Key::K => "k",
        Key::L => "l",
        Key::M => "m",
        Key::N => "n",
        Key::O => "o",
        Key::P => "p",
        Key::Q => "q",
        Key::R => "r",
        Key::S => "s",
        Key::T => "t",
        Key::U => "u",
        Key::V => "v",
        Key::W => "w",
        Key::X => "x",
        Key::Y => "y",
        Key::Z => "z",
        Key::Num0 => "0",
        Key::Num1 => "1",
        Key::Num2 => "2",
        Key::Num3 => "3",
        Key::Num4 => "4",
        Key::Num5 => "5",
        Key::Num6 => "6",
        Key::Num7 => "7",
        Key::Num8 => "8",
        Key::Num9 => "9",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::Semicolon => ";",
        Key::Quote => "'",
        Key::Minus => "-",
        Key::Plus | Key::Equals => "=",
        Key::Backslash => "\\",
        Key::Backtick => "`",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Space => "space",
        Key::Enter => "Enter",
        Key::Tab => "Tab",
        Key::Backspace => "Backspace",
        Key::Delete => "Delete",
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        Key::Home => "Home",
        Key::End => "End",
        Key::PageUp => "PageUp",
        Key::PageDown => "PageDown",
        Key::Insert => "Insert",
        Key::F1 => "F1",
        Key::F2 => "F2",
        Key::F3 => "F3",
        Key::F4 => "F4",
        Key::F5 => "F5",
        Key::F6 => "F6",
        Key::F7 => "F7",
        Key::F8 => "F8",
        Key::F9 => "F9",
        Key::F10 => "F10",
        Key::F11 => "F11",
        Key::F12 => "F12",
        _ => return None,
    };
    Some(token.to_owned())
}
