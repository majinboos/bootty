use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use tempfile::Builder;

pub(super) fn save(requested_path: &Path, source: &str) -> io::Result<()> {
    save_with_fault(requested_path, source, WriteFault::None)
}

fn save_with_fault(requested_path: &Path, source: &str, fault: WriteFault) -> io::Result<()> {
    #[cfg(not(test))]
    let _ = fault;
    let target = resolve_write_target(requested_path)?;
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "extension target has no parent",
        )
    })?;
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("target directory {} does not exist", parent.display()),
        ));
    }

    let _lease = WriteLease::acquire(&target)?;
    let existing = match fs::metadata(&target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };

    let mut builder = Builder::new();
    builder.prefix(".bootty-extension-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = existing
            .as_ref()
            .map_or(0o666, |metadata| metadata.permissions().mode() & 0o7777);
        builder.permissions(fs::Permissions::from_mode(mode));
    }
    let mut temporary = builder.tempfile_in(parent)?;
    #[cfg(unix)]
    if let Some(metadata) = existing.as_ref() {
        use std::os::unix::fs::PermissionsExt;

        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(
                metadata.permissions().mode() & 0o7777,
            ))?;
    }

    temporary.write_all(source.as_bytes())?;
    temporary.flush()?;
    #[cfg(test)]
    if fault == WriteFault::Sync {
        return Err(io::Error::other("injected sync failure"));
    }
    temporary.as_file().sync_all()?;

    let (temporary_file, temporary_path) = temporary.into_parts();
    drop(temporary_file);
    #[cfg(test)]
    if fault == WriteFault::Replace {
        return Err(io::Error::other("injected replacement failure"));
    }
    replace_file(temporary_path.as_ref(), &target, existing.is_some())?;

    #[cfg(unix)]
    {
        #[cfg(test)]
        if fault == WriteFault::SyncDirectory {
            return Ok(());
        }
        // Replacement already committed the new bytes. A directory-sync failure cannot
        // be returned without hiding that committed result.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteFault {
    None,
    #[cfg(test)]
    Sync,
    #[cfg(test)]
    Replace,
    #[cfg(all(test, unix))]
    SyncDirectory,
}

struct WriteLease {
    _process: MutexGuard<'static, ()>,
    _file: File,
}

impl WriteLease {
    fn acquire(target: &Path) -> io::Result<Self> {
        static PROCESS_EXTENSION_WRITE_LOCK: Mutex<()> = Mutex::new(());

        let process = PROCESS_EXTENSION_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("extension writer lock is poisoned"))?;
        let file_name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "extension target has no file name",
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
                "extension symlink cycle detected",
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
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "extension target has no parent",
                    )
                })?;
                let parent = fs::canonicalize(parent)?;
                let file_name = current.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "extension target has no file name",
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

#[cfg(test)]
#[path = "extension_source_writer_tests.rs"]
mod tests;
