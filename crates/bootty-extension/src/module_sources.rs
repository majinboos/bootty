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

/// A runaway backstop on how many modules the extension root may load, not a budget: each one costs
/// a worker thread and a Lua VM for as long as it stays loaded. Set far above any real extension
/// directory, and overflow sheds the excess rather than refusing the whole set.
const MODULE_LIMIT: usize = 256;
const MODULE_SOURCE_LIMIT: u64 = 1024 * 1024;

pub(crate) struct ModuleSource {
    pub(crate) identity: ModuleIdentity,
    pub(crate) namespace: String,
    pub(crate) source: String,
    pub(crate) fingerprint: u64,
    /// True when this came from a file in the extension root rather than from the built-in set. A
    /// built-in is always discovered, so "exists" says nothing about whether the user owns it.
    pub(crate) from_user_file: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditableModuleSource {
    pub identity: ModuleIdentity,
    pub source: String,
    pub path: PathBuf,
    pub customized: bool,
    pub has_builtin: bool,
}

/// The editable module set, borrowed for one settings frame.
#[derive(Clone, Debug, Default)]
pub struct ModuleSources<'a> {
    pub identities: &'a [ModuleIdentity],
    pub legacy: &'a [LegacyExtensionModule],
    /// Surface ids the loaded modules declared for `placement`, so a picker offers only what could
    /// actually render there. A file stem is not a surface id — a module names its own surfaces,
    /// and one file may publish several.
    pub declared: Vec<(crate::SurfacePlacement, String)>,
    /// Why the last scan of the extension root failed, if it did. A failed scan reconciles nothing,
    /// so every module keeps its last state — which looks like modules randomly vanishing unless
    /// the reason is on screen.
    pub scan_error: Option<String>,
    /// Modules that failed to load or publish, with why. A broken module is skipped so the rest
    /// still load, which means nothing renders it and nothing says why unless this is shown.
    pub failures: Vec<(ModuleIdentity, String)>,
    /// What the loaded modules are publishing right now. A preview of an unedited module shows this
    /// instead of a sandbox render, so a module that reads the machine previews as itself.
    pub live: Vec<crate::PublishedSurfaceSnapshot>,
}

impl ModuleSources<'_> {
    /// What `module` is publishing right now, in declaration order.
    #[must_use]
    pub fn live_for(&self, module: &str) -> Vec<crate::SurfaceSnapshot> {
        self.live
            .iter()
            .filter(|surface| surface.module == module)
            .map(|surface| surface.snapshot.clone())
            .collect()
    }

    /// The declared surface ids for `placement`, sorted and deduplicated.
    #[must_use]
    pub fn declared_for(&self, placement: crate::SurfacePlacement) -> Vec<String> {
        let mut ids = self
            .declared
            .iter()
            .filter(|(declared, _)| *declared == placement)
            .map(|(_, id)| id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        ids
    }
}

/// One module-source edit requested by the settings editor. The extension host owns the
/// extension root and the live module set, so every path decision stays behind it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleSourceRequest {
    Load(ModuleIdentity),
    Create(String),
    Save {
        identity: ModuleIdentity,
        source: String,
    },
    Reset(ModuleIdentity),
    ImportLegacy(LegacyExtensionModule),
}

