use bootty_winit::modifier_remap::{ModifierRemapParseError, ModifierRemapSet};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid modifier-remap {entry:?}: {source}")]
pub struct ModifierRemapConfigError {
    entry: String,
    r#source: ModifierRemapParseError,
}

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
