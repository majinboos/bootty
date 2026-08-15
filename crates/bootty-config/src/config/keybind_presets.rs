use super::{KeybindPreset, MacosOptionAsAltConfig};

pub(super) fn resolve_macos_option_alt_keybinds(
    keybinds: Vec<String>,
    macos_option_as_alt: MacosOptionAsAltConfig,
) -> Vec<String> {
    if !cfg!(target_os = "macos") {
        return keybinds;
    }
    keybinds
        .into_iter()
        .flat_map(|entry| expand_macos_option_alt_keybind(entry, macos_option_as_alt))
        .collect()
}

fn expand_macos_option_alt_keybind(
    entry: String,
    macos_option_as_alt: MacosOptionAsAltConfig,
) -> Vec<String> {
    let Some((trigger, action)) = split_keybind_entry(&entry) else {
        return vec![entry];
    };
    if !trigger_has_replaceable_unsided_alt(trigger) {
        return vec![entry];
    }
    let sides = match macos_option_as_alt {
        MacosOptionAsAltConfig::None => return Vec::new(),
        MacosOptionAsAltConfig::Left => &["left_alt"][..],
        MacosOptionAsAltConfig::Right => &["right_alt"][..],
        MacosOptionAsAltConfig::Both => &["left_alt", "right_alt"][..],
    };
    sides
        .iter()
        .map(|side| format!("{}={action}", replace_unsided_alt(trigger, side)))
        .collect()
}

pub fn split_keybind_entry(entry: &str) -> Option<(&str, &str)> {
    let bytes = entry.as_bytes();
    let mut offset = 0;
    while let Some(rel) = entry[offset..].find('=') {
        let index = offset + rel;
        if index + 1 < entry.len() && matches!(bytes[index + 1], b'+' | b'=') {
            offset = index + 1;
            continue;
        }
        return Some((&entry[..index], &entry[index + 1..]));
    }
    None
}

fn trigger_has_replaceable_unsided_alt(trigger: &str) -> bool {
    trigger
        .split('>')
        .any(|step| !step_has_command_modifier(step) && step.split('+').any(is_unsided_alt_token))
}

