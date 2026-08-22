//! Adapters a module needs installed in *another* tool: a hook script on disk, and a JSON config
//! entry pointing at it. Nothing here knows what an agent is. A module declares the files and the
//! JSON, and the host only writes files and merges JSON — every tool-specific name, path and event
//! stays in the Luau module that owns it.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

use mlua::Table;
use serde_json::{Map, Value};

/// Per-declaration bounds. An integration writes real files into the user's home directory, so the
/// cost of a runaway declaration is not memory but litter on disk: keep both counts small and the
/// payload far above any adapter script yet far below a bundled binary.
const INTEGRATION_ENTRY_LIMIT: usize = 16;
const INTEGRATION_FILE_SIZE_LIMIT: usize = 64 * 1024;

/// One adapter a module declared through `bootty.integration.register`. The host stamps `module`
/// from the module's own identity, so a declaration can only ever be installed under its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationDeclaration {
    pub module: String,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub files: Vec<IntegrationFile>,
    pub merge: Vec<IntegrationMerge>,
}

/// A file written under the integration directory. `path` is relative to it and may not escape it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationFile {
    pub path: String,
    pub contents: String,
    pub executable: bool,
}

/// A JSON file somewhere else on the machine that gains `value`, additively.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationMerge {
    pub path: String,
    pub value: Value,
}

/// Whether everything a declaration asks for is on disk. `Partial` is its own answer because a
/// half-installed adapter is exactly the silent failure this feature exists to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntegrationStatus {
    Missing,
    Partial,
    Installed,
}

/// One declaration with its install status, computed at reconcile so painting never stats a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationState {
    pub declaration: IntegrationDeclaration,
    pub status: IntegrationStatus,
}

pub(crate) fn declaration_from_table(spec: &Table) -> mlua::Result<IntegrationDeclaration> {
    let id: String = spec.get("id")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(mlua::Error::runtime(
            "an integration id must be non-empty and use only letters, digits, - or _",
        ));
    }
    let mut files = Vec::new();
    if let Some(table) = spec.get::<Option<Table>>("files")? {
        for entry in table.sequence_values::<Table>() {
            let entry = entry?;
            let path: String = entry.get("path")?;
            // Reject the escape at declaration time: a module that would write outside the
            // integration directory never loads, rather than failing only when someone installs it.
            relative_path(&path).map_err(mlua::Error::runtime)?;
            let contents: String = entry.get("contents")?;
            if contents.len() > INTEGRATION_FILE_SIZE_LIMIT {
                return Err(mlua::Error::runtime(format!(
                    "integration file `{path}` exceeds the limit of {INTEGRATION_FILE_SIZE_LIMIT} bytes"
                )));
            }
            files.push(IntegrationFile {
                path,
                contents,
                executable: entry.get::<Option<bool>>("executable")?.unwrap_or(false),
            });
        }
    }
    let mut merge = Vec::new();
    if let Some(table) = spec.get::<Option<Table>>("merge")? {
        for entry in table.sequence_values::<Table>() {
            let entry = entry?;
            merge.push(IntegrationMerge {
                path: entry.get("path")?,
                value: crate::module_runtime::lua_value(entry.get("value")?, 0)?,
            });
        }
    }
    for (kind, count) in [("file", files.len()), ("merge", merge.len())] {
        if count > INTEGRATION_ENTRY_LIMIT {
            return Err(mlua::Error::runtime(format!(
                "integration {kind} count exceeds the limit of {INTEGRATION_ENTRY_LIMIT}"
            )));
        }
    }
    Ok(IntegrationDeclaration {
        // Filled in by the host, which knows the module's identity.
        module: String::new(),
        id,
        title: spec.get::<Option<String>>("title")?.unwrap_or_default(),
        summary: spec.get::<Option<String>>("summary")?.unwrap_or_default(),
        files,
        merge,
    })
}

