use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use tempfile::Builder;

pub(super) fn toggle_favorite_project_path_at(
    favorites_file: &Path,
    home: Option<&Path>,
    project_path: &str,
) -> io::Result<bool> {
    if let Some(parent) = favorites_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let target = resolve_write_target(favorites_file)?;
    let _lease = FavoriteWriteLease::acquire(&target)?;
    let content = match fs::read_to_string(&target) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let selected = PathBuf::from(project_path);
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let Some(index) = lines
        .iter()
        .position(|line| same_project_path(&expand_home_path(home, line), &selected))
    {
        lines.remove(index);
        write_favorite_project_paths(&target, &lines)?;
        return Ok(false);
    }
    lines.push(project_path.to_owned());
    write_favorite_project_paths(&target, &lines)?;
    Ok(true)
}

fn write_favorite_project_paths(target: &Path, lines: &[String]) -> io::Result<()> {
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "favorite target has no parent")
    })?;
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "favorite target directory {} does not exist",
                parent.display()
            ),
        ));
    }
    let existing = match fs::metadata(target) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    let mut temporary = Builder::new()
        .prefix(".bootty-session-favorites-")
        .tempfile_in(parent)?;
    temporary.write_all(content.as_bytes())?;
    temporary.flush()?;
    set_replacement_permissions(temporary.as_file(), existing.as_ref())?;
    temporary.as_file().sync_all()?;

    // Close the source handle before replacement. This is required by the
    // Windows replacement APIs. TempPath removes the source on an error.
    let (temporary_file, temporary_path) = temporary.into_parts();
    drop(temporary_file);
    replace_file(&temporary_path, target, existing.is_some())?;

    #[cfg(unix)]
    {
        // Replacement already committed the new bytes. A directory-sync
        // failure cannot be returned without hiding that committed result.
        let _ = File::open(parent).and_then(|directory| directory.sync_all());
    }

    Ok(())
}

struct FavoriteWriteLease {
    _process: MutexGuard<'static, ()>,
    _file: File,
}

impl FavoriteWriteLease {
    fn acquire(target: &Path) -> io::Result<Self> {
        static PROCESS_FAVORITE_WRITE_LOCK: Mutex<()> = Mutex::new(());

        let process = PROCESS_FAVORITE_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("favorite writer lock is poisoned"))?;
        let file_name = target.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "favorite target has no file name",
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
                "favorite symlink cycle detected",
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
                    io::Error::new(io::ErrorKind::InvalidInput, "favorite target has no parent")
                })?;
                let parent = fs::canonicalize(parent)?;
                let file_name = current.file_name().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "favorite target has no file name",
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

fn same_project_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn expand_home_path(home: Option<&Path>, path: &str) -> PathBuf {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix(r"~\"))
        .and_then(|path| home.map(|home| home.join(path)))
        .unwrap_or_else(|| PathBuf::from(path))
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