fn replace_unsided_alt(trigger: &str, side: &str) -> String {
    trigger
        .split('>')
        .map(|step| {
            if step_has_command_modifier(step) {
                return step.to_owned();
            }
            step.split('+')
                .map(|part| {
                    if is_unsided_alt_token(part) {
                        side
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(">")
}

fn step_has_command_modifier(step: &str) -> bool {
    step.split('+').any(is_command_modifier_token)
}

fn is_unsided_alt_token(token: &str) -> bool {
    matches!(token, "alt" | "opt" | "option")
}

fn is_command_modifier_token(token: &str) -> bool {
    matches!(
        token,
        "cmd"
            | "command"
            | "super"
            | "left_cmd"
            | "left_command"
            | "left_super"
            | "right_cmd"
            | "right_command"
            | "right_super"
    )
}

fn common_keybinds() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        common_keybinds_macos()
    } else if cfg!(windows) {
        common_keybinds_windows()
    } else {
        common_keybinds_other()
    }
}

// macOS uses the Command key (winit reports it as Super) for app/session shortcuts.
pub(super) fn common_keybinds_macos() -> &'static [&'static str] {
    &[
        "cmd+shift+r=reload_config",
        "cmd+-=decrease_font_size:1",
        "cmd+==increase_font_size:1",
        "cmd++=increase_font_size:1",
        "cmd+0=reset_font_size",
        "alt+shift+scroll_up=increase_font_size:0.25",
        "alt+shift+scroll_down=decrease_font_size:0.25",
        "performable:cmd+v=paste_from_clipboard",
        "shift+Enter=text:\\n",
        "cmd+alt+n=new_window",
        "cmd+shift+w=close_window",
        "cmd+w=close_surface",
        "cmd+q=quit",
        "cmd+alt+ctrl+f=toggle_fullscreen",
        "cmd+,=open_settings",
        "cmd+f=start_search",
        "cmd+p=command_palette",
        "cmd+shift+o=session_picker",
        "cmd+o=toggle_sidebar_focus",
        "cmd+shift+e=toggle_sidebar_visibility",
        "cmd+n=new_mux_session",
        "cmd+alt+r=rename_session",
        "cmd+t=new_tab",
        "ctrl+Tab=last_session",
        "ctrl+shift+Tab=last_session",
        "cmd+shift+n=next_session",
        "cmd+shift+]=next_session",
        "cmd+shift+p=previous_session",
        "cmd+shift+[=previous_session",
        "cmd+shift+,=move_session:-1",
        "cmd+shift+.=move_session:1",
        "cmd+1=select_session:1",
        "cmd+2=select_session:2",
        "cmd+3=select_session:3",
        "cmd+4=select_session:4",
        "cmd+5=select_session:5",
        "cmd+6=select_session:6",
        "cmd+7=select_session:7",
        "cmd+8=select_session:8",
        "cmd+9=select_session:9",
        "cmd+alt+x=ditch_session",
    ]
}

// Linux/Windows use Ctrl+Shift like WezTerm, because the Super/Windows key is reserved by the
// desktop environment and never reaches the app. Hand-authored (not a cmd->ctrl+shift swap):
// where macOS pairs a bare-cmd and a cmd+shift binding (w, n, p), the variants are reassigned to
// keep every Ctrl+Shift trigger unique.
pub(super) fn common_keybinds_other() -> &'static [&'static str] {
    &[
        "ctrl+shift+r=reload_config",
        "ctrl+-=decrease_font_size:1",
        "ctrl+==increase_font_size:1",
        "ctrl++=increase_font_size:1",
        "ctrl+0=reset_font_size",
        "alt+shift+scroll_up=increase_font_size:0.25",
        "alt+shift+scroll_down=decrease_font_size:0.25",
        "performable:ctrl+shift+v=paste_from_clipboard",
        "shift+Enter=text:\\n",
        "ctrl+shift+alt+n=new_window",
        "ctrl+shift+alt+w=close_window",
        "ctrl+shift+w=close_surface",
        "ctrl+shift+q=quit",
        "ctrl+shift+alt+f=toggle_fullscreen",
        "ctrl+shift+f=start_search",
        "ctrl+shift+p=command_palette",
        "ctrl+shift+alt+o=session_picker",
        "ctrl+shift+o=toggle_sidebar_focus",
        "ctrl+shift+e=toggle_sidebar_visibility",
        "ctrl+shift+n=new_mux_session",
        "ctrl+shift+alt+r=rename_session",
        "ctrl+shift+t=new_tab",
        "ctrl+Tab=last_session",
        "ctrl+shift+Tab=last_session",
        "ctrl+shift+]=next_session",
        "ctrl+shift+[=previous_session",
        "ctrl+shift+,=open_settings",
        "ctrl+shift+alt+,=move_session:-1",
        "ctrl+shift+alt+.=move_session:1",
        "ctrl+shift+1=select_session:1",
        "ctrl+shift+2=select_session:2",
        "ctrl+shift+3=select_session:3",
        "ctrl+shift+4=select_session:4",
        "ctrl+shift+5=select_session:5",
        "ctrl+shift+6=select_session:6",
        "ctrl+shift+7=select_session:7",
        "ctrl+shift+8=select_session:8",
        "ctrl+shift+9=select_session:9",
        "ctrl+shift+alt+x=ditch_session",
    ]
}

pub(super) fn common_keybinds_windows() -> &'static [&'static str] {
    &[
        "ctrl+shift+r=reload_config",
        "ctrl+-=decrease_font_size:1",
        "ctrl+==increase_font_size:1",
        "ctrl++=increase_font_size:1",
        "ctrl+0=reset_font_size",
        "alt+shift+scroll_up=increase_font_size:0.25",
        "alt+shift+scroll_down=decrease_font_size:0.25",
        "performable:ctrl+v=paste_from_clipboard",
        "performable:ctrl+shift+v=paste_from_clipboard",
        "performable:shift+Insert=paste_from_clipboard",
        "shift+Enter=text:\\n",
        "ctrl+shift+alt+n=new_window",
        "ctrl+shift+alt+w=close_window",
        "ctrl+shift+w=close_surface",
        "ctrl+shift+q=quit",
        "ctrl+shift+alt+f=toggle_fullscreen",
        "ctrl+shift+p=command_palette",
        "ctrl+shift+f=start_search",
        "ctrl+shift+alt+o=session_picker",
        "ctrl+shift+o=toggle_sidebar_focus",
        "ctrl+shift+e=toggle_sidebar_visibility",
        "ctrl+shift+n=new_mux_session",
        "ctrl+shift+alt+r=rename_session",
        "ctrl+shift+t=new_tab",
        "ctrl+Tab=last_session",
        "ctrl+shift+Tab=last_session",
        "ctrl+shift+]=next_session",
        "ctrl+shift+[=previous_session",
        "ctrl+shift+,=open_settings",
        "ctrl+shift+alt+,=move_session:-1",
        "ctrl+shift+alt+.=move_session:1",
        "ctrl+shift+1=select_session:1",
        "ctrl+shift+2=select_session:2",
        "ctrl+shift+3=select_session:3",
        "ctrl+shift+4=select_session:4",
        "ctrl+shift+5=select_session:5",
        "ctrl+shift+6=select_session:6",
        "ctrl+shift+7=select_session:7",
        "ctrl+shift+8=select_session:8",
        "ctrl+shift+9=select_session:9",
        "ctrl+shift+alt+x=ditch_session",
    ]
}

