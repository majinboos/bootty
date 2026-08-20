use std::{error::Error, fmt, str::FromStr};

use crate::terminal::KeyMods;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Command,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierSide {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModifierSpec {
    modifier: Modifier,
    side: ModifierSide,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RemapEntry {
    from: ModifierSpec,
    to: ModifierSpec,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifierRemapSet {
    entries: Vec<RemapEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModifierRemapParseError {
    MissingAssignment,
    InvalidModifier(String),
}

impl fmt::Display for ModifierRemapParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAssignment => write!(f, "missing modifier remap assignment"),
            Self::InvalidModifier(input) => write!(f, "invalid modifier remap modifier {input:?}"),
        }
    }
}

impl Error for ModifierRemapParseError {}

impl ModifierRemapSet {
    pub fn parse(&mut self, input: &str) -> Result<(), ModifierRemapParseError> {
        let (from, to) = input
            .split_once('=')
            .ok_or(ModifierRemapParseError::MissingAssignment)?;
        let from = ParsedModifier::from_str(from)?;
        let to = ParsedModifier::from_str(to)?;
        let to = to.spec_or_default_left();

        match from.side {
            Some(side) => self.entries.push(RemapEntry {
                from: ModifierSpec {
                    modifier: from.modifier,
                    side,
                },
                to,
            }),
            None => {
                for side in [ModifierSide::Left, ModifierSide::Right] {
                    self.entries.push(RemapEntry {
                        from: ModifierSpec {
                            modifier: from.modifier,
                            side,
                        },
                        to,
                    });
                }
            }
        }
        Ok(())
    }

    pub fn finalize(&mut self) {
        self.entries
            .sort_by_key(|entry| entry.from.side != ModifierSide::Right);
    }

    pub fn apply(&self, mods: KeyMods) -> KeyMods {
        let Some(entry) = self.entries.iter().find(|entry| entry.matches(mods)) else {
            return mods;
        };
        let mut remapped = mods;
        entry.from.unset(&mut remapped);
        entry.to.set(&mut remapped);
        remapped
    }

    pub fn formatted_entries(&self) -> Vec<String> {
        if self.entries.is_empty() {
            return vec![String::new()];
        }
        self.entries
            .iter()
            .map(|entry| format!("{}={}", entry.from, entry.to))
            .collect()
    }
}

impl RemapEntry {
    fn matches(self, mods: KeyMods) -> bool {
        self.from.matches(mods)
    }
}

impl ModifierSpec {
    fn matches(self, mods: KeyMods) -> bool {
        match self.modifier {
            Modifier::Shift => mods.shift && mods.right_shift == (self.side == ModifierSide::Right),
            Modifier::Ctrl => mods.ctrl && mods.right_ctrl == (self.side == ModifierSide::Right),
            Modifier::Alt => mods.alt && mods.right_alt == (self.side == ModifierSide::Right),
            Modifier::Command => {
                mods.command && mods.right_command == (self.side == ModifierSide::Right)
            }
        }
    }

    fn set(self, mods: &mut KeyMods) {
        let right = self.side == ModifierSide::Right;
        match self.modifier {
            Modifier::Shift => {
                mods.shift = true;
                mods.right_shift = right;
            }
            Modifier::Ctrl => {
                mods.ctrl = true;
                mods.right_ctrl = right;
            }
            Modifier::Alt => {
                mods.alt = true;
                mods.right_alt = right;
            }
            Modifier::Command => {
                mods.command = true;
                mods.right_command = right;
            }
        }
    }

    fn unset(self, mods: &mut KeyMods) {
        match self.modifier {
            Modifier::Shift => {
                mods.shift = false;
                mods.right_shift = false;
            }
            Modifier::Ctrl => {
                mods.ctrl = false;
                mods.right_ctrl = false;
            }
            Modifier::Alt => {
                mods.alt = false;
                mods.right_alt = false;
            }
            Modifier::Command => {
                mods.command = false;
                mods.right_command = false;
            }
        }
    }
}

impl fmt::Display for ModifierSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let side = match self.side {
            ModifierSide::Left => "left",
            ModifierSide::Right => "right",
        };
        let modifier = match self.modifier {
            Modifier::Shift => "shift",
            Modifier::Ctrl => "ctrl",
            Modifier::Alt => "alt",
            Modifier::Command => "super",
        };
        write!(f, "{side}_{modifier}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParsedModifier {
    modifier: Modifier,
    side: Option<ModifierSide>,
}

impl ParsedModifier {
    fn spec_or_default_left(self) -> ModifierSpec {
        ModifierSpec {
            modifier: self.modifier,
            side: self.side.unwrap_or(ModifierSide::Left),
        }
    }
}

impl FromStr for ParsedModifier {
    type Err = ModifierRemapParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (side, modifier) = match input.split_once('_') {
            Some((side, modifier)) => (
                Some(match side {
                    "left" => ModifierSide::Left,
                    "right" => ModifierSide::Right,
                    _ => return Err(ModifierRemapParseError::InvalidModifier(input.to_owned())),
                }),
                modifier,
            ),
            None => (None, input),
        };
        let modifier = match modifier {
            "shift" => Modifier::Shift,
            "ctrl" | "control" => Modifier::Ctrl,
            "alt" | "opt" | "option" => Modifier::Alt,
            "super" | "cmd" | "command" => Modifier::Command,
            _ => return Err(ModifierRemapParseError::InvalidModifier(input.to_owned())),
        };
        Ok(Self { modifier, side })
    }
}
