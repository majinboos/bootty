use std::str::FromStr;

use anyhow::Result;
use eframe::egui;

use crate::input::terminal_key;
use bootty_command::{Caller, CommandInvocation};
use bootty_config::config::{InputConfig, split_keybind_entry};
use bootty_mux::command::MuxDirection;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
use bootty_winit::{
    direct_input::ModifierSideState,
    input_binding::{
        AppearanceChoice, BindingAction, BindingElement, BindingKey, BindingTrigger,
        CopyToClipboard, NavigateSearch, PaneDirection, parse_action, parse_binding_elements,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAction {
    ReloadConfig,
    Ignore,
    NewWindow,
    NewMuxSession,
    SessionPicker,
    CommandPalette,
    Close,
    Quit,
    ToggleFullscreen,
    ToggleSidebarFocus,
    ToggleSidebarVisibility,
    OpenSettings,
    ChangeAppearance(bootty_config::config::AppearanceMode),
    SwitchTheme,
    RenameSession,
    MoveSessionToSpace,
    RenameTab,
    DitchSession,
    CreateSpace,
    CloseSpace,
    EditSpace,
    NextSpace,
    PreviousSpace,
    SelectSpace(u32),
    ShowKeybinds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalFindAction {
    Prompt,
    Search(String),
    SearchSelection,
    Next,
    Previous,
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeybindAction {
    App(AppAction),
    Mux(MuxKeyAction),
    Scroll(TerminalScrollAction),
    Write(Vec<u8>),
    Font(FontSizeAction),
    Find(TerminalFindAction),
    CopyToClipboard(CopyToClipboard),
    CopyMode,
    PasteFromClipboard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MuxKeyAction {
    NewTab,
    NextTab,
    PreviousTab,
    LastTab,
    SelectTab(u32),
    MoveTab(i32),
    SplitPane(crate::layout::SplitDirection),
    SelectPane(MuxDirection),
    NextPane,
    PreviousPane,
    KillPane,
    ClosePane,
    TogglePaneZoom,
    NextSession,
    PreviousSession,
    LastSession,
    SelectSession(u32),
    MoveSession(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalScrollAction {
    Top,
    Bottom,
    PageUp,
    PageDown,
    Lines(i16),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FontSizeAction {
    Increase(f32),
    Decrease(f32),
    Reset,
    Set(f32),
}

#[derive(Clone, Debug)]
struct AppKeyBinding {
    leader: Option<BindingTrigger>,
    trigger: BindingTrigger,
    action_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct AppKeyBindings {
    bindings: Vec<AppKeyBinding>,
    leaders: Vec<BindingTrigger>,
    active_leader: Option<BindingTrigger>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarAction {
    Ignore,
    PreviousSession,
    NextSession,
    ActivateSession,
    FocusTerminal,
}

impl SidebarAction {
    pub const ALL: [Self; 5] = [
        Self::Ignore,
        Self::PreviousSession,
        Self::NextSession,
        Self::ActivateSession,
        Self::FocusTerminal,
    ];

    pub const fn command_id(self) -> &'static str {
        match self {
            Self::Ignore => "ui.sidebar.ignore",
            Self::PreviousSession => "ui.sidebar.previous_session",
            Self::NextSession => "ui.sidebar.next_session",
            Self::ActivateSession => "ui.sidebar.activate_session",
            Self::FocusTerminal => "ui.sidebar.focus_terminal",
        }
    }
}

#[derive(Clone, Debug)]
struct SidebarKeyBinding {
    trigger: BindingTrigger,
    command: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct SidebarKeyBindings {
    bindings: Vec<SidebarKeyBinding>,
}

impl AppKeyBindings {
    pub fn from_config(input: &InputConfig) -> Result<Self> {
        Self::from_keybinds(&input.keybind)
    }

    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        let mut leaders = Vec::new();
        for entry in keybinds {
            let elements = parse_binding_elements(entry)
                .map_err(|error| anyhow::anyhow!("invalid keybind {entry:?}: {error:?}"))?;
            let mut pending_leader = None;
            for element in elements {
                match element {
                    BindingElement::Leader(trigger) => {
                        if !leaders.contains(&trigger) {
                            leaders.push(trigger.clone());
                        }
                        pending_leader = Some(trigger);
                    }
                    BindingElement::Binding(binding) => {
                        let action_name = binding.action.format_entry();
                        keybind_action(binding.action).map_err(|error| {
                            anyhow::anyhow!("unsupported keybind {entry:?}: {error}")
                        })?;
                        bindings.push(AppKeyBinding {
                            leader: pending_leader.take(),
                            trigger: binding.trigger,
                            action_name,
                        });
                    }
                    BindingElement::Chain(_) => {
                        anyhow::bail!(
                            "chain keybinds are not supported for app-level keybind actions"
                        );
                    }
                }
            }
        }
        Ok(Self {
            bindings,
            leaders,
            active_leader: None,
        })
    }

    pub fn invocation_for_key_with_modifier_sides(
        &mut self,
        key: egui::Key,
        modifiers: egui::Modifiers,
        modifier_sides: ModifierSideState,
    ) -> Option<CommandInvocation> {
        self.command_for_candidates(binding_triggers_for_egui_key_with_modifier_sides(
            key,
            modifiers,
            modifier_sides,
        ))
        .map(|name| CommandInvocation::from_action(&name, Caller::Keybinding))
    }

    pub fn invocation_for_scroll_with_modifier_sides(
        &mut self,
        up: bool,
        modifiers: egui::Modifiers,
        modifier_sides: ModifierSideState,
    ) -> Option<CommandInvocation> {
        self.command_for_candidates(binding_triggers_for_egui_scroll_with_modifier_sides(
            up,
            modifiers,
            modifier_sides,
        ))
        .map(|name| CommandInvocation::from_action(&name, Caller::Keybinding))
    }

    pub fn invocation_for_input(&mut self, input: KeyInput) -> Option<CommandInvocation> {
        self.command_for_candidates(binding_triggers_for_key_input(input))
            .map(|name| CommandInvocation::from_action(&name, Caller::Keybinding))
    }

    fn command_for_candidates(&mut self, candidates: Vec<BindingTrigger>) -> Option<String> {
        if let Some(leader) = self.active_leader.take() {
            return candidates
                .iter()
                .find_map(|candidate| {
                    self.bindings
                        .iter()
                        .find(|binding| {
                            binding.leader.as_ref() == Some(&leader)
                                && binding.trigger == *candidate
                        })
                        .map(|binding| binding.action_name.clone())
                })
                .or_else(|| Some("ignore".to_owned()));
        }

        if let Some(leader) = candidates.iter().find_map(|candidate| {
            self.leaders
                .iter()
                .find(|leader| *leader == candidate)
                .cloned()
        }) {
            self.active_leader = Some(leader);
            return Some("ignore".to_owned());
        }

        candidates.iter().find_map(|candidate| {
            self.bindings
                .iter()
                .find(|binding| binding.leader.is_none() && binding.trigger == *candidate)
                .map(|binding| binding.action_name.clone())
        })
    }
}

impl SidebarKeyBindings {
    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        for entry in keybinds {
            let (trigger, action) = split_keybind_entry(entry)
                .ok_or_else(|| anyhow::anyhow!("invalid sidebar keybind {entry:?}"))?;
            let action = sidebar_action(action).map_err(|error| {
                anyhow::anyhow!("unsupported sidebar keybind {entry:?}: {error}")
            })?;
            bindings.push(SidebarKeyBinding {
                trigger: BindingTrigger::from_str(trigger).map_err(|error| {
                    anyhow::anyhow!("invalid sidebar keybind {entry:?}: {error:?}")
                })?,
                command: action.command_id(),
            });
        }
        Ok(Self { bindings })
    }

    pub fn invocation_for_key(
        &self,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Option<CommandInvocation> {
        let candidates = binding_triggers_for_egui_key(key, modifiers);
        self.bindings.iter().find_map(|binding| {
            candidates
                .iter()
                .any(|candidate| candidate == &binding.trigger)
                .then(|| CommandInvocation::from_action(binding.command, Caller::Keybinding))
        })
    }
}

fn sidebar_action(input: &str) -> Result<SidebarAction> {
    SidebarAction::ALL
        .into_iter()
        .find(|action| action.command_id().strip_prefix("ui.sidebar.") == Some(input))
        .ok_or_else(|| anyhow::anyhow!("{input} has no Bootty sidebar behavior"))
}

pub fn split_app_actions_for_bindings_with_modifier_sides(
    app_key_bindings: &mut AppKeyBindings,
    events: Vec<egui::Event>,
    modifier_sides: ModifierSideState,
) -> (Vec<egui::Event>, Vec<CommandInvocation>) {
    let mut terminal_events = Vec::with_capacity(events.len());
    let mut actions = Vec::new();
    let mut suppress_next_text = false;
    let mut suppress_next_paste = false;
    for event in events {
        if suppress_next_text && matches!(event, egui::Event::Text(_)) {
            continue;
        }
        if suppress_next_paste && matches!(event, egui::Event::Paste(_)) {
            suppress_next_paste = false;
            continue;
        }
        if matches!(event, egui::Event::Key { pressed: false, .. }) {
            suppress_next_text = false;
            suppress_next_paste = false;
        }

        let invocation = match &event {
            egui::Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => app_key_bindings
                .invocation_for_key_with_modifier_sides(*key, *modifiers, modifier_sides)
                .or_else(|| builtin_app_invocation_for_key(*key, *modifiers)),
            egui::Event::MouseWheel {
                delta, modifiers, ..
            } if delta.y != 0.0 => app_key_bindings.invocation_for_scroll_with_modifier_sides(
                delta.y > 0.0,
                *modifiers,
                modifier_sides,
            ),
            _ => None,
        };
        if let Some(invocation) = invocation {
            if matches!(event, egui::Event::Key { .. }) {
                suppress_next_text = true;
                suppress_next_paste = invocation.command == "paste_from_clipboard";
            }
            actions.push(invocation);
        } else {
            terminal_events.push(event);
        }
    }
    (terminal_events, actions)
}

// Safety net for new-session even when keybinds are cleared: Cmd+N on macOS, Ctrl+Shift+N
// elsewhere (matching the platform default tables).
pub fn builtin_app_invocation_for_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Option<CommandInvocation> {
    builtin_new_session_invocation(
        key == egui::Key::N,
        key_mods_for_egui_binding(modifiers, ModifierSideState::default()),
    )
}

pub fn builtin_app_invocation_for_direct_key(input: KeyInput) -> Option<CommandInvocation> {
    builtin_new_session_invocation(input.key == TerminalKey::N, input.mods)
}

fn builtin_new_session_invocation(is_n: bool, mods: KeyMods) -> Option<CommandInvocation> {
    let platform_modifiers = if cfg!(target_os = "macos") {
        mods.command && !mods.ctrl && !mods.shift
    } else {
        mods.ctrl && mods.shift && !mods.command
    };
    (is_n && platform_modifiers && !mods.alt)
        .then(|| CommandInvocation::from_action("new_mux_session", Caller::BuiltinKeybinding))
}

/// Resolve a snake_case binding-action name (e.g. `"rename_session"`) to its
/// runnable [`KeybindAction`], or `None` if it is unknown or has no app behavior.
/// [`crate::commands::CommandRegistry`] uses this to resolve core keybinding executors.
pub fn keybind_action_for_name(name: &str) -> Option<KeybindAction> {
    keybind_action(parse_action(name).ok()?).ok()
}

fn keybind_action(action: BindingAction) -> Result<KeybindAction> {
    use BindingAction as Binding;
    use FontSizeAction as Font;
    use KeybindAction as Keybind;
    use MuxKeyAction as Mux;
    use TerminalFindAction as Find;
    use TerminalScrollAction as Scroll;

    let action = match action {
        Binding::ReloadConfig => Keybind::App(AppAction::ReloadConfig),
        Binding::Ignore => Keybind::App(AppAction::Ignore),
        Binding::NewWindow => Keybind::App(AppAction::NewWindow),
        Binding::NewMuxSession => Keybind::App(AppAction::NewMuxSession),
        Binding::SessionPicker => Keybind::App(AppAction::SessionPicker),
        Binding::CommandPalette => Keybind::App(AppAction::CommandPalette),
        Binding::CloseWindow => Keybind::App(AppAction::Close),
        Binding::Quit => Keybind::App(AppAction::Quit),
        Binding::CloseSurface => Keybind::Mux(Mux::ClosePane),
        Binding::ToggleFullscreen => Keybind::App(AppAction::ToggleFullscreen),
        Binding::ToggleSidebarFocus => Keybind::App(AppAction::ToggleSidebarFocus),
        Binding::ToggleSidebarVisibility => Keybind::App(AppAction::ToggleSidebarVisibility),
        Binding::OpenSettings => Keybind::App(AppAction::OpenSettings),
        Binding::ChangeAppearance(choice) => {
            Keybind::App(AppAction::ChangeAppearance(appearance_mode(choice)))
        }
        Binding::SwitchTheme => Keybind::App(AppAction::SwitchTheme),
        Binding::RenameSession => Keybind::App(AppAction::RenameSession),
        Binding::MoveSessionToSpace => Keybind::App(AppAction::MoveSessionToSpace),
        Binding::RenameTab => Keybind::App(AppAction::RenameTab),
        Binding::NewTab => Keybind::Mux(Mux::NewTab),
        Binding::NextTab => Keybind::Mux(Mux::NextTab),
        Binding::PreviousTab => Keybind::Mux(Mux::PreviousTab),
        Binding::LastTab => Keybind::Mux(Mux::LastTab),
        Binding::SelectTab(index) => Keybind::Mux(Mux::SelectTab(index)),
        Binding::MoveTab(delta) => Keybind::Mux(Mux::MoveTab(delta)),
        Binding::SplitRight => Keybind::Mux(Mux::SplitPane(crate::layout::SplitDirection::Right)),
        Binding::SplitDown => Keybind::Mux(Mux::SplitPane(crate::layout::SplitDirection::Down)),
        Binding::SelectPane(direction) => Keybind::Mux(Mux::SelectPane(mux_direction(direction))),
        Binding::NextPane => Keybind::Mux(Mux::NextPane),
        Binding::PreviousPane => Keybind::Mux(Mux::PreviousPane),
        Binding::KillPane => Keybind::Mux(Mux::KillPane),
        Binding::TogglePaneZoom => Keybind::Mux(Mux::TogglePaneZoom),
        Binding::NextSession => Keybind::Mux(Mux::NextSession),
        Binding::PreviousSession => Keybind::Mux(Mux::PreviousSession),
        Binding::LastSession => Keybind::Mux(Mux::LastSession),
        Binding::SelectSession(index) => Keybind::Mux(Mux::SelectSession(index)),
        Binding::MoveSession(delta) => Keybind::Mux(Mux::MoveSession(delta)),
        Binding::CreateSpace => Keybind::App(AppAction::CreateSpace),
        Binding::EditSpace => Keybind::App(AppAction::EditSpace),
        Binding::CloseSpace => Keybind::App(AppAction::CloseSpace),
        Binding::NextSpace => Keybind::App(AppAction::NextSpace),
        Binding::PreviousSpace => Keybind::App(AppAction::PreviousSpace),
        Binding::SelectSpace(index) => Keybind::App(AppAction::SelectSpace(index)),
        Binding::DitchSession => Keybind::App(AppAction::DitchSession),
        Binding::ShowKeybinds => Keybind::App(AppAction::ShowKeybinds),
        Binding::ScrollToTop => Keybind::Scroll(Scroll::Top),
        Binding::ScrollToBottom => Keybind::Scroll(Scroll::Bottom),
        Binding::ScrollPageUp => Keybind::Scroll(Scroll::PageUp),
        Binding::ScrollPageDown => Keybind::Scroll(Scroll::PageDown),
        Binding::ScrollPageLines(lines) => Keybind::Scroll(Scroll::Lines(lines)),
        Binding::StartSearch => Keybind::Find(Find::Prompt),
        Binding::EndSearch => Keybind::Find(Find::Close),
        Binding::Search(value) => Keybind::Find(Find::Search(value)),
        Binding::SearchSelection => Keybind::Find(Find::SearchSelection),
        Binding::NavigateSearch(direction) => Keybind::Find(match direction {
            NavigateSearch::Previous => Find::Previous,
            NavigateSearch::Next => Find::Next,
        }),
        Binding::Csi(value) => Keybind::Write(csi_bytes(&value)),
        Binding::Esc(value) => Keybind::Write(esc_bytes(&value)),
        Binding::Text(value) => Keybind::Write(text_action_bytes(&value)),
        Binding::IncreaseFontSize(delta) => Keybind::Font(Font::Increase(delta)),
        Binding::DecreaseFontSize(delta) => Keybind::Font(Font::Decrease(delta)),
        Binding::ResetFontSize => Keybind::Font(Font::Reset),
        Binding::SetFontSize(size) => Keybind::Font(Font::Set(size)),
        Binding::CopyToClipboard(format) => Keybind::CopyToClipboard(format),
        Binding::CopyMode => Keybind::CopyMode,
        Binding::PasteFromClipboard => Keybind::PasteFromClipboard,
        unsupported => anyhow::bail!("{} has no Bootty app behavior", unsupported.format_entry()),
    };
    Ok(action)
}

fn appearance_mode(choice: AppearanceChoice) -> bootty_config::config::AppearanceMode {
    match choice {
        AppearanceChoice::System => bootty_config::config::AppearanceMode::System,
        AppearanceChoice::Light => bootty_config::config::AppearanceMode::Light,
        AppearanceChoice::Dark => bootty_config::config::AppearanceMode::Dark,
    }
}

fn mux_direction(direction: PaneDirection) -> MuxDirection {
    match direction {
        PaneDirection::Left => MuxDirection::Left,
        PaneDirection::Down => MuxDirection::Down,
        PaneDirection::Up => MuxDirection::Up,
        PaneDirection::Right => MuxDirection::Right,
    }
}

fn csi_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() + 2);
    bytes.extend_from_slice(b"\x1b[");
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn esc_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() + 1);
    bytes.push(0x1b);
    bytes.extend_from_slice(value.as_bytes());
    bytes
}

fn text_action_bytes(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                bytes.push(b'\n');
            }
            Some('r') => {
                chars.next();
                bytes.push(b'\r');
            }
            Some('t') => {
                chars.next();
                bytes.push(b'\t');
            }
            Some('e') => {
                chars.next();
                bytes.push(0x1b);
            }
            Some('\\') => {
                chars.next();
                bytes.push(b'\\');
            }
            Some('x') => {
                chars.next();
                let Some(high) = chars.next().and_then(|value| value.to_digit(16)) else {
                    bytes.extend_from_slice(b"\\x");
                    continue;
                };
                let Some(low) = chars.next().and_then(|value| value.to_digit(16)) else {
                    bytes.extend_from_slice(format!("\\x{high:x}").as_bytes());
                    continue;
                };
                bytes.push(((high << 4) | low) as u8);
            }
            Some(other) => {
                chars.next();
                bytes.push(b'\\');
                let mut buf = [0; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => bytes.push(b'\\'),
        }
    }
    bytes
}

fn binding_triggers_for_egui_key(
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> Vec<BindingTrigger> {
    binding_triggers_for_egui_key_with_modifier_sides(key, modifiers, ModifierSideState::default())
}

fn binding_triggers_for_egui_key_with_modifier_sides(
    key: egui::Key,
    modifiers: egui::Modifiers,
    modifier_sides: ModifierSideState,
) -> Vec<BindingTrigger> {
    let Some(terminal_key) = terminal_key(key) else {
        return Vec::new();
    };
    let input = KeyInput {
        key: terminal_key,
        mods: key_mods_for_egui_binding(modifiers, modifier_sides),
        repeat: false,
        utf8: None,
        unshifted: binding_char_for_egui_key(key),
    };
    binding_triggers_for_key_input(input)
}
fn binding_triggers_for_egui_scroll_with_modifier_sides(
    up: bool,
    modifiers: egui::Modifiers,
    modifier_sides: ModifierSideState,
) -> Vec<BindingTrigger> {
    let input = KeyInput {
        key: TerminalKey::A,
        mods: key_mods_for_egui_binding(modifiers, modifier_sides),
        repeat: false,
        utf8: None,
        unshifted: None,
    };
    let key = if up {
        BindingKey::ScrollUp
    } else {
        BindingKey::ScrollDown
    };
    BindingTrigger::input_mod_candidates(input)
        .into_iter()
        .map(|mods| BindingTrigger {
            mods,
            key: key.clone(),
        })
        .collect()
}

pub fn key_mods_for_egui_binding(
    modifiers: egui::Modifiers,
    modifier_sides: ModifierSideState,
) -> KeyMods {
    let mut input = KeyInput {
        key: TerminalKey::A,
        mods: KeyMods {
            shift: modifiers.shift,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            // egui aliases `command` to `ctrl` off macOS, which would spuriously set `command` for any
            // Ctrl press and break Ctrl / Ctrl+Shift bindings. Only treat the real Cmd key as command.
            command: cfg!(target_os = "macos") && (modifiers.command || modifiers.mac_cmd),
            ..Default::default()
        },
        repeat: false,
        utf8: None,
        unshifted: None,
    };
    modifier_sides.apply_to_key_input(&mut input);
    input.mods
}

fn binding_triggers_for_key_input(input: KeyInput) -> Vec<BindingTrigger> {
    let mut triggers = Vec::new();
    for mods in BindingTrigger::input_mod_candidates(input) {
        triggers.push(BindingTrigger {
            mods,
            key: BindingKey::Physical(input.key),
        });
        if let Some(ch) = input.unshifted.or_else(|| input.utf8.and_then(single_char)) {
            triggers.push(BindingTrigger {
                mods,
                key: BindingKey::Unicode(ch),
            });
        }
    }
    triggers
}

fn single_char(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    chars.next().is_none().then_some(ch)
}

fn binding_char_for_egui_key(key: egui::Key) -> Option<char> {
    Some(match key {
        egui::Key::A => 'a',
        egui::Key::B => 'b',
        egui::Key::C => 'c',
        egui::Key::D => 'd',
        egui::Key::E => 'e',
        egui::Key::F => 'f',
        egui::Key::G => 'g',
        egui::Key::H => 'h',
        egui::Key::I => 'i',
        egui::Key::J => 'j',
        egui::Key::K => 'k',
        egui::Key::L => 'l',
        egui::Key::M => 'm',
        egui::Key::N => 'n',
        egui::Key::O => 'o',
        egui::Key::P => 'p',
        egui::Key::Q => 'q',
        egui::Key::R => 'r',
        egui::Key::S => 's',
        egui::Key::T => 't',
        egui::Key::U => 'u',
        egui::Key::V => 'v',
        egui::Key::W => 'w',
        egui::Key::X => 'x',
        egui::Key::Y => 'y',
        egui::Key::Z => 'z',
        egui::Key::Num0 => '0',
        egui::Key::Num1 => '1',
        egui::Key::Num2 => '2',
        egui::Key::Num3 => '3',
        egui::Key::Num4 => '4',
        egui::Key::Num5 => '5',
        egui::Key::Num6 => '6',
        egui::Key::Num7 => '7',
        egui::Key::Num8 => '8',
        egui::Key::Num9 => '9',
        egui::Key::Comma => ',',
        egui::Key::Period => '.',
        egui::Key::Slash => '/',
        egui::Key::Semicolon => ';',
        egui::Key::Quote => '\'',
        egui::Key::Minus => '-',
        egui::Key::Plus | egui::Key::Equals => '=',
        egui::Key::Backslash => '\\',
        egui::Key::Backtick => '`',
        egui::Key::OpenBracket | egui::Key::OpenCurlyBracket => '[',
        egui::Key::CloseBracket | egui::Key::CloseCurlyBracket => ']',
        egui::Key::Space => ' ',
        _ => return None,
    })
}