pub(super) fn sidebar_keybinds() -> &'static [&'static str] {
    &[
        "Enter=activate_session",
        "j=next_session",
        "ArrowDown=next_session",
        "ctrl+n=next_session",
        "k=previous_session",
        "ArrowUp=previous_session",
        "ctrl+p=previous_session",
    ]
}

// Bootty's own prefixed chords; the leader is remappable (input.prefix), so the triggers are
// built at load time rather than stored as static strings.
pub(super) const BOOTTY_PREFIX_KEYBINDS: &[(&str, &str)] = &[
    ("c", "new_tab"),
    ("v", "split_right"),
    ("-", "split_down"),
    ("h", "select_pane:left"),
    ("j", "select_pane:down"),
    ("k", "select_pane:up"),
    ("l", "select_pane:right"),
    ("s", "new_mux_session"),
    ("x", "ditch_session"),
    ("shift+x", "ditch_session"),
    ("r", "rename_session"),
    ("[", "copy_mode"),
    ("?", "show_keybinds"),
    ("1", "select_session:1"),
    ("2", "select_session:2"),
    ("3", "select_session:3"),
    ("4", "select_session:4"),
    ("5", "select_session:5"),
    ("6", "select_session:6"),
    ("7", "select_session:7"),
    ("8", "select_session:8"),
    ("9", "select_session:9"),
    ("shift+,", "move_tab:-1"),
    ("shift+.", "move_tab:1"),
];

