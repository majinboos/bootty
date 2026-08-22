use std::{fmt::Write as _, str::FromStr};

use crate::terminal::{KeyInput, KeyMods, TerminalKey};

#[derive(Clone, Debug, PartialEq)]
pub struct InputBinding {
    pub trigger: BindingTrigger,
    pub action: BindingAction,
    pub flags: BindingFlags,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingElement {
    Leader(BindingTrigger),
    Binding(InputBinding),
    Chain(BindingAction),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindingFlags {
    pub consumed: bool,
    pub all: bool,
    pub global: bool,
    pub performable: bool,
}

impl Default for BindingFlags {
    fn default() -> Self {
        Self {
            consumed: true,
            all: false,
            global: false,
            performable: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingModSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BindingMods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub command: bool,
    pub shift_side: Option<BindingModSide>,
    pub ctrl_side: Option<BindingModSide>,
    pub alt_side: Option<BindingModSide>,
    pub command_side: Option<BindingModSide>,
}

impl BindingMods {
    fn from_key_mods_with_sides(value: KeyMods) -> Self {
        Self {
            shift: value.shift,
            ctrl: value.ctrl,
            alt: value.alt,
            command: value.command,
            shift_side: side_for_key_mod(value.shift, value.right_shift),
            ctrl_side: side_for_key_mod(value.ctrl, value.right_ctrl),
            alt_side: side_for_key_mod(value.alt, value.right_alt),
            command_side: side_for_key_mod(value.command, value.right_command),
        }
    }

    pub fn without_side_constraints(mut self) -> Self {
        self.shift_side = None;
        self.ctrl_side = None;
        self.alt_side = None;
        self.command_side = None;
        self
    }

    fn input_candidates(value: KeyMods) -> Vec<Self> {
        let sided = Self::from_key_mods_with_sides(value);
        let mut candidates = vec![sided];
        if sided.shift_side.is_some() {
            Self::push_without_side(&mut candidates, |mods| mods.shift_side = None);
        }
        if sided.ctrl_side.is_some() {
            Self::push_without_side(&mut candidates, |mods| mods.ctrl_side = None);
        }
        if sided.alt_side.is_some() {
            Self::push_without_side(&mut candidates, |mods| mods.alt_side = None);
        }
        if sided.command_side.is_some() {
            Self::push_without_side(&mut candidates, |mods| mods.command_side = None);
        }
        candidates
    }

    fn push_without_side(candidates: &mut Vec<Self>, clear_side: impl Fn(&mut Self)) {
        let existing_count = candidates.len();
        for index in 0..existing_count {
            let mut candidate = candidates[index];
            clear_side(&mut candidate);
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
}

impl From<KeyMods> for BindingMods {
    fn from(value: KeyMods) -> Self {
        Self::from_key_mods_with_sides(value).without_side_constraints()
    }
}

fn side_for_key_mod(pressed: bool, right: bool) -> Option<BindingModSide> {
    pressed.then_some(if right {
        BindingModSide::Right
    } else {
        BindingModSide::Left
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingTrigger {
    pub mods: BindingMods,
    pub key: BindingKey,
}

impl BindingTrigger {
    pub fn from_key_input(input: KeyInput) -> Self {
        Self {
            mods: input.mods.into(),
            key: BindingKey::Physical(input.key),
        }
    }

    pub fn from_key_input_with_modifier_sides(input: KeyInput) -> Self {
        Self {
            mods: BindingMods::from_key_mods_with_sides(input.mods),
            key: BindingKey::Physical(input.key),
        }
    }

    /// Wheel counterpart to [`Self::from_key_input_with_modifier_sides`]: a `scroll_up` /
    /// `scroll_down` trigger carrying the left/right side of every held modifier. Callers that want
    /// a side-agnostic trigger follow with [`BindingMods::without_side_constraints`].
    pub fn from_scroll_with_modifier_sides(up: bool, mods: KeyMods) -> Self {
        Self {
            mods: BindingMods::from_key_mods_with_sides(mods),
            key: if up {
                BindingKey::ScrollUp
            } else {
                BindingKey::ScrollDown
            },
        }
    }

    pub fn input_mod_candidates(input: KeyInput) -> Vec<BindingMods> {
        BindingMods::input_candidates(input.mods)
    }

    pub fn format_entry(&self) -> String {
        let mut output = String::new();
        if self.mods.command {
            push_binding_part(&mut output, mod_token("cmd", self.mods.command_side));
        }
        if self.mods.ctrl {
            push_binding_part(&mut output, mod_token("ctrl", self.mods.ctrl_side));
        }
        if self.mods.alt {
            push_binding_part(&mut output, mod_token("alt", self.mods.alt_side));
        }
        if self.mods.shift {
            push_binding_part(&mut output, mod_token("shift", self.mods.shift_side));
        }
        if !output.is_empty() {
            output.push('+');
        }
        self.key.push_format_entry(&mut output);
        output
    }
}

fn mod_token(base: &'static str, side: Option<BindingModSide>) -> &'static str {
    match (base, side) {
        ("cmd", Some(BindingModSide::Left)) => "left_cmd",
        ("cmd", Some(BindingModSide::Right)) => "right_cmd",
        ("ctrl", Some(BindingModSide::Left)) => "left_ctrl",
        ("ctrl", Some(BindingModSide::Right)) => "right_ctrl",
        ("alt", Some(BindingModSide::Left)) => "left_alt",
        ("alt", Some(BindingModSide::Right)) => "right_alt",
        ("shift", Some(BindingModSide::Left)) => "left_shift",
        ("shift", Some(BindingModSide::Right)) => "right_shift",
        _ => base,
    }
}

fn push_binding_part(output: &mut String, part: &str) {
    if !output.is_empty() {
        output.push('+');
    }
    output.push_str(part);
}

impl FromStr for BindingTrigger {
    type Err = BindingParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut mods = BindingMods::default();
        let mut key = None;

        for part in split_trigger_parts(input)? {
            match part {
                "shift" => set_mod(&mut mods.shift)?,
                "ctrl" | "control" => set_mod(&mut mods.ctrl)?,
                "alt" | "opt" | "option" => set_mod(&mut mods.alt)?,
                "cmd" | "command" | "super" => set_mod(&mut mods.command)?,
                "left_shift" => {
                    set_sided_mod(&mut mods.shift, &mut mods.shift_side, BindingModSide::Left)?
                }
                "right_shift" => {
                    set_sided_mod(&mut mods.shift, &mut mods.shift_side, BindingModSide::Right)?
                }
                "left_ctrl" | "left_control" => {
                    set_sided_mod(&mut mods.ctrl, &mut mods.ctrl_side, BindingModSide::Left)?
                }
                "right_ctrl" | "right_control" => {
                    set_sided_mod(&mut mods.ctrl, &mut mods.ctrl_side, BindingModSide::Right)?
                }
                "left_alt" | "left_opt" | "left_option" => {
                    set_sided_mod(&mut mods.alt, &mut mods.alt_side, BindingModSide::Left)?
                }
                "right_alt" | "right_opt" | "right_option" => {
                    set_sided_mod(&mut mods.alt, &mut mods.alt_side, BindingModSide::Right)?
                }
                "left_cmd" | "left_command" | "left_super" => set_sided_mod(
                    &mut mods.command,
                    &mut mods.command_side,
                    BindingModSide::Left,
                )?,
                "right_cmd" | "right_command" | "right_super" => set_sided_mod(
                    &mut mods.command,
                    &mut mods.command_side,
                    BindingModSide::Right,
                )?,
                "catch_all" => set_key(&mut key, BindingKey::CatchAll)?,
                _ => set_key(&mut key, BindingKey::parse(part)?)?,
            }
        }

        Ok(Self {
            mods,
            key: key.ok_or(BindingParseError::InvalidFormat)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingKey {
    Unicode(char),
    Physical(TerminalKey),
    ScrollUp,
    ScrollDown,
    CatchAll,
}

impl BindingKey {
    fn parse(input: &str) -> Result<Self, BindingParseError> {
        if let Some(legacy) = input.strip_prefix("physical:") {
            return Ok(Self::Physical(parse_legacy_physical_key(legacy)?));
        }
        if let Some(key) = parse_physical_key(input)? {
            return Ok(Self::Physical(key));
        }
        if input.eq_ignore_ascii_case("scroll_up") {
            return Ok(Self::ScrollUp);
        }
        if input.eq_ignore_ascii_case("scroll_down") {
            return Ok(Self::ScrollDown);
        }
        if input.eq_ignore_ascii_case("space") {
            return Ok(Self::Unicode(' '));
        }
        let mut chars = input.chars();
        let Some(ch) = chars.next() else {
            return Err(BindingParseError::InvalidFormat);
        };
        if chars.next().is_some() {
            return Err(BindingParseError::InvalidFormat);
        }
        Ok(Self::Unicode(ch))
    }

    fn push_format_entry(&self, output: &mut String) {
        match self {
            Self::Unicode(ch) => output.push(*ch),
            Self::Physical(key) => match physical_key_name(*key) {
                Some(name) => output.push_str(name),
                None => {
                    let _ = write!(output, "{key:?}");
                }
            },
            Self::ScrollUp => output.push_str("scroll_up"),
            Self::ScrollDown => output.push_str("scroll_down"),
            Self::CatchAll => output.push_str("catch_all"),
        }
    }
}

macro_rules! format_binding_action {
    (unit, $name:literal) => {
        $name.to_owned()
    };
    (required_string, $name:literal, $value:ident) => {
        format!("{}:{}", $name, $value)
    };
    (escaped_string, $name:literal, $value:ident) => {
        format!("{}:{}", $name, format_text_bytes($value))
    };
    (nested_enum($parse:path), $name:literal, $value:ident) => {
        format!("{}:{}", $name, $value.as_str())
    };
    (copy_to_clipboard, $name:literal, $value:ident) => {
        format!("{}:{}", $name, $value.as_str())
    };
    (write_screen, $name:literal, $value:ident) => {
        format!("{}:{}", $name, $value.format_entry())
    };
    ($kind:ident $(($parse:path))?, $name:literal, $value:ident) => {
        format!("{}:{}", $name, $value)
    };
}

macro_rules! parse_binding_action {
    (unit, $value:ident, $variant:path) => {
        parse_unit($value, $variant)
    };
    (required_string, $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant(value.to_owned())))
    };
    (escaped_string, $value:ident, $variant:path) => {
        parse_binding_action!(required_string, $value, $variant)
    };
    (finite_number, $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant(parse_f32(value)?)))
    };
    (positive_index, $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant(parse_u32(value)?)))
    };
    (number($parse:path), $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant($parse(value)?)))
    };
    (nested_enum($parse:path), $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant($parse(value)?)))
    };
    (write_screen, $value:ident, $variant:path) => {
        parse_required($value, |value| Ok($variant(WriteScreen::parse(value)?)))
    };
    (copy_to_clipboard, $value:ident, $variant:path) => {
        match $value {
            Some(value) => Ok($variant(CopyToClipboard::parse(value)?)),
            None => Ok($variant(CopyToClipboard::default())),
        }
    };
}

macro_rules! binding_actions {
    (
        $(
            $variant:ident $(($binding:ident: $ty:ty))?
                => $name:literal [$kind:ident $(($parse:path))?],
        )+
    ) => {
        #[derive(Clone, Debug, PartialEq)]
        pub enum BindingAction {
            $($variant $(($ty))?,)+
        }

        impl BindingAction {
            pub fn format_entry(&self) -> String {
                match self {
                    $(
                        Self::$variant $(($binding))? => format_binding_action!(
                            $kind $(($parse))?, $name $(, $binding)?
                        ),
                    )+
                }
            }
        }

        pub fn parse_action(input: &str) -> Result<BindingAction, BindingParseError> {
            let (name, value) = match input.split_once(':') {
                Some((name, value)) => (name, Some(value)),
                None => (input, None),
            };
            match name {
                $(
                    $name => parse_binding_action!(
                        $kind $(($parse))?, value, BindingAction::$variant
                    ),
                )+
                _ => Err(BindingParseError::InvalidAction),
            }
        }
    };
}

binding_actions! {
    Ignore => "ignore" [unit],
    Unbind => "unbind" [unit],
    Reset => "reset" [unit],
    ReloadConfig => "reload_config" [unit],
    NewWindow => "new_window" [unit],
    NewMuxSession => "new_mux_session" [unit],
    SessionPicker => "session_picker" [unit],
    CommandPalette => "command_palette" [unit],
    CloseWindow => "close_window" [unit],
    CloseSurface => "close_surface" [unit],
    Quit => "quit" [unit],
    ToggleFullscreen => "toggle_fullscreen" [unit],
    ToggleSidebarFocus => "toggle_sidebar_focus" [unit],
    ToggleSidebarVisibility => "toggle_sidebar_visibility" [unit],
    OpenSettings => "open_settings" [unit],
    ChangeAppearance(value: AppearanceChoice)
        => "change_appearance" [nested_enum(AppearanceChoice::parse)],
    SwitchTheme => "switch_theme" [unit],
    Csi(value: String) => "csi" [required_string],
    Esc(value: String) => "esc" [required_string],
    Text(value: String) => "text" [escaped_string],
    Search(value: String) => "search" [escaped_string],
    SearchSelection => "search_selection" [unit],
    NavigateSearch(value: NavigateSearch)
        => "navigate_search" [nested_enum(NavigateSearch::parse)],
    StartSearch => "start_search" [unit],
    EndSearch => "end_search" [unit],
    CopyToClipboard(value: CopyToClipboard) => "copy_to_clipboard" [copy_to_clipboard],
    CopyUrlToClipboard => "copy_url_to_clipboard" [unit],
    CopyTitleToClipboard => "copy_title_to_clipboard" [unit],
    PasteFromClipboard => "paste_from_clipboard" [unit],
    CopyMode => "copy_mode" [unit],
    PasteFromSelection => "paste_from_selection" [unit],
    IncreaseFontSize(value: f32) => "increase_font_size" [finite_number],
    DecreaseFontSize(value: f32) => "decrease_font_size" [finite_number],
    ResetFontSize => "reset_font_size" [unit],
    SetFontSize(value: f32) => "set_font_size" [finite_number],
    SetSurfaceTitle(value: String) => "set_surface_title" [escaped_string],
    SetTabTitle(value: String) => "set_tab_title" [escaped_string],
    ClearScreen => "clear_screen" [unit],
    SelectAll => "select_all" [unit],
    ScrollToTop => "scroll_to_top" [unit],
    ScrollToBottom => "scroll_to_bottom" [unit],
    ScrollToSelection => "scroll_to_selection" [unit],
    ScrollToRow(value: usize) => "scroll_to_row" [number(parse_usize)],
    ScrollPageUp => "scroll_page_up" [unit],
    ScrollPageDown => "scroll_page_down" [unit],
    ScrollPageFractional(value: f32) => "scroll_page_fractional" [finite_number],
    ScrollPageLines(value: i16) => "scroll_page_lines" [number(parse_i16)],
    AdjustSelection(value: AdjustSelection)
        => "adjust_selection" [nested_enum(AdjustSelection::parse)],
    JumpToPrompt(value: i16) => "jump_to_prompt" [number(parse_i16)],
    WriteScrollbackFile(value: WriteScreen) => "write_scrollback_file" [write_screen],
    NewTab => "new_tab" [unit],
    NextTab => "next_tab" [unit],
    PreviousTab => "previous_tab" [unit],
    LastTab => "last_tab" [unit],
    SelectTab(value: u32) => "select_tab" [positive_index],
    MoveTab(value: i32) => "move_tab" [number(parse_i32)],
    SplitRight => "split_right" [unit],
    SplitDown => "split_down" [unit],
    SelectPane(value: PaneDirection) => "select_pane" [nested_enum(PaneDirection::parse)],
    NextPane => "next_pane" [unit],
    PreviousPane => "previous_pane" [unit],
    KillPane => "kill_pane" [unit],
    TogglePaneZoom => "toggle_pane_zoom" [unit],
    NextSession => "next_session" [unit],
    PreviousSession => "previous_session" [unit],
    CreateSpace => "create_space" [unit],
    CloseSpace => "close_space" [unit],
    EditSpace => "edit_space" [unit],
    NextSpace => "next_space" [unit],
    PreviousSpace => "previous_space" [unit],
    SelectSpace(value: u32) => "select_space" [positive_index],
    LastSession => "last_session" [unit],
    SelectSession(value: u32) => "select_session" [positive_index],
    MoveSession(value: i32) => "move_session" [number(parse_i32)],
    DitchSession => "ditch_session" [unit],
    RenameSession => "rename_session" [unit],
    MoveSessionToSpace => "move_session_to_space" [unit],
    RenameTab => "rename_tab" [unit],
    ShowKeybinds => "show_keybinds" [unit],
    WriteScreenFile(value: WriteScreen) => "write_screen_file" [write_screen],
    WriteSelectionFile(value: WriteScreen) => "write_selection_file" [write_screen],
    ToggleMouseReporting => "toggle_mouse_reporting" [unit],
    EndKeySequence => "end_key_sequence" [unit],
    ActivateKeyTable(value: String) => "activate_key_table" [escaped_string],
    ActivateKeyTableOnce(value: String) => "activate_key_table_once" [escaped_string],
    DeactivateKeyTable => "deactivate_key_table" [unit],
    DeactivateAllKeyTables => "deactivate_all_key_tables" [unit],
}

macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident => $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            fn parse(input: &str) -> Result<Self, BindingParseError> {
                match input {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(BindingParseError::InvalidFormat),
                }
            }

            fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }
        }
    };
}

