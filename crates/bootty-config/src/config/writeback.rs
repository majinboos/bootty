use std::{fs, io, path::Path};

use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use toml_edit::DocumentMut;

use super::{ConfigDocument, ConfigLoadError, ConfigResult, load_or_create_config_document};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigWriteOutcome {
    Confirmed,
    CommittedWithDurabilityWarning(String),
}

impl ConfigWriteOutcome {
    pub fn durability_warning(&self) -> Option<&str> {
        match self {
            Self::Confirmed => None,
            Self::CommittedWithDurabilityWarning(warning) => Some(warning),
        }
    }
}

pub fn update_config_document(
    path: impl AsRef<Path>,
    mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
) -> ConfigResult<ConfigWriteOutcome> {
    let requested_path = path.as_ref();
    if let Some(parent) = requested_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| write_error(requested_path, "prepare", error))?;
    }
    let target = resolve_config_target(requested_path)?;
    let target = target
        .lock()
        .map_err(|error| write_error(requested_path, "claim writer lease", error))?;
    let mut document = load_or_create_config_document(requested_path)?;
    mutate(&mut document)?;
    let source = document.document.to_string();
    source.parse::<DocumentMut>().map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to write config file {}: validate: {error}",
            requested_path.display()
        ))
    })?;

    let outcome = target
        .replace(source.as_bytes(), NewFileMode::Private)
        .map_err(|error| {
            let phase = error.phase();
            write_error(requested_path, phase, error.into_io())
        })?;
    Ok(match outcome {
        CommitOutcome::Confirmed => ConfigWriteOutcome::Confirmed,
        CommitOutcome::CommittedWithDurabilityWarning(error) => {
            ConfigWriteOutcome::CommittedWithDurabilityWarning(format!(
                "config file {} was replaced, but sync directory failed: {error}",
                requested_path.display()
            ))
        }
    })
}

fn resolve_config_target(path: &Path) -> ConfigResult<WriteTarget> {
    WriteTarget::resolve(path).map_err(|error| {
        let error = match error {
            ResolveTargetError::SymlinkCycle => {
                io::Error::new(io::ErrorKind::InvalidInput, "config symlink cycle detected")
            }
            ResolveTargetError::Io(error) => error,
        };
        write_error(path, "resolve target", error)
    })
}

fn write_error(path: &Path, phase: &str, error: impl std::fmt::Display) -> ConfigLoadError {
    ConfigLoadError::new(format!(
        "failed to write config file {}: {phase}: {error}",
        path.display()
    ))
}