// Real tmux 3.4 default key table (key-bindings.c) ported onto bootty's action vocabulary.
// tmux window ≈ bootty tab; several rows are nearest-action ports rather than exact semantics:
// `;` last-pane → next_pane, `(`/`)` switch-client → previous/next_session, `:` command-prompt
// → command_palette, `/` describe-key → show_keybinds, `C` customize-mode → open_settings,
// `]` paste-buffer → paste_from_clipboard, `w` choose-window → session_picker, `[` copy-mode
// → copy_mode, `PPage` copy-mode -u → scroll_page_up, `M-n`/`M-p` alerted-window nav
// → plain tab nav. tmux defaults
// layouts (Space, M-1..5, E), break-pane (!), detach/client chooser (d, D), display-panes (q),
// clock (t), window info (i), marks (m, M), buffers (# - =), find-window (f), select-window 0
// / by prompted index (0, '), move-window (. — tmux prompts for an absolute index while
// bootty's move_tab is a relative delta), refresh/resize (r, S-/C-/M-arrows, DC), messages (~),
// and suspend (C-z).
pub(super) const TMUX_PREFIX_KEYBINDS: &[(&str, &str)] = &[
    ("%", "split_right"),
    ("\"", "split_down"),
    ("x", "kill_pane"),
    ("z", "toggle_pane_zoom"),
    (";", "next_pane"),
    ("o", "next_pane"),
    ("ArrowUp", "select_pane:up"),
    ("ArrowDown", "select_pane:down"),
    ("ArrowLeft", "select_pane:left"),
    ("ArrowRight", "select_pane:right"),
    ("c", "new_tab"),
    ("&", "close_surface"),
    ("n", "next_tab"),
    ("p", "previous_tab"),
    ("l", "last_tab"),
    ("alt+n", "next_tab"),
    ("alt+p", "previous_tab"),
    (",", "rename_tab"),
    ("1", "select_tab:1"),
    ("2", "select_tab:2"),
    ("3", "select_tab:3"),
    ("4", "select_tab:4"),
    ("5", "select_tab:5"),
    ("6", "select_tab:6"),
    ("7", "select_tab:7"),
    ("8", "select_tab:8"),
    ("9", "select_tab:9"),
    ("$", "rename_session"),
    ("s", "session_picker"),
    ("w", "session_picker"),
    (")", "next_session"),
    ("(", "previous_session"),
    ("shift+l", "last_session"),
    (":", "command_palette"),
    ("shift+c", "open_settings"),
    ("]", "paste_from_clipboard"),
    ("[", "copy_mode"),
    ("PageUp", "scroll_page_up"),
    ("?", "show_keybinds"),
    ("/", "show_keybinds"),
];

pub(super) fn prefixed_keybinds(prefix: &str, entries: &[(&str, &str)]) -> Vec<String> {
    entries
        .iter()
        .map(|(key, action)| format!("{prefix}>{key}={action}"))
        .collect()
}

// Tab and pane navigation, handled directly by bootty's mux layer on every backend (tmux included,
// now that the tmux backend implements every command). Shared so the bindings don't depend on a
// per-backend relay to an external config.
pub(super) fn navigation_keybinds() -> &'static [&'static str] {
    &[
        "left_alt+shift+n=next_tab",
        "left_alt+shift+p=previous_tab",
        "alt+shift+]=next_tab",
        "alt+shift+[=previous_tab",
        "alt+Tab=last_tab",
        "alt+1=select_tab:1",
        "alt+2=select_tab:2",
        "alt+3=select_tab:3",
        "alt+4=select_tab:4",
        "alt+5=select_tab:5",
        "alt+6=select_tab:6",
        "alt+7=select_tab:7",
        "alt+8=select_tab:8",
        "alt+9=select_tab:9",
        "left_alt+shift+,=move_tab:-1",
        "right_alt+shift+,=move_tab:-1",
        "left_alt+shift+.=move_tab:1",
        "right_alt+shift+.=move_tab:1",
        "alt+h=select_pane:left",
        "alt+j=select_pane:down",
        "alt+k=select_pane:up",
        "alt+l=select_pane:right",
        "alt+o=next_pane",
        "alt+x=kill_pane",
        "alt+z=toggle_pane_zoom",
    ]
}

// Scroll shortcuts differ per OS: macOS scrolls with Command, Linux/Windows follow the WezTerm
// convention of Shift+PageUp/PageDown (page) and Ctrl+Shift+Arrows (line).
fn native_scroll_keybinds() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        native_scroll_keybinds_macos()
    } else {
        native_scroll_keybinds_other()
    }
}

