use std::{error::Error, fmt};

use bootty_winit::modifier_remap::{ModifierRemapParseError, ModifierRemapSet};

#[derive(Debug)]
pub struct ModifierRemapConfigError {
    entry: String,
    source: ModifierRemapParseError,
}

impl fmt::Display for ModifierRemapConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid modifier-remap {:?}: {}",
            self.entry, self.source
        )
    }
}

impl Error for ModifierRemapConfigError {}

pub fn resolve_modifier_remaps(
    entries: &[String],
) -> Result<ModifierRemapSet, ModifierRemapConfigError> {
    let mut set = ModifierRemapSet::default();
    for entry in entries {
        set.parse(entry)
            .map_err(|source| ModifierRemapConfigError {
                entry: entry.clone(),
                source,
            })?;
    }
    set.finalize();
    Ok(set)
}
