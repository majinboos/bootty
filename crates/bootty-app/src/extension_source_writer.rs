use std::{fs, io, path::Path};

use bootty_write::{NewFileMode, ResolveTargetError, WriteTarget};

pub(super) fn save_within(root: &Path, requested_path: &Path, source: &str) -> io::Result<()> {
    let root = fs::canonicalize(root)?;
    let target = resolve_extension_target(requested_path)?;
    if !target.path().starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "extension module path escapes extension root",
        ));
    }
    save_target(target, source.as_bytes())
}

pub(crate) fn save_bytes(requested_path: &Path, bytes: &[u8]) -> io::Result<()> {
    save_target(resolve_extension_target(requested_path)?, bytes)
}

fn save_target(target: WriteTarget, bytes: &[u8]) -> io::Result<()> {
    let parent = target.path().parent().ok_or_else(|| {
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
    target
        .lock()?
        .replace(bytes, NewFileMode::UmaskWritable)
        .map_err(bootty_write::CommitError::into_io)?;
    Ok(())
}

fn resolve_extension_target(path: &Path) -> io::Result<WriteTarget> {
    WriteTarget::resolve(path).map_err(|error| match error {
        ResolveTargetError::SymlinkCycle => io::Error::new(
            io::ErrorKind::InvalidInput,
            "extension symlink cycle detected",
        ),
        ResolveTargetError::Io(error) => error,
    })
}
