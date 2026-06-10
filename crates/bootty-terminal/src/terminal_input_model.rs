use libghostty_vt::{key, mouse};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MacosOptionAsAlt {
    None,
    Left,
    Right,
    #[default]
    Both,
}

impl From<MacosOptionAsAlt> for key::OptionAsAlt {
    fn from(value: MacosOptionAsAlt) -> Self {
        match value {
            MacosOptionAsAlt::None => Self::False,
            MacosOptionAsAlt::Left => Self::Left,
            MacosOptionAsAlt::Right => Self::Right,
            MacosOptionAsAlt::Both => Self::True,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: TerminalKey,
    pub mods: KeyMods,
    pub repeat: bool,
    pub utf8: Option<&'static str>,
    pub unshifted: Option<char>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyMods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub command: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub right_shift: bool,
    pub right_alt: bool,
    pub right_ctrl: bool,
    pub right_command: bool,
}

impl From<KeyMods> for key::Mods {
    fn from(value: KeyMods) -> Self {
        let mut mods = key::Mods::empty();
        if value.shift {
            mods |= key::Mods::SHIFT;
        }
        if value.alt {
            mods |= key::Mods::ALT;
        }
        if value.ctrl {
            mods |= key::Mods::CTRL;
        }
        if value.command {
            mods |= key::Mods::SUPER;
        }
        if value.caps_lock {
            mods |= key::Mods::CAPS_LOCK;
        }
        if value.num_lock {
            mods |= key::Mods::NUM_LOCK;
        }
        if value.shift && value.right_shift {
            mods |= key::Mods::SHIFT_SIDE;
        }
        if value.alt && value.right_alt {
            mods |= key::Mods::ALT_SIDE;
        }
        if value.ctrl && value.right_ctrl {
            mods |= key::Mods::CTRL_SIDE;
        }
        if value.command && value.right_command {
            mods |= key::Mods::SUPER_SIDE;
        }
        mods
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseInput {
    pub action: MouseAction,
    pub button: Option<MouseButton>,
    pub mods: KeyMods,
    pub x: f32,
    pub y: f32,
    pub size: MouseEncoderSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEncoderSize {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_right: u32,
    pub padding_left: u32,
}

impl From<MouseEncoderSize> for mouse::EncoderSize {
    fn from(value: MouseEncoderSize) -> Self {
        Self {
            screen_width: value.screen_width,
            screen_height: value.screen_height,
            cell_width: value.cell_width,
            cell_height: value.cell_height,
            padding_top: value.padding_top,
            padding_bottom: value.padding_bottom,
            padding_right: value.padding_right,
            padding_left: value.padding_left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

impl From<MouseAction> for mouse::Action {
    fn from(value: MouseAction) -> Self {
        match value {
            MouseAction::Press => mouse::Action::Press,
            MouseAction::Release => mouse::Action::Release,
            MouseAction::Motion => mouse::Action::Motion,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Eleven,
}

impl From<MouseButton> for mouse::Button {
    fn from(value: MouseButton) -> Self {
        match value {
            MouseButton::Left => mouse::Button::Left,
            MouseButton::Right => mouse::Button::Right,
            MouseButton::Middle => mouse::Button::Middle,
            MouseButton::Four => mouse::Button::Four,
            MouseButton::Five => mouse::Button::Five,
            MouseButton::Six => mouse::Button::Six,
            MouseButton::Seven => mouse::Button::Seven,
            MouseButton::Eight => mouse::Button::Eight,
            MouseButton::Nine => mouse::Button::Nine,
            MouseButton::Ten => mouse::Button::Ten,
            MouseButton::Eleven => mouse::Button::Eleven,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalKey {
    Backquote,
    Backslash,
    BracketLeft,
    BracketRight,
    Comma,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    Equal,
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Minus,
    Period,
    Quote,
    Semicolon,
    Slash,
    Enter,
    Tab,
    Backspace,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowRight,
    ArrowLeft,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    Insert,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadDecimal,
    NumpadDivide,
    NumpadEnter,
    NumpadEqual,
    NumpadMultiply,
    NumpadSubtract,
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
}

impl From<TerminalKey> for key::Key {
    fn from(value: TerminalKey) -> Self {
        match value {
            TerminalKey::Backquote => key::Key::Backquote,
            TerminalKey::Backslash => key::Key::Backslash,
            TerminalKey::BracketLeft => key::Key::BracketLeft,
            TerminalKey::BracketRight => key::Key::BracketRight,
            TerminalKey::Comma => key::Key::Comma,
            TerminalKey::Digit0 => key::Key::Digit0,
            TerminalKey::Digit1 => key::Key::Digit1,
            TerminalKey::Digit2 => key::Key::Digit2,
            TerminalKey::Digit3 => key::Key::Digit3,
            TerminalKey::Digit4 => key::Key::Digit4,
            TerminalKey::Digit5 => key::Key::Digit5,
            TerminalKey::Digit6 => key::Key::Digit6,
            TerminalKey::Digit7 => key::Key::Digit7,
            TerminalKey::Digit8 => key::Key::Digit8,
            TerminalKey::Digit9 => key::Key::Digit9,
            TerminalKey::Equal => key::Key::Equal,
            TerminalKey::A => key::Key::A,
            TerminalKey::B => key::Key::B,
            TerminalKey::C => key::Key::C,
            TerminalKey::D => key::Key::D,
            TerminalKey::E => key::Key::E,
            TerminalKey::F => key::Key::F,
            TerminalKey::G => key::Key::G,
            TerminalKey::H => key::Key::H,
            TerminalKey::I => key::Key::I,
            TerminalKey::J => key::Key::J,
            TerminalKey::K => key::Key::K,
            TerminalKey::L => key::Key::L,
            TerminalKey::M => key::Key::M,
            TerminalKey::N => key::Key::N,
            TerminalKey::O => key::Key::O,
            TerminalKey::P => key::Key::P,
            TerminalKey::Q => key::Key::Q,
            TerminalKey::R => key::Key::R,
            TerminalKey::S => key::Key::S,
            TerminalKey::T => key::Key::T,
            TerminalKey::U => key::Key::U,
            TerminalKey::V => key::Key::V,
            TerminalKey::W => key::Key::W,
            TerminalKey::X => key::Key::X,
            TerminalKey::Y => key::Key::Y,
            TerminalKey::Z => key::Key::Z,
            TerminalKey::Minus => key::Key::Minus,
            TerminalKey::Period => key::Key::Period,
            TerminalKey::Quote => key::Key::Quote,
            TerminalKey::Semicolon => key::Key::Semicolon,
            TerminalKey::Slash => key::Key::Slash,
            TerminalKey::Enter => key::Key::Enter,
            TerminalKey::Tab => key::Key::Tab,
            TerminalKey::Backspace => key::Key::Backspace,
            TerminalKey::Escape => key::Key::Escape,
            TerminalKey::ArrowUp => key::Key::ArrowUp,
            TerminalKey::ArrowDown => key::Key::ArrowDown,
            TerminalKey::ArrowRight => key::Key::ArrowRight,
            TerminalKey::ArrowLeft => key::Key::ArrowLeft,
            TerminalKey::Delete => key::Key::Delete,
            TerminalKey::Home => key::Key::Home,
            TerminalKey::End => key::Key::End,
            TerminalKey::PageUp => key::Key::PageUp,
            TerminalKey::PageDown => key::Key::PageDown,
            TerminalKey::Space => key::Key::Space,
            TerminalKey::Insert => key::Key::Insert,
            TerminalKey::F1 => key::Key::F1,
            TerminalKey::F2 => key::Key::F2,
            TerminalKey::F3 => key::Key::F3,
            TerminalKey::F4 => key::Key::F4,
            TerminalKey::F5 => key::Key::F5,
            TerminalKey::F6 => key::Key::F6,
            TerminalKey::F7 => key::Key::F7,
            TerminalKey::F8 => key::Key::F8,
            TerminalKey::F9 => key::Key::F9,
            TerminalKey::F10 => key::Key::F10,
            TerminalKey::F11 => key::Key::F11,
            TerminalKey::F12 => key::Key::F12,
            TerminalKey::Numpad0 => key::Key::Numpad0,
            TerminalKey::Numpad1 => key::Key::Numpad1,
            TerminalKey::Numpad2 => key::Key::Numpad2,
            TerminalKey::Numpad3 => key::Key::Numpad3,
            TerminalKey::Numpad4 => key::Key::Numpad4,
            TerminalKey::Numpad5 => key::Key::Numpad5,
            TerminalKey::Numpad6 => key::Key::Numpad6,
            TerminalKey::Numpad7 => key::Key::Numpad7,
            TerminalKey::Numpad8 => key::Key::Numpad8,
            TerminalKey::Numpad9 => key::Key::Numpad9,
            TerminalKey::NumpadAdd => key::Key::NumpadAdd,
            TerminalKey::NumpadDecimal => key::Key::NumpadDecimal,
            TerminalKey::NumpadDivide => key::Key::NumpadDivide,
            TerminalKey::NumpadEnter => key::Key::NumpadEnter,
            TerminalKey::NumpadEqual => key::Key::NumpadEqual,
            TerminalKey::NumpadMultiply => key::Key::NumpadMultiply,
            TerminalKey::NumpadSubtract => key::Key::NumpadSubtract,
            TerminalKey::ShiftLeft => key::Key::ShiftLeft,
            TerminalKey::ShiftRight => key::Key::ShiftRight,
            TerminalKey::ControlLeft => key::Key::ControlLeft,
            TerminalKey::ControlRight => key::Key::ControlRight,
            TerminalKey::AltLeft => key::Key::AltLeft,
            TerminalKey::AltRight => key::Key::AltRight,
        }
    }
}