pub(super) fn native_scroll_keybinds_macos() -> &'static [&'static str] {
    &[
        "cmd+y=copy_mode",
        "cmd+shift+y=scroll_page_down",
        "cmd+ArrowUp=scroll_page_lines:-1",
        "cmd+ArrowDown=scroll_page_lines:1",
    ]
}

pub(super) fn native_scroll_keybinds_other() -> &'static [&'static str] {
    &[
        "shift+PageUp=scroll_page_up",
        "shift+PageDown=scroll_page_down",
        "ctrl+shift+ArrowUp=scroll_page_lines:-1",
        "ctrl+shift+ArrowDown=scroll_page_lines:1",
    ]
}

// Ghostty preset: Ghostty's upstream defaults with cmux's chrome layer on top (cmux vendors
// Ghostty for terminal-level actions; where the two disagree the cmux layer wins). Direct combos
// only — this preset has no prefix concept.
fn ghostty_common_keybinds() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        ghostty_common_keybinds_macos()
    } else {
        ghostty_common_keybinds_other()
    }
}

pub(super) fn ghostty_common_keybinds_macos() -> &'static [&'static str] {
    &[
        "cmd+shift+,=reload_config",
        "cmd+,=open_settings",
        "cmd+f=start_search",
        "performable:cmd+c=copy_to_clipboard",
        "performable:cmd+v=paste_from_clipboard",
        "cmd+==increase_font_size:1",
        "cmd++=increase_font_size:1",
        "cmd+-=decrease_font_size:1",
        "cmd+0=reset_font_size",
        "alt+shift+scroll_up=increase_font_size:0.25",
        "alt+shift+scroll_down=decrease_font_size:0.25",
        "cmd+shift+p=command_palette",
        "cmd+p=session_picker",
        "cmd+q=quit",
        "ctrl+cmd+w=close_window",
        "cmd+shift+w=ditch_session",
        "cmd+w=close_surface",
        "cmd+shift+n=new_window",
        "ctrl+cmd+f=toggle_fullscreen",
        "cmd+b=toggle_sidebar_visibility",
        "cmd+shift+e=toggle_sidebar_focus",
        "cmd+o=new_mux_session",
        "cmd+Home=scroll_to_top",
        "cmd+End=scroll_to_bottom",
        "cmd+y=copy_mode",
        "cmd+PageUp=scroll_page_up",
        "cmd+PageDown=scroll_page_down",
    ]
}

// Ghostty's Linux defaults; cmux is macOS-only, so its chrome actions (sessions, sidebar,
// renames) stay unbound here and remain reachable through the command palette.
pub(super) fn ghostty_common_keybinds_other() -> &'static [&'static str] {
    &[
        "ctrl+shift+,=reload_config",
        "ctrl+,=open_settings",
        "ctrl+shift+f=start_search",
        "performable:ctrl+shift+c=copy_to_clipboard",
        "performable:ctrl+shift+v=paste_from_clipboard",
        "performable:ctrl+Insert=copy_to_clipboard",
        "performable:shift+Insert=paste_from_clipboard",
        "ctrl+==increase_font_size:1",
        "ctrl++=increase_font_size:1",
        "ctrl+-=decrease_font_size:1",
        "ctrl+0=reset_font_size",
        "alt+shift+scroll_up=increase_font_size:0.25",
        "alt+shift+scroll_down=decrease_font_size:0.25",
        "ctrl+shift+p=command_palette",
        "ctrl+shift+q=quit",
        "ctrl+shift+w=close_surface",
        "ctrl+shift+n=new_window",
        "ctrl+Enter=toggle_fullscreen",
        "shift+Home=scroll_to_top",
        "shift+End=scroll_to_bottom",
        "shift+PageUp=scroll_page_up",
        "shift+PageDown=scroll_page_down",
    ]
}

fn ghostty_layout_keybinds() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        ghostty_layout_keybinds_macos()
    } else {
        ghostty_layout_keybinds_other()
    }
}

