use std::{borrow::Borrow, fmt, path::Path};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleIdentity(String);

impl ModuleIdentity {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let path = Path::new(&value);
        if value.is_empty() || path.is_absolute() {
            return Err("extension module identity is invalid".to_owned());
        }
        let parts = path
            .components()
            .map(|component| match component {
                std::path::Component::Normal(part) => part
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "extension module path must be valid UTF-8".to_owned()),
                _ => Err("extension module identity is invalid".to_owned()),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let Some(file) = parts.last() else {
            return Err("extension module identity is invalid".to_owned());
        };
        let file = Path::new(file);
        let stem = file.file_stem().and_then(|value| value.to_str());
        if !matches!(
            file.extension().and_then(|value| value.to_str()),
            Some("lua" | "luau")
        ) || stem.is_none()
            || parts[..parts.len() - 1]
                .iter()
                .any(|part| !valid_part(part))
            || stem.is_some_and(|part| !valid_part(part))
        {
            return Err("extension module identity is invalid".to_owned());
        }
        Ok(Self(parts.join("/")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn namespace(&self) -> String {
        let mut parts = Path::new(&self.0)
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if let Some(file) = parts.last_mut()
            && let Some(stem) = Path::new(file).file_stem().and_then(|stem| stem.to_str())
        {
            *file = stem.to_owned();
        }
        parts.join(".")
    }
}

fn valid_part(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && !value.ends_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

impl fmt::Display for ModuleIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<Path> for ModuleIdentity {
    fn as_ref(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl Borrow<str> for ModuleIdentity {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}
