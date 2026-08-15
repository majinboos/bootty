use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};

use crate::builtins;
use crate::identity::ModuleIdentity;
use crate::module_runtime::preview_module_surfaces;
use crate::surfaces::SurfacePlacement;

const MODULE_LIMIT: usize = 32;
const MODULE_SOURCE_LIMIT: u64 = 1024 * 1024;

pub(crate) struct ModuleSource {
    pub(crate) identity: ModuleIdentity,
    pub(crate) namespace: String,
    pub(crate) source: String,
    pub(crate) fingerprint: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditableModuleSource {
    pub identity: ModuleIdentity,
    pub source: String,
    pub path: PathBuf,
    pub customized: bool,
    pub has_builtin: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyExtensionModule {
    pub source_path: PathBuf,
    pub target_identity: ModuleIdentity,
    pub placement: SurfacePlacement,
    pub surface_id: String,
}

pub fn module_identities(root: &Path) -> Result<Vec<ModuleIdentity>, String> {
    discover_modules(root).map(|modules| modules.into_keys().collect())
}

pub fn legacy_extension_modules(config_dir: &Path) -> Result<Vec<LegacyExtensionModule>, String> {
    let mut discovered = BTreeMap::<(SurfacePlacement, String), LegacyExtensionModule>::new();
    for placement in [
        SurfacePlacement::Status,
        SurfacePlacement::Sidebar,
        SurfacePlacement::Session,
    ] {
        let directory = config_dir.join(placement.as_str());
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        let canonical_directory =
            fs::canonicalize(&directory).map_err(|error| error.to_string())?;
        let mut paths = entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort_by_key(|path| path.extension().and_then(|value| value.to_str()) == Some("luau"));
        for path in paths {
            if !path.is_file() || !is_module_path(&path) {
                continue;
            }
            let canonical_path = fs::canonicalize(&path).map_err(|error| error.to_string())?;
            if !canonical_path.starts_with(&canonical_directory) {
                return Err("legacy extension module path escapes its source directory".to_owned());
            }
            let Some(surface_id) = path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let target_identity = legacy_target_identity(placement, &surface_id, &path)?;
            discovered.insert(
                (placement, surface_id.clone()),
                LegacyExtensionModule {
                    source_path: path,
                    target_identity,
                    placement,
                    surface_id,
                },
            );
        }
    }
    Ok(discovered.into_values().collect())
}

pub fn import_legacy_extension_module(
    config_dir: &Path,
    legacy: &LegacyExtensionModule,
    theme: Vec<(String, String)>,
) -> Result<ModuleIdentity, String> {
    let current = legacy_extension_modules(config_dir)?;
    let legacy = current
        .iter()
        .find(|candidate| candidate == &legacy)
        .ok_or_else(|| "legacy extension module is no longer available".to_owned())?;
    let source = fs::read_to_string(&legacy.source_path).map_err(|error| error.to_string())?;
    let source = wrap_legacy_surface_source(&legacy.surface_id, legacy.placement, source.as_str());
    let surfaces = preview_module_surfaces(&legacy.target_identity, &source, theme)?;
    if surfaces.len() != 1
        || surfaces[0].declaration.id != legacy.surface_id
        || surfaces[0].declaration.placement != legacy.placement
    {
        return Err("legacy extension import did not produce its declared surface".to_owned());
    }
    save_module_source(
        &config_dir.join("extensions"),
        &legacy.target_identity,
        &source,
    )
    .map_err(|error| error.to_string())?;
    Ok(legacy.target_identity.clone())
}

fn legacy_target_identity(
    placement: SurfacePlacement,
    surface_id: &str,
    source_path: &Path,
) -> Result<ModuleIdentity, String> {
    let builtin = builtins::modules()
        .into_iter()
        .any(|builtin| builtin.identity == surface_id && builtin.placement == placement.as_str());
    let extension = source_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("luau");
    if builtin {
        ModuleIdentity::parse(format!("{surface_id}.luau"))
    } else {
        ModuleIdentity::parse(format!(
            "legacy/{}/{surface_id}.{extension}",
            placement.as_str()
        ))
    }
}

fn wrap_legacy_surface_source(
    surface_id: &str,
    placement: SurfacePlacement,
    source: &str,
) -> String {
    format!(
        r#"-- Imported from a legacy Bootty extension directory.
local candidate = (function()
{source}
end)()
local render = candidate
local interval = nil
if type(candidate) == "table" then
    render = candidate.render
    interval = candidate.interval
end
bootty.ui.register({{ id = "{surface_id}", placement = "{}", interval = interval }}, render)
"#,
        placement.as_str()
    )
}

pub fn editable_module_source(
    root: &Path,
    identity: &ModuleIdentity,
) -> Option<EditableModuleSource> {
    let path = root.join(identity.as_ref());
    let builtin = builtin_source(identity);
    match fs::read_to_string(&path) {
        Ok(source) => Some(EditableModuleSource {
            identity: identity.clone(),
            source,
            path,
            customized: true,
            has_builtin: builtin.is_some(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            builtin.map(|source| EditableModuleSource {
                identity: identity.clone(),
                source,
                path,
                customized: false,
                has_builtin: true,
            })
        }
        Err(_) => None,
    }
}

pub fn save_module_source(
    root: &Path,
    identity: &ModuleIdentity,
    source: &str,
) -> std::io::Result<PathBuf> {
    let path = root.join(identity.as_ref());
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "extension target has no parent",
        )
    })?;
    fs::create_dir_all(parent)?;
    crate::source_writer::save_within(root, &path, source)?;
    Ok(path)
}

pub fn reset_module_source(root: &Path, identity: &ModuleIdentity) -> std::io::Result<()> {
    let path = root.join(identity.as_ref());
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn builtin_source(identity: &ModuleIdentity) -> Option<String> {
    if let Some(builtin) =
        builtins::agent_modules().find(|builtin| identity.as_str() == builtin.identity)
    {
        return Some(builtin.source.to_owned());
    }
    builtins::modules()
        .into_iter()
        .find(|builtin| identity.as_str() == format!("{}.luau", builtin.identity))
        .map(|builtin| builtin_module_source(builtin.identity, builtin.placement, builtin.source))
}

pub(crate) fn discover_modules(
    root: &Path,
) -> Result<BTreeMap<ModuleIdentity, Result<ModuleSource, String>>, String> {
    let mut modules = builtins::modules()
        .into_iter()
        .map(|builtin| {
            let identity = ModuleIdentity::parse(format!("{}.luau", builtin.identity))
                .expect("built-in extension identity");
            let source = builtin_module_source(builtin.identity, builtin.placement, builtin.source);
            let loaded = Ok(module_source_from_text(identity.clone(), source));
            (identity, loaded)
        })
        .collect::<BTreeMap<_, _>>();
    modules.extend(builtins::agent_modules().map(|builtin| {
        let identity = ModuleIdentity::parse(builtin.identity).expect("built-in agent identity");
        let loaded = Ok(module_source_from_text(
            identity.clone(),
            builtin.source.to_owned(),
        ));
        (identity, loaded)
    }));
    if !root.exists() {
        return Ok(modules);
    }
    let mut paths = Vec::new();
    collect_module_paths(root, &mut paths)?;
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let mut canonical_paths = BTreeSet::new();
    for path in paths {
        let canonical_path = path.canonicalize().map_err(|error| error.to_string())?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err("extension module must stay inside the extension root".to_owned());
        }
        canonical_paths.insert(canonical_path);
    }
    let paths = canonical_paths.into_iter().collect::<Vec<_>>();
    if paths.len() + modules.len() > MODULE_LIMIT {
        return Err(format!(
            "extension module count exceeds the limit of {MODULE_LIMIT}"
        ));
    }
    for path in paths {
        let identity = module_identity(root, &path)?;
        let loaded = load_module_source(identity.clone(), &path);
        modules.insert(identity, loaded);
    }
    Ok(modules)
}

fn builtin_module_source(identity: &str, placement: &str, source: &str) -> String {
    format!(
        r#"
local candidate = (function()
{source}
end)()
local render = candidate
local interval = nil
if type(candidate) == "table" then
    render = candidate.render
    interval = candidate.interval
end
bootty.ui.register({{ id = "{identity}", placement = "{placement}", interval = interval }}, render)
"#
    )
}

fn collect_module_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| error.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_dir() {
            collect_module_paths(&path, paths)?;
        } else if (file_type.is_file() || path.is_file()) && is_module_path(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_module_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension, "lua" | "luau"))
}

fn module_identity(root: &Path, path: &Path) -> Result<ModuleIdentity, String> {
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_path = path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("extension module must stay inside the extension root".to_owned());
    }
    let relative = canonical_path
        .strip_prefix(&canonical_root)
        .map_err(|_| "extension module must stay inside the extension root".to_owned())?;
    let parts = relative
        .components()
        .map(|component| match component {
            std::path::Component::Normal(part) => part
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "extension module path must be valid UTF-8".to_owned()),
            _ => Err("extension module path is invalid".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    ModuleIdentity::parse(parts.join("/"))
}

fn load_module_source(identity: ModuleIdentity, path: &Path) -> Result<ModuleSource, String> {
    let size = fs::metadata(path).map_err(|error| error.to_string())?.len();
    if size > MODULE_SOURCE_LIMIT {
        return Err(format!(
            "extension source exceeds the limit of {MODULE_SOURCE_LIMIT} bytes"
        ));
    }
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    Ok(module_source_from_text(identity, source))
}

fn module_source_from_text(identity: ModuleIdentity, source: String) -> ModuleSource {
    let namespace = identity.namespace();
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    source.hash(&mut hasher);
    ModuleSource {
        identity,
        namespace,
        source,
        fingerprint: hasher.finish(),
    }
}