// cmux's Cmd+1-9 = workspace (bootty session) wins over Ghostty's Cmd+1-8 = goto_tab. Ctrl+1-9
// are global space selection defaults. Cmd+[/] follow Ghostty's goto_split previous/next.
pub(super) fn ghostty_layout_keybinds_macos() -> &'static [&'static str] {
    &[
        "cmd+n=new_mux_session",
        "cmd+t=new_tab",
        "cmd+alt+w=close_surface",
        "cmd+d=split_right",
        "cmd+shift+d=split_down",
        "cmd+shift+Enter=toggle_pane_zoom",
        "ctrl+Tab=next_tab",
        "ctrl+shift+Tab=previous_tab",
        "cmd+shift+]=next_tab",
        "cmd+shift+[=previous_tab",
        "cmd+]=next_pane",
        "cmd+[=previous_pane",
        "alt+cmd+ArrowLeft=select_pane:left",
        "alt+cmd+ArrowRight=select_pane:right",
        "alt+cmd+ArrowUp=select_pane:up",
        "alt+cmd+ArrowDown=select_pane:down",
        "cmd+1=select_session:1",
        "cmd+2=select_session:2",
        "cmd+3=select_session:3",
        "cmd+4=select_session:4",
        "cmd+5=select_session:5",
        "cmd+6=select_session:6",
        "cmd+7=select_session:7",
        "cmd+8=select_session:8",
        "cmd+9=select_session:9",
        "ctrl+cmd+]=next_session",
        "ctrl+cmd+[=previous_session",
        "cmd+r=rename_tab",
        "cmd+shift+r=rename_session",
    ]
}

pub(super) fn ghostty_layout_keybinds_other() -> &'static [&'static str] {
    &[
        "ctrl+shift+t=new_tab",
        "ctrl+shift+o=split_right",
        "ctrl+shift+e=split_down",
        "ctrl+shift+Enter=toggle_pane_zoom",
        "ctrl+Tab=next_tab",
        "ctrl+shift+Tab=previous_tab",
        "ctrl+PageDown=next_tab",
        "ctrl+PageUp=previous_tab",
        "performable:ctrl+shift+ArrowLeft=previous_tab",
        "performable:ctrl+shift+ArrowRight=next_tab",
        "alt+1=select_tab:1",
        "alt+2=select_tab:2",
        "alt+3=select_tab:3",
        "alt+4=select_tab:4",
        "alt+5=select_tab:5",
        "alt+6=select_tab:6",
        "alt+7=select_tab:7",
        "alt+8=select_tab:8",
        "alt+9=last_tab",
        "ctrl+alt+ArrowLeft=select_pane:left",
        "ctrl+alt+ArrowRight=select_pane:right",
        "ctrl+alt+ArrowUp=select_pane:up",
        "ctrl+alt+ArrowDown=select_pane:down",
    ]
}

pub(super) fn tmux_keybinds() -> &'static [&'static str] {
    &[
        "cmd+;=csi:61~",
        "cmd+ctrl+n=csi:68~",
        "ctrl+alt+[=csi:69~",
        "cmd+y=csi:71~",
        "cmd+c=csi:72~",
        "cmd+j=csi:90;1~",
        "cmd+s=csi:90;2~",
        "cmd+shift+c=csi:90;3~",
        "cmd+alt+shift+c=csi:90;4~",
        "cmd+.=csi:90;6~",
        "cmd+e=csi:90;7~",
        "cmd+b=csi:90;8~",
        "cmd+i=csi:90;9~",
        "cmd+l=csi:90;10~",
        "cmd+shift+i=csi:90;11~",
        "cmd+k=csi:90;12~",
        "cmd+alt+v=csi:90;13~",
        "cmd+d=csi:90;14~",
        "cmd+shift+d=csi:90;15~",
        "cmd+u=csi:90;16~",
        "cmd+shift+u=csi:90;17~",
        "cmd+alt+k=csi:90;18~",
        "cmd+alt+j=csi:90;19~",
        "cmd+alt+shift+k=csi:90;20~",
        "cmd+alt+shift+j=csi:90;21~",
        // Non-navigation tmux actions still relay to the user's tmux config; tab/pane navigation is
        // handled by bootty directly (see navigation_keybinds).
        "alt+\\=esc:\\",
        "alt+shift+c=esc:C",
        "ctrl+alt+]=text:\\x1b\\x1d",
        "alt+r=esc:R",
    ]
}