/// How much of `declaration` is already on disk.
#[must_use]
pub(crate) fn status(
    dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> IntegrationStatus {
    let mut applied = 0usize;
    let mut total = 0usize;
    for file in &declaration.files {
        total += 1;
        if file_installed(dir, file) {
            applied += 1;
        }
    }
    for entry in &declaration.merge {
        total += 1;
        let installed = resolve_merge_path(home, &entry.path)
            .ok()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .is_some_and(|existing| contains(&existing, &entry.value));
        if installed {
            applied += 1;
        }
    }
    if applied == total {
        IntegrationStatus::Installed
    } else if applied == 0 {
        IntegrationStatus::Missing
    } else {
        IntegrationStatus::Partial
    }
}

/// Write every file and merge every JSON entry. Installing twice is a no-op: the files are replaced
/// with the same bytes, and a merge adds only what is not already there.
pub(crate) fn install(
    dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> Result<(), String> {
    // Read and merge every JSON target before writing anything, so a target that is not valid JSON
    // — or that already holds a conflicting value — fails the whole install rather than leaving
    // half an adapter behind.
    let mut merged = Vec::with_capacity(declaration.merge.len());
    for entry in &declaration.merge {
        let path = resolve_merge_path(home, &entry.path)?;
        let mut value = read_json(&path)?;
        merge_value(&mut value, &entry.value);
        if !contains(&value, &entry.value) {
            return Err(format!(
                "{} already has a different value where this integration writes one",
                path.display()
            ));
        }
        merged.push((path, value));
    }
    fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    for file in &declaration.files {
        let path = dir.join(relative_path(&file.path)?);
        let parent = path
            .parent()
            .ok_or_else(|| format!("integration file `{}` has no parent", file.path))?;
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        crate::source_writer::save_within(dir, &path, &file.contents)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if file.executable {
            set_executable(&path)?;
        }
    }
    for (path, value) in merged {
        write_json(&path, &value)?;
    }
    Ok(())
}

/// Remove the files this declaration wrote and take back exactly what its merges added, leaving
/// everything else in those JSON files untouched.
pub(crate) fn uninstall(
    dir: &Path,
    home: Option<&Path>,
    declaration: &IntegrationDeclaration,
) -> Result<(), String> {
    for file in &declaration.files {
        let path = dir.join(relative_path(&file.path)?);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    for entry in &declaration.merge {
        let path = resolve_merge_path(home, &entry.path)?;
        if !path.exists() {
            continue;
        }
        let mut value = read_json(&path)?;
        if unmerge_value(&mut value, &entry.value) {
            write_json(&path, &value)?;
        }
    }
    Ok(())
}

fn file_installed(dir: &Path, file: &IntegrationFile) -> bool {
    let Ok(relative) = relative_path(&file.path) else {
        return false;
    };
    let path = dir.join(relative);
    if !fs::read_to_string(&path).is_ok_and(|contents| contents == file.contents) {
        return false;
    }
    !file.executable || is_executable(&path)
}

/// The path `value` names under the integration directory. Rejects anything that could land
/// outside it — absolute paths, `..`, and drive or root prefixes.
fn relative_path(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!(
            "integration file path `{value}` must stay inside the integration directory"
        ));
    }
    Ok(path.to_owned())
}

/// The absolute path a `merge` entry names. A leading `~/` expands against the home directory;
/// anything else has to already be absolute, so an integration can never write relative to
/// whatever directory Bootty happens to be running in.
fn resolve_merge_path(home: Option<&Path>, value: &str) -> Result<PathBuf, String> {
    let path = match value.strip_prefix("~/") {
        Some(rest) => home
            .ok_or_else(|| format!("integration path `{value}` needs a home directory"))?
            .join(rest),
        None => PathBuf::from(value),
    };
    if !path.is_absolute() {
        return Err(format!(
            "integration path `{value}` must be absolute or start with `~/`"
        ));
    }
    Ok(path)
}

/// The JSON a merge target currently holds. A missing file is an empty object — the merge creates
/// it. A file that is not valid JSON is an error, never something to overwrite.
fn read_json(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Value::Object(Map::new())),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid JSON: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Map::new())),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    crate::source_writer::save_bytes(path, &bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Add `addition` to `target` without dropping or reordering anything already there: objects merge
/// key by key, arrays gain only elements that are not already present, and a scalar the user
/// already wrote is left alone (the caller refuses the install rather than overwriting it).
fn merge_value(target: &mut Value, addition: &Value) {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => {
            for (key, value) in addition {
                match target.get_mut(key) {
                    Some(existing) => merge_value(existing, value),
                    None => {
                        target.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (Value::Array(target), Value::Array(addition)) => {
            for value in addition {
                if !target.contains(value) {
                    target.push(value.clone());
                }
            }
        }
        _ => {}
    }
}

/// Whether `target` already holds everything in `addition`, by the same rules `merge_value` adds it.
fn contains(target: &Value, addition: &Value) -> bool {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => addition
            .iter()
            .all(|(key, value)| target.get(key).is_some_and(|held| contains(held, value))),
        (Value::Array(target), Value::Array(addition)) => {
            addition.iter().all(|value| target.contains(value))
        }
        _ => target == addition,
    }
}

/// Take `addition` back out of `target`, and nothing else. Returns whether anything changed.
fn unmerge_value(target: &mut Value, addition: &Value) -> bool {
    match (target, addition) {
        (Value::Object(target), Value::Object(addition)) => {
            let mut changed = false;
            for (key, value) in addition {
                let Some(held) = target.get_mut(key) else {
                    continue;
                };
                if held == value {
                    target.remove(key);
                    changed = true;
                    continue;
                }
                changed |= unmerge_value(held, value);
                // A container we just emptied held only our entries, so the key goes with them.
                if matches!(target.get(key), Some(Value::Object(held)) if held.is_empty())
                    || matches!(target.get(key), Some(Value::Array(held)) if held.is_empty())
                {
                    target.remove(key);
                }
            }
            changed
        }
        (Value::Array(target), Value::Array(addition)) => {
            let before = target.len();
            target.retain(|value| !addition.contains(value));
            before != target.len()
        }
        _ => false,
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("{}: {error}", path.display()))?
        .permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}
