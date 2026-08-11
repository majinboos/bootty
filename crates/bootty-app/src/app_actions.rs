use std::str::FromStr;

use anyhow::Result;
use eframe::egui;

use crate::{
    commands::{Caller, CommandInvocation},
    config::InputConfig,
    direct_input::ModifierSideState,
    input::terminal_key,
    input_binding::{
        AppearanceChoice, BindingAction, BindingElement, BindingKey, BindingTrigger,
        CopyToClipboard, NavigateSearch, PaneDirection, parse_action, parse_binding_elements,
    },
    mux::command::MuxDirection,
    terminal::{KeyInput, KeyMods, TerminalKey},
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
    ChangeAppearance(crate::config::AppearanceMode),
    SwitchTheme,
    RenameSession,
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
struct BoundCommand {
    action_name: String,
}

impl BoundCommand {
    fn invocation(&self) -> CommandInvocation {
        CommandInvocation::from_action(&self.action_name, Caller::Keybinding)
    }
}

#[derive(Clone, Debug)]
struct AppKeyBinding {
    leader: Option<BindingTrigger>,
    trigger: BindingTrigger,
    command: BoundCommand,
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
                            command: BoundCommand { action_name },
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
        .map(|command| command.invocation())
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
        .map(|command| command.invocation())
    }

    pub fn invocation_for_input(&mut self, input: KeyInput) -> Option<CommandInvocation> {
        self.command_for_candidates(binding_triggers_for_key_input(input))
            .map(|command| command.invocation())
    }

    fn command_for_candidates(&mut self, candidates: Vec<BindingTrigger>) -> Option<BoundCommand> {
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
                        .map(|binding| binding.command.clone())
                })
                .or_else(|| {
                    Some(BoundCommand {
                        action_name: "ignore".to_owned(),
                    })
                });
        }

        if let Some(leader) = candidates.iter().find_map(|candidate| {
            self.leaders
                .iter()
                .find(|leader| *leader == candidate)
                .cloned()
        }) {
            self.active_leader = Some(leader);
            return Some(BoundCommand {
                action_name: "ignore".to_owned(),
            });
        }

        candidates.iter().find_map(|candidate| {
            self.bindings
                .iter()
                .find(|binding| binding.leader.is_none() && binding.trigger == *candidate)
                .map(|binding| binding.command.clone())
        })
    }
}

