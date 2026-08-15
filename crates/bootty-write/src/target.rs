use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use tempfile::Builder;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewFileMode {
    Private,
    UmaskWritable,
}

#[derive(Debug)]
pub enum ResolveTargetError {
    SymlinkCycle,
    Io(io::Error),
}

impl ResolveTargetError {
    pub fn into_io(self) -> io::Error {
        match self {
            Self::SymlinkCycle => {
                io::Error::new(io::ErrorKind::InvalidInput, "symlink cycle detected")
            }
            Self::Io(error) => error,
        }
    }
}

impl From<io::Error> for ResolveTargetError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug)]
pub struct WriteTarget {
    path: PathBuf,
}

impl WriteTarget {
    pub fn resolve(requested_path: &Path) -> Result<Self, ResolveTargetError> {
        let mut current = normalize_absolute_path(requested_path)?;
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current.clone()) {
                return Err(ResolveTargetError::SymlinkCycle);
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
                Ok(_) => {
                    return Ok(Self {
                        path: fs::canonicalize(current)?,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let parent = current.parent().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "write target has no parent")
                    })?;
                    let parent = fs::canonicalize(parent)?;
                    let file_name = current.file_name().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "write target has no file name")
                    })?;
                    return Ok(Self {
                        path: parent.join(file_name),
                    });
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock(self) -> io::Result<LockedWriteTarget> {
        static PROCESS_WRITE_LOCK: Mutex<()> = Mutex::new(());

        let process = PROCESS_WRITE_LOCK
            .lock()
            .map_err(|_| io::Error::other("Bootty writer lock is poisoned"))?;
        let file_name = self.path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write target has no file name")
        })?;
        let mut lock_name = std::ffi::OsString::from(".");
        lock_name.push(file_name);
        lock_name.push(".bootty-write.lock");
        let lock_file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path.with_file_name(lock_name))?;
        lock_file.lock()?;
        Ok(LockedWriteTarget {
            path: self.path,
            _process: process,
            _lock_file: lock_file,
        })
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

pub struct LockedWriteTarget {
    path: PathBuf,
    _process: MutexGuard<'static, ()>,
    _lock_file: File,
}

impl LockedWriteTarget {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn replace(
        &self,
        bytes: &[u8],
        new_file_mode: NewFileMode,
    ) -> Result<CommitOutcome, CommitError> {
        let parent = self
            .path
            .parent()
            .expect("resolved write target has a parent");
        let existing = match fs::metadata(&self.path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(CommitError::new("read permissions", error)),
        };
        let mut builder = Builder::new();
        builder.prefix(".bootty-write-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = match new_file_mode {
                NewFileMode::Private => 0o600,
                NewFileMode::UmaskWritable => 0o666,
            };
            builder.permissions(fs::Permissions::from_mode(mode));
        }
        #[cfg(not(unix))]
        let _ = new_file_mode;
        let mut temporary = builder
            .tempfile_in(parent)
            .map_err(|error| CommitError::new("prepare", error))?;
        set_replacement_permissions(temporary.as_file(), existing.as_ref())
            .map_err(|error| CommitError::new("prepare permissions", error))?;
        temporary
            .write_all(bytes)
            .and_then(|()| temporary.flush())
            .map_err(|error| CommitError::new("write", error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| CommitError::new("sync", error))?;

        let (temporary_file, temporary_path) = temporary.into_parts();
        drop(temporary_file);
        replace_file(temporary_path.as_ref(), &self.path, existing.is_some())
            .map_err(|error| CommitError::new("replace", error))?;

        #[cfg(unix)]
        if let Err(error) = File::open(parent).and_then(|directory| directory.sync_all()) {
            return Ok(CommitOutcome::CommittedWithDurabilityWarning(error));
        }

        Ok(CommitOutcome::Confirmed)
    }
}

#[derive(Debug)]
pub struct CommitError {
    phase: &'static str,
    source: io::Error,
}

impl CommitError {
    fn new(phase: &'static str, source: io::Error) -> Self {
        Self { phase, source }
    }

    pub const fn phase(&self) -> &'static str {
        self.phase
    }

    pub fn into_io(self) -> io::Error {
        self.source
    }
}

#[derive(Debug)]
pub enum CommitOutcome {
    Confirmed,
    CommittedWithDurabilityWarning(io::Error),
}

#[cfg(unix)]
fn set_replacement_permissions(file: &File, existing: Option<&fs::Metadata>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(metadata) = existing {
        file.set_permissions(fs::Permissions::from_mode(
            metadata.permissions().mode() & 0o7777,
        ))?;
    }
    Ok(())
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
    use winsafe::{self as w, co};

    let temporary = temporary.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows paths must be valid UTF-8",
        )
    })?;
    let target = target.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows paths must be valid UTF-8",
        )
    })?;
    let result = if target_exists {
        w::ReplaceFile(target, temporary, None, co::REPLACEFILE::default())
    } else {
        w::MoveFileEx(temporary, Some(target), co::MOVEFILE::WRITE_THROUGH)
    };
    result.map_err(|error| io::Error::from_raw_os_error(error.raw() as i32))
}

#[cfg(not(any(unix, windows)))]
fn replace_file(temporary: &Path, target: &Path, _target_exists: bool) -> io::Result<()> {
    fs::rename(temporary, target)
}
