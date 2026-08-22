use std::{
    fs, io,
    path::{Path, PathBuf},
};

use bootty_write::{NewFileMode, ResolveTargetError, WriteTarget};

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
    let target = resolve_favorite_target(favorites_file)?.lock()?;
    let content = match fs::read_to_string(target.path()) {
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
    let selected = if let Some(index) = lines
        .iter()
        .position(|line| same_project_path(&expand_home_path(home, line), &selected))
    {
        lines.remove(index);
        false
    } else {
        lines.push(project_path.to_owned());
        true
    };
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    target
        .replace(content.as_bytes(), NewFileMode::Private)
        .map_err(bootty_write::CommitError::into_io)?;
    Ok(selected)
}

fn resolve_favorite_target(path: &Path) -> io::Result<WriteTarget> {
    WriteTarget::resolve(path).map_err(|error| match error {
        ResolveTargetError::SymlinkCycle => io::Error::new(
            io::ErrorKind::InvalidInput,
            "favorite symlink cycle detected",
        ),
        ResolveTargetError::Io(error) => error,
    })
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
