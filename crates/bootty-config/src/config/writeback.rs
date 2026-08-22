use std::{fs, io, path::Path};

use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Value};

use super::load::{
    ConfigDocument, ConfigLoadError, ConfigResult, load_or_create_config_document,
    validate_config_document,
};
use super::model::BoottyConfig;
use super::model::{SegmentAlign, SshProfileConfig, StatusSegment};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigWriteOutcome {
    Confirmed,
    CommittedWithDurabilityWarning(String),
}

#[derive(Clone, Debug)]
pub struct AcceptedConfigDocument {
    pub config: BoottyConfig,
    pub document: ConfigDocument,
    pub write_outcome: ConfigWriteOutcome,
}

impl ConfigWriteOutcome {
    pub fn durability_warning(&self) -> Option<&str> {
        match self {
            Self::Confirmed => None,
            Self::CommittedWithDurabilityWarning(warning) => Some(warning),
        }
    }
}

impl ConfigDocument {
    pub fn set_f32(&mut self, path: &[&str], value: f32) -> ConfigResult<()> {
        self.set_item(path, toml_edit::value(f64::from(value)))
    }

    pub fn set_bool(&mut self, path: &[&str], value: bool) -> ConfigResult<()> {
        self.set_item(path, toml_edit::value(value))
    }

    pub fn set_str(&mut self, path: &[&str], value: &str) -> ConfigResult<()> {
        self.set_item(path, toml_edit::value(value))
    }

    pub fn set_i64(&mut self, path: &[&str], value: i64) -> ConfigResult<()> {
        self.set_item(path, toml_edit::value(value))
    }

    pub fn set_strings(&mut self, path: &[&str], values: &[String]) -> ConfigResult<()> {
        let mut array = Array::new();
        for value in values {
            array.push(value.as_str());
        }
        self.set_item(path, toml_edit::value(array))
    }

    pub fn set_env(&mut self, path: &[&str], entries: &[(String, String)]) -> ConfigResult<()> {
        let mut array = Array::new();
        for (name, value) in entries {
            let mut table = InlineTable::new();
            table.insert("name", Value::from(name.as_str()));
            table.insert("value", Value::from(value.as_str()));
            array.push(table);
        }
        self.set_item(path, toml_edit::value(array))
    }

    pub fn set_top_bar_enabled(&mut self, enabled: bool) -> ConfigResult<()> {
        self.remove(&["chrome", "status-bar"])?;
        self.set_bool(&["chrome", "top-bar"], enabled)
    }

    pub fn set_top_status_segments(&mut self, segments: &[StatusSegment]) -> ConfigResult<()> {
        self.remove(&["chrome", "status-segment"])?;
        self.set_item(
            &["chrome", "top-segment"],
            toml_edit::value(serialize_status_segments(segments)),
        )
    }

    pub fn set_bottom_status_segments(&mut self, segments: &[StatusSegment]) -> ConfigResult<()> {
        self.set_item(
            &["chrome", "bottom-segment"],
            toml_edit::value(serialize_status_segments(segments)),
        )
    }

    pub fn set_ssh_profile(&mut self, id: &str, profile: &SshProfileConfig) -> ConfigResult<()> {
        self.remove_ssh_profile(id)?;
        let mut serialized = toml_edit::ser::to_document(profile).map_err(|error| {
            ConfigLoadError::new(format!("failed to serialize SSH profile {id:?}: {error}"))
        })?;
        if profile.args.is_empty() {
            serialized.remove("args");
        }
        self.set_item(
            &["ssh-profiles", id],
            Item::Table(serialized.as_table().clone()),
        )
    }

    pub fn remove_ssh_profile(&mut self, id: &str) -> ConfigResult<()> {
        self.remove(&["ssh-profiles", id])
    }
}

fn serialize_status_segments(segments: &[StatusSegment]) -> Array {
    let mut array = Array::new();
    for segment in segments {
        let mut table = InlineTable::new();
        let align = match segment.align {
            SegmentAlign::Left => "left",
            SegmentAlign::Center => "center",
            SegmentAlign::Right => "right",
        };
        table.insert("align", Value::from(align));
        table.insert("module", Value::from(segment.module.as_str()));
        for (key, color) in [("fg", segment.fg), ("bg", segment.bg)] {
            if let Some(color) = color {
                let hex = if color.a == 0xff {
                    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
                } else {
                    format!(
                        "#{:02x}{:02x}{:02x}{:02x}",
                        color.r, color.g, color.b, color.a
                    )
                };
                table.insert(key, Value::from(hex));
            }
        }
        if let Some(icon) = &segment.icon
            && !icon.is_empty()
        {
            table.insert("icon", Value::from(icon.as_str()));
        }
        array.push(table);
    }
    array
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
    let target = lock_config_target(requested_path)?;
    let mut document = load_or_create_config_document(requested_path)?;
    mutate(&mut document)?;
    replace_locked_document(requested_path, &target, document).map(|(_, outcome)| outcome)
}

/// Validate and atomically replace a complete editable config document.
///
/// Resolution, including includes and referenced themes, and the caller's application-specific
/// validation finish before the first byte is replaced. On failure both the caller's accepted
/// config and the file remain unchanged. The validator may return prepared publication state.
pub fn commit_config_document<T>(
    path: impl AsRef<Path>,
    document: ConfigDocument,
    validate: impl FnOnce(&BoottyConfig) -> Result<T, String>,
) -> ConfigResult<(AcceptedConfigDocument, T)> {
    let requested_path = path.as_ref();
    if let Some(parent) = requested_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| write_error(requested_path, "prepare", error))?;
    }
    let target = lock_config_target(requested_path)?;
    commit_locked_document(requested_path, &target, document, validate)
}

fn commit_locked_document<T>(
    requested_path: &Path,
    target: &bootty_write::LockedWriteTarget,
    document: ConfigDocument,
    validate: impl FnOnce(&BoottyConfig) -> Result<T, String>,
) -> ConfigResult<(AcceptedConfigDocument, T)> {
    let config = validate_config_document(requested_path, &document)?;
    let validated = validate(&config).map_err(ConfigLoadError::new)?;
    let (document, write_outcome) = replace_locked_document(requested_path, target, document)?;
    Ok((
        AcceptedConfigDocument {
            config,
            document,
            write_outcome,
        },
        validated,
    ))
}

fn replace_locked_document(
    requested_path: &Path,
    target: &bootty_write::LockedWriteTarget,
    document: ConfigDocument,
) -> ConfigResult<(ConfigDocument, ConfigWriteOutcome)> {
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
    let write_outcome = match outcome {
        CommitOutcome::Confirmed => ConfigWriteOutcome::Confirmed,
        CommitOutcome::CommittedWithDurabilityWarning(error) => {
            ConfigWriteOutcome::CommittedWithDurabilityWarning(format!(
                "config file {} was replaced, but sync directory failed: {error}",
                requested_path.display()
            ))
        }
    };
    Ok((document, write_outcome))
}

pub fn write_font_size_preference(
    path: impl AsRef<Path>,
    size: f32,
) -> ConfigResult<ConfigWriteOutcome> {
    update_config_document(path, |document| document.set_f32(&["font", "size"], size))
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

fn lock_config_target(path: &Path) -> ConfigResult<bootty_write::LockedWriteTarget> {
    resolve_config_target(path)?
        .lock()
        .map_err(|error| write_error(path, "claim writer lease", error))
}

fn write_error(path: &Path, phase: &str, error: impl std::fmt::Display) -> ConfigLoadError {
    ConfigLoadError::new(format!(
        "failed to write config file {}: {phase}: {error}",
        path.display()
    ))
}