string_enum! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum CopyToClipboard {
        Plain => "plain",
        Vt => "vt",
        Html => "html",
        #[default]
        Mixed => "mixed",
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum NavigateSearch {
        Previous => "previous",
        Next => "next",
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum PaneDirection {
        Left => "left",
        Down => "down",
        Up => "up",
        Right => "right",
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AppearanceChoice {
        System => "system",
        Light => "light",
        Dark => "dark",
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum AdjustSelection {
        Left => "left",
        Right => "right",
        Up => "up",
        Down => "down",
        PageUp => "page_up",
        PageDown => "page_down",
        Home => "home",
        End => "end",
        BeginningOfLine => "beginning_of_line",
        EndOfLine => "end_of_line",
    }
}

macro_rules! physical_keys {
    ($($canonical:literal | $alias:literal => $key:path,)+) => {
        fn physical_key_name(key: TerminalKey) -> Option<&'static str> {
            Some(match key {
                $($key => $canonical,)+
                _ => return None,
            })
        }

        fn parse_physical_key(input: &str) -> Result<Option<TerminalKey>, BindingParseError> {
            Ok(Some(match input {
                $($canonical | $alias => $key,)+
                _ if input.starts_with("Key") || input.starts_with("Digit") => {
                    return Err(BindingParseError::InvalidFormat);
                }
                _ => return Ok(None),
            }))
        }

    };
}

physical_keys! {
    "KeyA" | "key_a" => TerminalKey::A,
    "KeyB" | "key_b" => TerminalKey::B,
    "KeyC" | "key_c" => TerminalKey::C,
    "KeyD" | "key_d" => TerminalKey::D,
    "KeyE" | "key_e" => TerminalKey::E,
    "KeyF" | "key_f" => TerminalKey::F,
    "KeyG" | "key_g" => TerminalKey::G,
    "KeyH" | "key_h" => TerminalKey::H,
    "KeyI" | "key_i" => TerminalKey::I,
    "KeyJ" | "key_j" => TerminalKey::J,
    "KeyK" | "key_k" => TerminalKey::K,
    "KeyL" | "key_l" => TerminalKey::L,
    "KeyM" | "key_m" => TerminalKey::M,
    "KeyN" | "key_n" => TerminalKey::N,
    "KeyO" | "key_o" => TerminalKey::O,
    "KeyP" | "key_p" => TerminalKey::P,
    "KeyQ" | "key_q" => TerminalKey::Q,
    "KeyR" | "key_r" => TerminalKey::R,
    "KeyS" | "key_s" => TerminalKey::S,
    "KeyT" | "key_t" => TerminalKey::T,
    "KeyU" | "key_u" => TerminalKey::U,
    "KeyV" | "key_v" => TerminalKey::V,
    "KeyW" | "key_w" => TerminalKey::W,
    "KeyX" | "key_x" => TerminalKey::X,
    "KeyY" | "key_y" => TerminalKey::Y,
    "KeyZ" | "key_z" => TerminalKey::Z,
    "Digit0" | "digit_0" => TerminalKey::Digit0,
    "Digit1" | "digit_1" => TerminalKey::Digit1,
    "Digit2" | "digit_2" => TerminalKey::Digit2,
    "Digit3" | "digit_3" => TerminalKey::Digit3,
    "Digit4" | "digit_4" => TerminalKey::Digit4,
    "Digit5" | "digit_5" => TerminalKey::Digit5,
    "Digit6" | "digit_6" => TerminalKey::Digit6,
    "Digit7" | "digit_7" => TerminalKey::Digit7,
    "Digit8" | "digit_8" => TerminalKey::Digit8,
    "Digit9" | "digit_9" => TerminalKey::Digit9,
    "Backquote" | "backquote" => TerminalKey::Backquote,
    "Backslash" | "backslash" => TerminalKey::Backslash,
    "BracketLeft" | "bracket_left" => TerminalKey::BracketLeft,
    "BracketRight" | "bracket_right" => TerminalKey::BracketRight,
    "Comma" | "comma" => TerminalKey::Comma,
    "Equal" | "equal" => TerminalKey::Equal,
    "Minus" | "minus" => TerminalKey::Minus,
    "Period" | "period" => TerminalKey::Period,
    "Quote" | "quote" => TerminalKey::Quote,
    "Semicolon" | "semicolon" => TerminalKey::Semicolon,
    "Slash" | "slash" => TerminalKey::Slash,
    "ArrowUp" | "arrow_up" => TerminalKey::ArrowUp,
    "ArrowDown" | "arrow_down" => TerminalKey::ArrowDown,
    "ArrowRight" | "arrow_right" => TerminalKey::ArrowRight,
    "ArrowLeft" | "arrow_left" => TerminalKey::ArrowLeft,
    "Delete" | "delete" => TerminalKey::Delete,
    "Home" | "home" => TerminalKey::Home,
    "End" | "end" => TerminalKey::End,
    "PageUp" | "page_up" => TerminalKey::PageUp,
    "PageDown" | "page_down" => TerminalKey::PageDown,
    "Space" | "space" => TerminalKey::Space,
    "Insert" | "insert" => TerminalKey::Insert,
    "Enter" | "enter" => TerminalKey::Enter,
    "Tab" | "tab" => TerminalKey::Tab,
    "Backspace" | "backspace" => TerminalKey::Backspace,
    "Escape" | "escape" => TerminalKey::Escape,
    "F1" | "f1" => TerminalKey::F1,
    "F2" | "f2" => TerminalKey::F2,
    "F3" | "f3" => TerminalKey::F3,
    "F4" | "f4" => TerminalKey::F4,
    "F5" | "f5" => TerminalKey::F5,
    "F6" | "f6" => TerminalKey::F6,
    "F7" | "f7" => TerminalKey::F7,
    "F8" | "f8" => TerminalKey::F8,
    "F9" | "f9" => TerminalKey::F9,
    "F10" | "f10" => TerminalKey::F10,
    "F11" | "f11" => TerminalKey::F11,
    "F12" | "f12" => TerminalKey::F12,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteScreen {
    pub action: WriteScreenAction,
    pub emit: WriteScreenFormat,
}

impl WriteScreen {
    fn parse(input: &str) -> Result<Self, BindingParseError> {
        let (action, emit) = match input.split_once(',') {
            Some((action, emit)) if !action.is_empty() && !emit.is_empty() => {
                if emit.contains(',') {
                    return Err(BindingParseError::InvalidFormat);
                }
                (action, WriteScreenFormat::parse(emit)?)
            }
            Some(_) => return Err(BindingParseError::InvalidFormat),
            None => (input, WriteScreenFormat::Plain),
        };
        Ok(Self {
            action: WriteScreenAction::parse(action)?,
            emit,
        })
    }

    fn format_entry(self) -> String {
        format!("{},{}", self.action.as_str(), self.emit.as_str())
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WriteScreenAction {
        Copy => "copy",
        Paste => "paste",
        Open => "open",
    }
}

string_enum! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WriteScreenFormat {
        Plain => "plain",
        Vt => "vt",
        Html => "html",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingParseError {
    InvalidFormat,
    InvalidAction,
}

pub fn parse_binding(input: &str) -> Result<InputBinding, BindingParseError> {
    let (flags, input) = parse_flags(input)?;
    let (trigger, action) = split_binding(input)?;
    Ok(InputBinding {
        trigger: trigger.parse()?,
        action: parse_action(action)?,
        flags,
    })
}

pub fn parse_binding_elements(input: &str) -> Result<Vec<BindingElement>, BindingParseError> {
    let (flags, input) = parse_flags(input)?;
    let (trigger, action) = split_binding(input)?;
    let action = parse_action(action)?;
    if trigger == "chain" {
        if flags != BindingFlags::default() {
            return Err(BindingParseError::InvalidFormat);
        }
        return Ok(vec![BindingElement::Chain(action)]);
    }

    let triggers = parse_trigger_sequence(trigger)?;
    if triggers.len() > 1 && (flags.global || flags.all) {
        return Err(BindingParseError::InvalidFormat);
    }
    let last_index = triggers.len() - 1;
    Ok(triggers
        .into_iter()
        .enumerate()
        .map(|(index, trigger)| {
            if index == last_index {
                BindingElement::Binding(InputBinding {
                    trigger,
                    action: action.clone(),
                    flags,
                })
            } else {
                BindingElement::Leader(trigger)
            }
        })
        .collect())
}

fn parse_flags(mut input: &str) -> Result<(BindingFlags, &str), BindingParseError> {
    let mut flags = BindingFlags::default();
    loop {
        let Some((prefix, rest)) = input.split_once(':') else {
            return Ok((flags, input));
        };
        match prefix {
            "all" if !flags.all => flags.all = true,
            "global" if !flags.global => flags.global = true,
            "unconsumed" if flags.consumed => flags.consumed = false,
            "performable" if !flags.performable => flags.performable = true,
            "all" | "global" | "unconsumed" | "performable" => {
                return Err(BindingParseError::InvalidFormat);
            }
            _ => return Ok((flags, input)),
        }
        input = rest;
    }
}

fn split_binding(input: &str) -> Result<(&str, &str), BindingParseError> {
    let mut offset = 0;
    while let Some(index) = input[offset..].find('=') {
        let index = offset + index;
        if index + 1 < input.len() && matches!(input.as_bytes()[index + 1], b'+' | b'=') {
            offset = index + 1;
            continue;
        }
        return Ok((&input[..index], &input[index + 1..]));
    }
    Err(BindingParseError::InvalidFormat)
}

fn parse_unit(
    value: Option<&str>,
    action: BindingAction,
) -> Result<BindingAction, BindingParseError> {
    match value {
        None => Ok(action),
        Some(_) => Err(BindingParseError::InvalidFormat),
    }
}

fn parse_i32(input: &str) -> Result<i32, BindingParseError> {
    input
        .parse::<i32>()
        .map_err(|_| BindingParseError::InvalidFormat)
}

fn parse_u32(input: &str) -> Result<u32, BindingParseError> {
    let value = input
        .parse::<u32>()
        .map_err(|_| BindingParseError::InvalidFormat)?;
    if value > 0 {
        Ok(value)
    } else {
        Err(BindingParseError::InvalidFormat)
    }
}
fn parse_required(
    value: Option<&str>,
    parse: impl FnOnce(&str) -> Result<BindingAction, BindingParseError>,
) -> Result<BindingAction, BindingParseError> {
    value.map_or(Err(BindingParseError::InvalidFormat), parse)
}

fn parse_f32(input: &str) -> Result<f32, BindingParseError> {
    let value = input
        .parse::<f32>()
        .map_err(|_| BindingParseError::InvalidFormat)?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(BindingParseError::InvalidFormat)
    }
}

fn parse_i16(input: &str) -> Result<i16, BindingParseError> {
    input
        .parse::<i16>()
        .map_err(|_| BindingParseError::InvalidFormat)
}

fn parse_usize(input: &str) -> Result<usize, BindingParseError> {
    input
        .parse::<usize>()
        .map_err(|_| BindingParseError::InvalidFormat)
}

fn format_text_bytes(input: &str) -> String {
    let mut output = String::new();
    for byte in input.bytes() {
        match byte {
            b' '..=b'~' => output.push(char::from(byte)),
            _ => {
                let _ = write!(output, "\\x{byte:02x}");
            }
        }
    }
    output
}

fn split_trigger_parts(input: &str) -> Result<Vec<&str>, BindingParseError> {
    if input.is_empty() {
        return Err(BindingParseError::InvalidFormat);
    }
    let bytes = input.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' && i != start {
            parts.push(&input[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    parts.push(&input[start..]);
    if parts.iter().any(|part| part.is_empty()) {
        return Err(BindingParseError::InvalidFormat);
    }
    Ok(parts)
}

fn parse_trigger_sequence(input: &str) -> Result<Vec<BindingTrigger>, BindingParseError> {
    let mut triggers = Vec::new();
    for part in input.split('>') {
        if part.is_empty() {
            return Err(BindingParseError::InvalidFormat);
        }
        triggers.push(part.parse()?);
    }
    Ok(triggers)
}

fn set_mod(field: &mut bool) -> Result<(), BindingParseError> {
    if *field {
        return Err(BindingParseError::InvalidFormat);
    }
    *field = true;
    Ok(())
}

fn set_sided_mod(
    field: &mut bool,
    side_field: &mut Option<BindingModSide>,
    side: BindingModSide,
) -> Result<(), BindingParseError> {
    set_mod(field)?;
    *side_field = Some(side);
    Ok(())
}

fn set_key(slot: &mut Option<BindingKey>, key: BindingKey) -> Result<(), BindingParseError> {
    if slot.is_some() {
        return Err(BindingParseError::InvalidFormat);
    }
    *slot = Some(key);
    Ok(())
}

fn parse_legacy_physical_key(input: &str) -> Result<TerminalKey, BindingParseError> {
    match input {
        "zero" => Ok(TerminalKey::Digit0),
        _ => parse_physical_key(input)?.ok_or(BindingParseError::InvalidFormat),
    }
}
