use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use tempfile::Builder;
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
    update_config_document_with_fault(path.as_ref(), mutate, WriteFault::None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteFault {
    None,
    #[cfg(test)]
    Sync,
    #[cfg(test)]
    Replace,
    #[cfg(all(test, unix))]
    SyncDirectory,
}

fn update_config_document_with_fault(
    requested_path: &Path,
    mutate: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
    fault: WriteFault,
) -> ConfigResult<ConfigWriteOutcome> {
    if let Some(parent) = requested_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| write_error(requested_path, "prepare", error))?;
    }
    let target = resolve_write_target(requested_path)
        .map_err(|error| write_error(requested_path, "resolve target", error))?;
    let parent = target.parent().ok_or_else(|| {
        write_error(
            requested_path,
            "prepare",
            io::Error::new(io::ErrorKind::InvalidInput, "config target has no parent"),
        )
    })?;
    if !parent.is_dir() {
        return Err(write_error(
            requested_path,
            "prepare",
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("target directory {} does not exist", parent.display()),
            ),
        ));
    }

    let _lease = ConfigWriteLease::acquire(&target)
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

    atomic_replace(requested_path, &target, source.as_bytes(), fault)
}

struct ConfigWriteLease {
    _process: MutexGuard<'static, ()>,
    _file: File,
}

impl ConfigWriteLease {
    fn acquire(target: &Path) -> io::Result<Self> {
        static PROCESS_CONFIG_WRITE_LOCK: Mutex<()> = Mutex::new(());

        let process = PROCESS_CONFIG_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("config writer lock is poisoned"))?;
        let file_name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "config target has no file name",
            )
        })?;
        let mut lock_name = std::ffi::OsString::from(".");
        lock_name.push(file_name);
        lock_name.push(".bootty-write.lock");
        let path = target.with_file_name(lock_name);
        let file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock()?;
        Ok(Self {
            _process: process,
            _file: file,
        })
    }
}

fn resolve_write_target(requested_path: &Path) -> io::Result<PathBuf> {
    let mut current = normalize_absolute_path(requested_path)?;
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "config symlink cycle detected",
            ));
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let link = fs::read_link(&current)?;
                current = if link.is_absolute() {
                    link
                } else {
                    current
                        .parent()
                        .unwrap_or_else(|| Path::new("."))
                        .join(link)
                };
                current = normalize_absolute_path(&current)?;
            }
            Ok(_) => return fs::canonicalize(current),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = current.parent().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "config target has no parent")
                })?;
                let parent = fs::canonicalize(parent)?;
                let file_name = current.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "config target has no file name",
                    )
                })?;
                return Ok(parent.join(file_name));
            }
            Err(error) => return Err(error),
        }
    }
}

fn normalize_absolute_path(path: &Path) -> io::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

fn atomic_replace(
    requested_path: &Path,
    target: &Path,
    bytes: &[u8],
    fault: WriteFault,
) -> ConfigResult<ConfigWriteOutcome> {
    #[cfg(not(test))]
    let _ = fault;
    let parent = target
        .parent()
        .expect("resolved config target has a parent");
    let existing = match fs::metadata(target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(write_error(requested_path, "read permissions", error)),
    };
    let mut temporary = Builder::new()
        .prefix(".bootty-config-")
        .tempfile_in(parent)
        .map_err(|error| write_error(requested_path, "prepare", error))?;

    set_replacement_permissions(temporary.as_file(), existing.as_ref())
        .map_err(|error| write_error(requested_path, "prepare permissions", error))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.flush())
        .map_err(|error| write_error(requested_path, "write", error))?;
    #[cfg(test)]
    if fault == WriteFault::Sync {
        return Err(write_error(
            requested_path,
            "sync",
            io::Error::other("injected sync failure"),
        ));
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| write_error(requested_path, "sync", error))?;
    #[cfg(test)]
    if fault == WriteFault::Replace {
        return Err(write_error(
            requested_path,
            "replace",
            io::Error::other("injected replacement failure"),
        ));
    }

    let temporary = temporary.into_temp_path();
    replace_file(temporary.as_ref(), target, existing.is_some())
        .map_err(|error| write_error(requested_path, "replace", error))?;

    #[cfg(unix)]
    {
        #[cfg(test)]
        if fault == WriteFault::SyncDirectory {
            return Ok(ConfigWriteOutcome::CommittedWithDurabilityWarning(format!(
                "config file {} was replaced, but sync directory failed: injected directory sync failure",
                requested_path.display()
            )));
        }
        if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
            return Ok(ConfigWriteOutcome::CommittedWithDurabilityWarning(format!(
                "config file {} was replaced, but sync directory failed: {error}",
                requested_path.display()
            )));
        }
    }

    Ok(ConfigWriteOutcome::Confirmed)
}

#[cfg(unix)]
fn set_replacement_permissions(file: &File, existing: Option<&fs::Metadata>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = existing.map_or(0o600, |metadata| metadata.permissions().mode() & 0o7777);
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_replacement_permissions(_file: &File, _existing: Option<&fs::Metadata>) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn replace_file(temporary: &Path, target: &Path, _target_exists: bool) -> io::Result<()> {
    fs::rename(temporary, target)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, target: &Path, target_exists: bool) -> io::Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_WRITE_THROUGH, MoveFileExW, ReplaceFileW,
    };

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let temporary = wide(temporary);
    let target = wide(target);
    let result = unsafe {
        if target_exists {
            ReplaceFileW(
                target.as_ptr(),
                temporary.as_ptr(),
                ptr::null(),
                0,
                ptr::null(),
                ptr::null(),
            )
        } else {
            MoveFileExW(temporary.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH)
        }
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(temporary: &Path, target: &Path, _target_exists: bool) -> io::Result<()> {
    fs::rename(temporary, target)
}

fn write_error(path: &Path, phase: &str, error: impl std::fmt::Display) -> ConfigLoadError {
    ConfigLoadError::new(format!(
        "failed to write config file {}: {phase}: {error}",
        path.display()
    ))
}

#[cfg(test)]
#[path = "writeback_tests.rs"]
mod tests;
