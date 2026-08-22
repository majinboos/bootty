use bootty_winit::direct_input::ModifierSideState;
use bootty_winit::input_binding::{BindingModSide, BindingTrigger};
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

/// Whether any step of `combo` constrains a modifier to one side. Read from the parsed trigger,
/// not the text, so `control+a` and `ctrl+a` both report "no side" rather than differing.
pub(super) fn combo_has_modifier_sides(combo: &str) -> bool {
    combo.split('>').any(|step| {
        step.parse::<BindingTrigger>().is_ok_and(|trigger| {
            let mods = trigger.mods;
            mods.shift_side.is_some()
                || mods.ctrl_side.is_some()
                || mods.alt_side.is_some()
                || mods.command_side.is_some()
        })
    })
}

pub(super) fn strip_modifier_sides(combo: &str) -> String {
    rewrite_modifier_sides(combo, |trigger| {
        trigger.mods = trigger.mods.without_side_constraints();
    })
}

/// Constrain every held modifier to its left side, the side a recorder defaults to.
pub(super) fn add_default_modifier_sides(combo: &str) -> String {
    rewrite_modifier_sides(combo, |trigger| {
        for (held, side) in [
            (trigger.mods.shift, &mut trigger.mods.shift_side),
            (trigger.mods.ctrl, &mut trigger.mods.ctrl_side),
            (trigger.mods.alt, &mut trigger.mods.alt_side),
            (trigger.mods.command, &mut trigger.mods.command_side),
        ] {
            if held && side.is_none() {
                *side = Some(BindingModSide::Left);
            }
        }
    })
}

/// Rewrite each step through the binding grammar's own parser and writer. A step that does not
/// parse is kept verbatim, so toggling the side of a half-typed row never blanks it.
fn rewrite_modifier_sides(combo: &str, rewrite: impl Fn(&mut BindingTrigger)) -> String {
    combo
        .split('>')
        .map(|step| match step.parse::<BindingTrigger>() {
            Ok(mut trigger) => {
                rewrite(&mut trigger);
                trigger.format_entry()
            }
            Err(_) => step.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(">")
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
    // egui aliases `command` to `ctrl` off macOS, so only treat the real Cmd key as cmd.
    [
        (cfg!(target_os = "macos") && (modifiers.mac_cmd || modifiers.command)).then_some("cmd"),
        modifiers.ctrl.then_some("ctrl"),
        modifiers.alt.then_some("alt"),
        modifiers.shift.then_some("shift"),
        Some(token),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("+")
}

fn key_token(key: egui::Key) -> Option<String> {
    use egui::Key;
    let token = match key {
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
        Key::ArrowUp => "ArrowUp",
        Key::ArrowDown => "ArrowDown",
        Key::ArrowLeft => "ArrowLeft",
        Key::ArrowRight => "ArrowRight",
        _ => {
            let name = key.name();
            if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() {
                return Some(name.to_ascii_lowercase());
            }
            return ((Key::F1..=Key::F12).contains(&key)
                || matches!(
                    key,
                    Key::Enter
                        | Key::Tab
                        | Key::Backspace
                        | Key::Delete
                        | Key::Home
                        | Key::End
                        | Key::PageUp
                        | Key::PageDown
                        | Key::Insert
                ))
            .then(|| name.to_owned());
        }
    };
    Some(token.to_owned())
}