impl SidebarKeyBindings {
    pub fn from_keybinds(keybinds: &[String]) -> Result<Self> {
        let mut bindings = Vec::new();
        for entry in keybinds {
            let (trigger, action) = split_sidebar_binding(entry)
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

fn split_sidebar_binding(input: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    while let Some(index) = input[offset..].find('=') {
        let index = offset + index;
        if index + 1 < input.len() && matches!(input.as_bytes()[index + 1], b'+' | b'=') {
            offset = index + 1;
            continue;
        }
        return Some((&input[..index], &input[index + 1..]));
    }
    None
}

fn sidebar_action(input: &str) -> Result<SidebarAction> {
    match input {
        "ignore" => Ok(SidebarAction::Ignore),
        "previous_session" => Ok(SidebarAction::PreviousSession),
        "next_session" => Ok(SidebarAction::NextSession),
        "activate_session" => Ok(SidebarAction::ActivateSession),
        "focus_terminal" => Ok(SidebarAction::FocusTerminal),
        _ => anyhow::bail!("{input} has no Bootty sidebar behavior"),
    }
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
    let matches = if cfg!(target_os = "macos") {
        (modifiers.command || modifiers.mac_cmd)
            && !modifiers.alt
            && !modifiers.ctrl
            && !modifiers.shift
    } else {
        // egui inflates `command` from `ctrl` off macOS, so it is not checked here.
        modifiers.ctrl && modifiers.shift && !modifiers.alt
    };
    (key == egui::Key::N && matches)
        .then(|| CommandInvocation::from_action("new_mux_session", Caller::BuiltinKeybinding))
}

pub fn builtin_app_invocation_for_direct_key(input: KeyInput) -> Option<CommandInvocation> {
    let matches = if cfg!(target_os = "macos") {
        input.mods.command && !input.mods.alt && !input.mods.ctrl && !input.mods.shift
    } else {
        input.mods.ctrl && input.mods.shift && !input.mods.alt && !input.mods.command
    };
    (input.key == TerminalKey::N && matches)
        .then(|| CommandInvocation::from_action("new_mux_session", Caller::BuiltinKeybinding))
}

/// Resolve a snake_case binding-action name (e.g. `"rename_session"`) to its
/// runnable [`KeybindAction`], or `None` if it is unknown or has no app behavior.
/// [`crate::commands::CommandRegistry`] uses this to resolve core keybinding executors.
pub fn keybind_action_for_name(name: &str) -> Option<KeybindAction> {
    keybind_action(parse_action(name).ok()?).ok()
}

fn keybind_action(action: BindingAction) -> Result<KeybindAction> {
    match action {
        BindingAction::ReloadConfig => Ok(KeybindAction::App(AppAction::ReloadConfig)),
        BindingAction::Ignore => Ok(KeybindAction::App(AppAction::Ignore)),
        BindingAction::NewWindow => Ok(KeybindAction::App(AppAction::NewWindow)),
        BindingAction::NewMuxSession => Ok(KeybindAction::App(AppAction::NewMuxSession)),
        BindingAction::SessionPicker => Ok(KeybindAction::App(AppAction::SessionPicker)),
        BindingAction::CommandPalette => Ok(KeybindAction::App(AppAction::CommandPalette)),
        BindingAction::CloseWindow => Ok(KeybindAction::App(AppAction::Close)),
        BindingAction::Quit => Ok(KeybindAction::App(AppAction::Quit)),
        BindingAction::CloseSurface => Ok(KeybindAction::Mux(MuxKeyAction::ClosePane)),
        BindingAction::ToggleFullscreen => Ok(KeybindAction::App(AppAction::ToggleFullscreen)),
        BindingAction::ToggleSidebarFocus => Ok(KeybindAction::App(AppAction::ToggleSidebarFocus)),
        BindingAction::ToggleSidebarVisibility => {
            Ok(KeybindAction::App(AppAction::ToggleSidebarVisibility))
        }
        BindingAction::OpenSettings => Ok(KeybindAction::App(AppAction::OpenSettings)),
        BindingAction::ChangeAppearance(choice) => Ok(KeybindAction::App(
            AppAction::ChangeAppearance(appearance_mode(choice)),
        )),
        BindingAction::SwitchTheme => Ok(KeybindAction::App(AppAction::SwitchTheme)),
        BindingAction::RenameSession => Ok(KeybindAction::App(AppAction::RenameSession)),
        BindingAction::RenameTab => Ok(KeybindAction::App(AppAction::RenameTab)),
        BindingAction::NewTab => Ok(KeybindAction::Mux(MuxKeyAction::NewTab)),
        BindingAction::NextTab => Ok(KeybindAction::Mux(MuxKeyAction::NextTab)),
        BindingAction::PreviousTab => Ok(KeybindAction::Mux(MuxKeyAction::PreviousTab)),
        BindingAction::LastTab => Ok(KeybindAction::Mux(MuxKeyAction::LastTab)),
        BindingAction::SelectTab(index) => Ok(KeybindAction::Mux(MuxKeyAction::SelectTab(index))),
        BindingAction::MoveTab(delta) => Ok(KeybindAction::Mux(MuxKeyAction::MoveTab(delta))),
        BindingAction::SplitRight => Ok(KeybindAction::Mux(MuxKeyAction::SplitPane(
            crate::layout::SplitDirection::Right,
        ))),
        BindingAction::SplitDown => Ok(KeybindAction::Mux(MuxKeyAction::SplitPane(
            crate::layout::SplitDirection::Down,
        ))),
        BindingAction::SelectPane(direction) => Ok(KeybindAction::Mux(MuxKeyAction::SelectPane(
            mux_direction(direction),
        ))),
        BindingAction::NextPane => Ok(KeybindAction::Mux(MuxKeyAction::NextPane)),
        BindingAction::PreviousPane => Ok(KeybindAction::Mux(MuxKeyAction::PreviousPane)),
        BindingAction::KillPane => Ok(KeybindAction::Mux(MuxKeyAction::KillPane)),
        BindingAction::TogglePaneZoom => Ok(KeybindAction::Mux(MuxKeyAction::TogglePaneZoom)),
        BindingAction::NextSession => Ok(KeybindAction::Mux(MuxKeyAction::NextSession)),
        BindingAction::PreviousSession => Ok(KeybindAction::Mux(MuxKeyAction::PreviousSession)),
        BindingAction::CreateSpace => Ok(KeybindAction::App(AppAction::CreateSpace)),
        BindingAction::EditSpace => Ok(KeybindAction::App(AppAction::EditSpace)),
        BindingAction::CloseSpace => Ok(KeybindAction::App(AppAction::CloseSpace)),
        BindingAction::NextSpace => Ok(KeybindAction::App(AppAction::NextSpace)),
        BindingAction::PreviousSpace => Ok(KeybindAction::App(AppAction::PreviousSpace)),
        BindingAction::SelectSpace(index) => Ok(KeybindAction::App(AppAction::SelectSpace(index))),
        BindingAction::LastSession => Ok(KeybindAction::Mux(MuxKeyAction::LastSession)),
        BindingAction::SelectSession(index) => {
            Ok(KeybindAction::Mux(MuxKeyAction::SelectSession(index)))
        }
        BindingAction::MoveSession(delta) => {
            Ok(KeybindAction::Mux(MuxKeyAction::MoveSession(delta)))
        }
        BindingAction::DitchSession => Ok(KeybindAction::App(AppAction::DitchSession)),
        BindingAction::ShowKeybinds => Ok(KeybindAction::App(AppAction::ShowKeybinds)),
        BindingAction::ScrollToTop => Ok(KeybindAction::Scroll(TerminalScrollAction::Top)),
        BindingAction::ScrollToBottom => Ok(KeybindAction::Scroll(TerminalScrollAction::Bottom)),
        BindingAction::ScrollPageUp => Ok(KeybindAction::Scroll(TerminalScrollAction::PageUp)),
        BindingAction::ScrollPageDown => Ok(KeybindAction::Scroll(TerminalScrollAction::PageDown)),
        BindingAction::ScrollPageLines(lines) => {
            Ok(KeybindAction::Scroll(TerminalScrollAction::Lines(lines)))
        }
        BindingAction::StartSearch => Ok(KeybindAction::Find(TerminalFindAction::Prompt)),
        BindingAction::EndSearch => Ok(KeybindAction::Find(TerminalFindAction::Close)),
        BindingAction::Search(value) => Ok(KeybindAction::Find(TerminalFindAction::Search(value))),
        BindingAction::SearchSelection => {
            Ok(KeybindAction::Find(TerminalFindAction::SearchSelection))
        }
        BindingAction::NavigateSearch(direction) => Ok(KeybindAction::Find(match direction {
            NavigateSearch::Previous => TerminalFindAction::Previous,
            NavigateSearch::Next => TerminalFindAction::Next,
        })),
        BindingAction::Csi(value) => Ok(KeybindAction::Write(csi_bytes(&value))),
        BindingAction::Esc(value) => Ok(KeybindAction::Write(esc_bytes(&value))),
        BindingAction::Text(value) => Ok(KeybindAction::Write(text_action_bytes(&value))),
        BindingAction::IncreaseFontSize(delta) => {
            Ok(KeybindAction::Font(FontSizeAction::Increase(delta)))
        }
        BindingAction::DecreaseFontSize(delta) => {
            Ok(KeybindAction::Font(FontSizeAction::Decrease(delta)))
        }
        BindingAction::ResetFontSize => Ok(KeybindAction::Font(FontSizeAction::Reset)),
        BindingAction::SetFontSize(size) => Ok(KeybindAction::Font(FontSizeAction::Set(size))),
        BindingAction::CopyToClipboard(format) => Ok(KeybindAction::CopyToClipboard(format)),
        BindingAction::CopyMode => Ok(KeybindAction::CopyMode),
        BindingAction::PasteFromClipboard => Ok(KeybindAction::PasteFromClipboard),
        unsupported => anyhow::bail!("{} has no Bootty app behavior", unsupported.format_entry()),
    }
}

fn appearance_mode(choice: AppearanceChoice) -> crate::config::AppearanceMode {
    match choice {
        AppearanceChoice::System => crate::config::AppearanceMode::System,
        AppearanceChoice::Light => crate::config::AppearanceMode::Light,
        AppearanceChoice::Dark => crate::config::AppearanceMode::Dark,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandRegistry, CoreCommandExecutor};

    fn resolved_keybind_action(invocation: Option<CommandInvocation>) -> Option<KeybindAction> {
        let executor = CommandRegistry::core().resolve(invocation?).ok()?.executor;
        match executor {
            CoreCommandExecutor::Keybind(action) => Some(action),
            CoreCommandExecutor::Mux(_)
            | CoreCommandExecutor::Sidebar(_)
            | CoreCommandExecutor::ReadTerminal
            | CoreCommandExecutor::SessionSelect { .. }
            | CoreCommandExecutor::SessionCreate(_)
            | CoreCommandExecutor::DirectoryResolve { .. }
            | CoreCommandExecutor::DirectoryUsageList { .. }
            | CoreCommandExecutor::WorktreeList { .. }
            | CoreCommandExecutor::WorktreeGet { .. }
            | CoreCommandExecutor::WorktreeCreate { .. }
            | CoreCommandExecutor::WorktreeRemove { .. }
            | CoreCommandExecutor::Extension { .. } => None,
        }
    }

    fn keybind_action_for_key(
        bindings: &mut AppKeyBindings,
        key: egui::Key,
        modifiers: egui::Modifiers,
        modifier_sides: ModifierSideState,
    ) -> Option<KeybindAction> {
        resolved_keybind_action(bindings.invocation_for_key_with_modifier_sides(
            key,
            modifiers,
            modifier_sides,
        ))
    }

    fn keybind_action_for_input(
        bindings: &mut AppKeyBindings,
        input: KeyInput,
    ) -> Option<KeybindAction> {
        resolved_keybind_action(bindings.invocation_for_input(input))
    }

    fn action_for_key(
        bindings: &mut AppKeyBindings,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Option<KeybindAction> {
        keybind_action_for_key(bindings, key, modifiers, ModifierSideState::default())
    }

    fn split_app_actions_for_bindings(
        app_key_bindings: &mut AppKeyBindings,
        events: Vec<egui::Event>,
    ) -> (Vec<egui::Event>, Vec<KeybindAction>) {
        let (events, invocations) = split_app_actions_for_bindings_with_modifier_sides(
            app_key_bindings,
            events,
            ModifierSideState::default(),
        );
        let actions = invocations
            .into_iter()
            .map(|invocation| {
                resolved_keybind_action(Some(invocation)).expect("test keybind command")
            })
            .collect();
        (events, actions)
    }

    #[test]
    fn sidebar_keybindings_create_registry_backed_invocations() {
        let bindings = SidebarKeyBindings::from_keybinds(&["j=next_session".to_owned()])
            .expect("sidebar binding");

        let invocation = bindings
            .invocation_for_key(egui::Key::J, egui::Modifiers::NONE)
            .expect("configured invocation");
        assert_eq!(invocation.caller, Caller::Keybinding);
        assert_eq!(invocation.command, SidebarAction::NextSession.command_id());
        assert_eq!(
            CommandRegistry::core()
                .resolve(invocation)
                .unwrap()
                .executor,
            CoreCommandExecutor::Sidebar(SidebarAction::NextSession)
        );
    }

    #[test]
    fn app_keybindings_route_default_app_shortcuts() {
        // Defaults are the Ghostty preset: reload on cmd/ctrl+shift+, and the command palette on
        // cmd/ctrl+shift+p.
        let mut bindings = AppKeyBindings::from_config(&InputConfig::default()).unwrap();
        let shifted_primary = if cfg!(target_os = "macos") {
            egui::Modifiers {
                shift: true,
                command: true,
                ..Default::default()
            }
        } else {
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        };

        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Comma, shifted_primary),
            Some(KeybindAction::App(AppAction::ReloadConfig))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::P, shifted_primary),
            Some(KeybindAction::App(AppAction::CommandPalette))
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            action_for_key(
                &mut bindings,
                egui::Key::P,
                egui::Modifiers {
                    command: true,
                    ..Default::default()
                }
            ),
            Some(KeybindAction::App(AppAction::SessionPicker))
        );
    }
    #[test]
    fn app_keybindings_route_modified_scroll_to_fractional_font_steps() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "alt+scroll_up=increase_font_size:0.25".to_owned(),
            "alt+scroll_down=decrease_font_size:0.25".to_owned(),
        ])
        .unwrap();
        let modifiers = egui::Modifiers {
            alt: true,
            ..Default::default()
        };
        let wheel = |delta_y| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, delta_y),
            modifiers,
            phase: egui::TouchPhase::Move,
        };

        let (terminal_events, actions) =
            split_app_actions_for_bindings(&mut bindings, vec![wheel(2.0), wheel(-2.0)]);

        assert!(terminal_events.is_empty());
        assert_eq!(
            actions,
            vec![
                KeybindAction::Font(FontSizeAction::Increase(0.25)),
                KeybindAction::Font(FontSizeAction::Decrease(0.25)),
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cmd_y_enters_copy_mode_in_default_ghostty_bindings() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig::default()).unwrap();

        assert_eq!(
            action_for_key(
                &mut bindings,
                egui::Key::Y,
                egui::Modifiers {
                    command: true,
                    ..Default::default()
                }
            ),
            Some(KeybindAction::CopyMode)
        );
    }

    // Bracket keys have no `unshifted` char in egui's Key enum by name; a regression here makes
    // every default `[`/`]` binding (cmd+shift+[ previous_tab, cmd+] next_pane, …) dead keys.
    #[cfg(target_os = "macos")]
    #[test]
    fn bracket_shortcuts_route_through_egui_key_path() {
        let config = crate::config::BoottyConfig::default();
        let mut bindings = AppKeyBindings::from_keybinds(
            &config
                .input
                .keybinds_for_backend(crate::config::MultiplexerBackendConfig::Native),
        )
        .unwrap();
        let cmd_shift = egui::Modifiers {
            command: true,
            shift: true,
            ..Default::default()
        };

        assert_eq!(
            action_for_key(&mut bindings, egui::Key::OpenBracket, cmd_shift),
            Some(KeybindAction::Mux(MuxKeyAction::PreviousTab))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::CloseBracket, cmd_shift),
            Some(KeybindAction::Mux(MuxKeyAction::NextTab))
        );
        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::OpenBracket, cmd),
            Some(KeybindAction::Mux(MuxKeyAction::PreviousPane))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::CloseBracket, cmd),
            Some(KeybindAction::Mux(MuxKeyAction::NextPane))
        );
    }

    #[test]
    fn move_tab_bindings_route_from_egui_and_direct_input() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "alt+shift+,=move_tab:-1".to_owned(),
            "alt+shift+.=move_tab:1".to_owned(),
        ])
        .unwrap();

        assert_eq!(
            keybind_action_for_key(
                &mut bindings,
                egui::Key::Comma,
                egui::Modifiers {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                ModifierSideState {
                    right_alt: true,
                    ..Default::default()
                },
            ),
            Some(KeybindAction::Mux(MuxKeyAction::MoveTab(-1)))
        );

        assert_eq!(
            keybind_action_for_input(
                &mut bindings,
                KeyInput {
                    key: TerminalKey::Comma,
                    mods: crate::terminal::KeyMods {
                        shift: true,
                        alt: true,
                        right_alt: true,
                        ..Default::default()
                    },
                    repeat: false,
                    utf8: Some("<"),
                    unshifted: Some(','),
                },
            ),
            Some(KeybindAction::Mux(MuxKeyAction::MoveTab(-1)))
        );
        assert_eq!(
            keybind_action_for_input(
                &mut bindings,
                KeyInput {
                    key: TerminalKey::Period,
                    mods: crate::terminal::KeyMods {
                        shift: true,
                        alt: true,
                        ..Default::default()
                    },
                    repeat: false,
                    utf8: Some(">"),
                    unshifted: Some('.'),
                },
            ),
            Some(KeybindAction::Mux(MuxKeyAction::MoveTab(1)))
        );
    }

    #[test]
    fn explicit_move_tab_bindings_accept_left_and_right_alt() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "left_alt+shift+,=move_tab:-1".to_owned(),
            "right_alt+shift+,=move_tab:-1".to_owned(),
            "left_alt+shift+.=move_tab:1".to_owned(),
            "right_alt+shift+.=move_tab:1".to_owned(),
        ])
        .unwrap();

        for (side, mods) in [
            (
                ModifierSideState {
                    left_alt: true,
                    ..Default::default()
                },
                crate::terminal::KeyMods {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
            ),
            (
                ModifierSideState {
                    right_alt: true,
                    ..Default::default()
                },
                crate::terminal::KeyMods {
                    shift: true,
                    alt: true,
                    right_alt: true,
                    ..Default::default()
                },
            ),
        ] {
            assert_eq!(
                keybind_action_for_key(
                    &mut bindings,
                    egui::Key::Comma,
                    egui::Modifiers {
                        shift: true,
                        alt: true,
                        ..Default::default()
                    },
                    side,
                ),
                Some(KeybindAction::Mux(MuxKeyAction::MoveTab(-1)))
            );
            assert_eq!(
                keybind_action_for_input(
                    &mut bindings,
                    KeyInput {
                        key: TerminalKey::Comma,
                        mods,
                        repeat: false,
                        utf8: Some("<"),
                        unshifted: Some(','),
                    },
                ),
                Some(KeybindAction::Mux(MuxKeyAction::MoveTab(-1)))
            );
        }
    }

    #[test]
    fn builtin_new_session_shortcut_creates_mux_session_not_terminal_input() {
        let modifiers = if cfg!(target_os = "macos") {
            egui::Modifiers {
                command: true,
                ..Default::default()
            }
        } else {
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        };

        let invocation = builtin_app_invocation_for_key(egui::Key::N, modifiers);

        assert_eq!(
            invocation,
            Some(CommandInvocation::from_action(
                "new_mux_session",
                Caller::BuiltinKeybinding
            ))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn builtin_macos_cmd_n_creates_mux_session_not_terminal_input() {
        let invocation = builtin_app_invocation_for_key(
            egui::Key::N,
            egui::Modifiers {
                mac_cmd: true,
                ..Default::default()
            },
        );

        assert_eq!(
            invocation,
            Some(CommandInvocation::from_action(
                "new_mux_session",
                Caller::BuiltinKeybinding
            ))
        );
    }

    #[test]
    fn builtin_direct_new_session_shortcut_creates_mux_session_not_terminal_input() {
        let mods = if cfg!(target_os = "macos") {
            crate::terminal::KeyMods {
                command: true,
                ..Default::default()
            }
        } else {
            crate::terminal::KeyMods {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        };

        let invocation = builtin_app_invocation_for_direct_key(KeyInput {
            key: TerminalKey::N,
            mods,
            repeat: false,
            utf8: Some("n"),
            unshifted: Some('n'),
        });

        assert_eq!(
            invocation,
            Some(CommandInvocation::from_action(
                "new_mux_session",
                Caller::BuiltinKeybinding
            ))
        );
    }

    #[test]
    fn split_app_actions_consumes_text_paired_with_new_session_shortcut() {
        let modifiers = if cfg!(target_os = "macos") {
            egui::Modifiers::MAC_CMD
        } else {
            egui::Modifiers {
                ctrl: true,
                shift: true,
                ..Default::default()
            }
        };
        let mut bindings = AppKeyBindings::default();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![
                egui::Event::Key {
                    key: egui::Key::N,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Text("~2".to_owned()),
                egui::Event::Text("2~".to_owned()),
                egui::Event::Key {
                    key: egui::Key::N,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers,
                },
            ],
        );

        assert!(
            !terminal_events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(_)))
        );
        assert_eq!(actions, vec![KeybindAction::App(AppAction::NewMuxSession)]);
    }

    #[test]
    fn split_app_actions_consumes_paste_event_paired_with_paste_binding() {
        let (keybind, modifiers) = if cfg!(target_os = "macos") {
            (
                "performable:cmd+v=paste_from_clipboard",
                egui::Modifiers {
                    command: true,
                    ..Default::default()
                },
            )
        } else {
            (
                "performable:ctrl+shift+v=paste_from_clipboard",
                egui::Modifiers {
                    ctrl: true,
                    shift: true,
                    ..Default::default()
                },
            )
        };
        let mut bindings = AppKeyBindings::from_config(&InputConfig {
            keybind: vec![keybind.to_owned()],
            ..Default::default()
        })
        .unwrap();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![
                egui::Event::Key {
                    key: egui::Key::V,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Paste("clipboard".to_owned()),
            ],
        );

        assert!(terminal_events.is_empty());
        assert_eq!(actions, vec![KeybindAction::PasteFromClipboard]);
    }

    #[test]
    fn app_keybindings_match_side_specific_direct_input() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "alt+n=previous_tab".to_owned(),
            "right_alt+n=next_tab".to_owned(),
        ])
        .unwrap();

        let action = keybind_action_for_input(
            &mut bindings,
            KeyInput {
                key: TerminalKey::N,
                mods: crate::terminal::KeyMods {
                    alt: true,
                    right_alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("n"),
                unshifted: Some('n'),
            },
        );

        assert_eq!(action, Some(KeybindAction::Mux(MuxKeyAction::NextTab)));
    }

    #[test]
    fn app_keybindings_match_side_specific_egui_input() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "left_alt+p=previous_tab".to_owned(),
            "right_alt+p=next_tab".to_owned(),
        ])
        .unwrap();

        let action = keybind_action_for_key(
            &mut bindings,
            egui::Key::P,
            egui::Modifiers {
                alt: true,
                ..Default::default()
            },
            ModifierSideState {
                left_alt: true,
                ..Default::default()
            },
        );

        assert_eq!(action, Some(KeybindAction::Mux(MuxKeyAction::PreviousTab)));
    }

    #[test]
    fn app_keybindings_match_partially_side_specific_modifier_chords() {
        let mut bindings = AppKeyBindings::from_keybinds(&[
            "left_alt+shift+n=next_tab".to_owned(),
            "right_alt+shift+n=previous_tab".to_owned(),
        ])
        .unwrap();

        let left_action = keybind_action_for_input(
            &mut bindings,
            KeyInput {
                key: TerminalKey::N,
                mods: crate::terminal::KeyMods {
                    shift: true,
                    alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("N"),
                unshifted: Some('n'),
            },
        );
        let right_action = keybind_action_for_input(
            &mut bindings,
            KeyInput {
                key: TerminalKey::N,
                mods: crate::terminal::KeyMods {
                    shift: true,
                    alt: true,
                    right_alt: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("N"),
                unshifted: Some('n'),
            },
        );

        assert_eq!(left_action, Some(KeybindAction::Mux(MuxKeyAction::NextTab)));
        assert_eq!(
            right_action,
            Some(KeybindAction::Mux(MuxKeyAction::PreviousTab))
        );
    }

    #[test]
    fn split_app_actions_keeps_plain_paste_event_without_paste_binding() {
        let mut bindings = AppKeyBindings::default();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![egui::Event::Paste("clipboard".to_owned())],
        );

        assert_eq!(
            terminal_events,
            vec![egui::Event::Paste("clipboard".to_owned())]
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn split_app_actions_consumes_text_paired_with_leader_sequence_binding() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig {
            keybind: vec!["ctrl+space>c=new_tab".to_owned()],
            ..Default::default()
        })
        .unwrap();
        let (terminal_events, actions) = split_app_actions_for_bindings(
            &mut bindings,
            vec![
                egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        ..Default::default()
                    },
                },
                egui::Event::Key {
                    key: egui::Key::C,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::Text("c".to_owned()),
            ],
        );

        assert!(
            !terminal_events
                .iter()
                .any(|event| matches!(event, egui::Event::Text(_)))
        );
        assert_eq!(
            actions,
            vec![
                KeybindAction::App(AppAction::Ignore),
                KeybindAction::Mux(MuxKeyAction::NewTab)
            ]
        );
    }

    // macOS-only: dispatches `cmd+…` bindings through the egui path, where the primary modifier is
    // representable as `command`. Off macOS egui aliases `command` to `ctrl`, so the primary
    // modifier is Ctrl+Shift instead (covered by the platform-aware tests above).
    #[cfg(target_os = "macos")]
    #[test]
    fn app_keybindings_route_escape_and_text_actions_to_terminal_bytes() {
        let mut bindings = AppKeyBindings::from_config(&InputConfig {
            keybind: vec![
                "cmd+b=esc:090;8~".to_owned(),
                "shift+Enter=text:\\n".to_owned(),
                "cmd+1=text:\\x001".to_owned(),
                "cmd+o=ignore".to_owned(),
                "cmd+==increase_font_size:1".to_owned(),
                "cmd+0=reset_font_size".to_owned(),
                "performable:cmd+c=copy_to_clipboard".to_owned(),
                "performable:cmd+v=paste_from_clipboard".to_owned(),
                "cmd+alt+n=new_window".to_owned(),
                "cmd+q=quit".to_owned(),
                "cmd+alt+ctrl+f=toggle_fullscreen".to_owned(),
            ],
            ..Default::default()
        })
        .unwrap();

        let cmd = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };

        assert_eq!(
            action_for_key(&mut bindings, egui::Key::B, cmd),
            Some(KeybindAction::Write(b"\x1b090;8~".to_vec()))
        );
        assert_eq!(
            keybind_action_for_input(
                &mut bindings,
                KeyInput {
                    key: TerminalKey::B,
                    mods: crate::terminal::KeyMods {
                        command: true,
                        ..Default::default()
                    },
                    repeat: false,
                    utf8: Some("b"),
                    unshifted: Some('b'),
                },
            ),
            Some(KeybindAction::Write(b"\x1b090;8~".to_vec()))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Enter, shift),
            Some(KeybindAction::Write(b"\n".to_vec()))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Num1, cmd),
            Some(KeybindAction::Write(vec![0x00, b'1']))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::O, cmd),
            Some(KeybindAction::App(AppAction::Ignore))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Equals, cmd),
            Some(KeybindAction::Font(FontSizeAction::Increase(1.0)))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Num0, cmd),
            Some(KeybindAction::Font(FontSizeAction::Reset))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::C, cmd),
            Some(KeybindAction::CopyToClipboard(CopyToClipboard::Mixed))
        );
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::V, cmd),
            Some(KeybindAction::PasteFromClipboard)
        );
        let invocation = bindings
            .invocation_for_key_with_modifier_sides(
                egui::Key::N,
                egui::Modifiers {
                    command: true,
                    alt: true,
                    ..Default::default()
                },
                ModifierSideState::default(),
            )
            .expect("configured new-window invocation");
        assert_eq!(invocation.command, "new_window");
        assert_eq!(
            action_for_key(&mut bindings, egui::Key::Q, cmd),
            Some(KeybindAction::App(AppAction::Quit))
        );
        assert_eq!(
            action_for_key(
                &mut bindings,
                egui::Key::F,
                egui::Modifiers {
                    command: true,
                    alt: true,
                    ctrl: true,
                    ..Default::default()
                }
            ),
            Some(KeybindAction::App(AppAction::ToggleFullscreen))
        );
    }

    #[test]
    fn command_palette_action_resolves_rename_tab() {
        assert_eq!(
            keybind_action_for_name("rename_tab"),
            Some(KeybindAction::App(AppAction::RenameTab))
        );
    }
    #[test]
    fn command_palette_action_resolves_space_commands() {
        assert_eq!(
            keybind_action_for_name("create_space"),
            Some(KeybindAction::App(AppAction::CreateSpace))
        );
        assert_eq!(
            keybind_action_for_name("edit_space"),
            Some(KeybindAction::App(AppAction::EditSpace))
        );
        assert_eq!(
            keybind_action_for_name("close_space"),
            Some(KeybindAction::App(AppAction::CloseSpace))
        );
        assert_eq!(
            keybind_action_for_name("next_space"),
            Some(KeybindAction::App(AppAction::NextSpace))
        );
        assert_eq!(
            keybind_action_for_name("previous_space"),
            Some(KeybindAction::App(AppAction::PreviousSpace))
        );
        assert_eq!(
            keybind_action_for_name("select_space:2"),
            Some(KeybindAction::App(AppAction::SelectSpace(2)))
        );
    }
    #[test]
    fn app_keybindings_resolve_space_actions() {
        let keybinds = vec![
            "ctrl+a=next_space".to_owned(),
            "ctrl+b=previous_space".to_owned(),
            "ctrl+c=edit_space".to_owned(),
        ];
        let mut bindings = AppKeyBindings::from_keybinds(&keybinds).unwrap();

        for (key, unshifted, action) in [
            (TerminalKey::A, 'a', AppAction::NextSpace),
            (TerminalKey::B, 'b', AppAction::PreviousSpace),
            (TerminalKey::C, 'c', AppAction::EditSpace),
        ] {
            assert_eq!(
                keybind_action_for_input(
                    &mut bindings,
                    KeyInput {
                        key,
                        mods: crate::terminal::KeyMods {
                            ctrl: true,
                            ..Default::default()
                        },
                        repeat: false,
                        utf8: None,
                        unshifted: Some(unshifted),
                    },
                ),
                Some(KeybindAction::App(action))
            );
        }
    }

    #[test]
    fn app_keybindings_reject_unsupported_configured_actions() {
        let error = AppKeyBindings::from_config(&InputConfig {
            keybind: vec!["cmd+a=select_all".to_owned()],
            ..Default::default()
        })
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported keybind"));
        assert!(error.contains("select_all"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_t_resolves_to_new_tab_in_default_bindings() {
        let keybinds = InputConfig::default()
            .keybinds_for_backend(crate::config::MultiplexerBackendConfig::Native);
        let mut bindings = AppKeyBindings::from_keybinds(&keybinds).unwrap();

        let action = keybind_action_for_input(
            &mut bindings,
            KeyInput {
                key: TerminalKey::T,
                mods: crate::terminal::KeyMods {
                    command: true,
                    ..Default::default()
                },
                repeat: false,
                utf8: Some("t"),
                unshifted: Some('t'),
            },
        );

        assert_eq!(action, Some(KeybindAction::Mux(MuxKeyAction::NewTab)));
    }
}
