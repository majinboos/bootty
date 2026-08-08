use std::{fmt::Write as _, str::FromStr};

use crate::terminal::{KeyInput, KeyMods, TerminalKey};

#[derive(Clone, Debug, PartialEq)]
pub struct InputBinding {
    pub trigger: BindingTrigger,
    pub action: BindingAction,
    pub flags: BindingFlags,
}

impl InputBinding {
    pub fn sorts_before(&self, other: &Self) -> bool {
        self.trigger.sort_key() < other.trigger.sort_key()
    }
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
    fn count(self) -> u8 {
        u8::from(self.command) + u8::from(self.ctrl) + u8::from(self.shift) + u8::from(self.alt)
    }

    fn ghostty_int(self) -> u8 {
        u8::from(self.shift)
            | (u8::from(self.ctrl) << 1)
            | (u8::from(self.alt) << 2)
            | (u8::from(self.command) << 3)
    }

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

    pub fn matches_key_input(&self, input: KeyInput) -> bool {
        Self::input_mod_candidates(input).iter().any(|mods| {
            self.mods == *mods
                && (self.key == BindingKey::Physical(input.key) || self.key == BindingKey::CatchAll)
        })
    }

    fn sort_key(&self) -> (std::cmp::Reverse<u8>, std::cmp::Reverse<u8>, i32) {
        (
            std::cmp::Reverse(self.mods.count()),
            std::cmp::Reverse(self.mods.ghostty_int()),
            self.key.sort_key(),
        )
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

    #[cfg(test)]
    fn format_entry(&self) -> String {
        let mut output = String::new();
        self.push_format_entry(&mut output);
        output
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

    fn sort_key(&self) -> i32 {
        match self {
            Self::Unicode(ch) => i32::try_from(u32::from(*ch)).unwrap_or(i32::MAX),
            Self::Physical(key) => *key as i32,
            Self::ScrollUp => i32::MAX - 2,
            Self::ScrollDown => i32::MAX - 1,
            Self::CatchAll => i32::MAX,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingAction {
    Ignore,
    Unbind,
    Reset,
    ReloadConfig,
    NewWindow,
    NewMuxSession,
    SessionPicker,
    CommandPalette,
    CloseWindow,
    CloseSurface,
    Quit,
    ToggleFullscreen,
    ToggleSidebarFocus,
    ToggleSidebarVisibility,
    OpenSettings,
    ChangeAppearance(AppearanceChoice),
    SwitchTheme,
    Csi(String),
    Esc(String),
    Text(String),
    Search(String),
    SearchSelection,
    NavigateSearch(NavigateSearch),
    StartSearch,
    EndSearch,
    CopyToClipboard(CopyToClipboard),
    CopyUrlToClipboard,
    CopyTitleToClipboard,
    PasteFromClipboard,
    CopyMode,
    PasteFromSelection,
    IncreaseFontSize(f32),
    DecreaseFontSize(f32),
    ResetFontSize,
    SetFontSize(f32),
    SetSurfaceTitle(String),
    SetTabTitle(String),
    ClearScreen,
    SelectAll,
    ScrollToTop,
    ScrollToBottom,
    ScrollToSelection,
    ScrollToRow(usize),
    ScrollPageUp,
    ScrollPageDown,
    ScrollPageFractional(f32),
    ScrollPageLines(i16),
    AdjustSelection(AdjustSelection),
    JumpToPrompt(i16),
    WriteScrollbackFile(WriteScreen),
    NewTab,
    NextTab,
    PreviousTab,
    LastTab,
    SelectTab(u32),
    MoveTab(i32),
    SplitRight,
    SplitDown,
    SelectPane(PaneDirection),
    NextPane,
    PreviousPane,
    KillPane,
    TogglePaneZoom,
    NextSession,
    PreviousSession,
    CreateSpace,
    CloseSpace,
    EditSpace,
    NextSpace,
    PreviousSpace,
    SelectSpace(u32),
    LastSession,
    SelectSession(u32),
    MoveSession(i32),
    DitchSession,
    RenameSession,
    RenameTab,
    ShowKeybinds,
    WriteScreenFile(WriteScreen),
    WriteSelectionFile(WriteScreen),
    ToggleMouseReporting,
    EndKeySequence,
    ActivateKeyTable(String),
    ActivateKeyTableOnce(String),
    DeactivateKeyTable,
    DeactivateAllKeyTables,
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

        #[cfg(test)]
        fn physical_key_cases() -> &'static [(&'static str, &'static str, TerminalKey)] {
            &[$(($canonical, $alias, $key),)+]
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

impl BindingAction {
    pub fn format_entry(&self) -> String {
        match self {
            Self::Ignore => "ignore".to_owned(),
            Self::Unbind => "unbind".to_owned(),
            Self::Reset => "reset".to_owned(),
            Self::ReloadConfig => "reload_config".to_owned(),
            Self::NewWindow => "new_window".to_owned(),
            Self::NewMuxSession => "new_mux_session".to_owned(),
            Self::SessionPicker => "session_picker".to_owned(),
            Self::CommandPalette => "command_palette".to_owned(),
            Self::CloseWindow => "close_window".to_owned(),
            Self::CloseSurface => "close_surface".to_owned(),
            Self::Quit => "quit".to_owned(),
            Self::ToggleFullscreen => "toggle_fullscreen".to_owned(),
            Self::ToggleSidebarFocus => "toggle_sidebar_focus".to_owned(),
            Self::ToggleSidebarVisibility => "toggle_sidebar_visibility".to_owned(),
            Self::OpenSettings => "open_settings".to_owned(),
            Self::ChangeAppearance(value) => format!("change_appearance:{}", value.as_str()),
            Self::SwitchTheme => "switch_theme".to_owned(),
            Self::Csi(value) => format!("csi:{value}"),
            Self::Esc(value) => format!("esc:{value}"),
            Self::Text(value) => format!("text:{}", format_text_bytes(value)),
            Self::Search(value) => format!("search:{}", format_text_bytes(value)),
            Self::SearchSelection => "search_selection".to_owned(),
            Self::NavigateSearch(value) => format!("navigate_search:{}", value.as_str()),
            Self::StartSearch => "start_search".to_owned(),
            Self::EndSearch => "end_search".to_owned(),
            Self::CopyToClipboard(value) => format!("copy_to_clipboard:{}", value.as_str()),
            Self::CopyUrlToClipboard => "copy_url_to_clipboard".to_owned(),
            Self::CopyTitleToClipboard => "copy_title_to_clipboard".to_owned(),
            Self::PasteFromClipboard => "paste_from_clipboard".to_owned(),
            Self::CopyMode => "copy_mode".to_owned(),
            Self::PasteFromSelection => "paste_from_selection".to_owned(),
            Self::IncreaseFontSize(value) => format!("increase_font_size:{value}"),
            Self::DecreaseFontSize(value) => format!("decrease_font_size:{value}"),
            Self::ResetFontSize => "reset_font_size".to_owned(),
            Self::SetFontSize(value) => format!("set_font_size:{value}"),
            Self::SetSurfaceTitle(value) => {
                format!("set_surface_title:{}", format_text_bytes(value))
            }
            Self::SetTabTitle(value) => format!("set_tab_title:{}", format_text_bytes(value)),
            Self::ClearScreen => "clear_screen".to_owned(),
            Self::SelectAll => "select_all".to_owned(),
            Self::ScrollToTop => "scroll_to_top".to_owned(),
            Self::ScrollToBottom => "scroll_to_bottom".to_owned(),
            Self::ScrollToSelection => "scroll_to_selection".to_owned(),
            Self::ScrollToRow(value) => format!("scroll_to_row:{value}"),
            Self::ScrollPageUp => "scroll_page_up".to_owned(),
            Self::ScrollPageDown => "scroll_page_down".to_owned(),
            Self::ScrollPageFractional(value) => format!("scroll_page_fractional:{value}"),
            Self::ScrollPageLines(value) => format!("scroll_page_lines:{value}"),
            Self::NewTab => "new_tab".to_owned(),
            Self::NextTab => "next_tab".to_owned(),
            Self::PreviousTab => "previous_tab".to_owned(),
            Self::LastTab => "last_tab".to_owned(),
            Self::SelectTab(value) => format!("select_tab:{value}"),
            Self::MoveTab(value) => format!("move_tab:{value}"),
            Self::SplitRight => "split_right".to_owned(),
            Self::SplitDown => "split_down".to_owned(),
            Self::SelectPane(value) => format!("select_pane:{}", value.as_str()),
            Self::NextPane => "next_pane".to_owned(),
            Self::PreviousPane => "previous_pane".to_owned(),
            Self::KillPane => "kill_pane".to_owned(),
            Self::TogglePaneZoom => "toggle_pane_zoom".to_owned(),
            Self::NextSession => "next_session".to_owned(),
            Self::PreviousSession => "previous_session".to_owned(),
            Self::CreateSpace => "create_space".to_owned(),
            Self::EditSpace => "edit_space".to_owned(),
            Self::CloseSpace => "close_space".to_owned(),
            Self::NextSpace => "next_space".to_owned(),
            Self::PreviousSpace => "previous_space".to_owned(),
            Self::SelectSpace(value) => format!("select_space:{value}"),
            Self::LastSession => "last_session".to_owned(),
            Self::SelectSession(value) => format!("select_session:{value}"),
            Self::MoveSession(value) => format!("move_session:{value}"),
            Self::DitchSession => "ditch_session".to_owned(),
            Self::RenameSession => "rename_session".to_owned(),
            Self::RenameTab => "rename_tab".to_owned(),
            Self::ShowKeybinds => "show_keybinds".to_owned(),
            Self::AdjustSelection(value) => format!("adjust_selection:{}", value.as_str()),
            Self::JumpToPrompt(value) => format!("jump_to_prompt:{value}"),
            Self::WriteScrollbackFile(value) => {
                format!("write_scrollback_file:{}", value.format_entry())
            }
            Self::WriteScreenFile(value) => format!("write_screen_file:{}", value.format_entry()),
            Self::WriteSelectionFile(value) => {
                format!("write_selection_file:{}", value.format_entry())
            }
            Self::ToggleMouseReporting => "toggle_mouse_reporting".to_owned(),
            Self::EndKeySequence => "end_key_sequence".to_owned(),
            Self::ActivateKeyTable(value) => {
                format!("activate_key_table:{}", format_text_bytes(value))
            }
            Self::ActivateKeyTableOnce(value) => {
                format!("activate_key_table_once:{}", format_text_bytes(value))
            }
            Self::DeactivateKeyTable => "deactivate_key_table".to_owned(),
            Self::DeactivateAllKeyTables => "deactivate_all_key_tables".to_owned(),
        }
    }
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

pub fn parse_action(input: &str) -> Result<BindingAction, BindingParseError> {
    let (name, value) = match input.split_once(':') {
        Some((name, value)) => (name, Some(value)),
        None => (input, None),
    };
    match name {
        "ignore" => parse_unit(value, BindingAction::Ignore),
        "unbind" => parse_unit(value, BindingAction::Unbind),
        "reset" => parse_unit(value, BindingAction::Reset),
        "reload_config" => parse_unit(value, BindingAction::ReloadConfig),
        "new_window" => parse_unit(value, BindingAction::NewWindow),
        "new_mux_session" => parse_unit(value, BindingAction::NewMuxSession),
        "session_picker" => parse_unit(value, BindingAction::SessionPicker),
        "command_palette" => parse_unit(value, BindingAction::CommandPalette),
        "close_window" => parse_unit(value, BindingAction::CloseWindow),
        "close_surface" => parse_unit(value, BindingAction::CloseSurface),
        "quit" => parse_unit(value, BindingAction::Quit),
        "toggle_fullscreen" => parse_unit(value, BindingAction::ToggleFullscreen),
        "toggle_sidebar_focus" => parse_unit(value, BindingAction::ToggleSidebarFocus),
        "toggle_sidebar_visibility" => parse_unit(value, BindingAction::ToggleSidebarVisibility),
        "open_settings" => parse_unit(value, BindingAction::OpenSettings),
        "change_appearance" => parse_required(value, |value| {
            Ok(BindingAction::ChangeAppearance(AppearanceChoice::parse(
                value,
            )?))
        }),
        "switch_theme" => parse_unit(value, BindingAction::SwitchTheme),
        "csi" => parse_required(value, |value| Ok(BindingAction::Csi(value.to_owned()))),
        "esc" => parse_required(value, |value| Ok(BindingAction::Esc(value.to_owned()))),
        "text" => parse_required(value, |value| Ok(BindingAction::Text(value.to_owned()))),
        "search" => parse_required(value, |value| Ok(BindingAction::Search(value.to_owned()))),
        "search_selection" => parse_unit(value, BindingAction::SearchSelection),
        "navigate_search" => parse_required(value, |value| {
            Ok(BindingAction::NavigateSearch(NavigateSearch::parse(value)?))
        }),
        "start_search" => parse_unit(value, BindingAction::StartSearch),
        "end_search" => parse_unit(value, BindingAction::EndSearch),
        "copy_to_clipboard" => match value {
            Some(value) => Ok(BindingAction::CopyToClipboard(CopyToClipboard::parse(
                value,
            )?)),
            None => Ok(BindingAction::CopyToClipboard(CopyToClipboard::default())),
        },
        "copy_url_to_clipboard" => parse_unit(value, BindingAction::CopyUrlToClipboard),
        "copy_title_to_clipboard" => parse_unit(value, BindingAction::CopyTitleToClipboard),
        "paste_from_clipboard" => parse_unit(value, BindingAction::PasteFromClipboard),
        "paste_from_selection" => parse_unit(value, BindingAction::PasteFromSelection),
        "copy_mode" => parse_unit(value, BindingAction::CopyMode),
        "increase_font_size" => parse_required(value, |value| {
            Ok(BindingAction::IncreaseFontSize(parse_f32(value)?))
        }),
        "decrease_font_size" => parse_required(value, |value| {
            Ok(BindingAction::DecreaseFontSize(parse_f32(value)?))
        }),
        "reset_font_size" => parse_unit(value, BindingAction::ResetFontSize),
        "set_font_size" => parse_required(value, |value| {
            Ok(BindingAction::SetFontSize(parse_f32(value)?))
        }),
        "set_surface_title" => parse_required(value, |value| {
            Ok(BindingAction::SetSurfaceTitle(value.to_owned()))
        }),
        "set_tab_title" => parse_required(value, |value| {
            Ok(BindingAction::SetTabTitle(value.to_owned()))
        }),
        "clear_screen" => parse_unit(value, BindingAction::ClearScreen),
        "select_all" => parse_unit(value, BindingAction::SelectAll),
        "scroll_to_top" => parse_unit(value, BindingAction::ScrollToTop),
        "scroll_to_bottom" => parse_unit(value, BindingAction::ScrollToBottom),
        "scroll_to_selection" => parse_unit(value, BindingAction::ScrollToSelection),
        "scroll_to_row" => parse_required(value, |value| {
            Ok(BindingAction::ScrollToRow(parse_usize(value)?))
        }),
        "scroll_page_up" => parse_unit(value, BindingAction::ScrollPageUp),
        "scroll_page_down" => parse_unit(value, BindingAction::ScrollPageDown),
        "scroll_page_fractional" => parse_required(value, |value| {
            Ok(BindingAction::ScrollPageFractional(parse_f32(value)?))
        }),
        "scroll_page_lines" => parse_required(value, |value| {
            Ok(BindingAction::ScrollPageLines(parse_i16(value)?))
        }),
        "adjust_selection" => parse_required(value, |value| {
            Ok(BindingAction::AdjustSelection(AdjustSelection::parse(
                value,
            )?))
        }),
        "new_tab" => parse_unit(value, BindingAction::NewTab),
        "next_tab" => parse_unit(value, BindingAction::NextTab),
        "previous_tab" => parse_unit(value, BindingAction::PreviousTab),
        "last_tab" => parse_unit(value, BindingAction::LastTab),
        "select_tab" => parse_required(value, |value| {
            Ok(BindingAction::SelectTab(parse_u32(value)?))
        }),
        "move_tab" => parse_required(value, |value| Ok(BindingAction::MoveTab(parse_i32(value)?))),
        "split_right" => parse_unit(value, BindingAction::SplitRight),
        "split_down" => parse_unit(value, BindingAction::SplitDown),
        "select_pane" => parse_required(value, |value| {
            Ok(BindingAction::SelectPane(PaneDirection::parse(value)?))
        }),
        "next_pane" => parse_unit(value, BindingAction::NextPane),
        "previous_pane" => parse_unit(value, BindingAction::PreviousPane),
        "kill_pane" => parse_unit(value, BindingAction::KillPane),
        "toggle_pane_zoom" => parse_unit(value, BindingAction::TogglePaneZoom),
        "next_session" => parse_unit(value, BindingAction::NextSession),
        "previous_session" => parse_unit(value, BindingAction::PreviousSession),
        "create_space" => parse_unit(value, BindingAction::CreateSpace),
        "edit_space" => parse_unit(value, BindingAction::EditSpace),
        "close_space" => parse_unit(value, BindingAction::CloseSpace),
        "next_space" => parse_unit(value, BindingAction::NextSpace),
        "previous_space" => parse_unit(value, BindingAction::PreviousSpace),
        "select_space" => parse_required(value, |value| {
            Ok(BindingAction::SelectSpace(parse_u32(value)?))
        }),
        "last_session" => parse_unit(value, BindingAction::LastSession),
        "select_session" => parse_required(value, |value| {
            Ok(BindingAction::SelectSession(parse_u32(value)?))
        }),
        "move_session" => parse_required(value, |value| {
            Ok(BindingAction::MoveSession(parse_i32(value)?))
        }),
        "ditch_session" => parse_unit(value, BindingAction::DitchSession),
        "rename_session" => parse_unit(value, BindingAction::RenameSession),
        "rename_tab" => parse_unit(value, BindingAction::RenameTab),
        "show_keybinds" => parse_unit(value, BindingAction::ShowKeybinds),
        "jump_to_prompt" => parse_required(value, |value| {
            Ok(BindingAction::JumpToPrompt(parse_i16(value)?))
        }),
        "write_scrollback_file" => parse_required(value, |value| {
            Ok(BindingAction::WriteScrollbackFile(WriteScreen::parse(
                value,
            )?))
        }),
        "write_screen_file" => parse_required(value, |value| {
            Ok(BindingAction::WriteScreenFile(WriteScreen::parse(value)?))
        }),
        "write_selection_file" => parse_required(value, |value| {
            Ok(BindingAction::WriteSelectionFile(WriteScreen::parse(
                value,
            )?))
        }),
        "toggle_mouse_reporting" => parse_unit(value, BindingAction::ToggleMouseReporting),
        "end_key_sequence" => parse_unit(value, BindingAction::EndKeySequence),
        "activate_key_table" => parse_required(value, |value| {
            Ok(BindingAction::ActivateKeyTable(value.to_owned()))
        }),
        "activate_key_table_once" => parse_required(value, |value| {
            Ok(BindingAction::ActivateKeyTableOnce(value.to_owned()))
        }),
        "deactivate_key_table" => parse_unit(value, BindingAction::DeactivateKeyTable),
        "deactivate_all_key_tables" => parse_unit(value, BindingAction::DeactivateAllKeyTables),
        _ => Err(BindingParseError::InvalidAction),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_binding_parser_ports_trigger_modifiers_and_edges() {
        for (input, mods, key) in [
            (
                "shift+ctrl+a=ignore",
                BindingMods {
                    shift: true,
                    ctrl: true,
                    ..Default::default()
                },
                BindingKey::Unicode('a'),
            ),
            (
                "a+shift=ignore",
                BindingMods {
                    shift: true,
                    ..Default::default()
                },
                BindingKey::Unicode('a'),
            ),
            (
                "ctrl++=ignore",
                BindingMods {
                    ctrl: true,
                    ..Default::default()
                },
                BindingKey::Unicode('+'),
            ),
            (
                "alt+scroll_up=ignore",
                BindingMods {
                    alt: true,
                    ..Default::default()
                },
                BindingKey::ScrollUp,
            ),
            (
                "alt+scroll_down=ignore",
                BindingMods {
                    alt: true,
                    ..Default::default()
                },
                BindingKey::ScrollDown,
            ),
        ] {
            assert_eq!(
                parse_binding(input).unwrap().trigger,
                BindingTrigger { mods, key }
            );
        }
        assert_eq!(
            parse_binding("ctrl+==text:=hello").unwrap(),
            InputBinding {
                trigger: BindingTrigger {
                    mods: BindingMods {
                        ctrl: true,
                        ..Default::default()
                    },
                    key: BindingKey::Unicode('=')
                },
                action: BindingAction::Text("=hello".to_owned()),
                flags: BindingFlags::default()
            }
        );
        assert_eq!(
            parse_binding("shift+ö=ignore").unwrap().trigger.key,
            BindingKey::Unicode('ö')
        );
        let flags = parse_binding("unconsumed:performable:shift+a=ignore")
            .unwrap()
            .flags;
        assert_eq!(
            flags,
            BindingFlags {
                consumed: false,
                performable: true,
                ..Default::default()
            }
        );
        for input in ["foo=ignore", "shift+shift+a=ignore", "a+b=ignore"] {
            assert_eq!(parse_binding(input), Err(BindingParseError::InvalidFormat));
        }
    }

    #[test]
    fn input_binding_parser_preserves_modifier_sides() {
        let trigger = parse_binding("left_ctrl+right_alt+a=ignore")
            .unwrap()
            .trigger;

        assert_eq!(
            trigger.mods,
            BindingMods {
                ctrl: true,
                alt: true,
                ctrl_side: Some(BindingModSide::Left),
                alt_side: Some(BindingModSide::Right),
                ..Default::default()
            }
        );
        assert_eq!(trigger.format_entry(), "left_ctrl+right_alt+a");
        assert_eq!(
            parse_binding("alt+right_alt+a=ignore"),
            Err(BindingParseError::InvalidFormat)
        );
    }

    #[test]
    fn input_binding_parser_ports_physical_names_aliases_and_catch_all() {
        for &(canonical, alias, key) in physical_key_cases() {
            for input in [canonical, alias] {
                assert_eq!(
                    parse_binding(&format!("{input}=ignore"))
                        .unwrap()
                        .trigger
                        .key,
                    BindingKey::Physical(key)
                );
            }
            assert_eq!(
                BindingKey::Physical(key).format_entry(),
                canonical,
                "{canonical}"
            );
        }
        assert_eq!(
            parse_binding("physical:zero=ignore").unwrap().trigger.key,
            BindingKey::Physical(TerminalKey::Digit0)
        );
        assert_eq!(
            parse_binding("ctrl+catch_all=ignore").unwrap().trigger,
            BindingTrigger {
                mods: BindingMods {
                    ctrl: true,
                    ..Default::default()
                },
                key: BindingKey::CatchAll
            }
        );
        assert_eq!(
            parse_binding("cmd+option+control+a=ignore")
                .unwrap()
                .trigger
                .mods,
            BindingMods {
                ctrl: true,
                alt: true,
                command: true,
                ..Default::default()
            }
        );
        assert_eq!(
            parse_binding("Keya=ignore"),
            Err(BindingParseError::InvalidFormat)
        );
    }

    #[test]
    fn input_binding_trigger_matches_terminal_key_input() {
        let trigger = parse_binding("ctrl+KeyA=ignore").unwrap().trigger;
        assert!(trigger.matches_key_input(KeyInput {
            key: TerminalKey::A,
            mods: KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("a"),
            unshifted: Some('a'),
        }));
        assert!(!trigger.matches_key_input(KeyInput {
            key: TerminalKey::B,
            mods: KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: Some("b"),
            unshifted: Some('b'),
        }));
    }

    #[test]
    fn input_binding_parser_ports_terminal_actions() {
        for (input, action) in [
            ("a=ignore", BindingAction::Ignore),
            ("a=unbind", BindingAction::Unbind),
            ("a=reset", BindingAction::Reset),
            ("a=reload_config", BindingAction::ReloadConfig),
            ("a=new_window", BindingAction::NewWindow),
            ("a=close_window", BindingAction::CloseWindow),
            ("a=close_surface", BindingAction::CloseSurface),
            ("a=quit", BindingAction::Quit),
            ("a=toggle_fullscreen", BindingAction::ToggleFullscreen),
            ("a=open_settings", BindingAction::OpenSettings),
            ("a=csi:A", BindingAction::Csi("A".to_owned())),
            ("a=esc:7", BindingAction::Esc("7".to_owned())),
            ("a=text:hello", BindingAction::Text("hello".to_owned())),
            ("a=text:=hello", BindingAction::Text("=hello".to_owned())),
        ] {
            assert_eq!(parse_binding(input).unwrap().action, action);
        }
        for input in [
            "a=nopenopenope",
            "a=ignore:A",
            "a=reset:A",
            "a=reload_config:A",
            "a=new_window:A",
            "a=csi",
            "a=esc",
            "a=text",
        ] {
            assert!(parse_binding(input).is_err(), "{input}");
        }
        assert_eq!(
            parse_binding("a=nopenopenope"),
            Err(BindingParseError::InvalidAction)
        );
        assert_eq!(
            parse_binding("a=ignore:A"),
            Err(BindingParseError::InvalidFormat)
        );
    }

    #[test]
    fn input_binding_parser_ports_terminal_action_parameters() {
        for (input, action) in [
            (
                "a=search:needle",
                BindingAction::Search("needle".to_owned()),
            ),
            ("a=search_selection", BindingAction::SearchSelection),
            (
                "a=navigate_search:previous",
                BindingAction::NavigateSearch(NavigateSearch::Previous),
            ),
            ("a=start_search", BindingAction::StartSearch),
            ("a=end_search", BindingAction::EndSearch),
            (
                "a=copy_to_clipboard",
                BindingAction::CopyToClipboard(CopyToClipboard::Mixed),
            ),
            (
                "a=copy_to_clipboard:html",
                BindingAction::CopyToClipboard(CopyToClipboard::Html),
            ),
            ("a=copy_url_to_clipboard", BindingAction::CopyUrlToClipboard),
            (
                "a=copy_title_to_clipboard",
                BindingAction::CopyTitleToClipboard,
            ),
            ("a=paste_from_clipboard", BindingAction::PasteFromClipboard),
            ("a=paste_from_selection", BindingAction::PasteFromSelection),
            ("a=copy_mode", BindingAction::CopyMode),
            (
                "a=increase_font_size:1.5",
                BindingAction::IncreaseFontSize(1.5),
            ),
            (
                "a=decrease_font_size:2.5",
                BindingAction::DecreaseFontSize(2.5),
            ),
            ("a=reset_font_size", BindingAction::ResetFontSize),
            ("a=set_font_size:13.5", BindingAction::SetFontSize(13.5)),
            (
                "a=set_surface_title:surface",
                BindingAction::SetSurfaceTitle("surface".to_owned()),
            ),
            (
                "a=set_tab_title:tab",
                BindingAction::SetTabTitle("tab".to_owned()),
            ),
            ("a=clear_screen", BindingAction::ClearScreen),
            ("a=select_all", BindingAction::SelectAll),
            ("a=scroll_to_top", BindingAction::ScrollToTop),
            ("a=scroll_to_bottom", BindingAction::ScrollToBottom),
            ("a=scroll_to_selection", BindingAction::ScrollToSelection),
            ("a=scroll_to_row:12", BindingAction::ScrollToRow(12)),
            ("a=scroll_page_up", BindingAction::ScrollPageUp),
            ("a=scroll_page_down", BindingAction::ScrollPageDown),
            (
                "a=scroll_page_fractional:-0.5",
                BindingAction::ScrollPageFractional(-0.5),
            ),
            (
                "a=scroll_page_fractional:+0.5",
                BindingAction::ScrollPageFractional(0.5),
            ),
            (
                "a=scroll_page_lines:-10",
                BindingAction::ScrollPageLines(-10),
            ),
            (
                "a=adjust_selection:beginning_of_line",
                BindingAction::AdjustSelection(AdjustSelection::BeginningOfLine),
            ),
            ("a=jump_to_prompt:-1", BindingAction::JumpToPrompt(-1)),
            ("a=jump_to_prompt:10", BindingAction::JumpToPrompt(10)),
            (
                "a=write_scrollback_file:paste,vt",
                BindingAction::WriteScrollbackFile(WriteScreen {
                    action: WriteScreenAction::Paste,
                    emit: WriteScreenFormat::Vt,
                }),
            ),
            (
                "a=write_screen_file:copy",
                BindingAction::WriteScreenFile(WriteScreen {
                    action: WriteScreenAction::Copy,
                    emit: WriteScreenFormat::Plain,
                }),
            ),
            (
                "a=write_screen_file:copy,html",
                BindingAction::WriteScreenFile(WriteScreen {
                    action: WriteScreenAction::Copy,
                    emit: WriteScreenFormat::Html,
                }),
            ),
            (
                "a=write_selection_file:open",
                BindingAction::WriteSelectionFile(WriteScreen {
                    action: WriteScreenAction::Open,
                    emit: WriteScreenFormat::Plain,
                }),
            ),
            (
                "a=activate_key_table:copy-mode",
                BindingAction::ActivateKeyTable("copy-mode".to_owned()),
            ),
            (
                "a=activate_key_table_once:copy-mode",
                BindingAction::ActivateKeyTableOnce("copy-mode".to_owned()),
            ),
            ("a=deactivate_key_table", BindingAction::DeactivateKeyTable),
            (
                "a=deactivate_all_key_tables",
                BindingAction::DeactivateAllKeyTables,
            ),
            (
                "a=toggle_mouse_reporting",
                BindingAction::ToggleMouseReporting,
            ),
            ("a=end_key_sequence", BindingAction::EndKeySequence),
        ] {
            assert_eq!(parse_binding(input).unwrap().action, action, "{input}");
        }

        for input in [
            "a=copy_to_clipboard:invalid",
            "a=navigate_search:sideways",
            "a=increase_font_size:nope",
            "a=set_font_size:nan",
            "a=scroll_page_fractional:inf",
            "a=scroll_page_lines:100000",
            "a=adjust_selection:middle",
            "a=write_screen_file:",
            "a=write_screen_file:,",
            "a=write_screen_file:copy,",
            "a=write_screen_file:copy,html,extra",
            "a=cursor_key:normal,application",
        ] {
            assert!(parse_binding(input).is_err(), "{input}");
        }
        assert_eq!(
            parse_binding("a=write_screen_file:copy,html")
                .unwrap()
                .action
                .format_entry(),
            "write_screen_file:copy,html"
        );
        assert_eq!(
            BindingAction::SetTabTitle("foo bar".to_owned()).format_entry(),
            "set_tab_title:foo bar"
        );
    }
    #[test]
    fn input_binding_parser_accepts_space_actions() {
        for (input, action) in [
            ("a=create_space", BindingAction::CreateSpace),
            ("a=close_space", BindingAction::CloseSpace),
            ("a=edit_space", BindingAction::EditSpace),
            ("a=next_space", BindingAction::NextSpace),
            ("a=previous_space", BindingAction::PreviousSpace),
            ("a=select_space:3", BindingAction::SelectSpace(3)),
        ] {
            assert_eq!(parse_binding(input).unwrap().action, action, "{input}");
        }
    }

    #[test]
    fn input_binding_ports_ordering() {
        let ctrl_shift = parse_binding("ctrl+shift+a=ignore").unwrap();
        let command = parse_binding("cmd+a=ignore").unwrap();
        let ctrl_b = parse_binding("ctrl+b=ignore").unwrap();
        let ctrl_a = parse_binding("ctrl+a=ignore").unwrap();
        let key_a = parse_binding("ctrl+KeyA=ignore").unwrap();

        assert!(ctrl_shift.sorts_before(&command));
        assert!(command.sorts_before(&ctrl_b));
        assert!(ctrl_a.sorts_before(&ctrl_b));
        assert!(key_a.sorts_before(&ctrl_a));
    }

    #[test]
    fn input_binding_parser_ports_sequences_and_chains() {
        assert_eq!(
            parse_binding_elements("ctrl+a>ctrl+b=ignore").unwrap(),
            vec![
                BindingElement::Leader(BindingTrigger {
                    mods: BindingMods {
                        ctrl: true,
                        ..Default::default()
                    },
                    key: BindingKey::Unicode('a')
                }),
                BindingElement::Binding(InputBinding {
                    trigger: BindingTrigger {
                        mods: BindingMods {
                            ctrl: true,
                            ..Default::default()
                        },
                        key: BindingKey::Unicode('b')
                    },
                    action: BindingAction::Ignore,
                    flags: BindingFlags::default()
                })
            ]
        );
        assert_eq!(
            parse_binding_elements("chain=text:hello").unwrap(),
            vec![BindingElement::Chain(BindingAction::Text(
                "hello".to_owned()
            ))]
        );
        for input in [
            "global:ctrl+a>ctrl+b=ignore",
            "all:ctrl+a>ctrl+b=ignore",
            "unconsumed:chain=ignore",
            "ctrl+a>=ignore",
        ] {
            assert_eq!(
                parse_binding_elements(input),
                Err(BindingParseError::InvalidFormat),
                "{input}"
            );
        }
    }

    #[test]
    fn input_binding_action_formats_terminal_actions() {
        for (action, expected) in [
            (BindingAction::Ignore, "ignore"),
            (BindingAction::Unbind, "unbind"),
            (BindingAction::Reset, "reset"),
            (BindingAction::ReloadConfig, "reload_config"),
            (BindingAction::NewWindow, "new_window"),
            (BindingAction::CloseWindow, "close_window"),
            (BindingAction::CloseSurface, "close_surface"),
            (BindingAction::Quit, "quit"),
            (BindingAction::ToggleFullscreen, "toggle_fullscreen"),
            (BindingAction::OpenSettings, "open_settings"),
            (BindingAction::EditSpace, "edit_space"),
            (BindingAction::Csi("0m".to_owned()), "csi:0m"),
            (BindingAction::Esc("7".to_owned()), "esc:7"),
            (BindingAction::Text("plain".to_owned()), "text:plain"),
            (BindingAction::CopyMode, "copy_mode"),
        ] {
            assert_eq!(action.format_entry(), expected);
        }

        let ghost = String::from_utf8(vec![0xf0, 0x9f, 0x91, 0xbb]).unwrap();
        assert_eq!(
            BindingAction::Text(ghost).format_entry(),
            "text:\\xf0\\x9f\\x91\\xbb"
        );
    }

    #[test]
    fn input_binding_trigger_formats_terminal_keys() {
        assert_eq!(
            BindingTrigger {
                mods: BindingMods {
                    ctrl: true,
                    ..Default::default()
                },
                key: BindingKey::Unicode('a')
            }
            .format_entry(),
            "ctrl+a"
        );
        assert_eq!(
            BindingTrigger {
                mods: BindingMods::default(),
                key: BindingKey::Physical(TerminalKey::A)
            }
            .format_entry(),
            "KeyA"
        );
        assert_eq!(
            BindingTrigger {
                mods: BindingMods::default(),
                key: BindingKey::CatchAll
            }
            .format_entry(),
            "catch_all"
        );
    }
}