/// The result of one [`ModuleSourceRequest`], applied to the editor after painting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleSourceOutcome {
    Loaded {
        source: EditableModuleSource,
        /// False when no file and no built-in exists yet; editing creates it.
        exists: bool,
    },
    Created(Result<ModuleIdentity, String>),
    Saved(Result<PathBuf, String>),
    Reset(Result<ModuleIdentity, String>),
    Imported(Result<ModuleIdentity, String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyExtensionModule {
    pub source_path: PathBuf,
    pub target_identity: ModuleIdentity,
    pub placement: SurfacePlacement,
    pub surface_id: String,
}

pub fn module_identities(root: &Path) -> Result<Vec<ModuleIdentity>, String> {
    discover_modules(root).map(|scan| scan.modules.into_keys().collect())
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
    let legacy = legacy_extension_modules(config_dir)?
        .into_iter()
        .find(|candidate| candidate == legacy)
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
    Ok(legacy.target_identity)
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
    surface_module_source(
        "-- Imported from a legacy Bootty extension directory.\n",
        surface_id,
        placement.as_str(),
        source,
    )
}

fn surface_module_source(header: &str, identity: &str, placement: &str, source: &str) -> String {
    format!(
        r#"{header}local candidate = (function()
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

pub fn editable_module_source(
    root: &Path,
    identity: &ModuleIdentity,
) -> Option<EditableModuleSource> {
    let path = root.join(identity.as_ref());
    let builtin = builtin_source(identity);
    let has_builtin = builtin.is_some();
    let (source, customized) = match fs::read_to_string(&path) {
        Ok(source) => (source, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (builtin?, false),
        Err(_) => return None,
    };
    Some(EditableModuleSource {
        identity: identity.clone(),
        source,
        path,
        customized,
        has_builtin,
    })
}

/// Whether `source` is exactly the built-in for `identity`, in which case saving it would create an
/// override that changes nothing — and would shadow future updates to the built-in.
#[must_use]
pub fn matches_builtin(identity: &ModuleIdentity, source: &str) -> bool {
    builtin_source(identity).is_some_and(|builtin| builtin.trim_end() == source.trim_end())
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

/// Write the starter source for a module that does not exist yet. Rejects an identity that
/// is already backed by a file so a create never overwrites an edited module.
pub fn create_module_source(root: &Path, value: &str) -> Result<ModuleIdentity, String> {
    let identity = ModuleIdentity::parse(value.trim().to_owned())?;
    if root.join(identity.as_ref()).exists() {
        return Err(format!("Module `{identity}` already exists."));
    }
    save_module_source(root, &identity, &module_template(&identity))
        .map_err(|error| format!("Create failed: {error}"))?;
    Ok(identity)
}

/// Starter source for a new module: one registered sidebar surface that renders its own name.
pub fn module_template(identity: &ModuleIdentity) -> String {
    // The namespace, not the file stem: a nested module declaring a bare `thing` would collide with
    // a top-level `thing.luau`, and every built-in already names itself by namespace.
    let id = identity.namespace();
    format!(
        "--!strict\nbootty.ui.register({{ id = \"{id}\", placement = \"sidebar\" }}, function()\n\treturn {{ {{ text = \"{id}\" }} }}\nend)\n"
    )
}

pub fn reset_module_source(root: &Path, identity: &ModuleIdentity) -> std::io::Result<()> {
    let path = root.join(identity.as_ref());
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// The built-in source for `identity`, wrapped and ready to load. Used to fall back when a user's
/// override will not load, so a typo in one file cannot take a built-in feature down with it.
pub(crate) fn builtin_module(identity: &ModuleIdentity) -> Option<ModuleSource> {
    builtin_source(identity).map(|source| module_source_from_text(identity.clone(), source))
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

/// The outcome of one scan of the extension root.
pub(crate) struct ModuleScan {
    pub(crate) modules: BTreeMap<ModuleIdentity, Result<ModuleSource, String>>,
    /// Set when the scan loaded less than the root holds, with the reason.
    pub(crate) warning: Option<String>,
}

pub(crate) fn discover_modules(root: &Path) -> Result<ModuleScan, String> {
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
        return Ok(ModuleScan {
            modules,
            warning: None,
        });
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
    // Bound what the user added; the built-ins are ours and fixed. Over the backstop, load the
    // first `MODULE_LIMIT` in path order and say what was left out — refusing the whole scan would
    // leave every module frozen at its last published state instead.
    let dropped = canonical_paths.len().saturating_sub(MODULE_LIMIT);
    for path in canonical_paths.into_iter().take(MODULE_LIMIT) {
        let identity = module_identity(&canonical_root, &path)?;
        let loaded = load_module_source(identity.clone(), &path);
        modules.insert(identity, loaded);
    }
    Ok(ModuleScan {
        modules,
        warning: (dropped > 0)
            .then(|| format!("{dropped} modules past the limit of {MODULE_LIMIT} were not loaded")),
    })
}

fn builtin_module_source(identity: &str, placement: &str, source: &str) -> String {
    surface_module_source("\n", identity, placement, source)
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

fn module_identity(canonical_root: &Path, canonical_path: &Path) -> Result<ModuleIdentity, String> {
    let relative = canonical_path
        .strip_prefix(canonical_root)
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
    Ok(ModuleSource {
        from_user_file: true,
        ..module_source_from_text(identity, source)
    })
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
        from_user_file: false,
    }
}
