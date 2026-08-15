use std::{fs, io, path::Path};

use bootty_write::{CommitOutcome, NewFileMode, ResolveTargetError, WriteTarget};
use toml_edit::{Array, DocumentMut, InlineTable, Value};

use super::{
    ConfigDocument, ConfigLoadError, ConfigResult, SegmentAlign, SshAuthenticationConfig,
    SshHostKeyPolicyConfig, SshProfileConfig, StatusSegment, load_or_create_config_document,
};

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
        let root = ["ssh-profiles", id];
        self.set_item(&[root[0], root[1], "name"], toml_edit::value(&profile.name))?;
        self.set_item(&[root[0], root[1], "host"], toml_edit::value(&profile.host))?;
        if let Some(user) = &profile.user {
            self.set_item(&[root[0], root[1], "user"], toml_edit::value(user))?;
        }
        if let Some(port) = profile.port {
            self.set_item(
                &[root[0], root[1], "port"],
                toml_edit::value(i64::from(port)),
            )?;
        }
        let authentication = match profile.authentication {
            SshAuthenticationConfig::Auto => "auto",
            SshAuthenticationConfig::Agent => "agent",
            SshAuthenticationConfig::KeyFile => "key-file",
        };
        self.set_item(
            &[root[0], root[1], "authentication"],
            toml_edit::value(authentication),
        )?;
        let host_key_policy = match profile.host_key_policy {
            SshHostKeyPolicyConfig::Strict => "strict",
            SshHostKeyPolicyConfig::AcceptNew => "accept-new",
        };
        self.set_item(
            &[root[0], root[1], "host-key-policy"],
            toml_edit::value(host_key_policy),
        )?;
        if let Some(identity_file) = &profile.identity_file {
            self.set_item(
                &[root[0], root[1], "identity-file"],
                toml_edit::value(identity_file.display().to_string()),
            )?;
        }
        if let Some(proxy_jump) = &profile.proxy_jump {
            self.set_item(
                &[root[0], root[1], "proxy-jump"],
                toml_edit::value(proxy_jump),
            )?;
        }
        self.set_item(
            &[root[0], root[1], "program"],
            toml_edit::value(&profile.program),
        )?;
        if !profile.args.is_empty() {
            let mut args = Array::new();
            for arg in &profile.args {
                args.push(arg.as_str());
            }
            self.set_item(&[root[0], root[1], "args"], toml_edit::value(args))?;
        }
        Ok(())
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

fn write_error(path: &Path, phase: &str, error: impl std::fmt::Display) -> ConfigLoadError {
    ConfigLoadError::new(format!(
        "failed to write config file {}: {phase}: {error}",
        path.display()
    ))
}