/// The raw control byte a `ctrl+space`/`ctrl+letter` prefix produces in a terminal; `None` for
/// prefixes outside that family.
fn prefix_control_byte(prefix: &str) -> Option<u8> {
    let key = prefix.strip_prefix("ctrl+")?;
    if key == "space" {
        return Some(0);
    }
    let [letter] = key.as_bytes() else {
        return None;
    };
    letter.is_ascii_lowercase().then(|| letter - b'a' + 1)
}

// The external tmux must receive its prefix as the raw control byte even when bootty's own
// direct-input path wouldn't encode it (ctrl+space -> NUL). Prefixes outside the ctrl+key
// family already reach the terminal unmodified, so no passthrough entry is needed for them.
pub(super) fn prefix_passthrough_keybind(prefix: &str) -> Option<String> {
    let byte = prefix_control_byte(prefix)?;
    Some(format!("{prefix}=text:\\x{byte:02x}"))
}

// tmux's `send-prefix` (prefix pressed twice): deliver the literal prefix byte to the terminal.
fn send_prefix_keybind(prefix: &str) -> Option<String> {
    let byte = prefix_control_byte(prefix)?;
    Some(format!("{prefix}>{prefix}=text:\\x{byte:02x}"))
}

pub(super) fn owned_keybinds(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_owned()).collect()
}

pub(super) fn preset_global_keybinds(preset: KeybindPreset) -> Vec<String> {
    let mut keybinds = match preset {
        // Tmux reuses Bootty's chrome — tmux itself has no opinion outside its prefix table.
        KeybindPreset::Bootty | KeybindPreset::Tmux => {
            let mut keybinds = owned_keybinds(common_keybinds());
            keybinds.extend(owned_keybinds(navigation_keybinds()));
            keybinds
        }
        KeybindPreset::Ghostty => owned_keybinds(ghostty_common_keybinds()),
    };
    if cfg!(target_os = "macos") {
        keybinds.extend((1..=9).map(|index| format!("ctrl+{index}=select_space:{index}")));
    }
    keybinds
}

pub(super) fn preset_layout_keybinds(preset: KeybindPreset, prefix: Option<&str>) -> Vec<String> {
    let table = match preset {
        KeybindPreset::Ghostty => return owned_keybinds(ghostty_layout_keybinds()),
        KeybindPreset::Bootty => BOOTTY_PREFIX_KEYBINDS,
        KeybindPreset::Tmux => TMUX_PREFIX_KEYBINDS,
    };
    // effective_prefix is always Some for prefixed presets; the fallback keeps this total.
    let prefix = prefix
        .or(preset.default_prefix())
        .expect("prefixed presets define a default prefix");
    let mut keybinds = prefixed_keybinds(prefix, table);
    if preset == KeybindPreset::Tmux {
        keybinds.extend(send_prefix_keybind(prefix));
    }
    keybinds.extend(owned_keybinds(native_scroll_keybinds()));
    keybinds
}

pub(super) fn preset_tmux_backend_keybinds(
    preset: KeybindPreset,
    prefix: Option<&str>,
) -> Vec<String> {
    match preset {
        KeybindPreset::Bootty => {
            let mut keybinds = owned_keybinds(tmux_keybinds());
            keybinds.extend(prefix.and_then(prefix_passthrough_keybind));
            keybinds
        }
        // No relay layer. For the Tmux preset the emptiness is load-bearing: an unbound prefix
        // passes through as raw input, so the external tmux handles its own prefix natively.
        KeybindPreset::Ghostty | KeybindPreset::Tmux => Vec::new(),
    }
}
