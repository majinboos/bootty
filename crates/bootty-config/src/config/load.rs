use super::model::BoottyConfig;
use super::raw::RawConfig;
use super::resolve::ConfigResolver;
use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table, TableLike};

#[derive(Debug, Error)]
#[error("{message}")]
pub struct ConfigLoadError {
    message: String,
}

impl ConfigLoadError {
    pub(super) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub type ConfigResult<T> = Result<T, ConfigLoadError>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigFileSnapshot {
    files: Vec<ConfigFileStamp>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigFileStamp {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: Option<u64>,
}

impl ConfigFileSnapshot {
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut files = paths
            .into_iter()
            .map(|path| ConfigFileStamp::from_path(config_file_id(&path)))
            .collect::<Vec<_>>();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Self { files }
    }

    #[must_use]
    pub fn refresh_known_paths(&self) -> Self {
        Self::from_paths(self.files.iter().map(|file| file.path.clone()))
    }
}

impl ConfigFileStamp {
    fn from_path(path: PathBuf) -> Self {
        let metadata = fs::metadata(&path).ok();
        Self {
            path,
            modified: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok()),
            len: metadata.map(|metadata| metadata.len()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConfigDocument {
    pub(super) document: DocumentMut,
}

impl ConfigDocument {
    pub(super) fn set_item(&mut self, path: &[&str], item: Item) -> ConfigResult<()> {
        let Some((leaf, parents)) = path.split_last() else {
            return Err(ConfigLoadError::new(
                "config writeback path cannot be empty",
            ));
        };
        let mut table = self.document.as_table_mut();
        for key in parents {
            let entry = &mut table[*key];
            if entry.is_none() {
                *entry = Item::Table(Table::new());
            }
            table = entry.as_table_mut().ok_or_else(|| {
                ConfigLoadError::new(format!(
                    "config writeback path {} is not a table",
                    parents.join(".")
                ))
            })?;
        }
        table[*leaf] = item;
        Ok(())
    }

    /// Remove a key, restoring its built-in default on the next load. Missing keys are a no-op.
    pub fn remove(&mut self, path: &[&str]) -> ConfigResult<()> {
        let Some((leaf, parents)) = path.split_last() else {
            return Err(ConfigLoadError::new(
                "config writeback path cannot be empty",
            ));
        };
        let mut table = self.document.as_table_mut();
        for key in parents {
            match table.get_mut(key).and_then(Item::as_table_mut) {
                Some(child) => table = child,
                None => return Ok(()),
            }
        }
        table.remove(leaf);
        Ok(())
    }

    /// The item at `path`, if the whole path exists. One table walk for every reader below.
    fn item_at(&self, path: &[&str]) -> Option<&Item> {
        let (leaf, parents) = path.split_last()?;
        let mut table: &dyn TableLike = self.document.as_table();
        for key in parents {
            table = table.get(key).and_then(Item::as_table_like)?;
        }
        table.get(leaf)
    }

    pub fn contains(&self, path: &[&str]) -> bool {
        self.item_at(path).is_some()
    }

    /// The value written at `path`, in the shape the config schema expects there. Returns `None`
    /// for a missing key, and for a key whose value is of another type; a caller that needs to
    /// tell those apart uses [`Self::contains`] as well.
    #[must_use]
    pub fn bool_at(&self, path: &[&str]) -> Option<bool> {
        self.item_at(path)?.as_bool()
    }

    #[must_use]
    pub fn f64_at(&self, path: &[&str]) -> Option<f64> {
        let item = self.item_at(path)?;
        item.as_float()
            .or_else(|| item.as_integer().map(|value| value as f64))
    }

    #[must_use]
    pub fn i64_at(&self, path: &[&str]) -> Option<i64> {
        self.item_at(path)?.as_integer()
    }

    #[must_use]
    pub fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.item_at(path)?.as_str()
    }

    pub fn string_array(&self, path: &[&str]) -> Option<Vec<String>> {
        self.item_at(path)?.as_array().map(|array| {
            array
                .iter()
                .filter_map(|value| value.as_str())
                .map(str::to_owned)
                .collect()
        })
    }
}

pub fn load_config_from_path(path: impl AsRef<Path>) -> ConfigResult<BoottyConfig> {
    let path = path.as_ref();
    load_config_attempt(path).config
}

pub(super) fn validate_config_document(
    path: &Path,
    document: &ConfigDocument,
) -> ConfigResult<BoottyConfig> {
    let mut traversal = ConfigGraphTraversal::default();
    let mut document = traversal.merge_root_document(path, document.document.clone())?;
    resolve_loaded_document(&mut document, path)
}

pub(crate) struct ConfigLoadAttempt {
    pub(crate) config: ConfigResult<BoottyConfig>,
    pub(crate) snapshot: ConfigFileSnapshot,
}

pub(crate) fn load_config_attempt(path: &Path) -> ConfigLoadAttempt {
    if !path.exists() {
        let config = BoottyConfig {
            config_path: path.to_path_buf(),
            ..Default::default()
        };
        return ConfigLoadAttempt {
            config: Ok(config),
            snapshot: ConfigFileSnapshot::from_paths([path.to_path_buf()]),
        };
    }

    let ConfigGraphLoad { document, snapshot } = load_config_graph(path);
    let config = document.and_then(|mut document| resolve_loaded_document(&mut document, path));
    ConfigLoadAttempt { config, snapshot }
}

pub fn config_file_snapshot(path: impl AsRef<Path>) -> ConfigResult<ConfigFileSnapshot> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(ConfigFileSnapshot::from_paths([path.to_path_buf()]));
    }
    let ConfigGraphLoad { document, snapshot } = load_config_graph(path);
    document?;
    Ok(snapshot)
}

pub(crate) fn config_dependency_snapshot(path: &Path) -> ConfigFileSnapshot {
    load_config_graph(path).snapshot
}

pub fn load_config_document(path: impl AsRef<Path>) -> ConfigResult<Option<ConfigDocument>> {
    let path = path.as_ref();
    match fs::read_to_string(path) {
        Ok(source) => {
            let document = source.parse::<DocumentMut>().map_err(|error| {
                ConfigLoadError::new(format!(
                    "failed to parse config file {}: {error}",
                    path.display()
                ))
            })?;
            Ok(Some(ConfigDocument { document }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ConfigLoadError::new(format!(
            "failed to read config file {}: {error}",
            path.display()
        ))),
    }
}

pub fn load_or_create_config_document(path: impl AsRef<Path>) -> ConfigResult<ConfigDocument> {
    let path = path.as_ref();
    load_config_document(path).map(|document| {
        document.unwrap_or_else(|| ConfigDocument {
            document: DocumentMut::new(),
        })
    })
}

struct ConfigGraphLoad {
    document: ConfigResult<DocumentMut>,
    snapshot: ConfigFileSnapshot,
}

#[derive(Default)]
struct ConfigGraphTraversal {
    stack: Vec<PathBuf>,
    loaded: HashSet<PathBuf>,
    paths: Vec<PathBuf>,
}

fn load_config_graph(path: &Path) -> ConfigGraphLoad {
    let mut traversal = ConfigGraphTraversal::default();
    let document = traversal.load_merged_document(path);
    ConfigGraphLoad {
        document,
        snapshot: ConfigFileSnapshot::from_paths(traversal.paths),
    }
}

impl ConfigGraphTraversal {
    fn merge_root_document(
        &mut self,
        path: &Path,
        document: DocumentMut,
    ) -> ConfigResult<DocumentMut> {
        let id = config_file_id(path);
        self.paths.push(id.clone());
        self.merge_includes(path, id, document)
    }

    fn load_merged_document(&mut self, path: &Path) -> ConfigResult<DocumentMut> {
        let id = config_file_id(path);
        self.paths.push(id.clone());
        if self.stack.contains(&id) {
            return Err(ConfigLoadError::new(format!(
                "config include cycle detected at {}",
                path.display()
            )));
        }
        if self.loaded.contains(&id) {
            return Ok(DocumentMut::new());
        }

        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(ConfigLoadError::new(format!(
                    "config file not found: {}",
                    path.display()
                )));
            }
            Err(error) => {
                return Err(ConfigLoadError::new(format!(
                    "failed to read config file {}: {error}",
                    path.display()
                )));
            }
        };
        let document = source.parse::<DocumentMut>().map_err(|error| {
            ConfigLoadError::new(format!(
                "failed to parse config file {}: {error}",
                path.display()
            ))
        })?;
        self.merge_includes(path, id, document)
    }

    fn merge_includes(
        &mut self,
        path: &Path,
        id: PathBuf,
        mut document: DocumentMut,
    ) -> ConfigResult<DocumentMut> {
        let includes = config_document_includes(&document, path)?;
        self.stack.push(id.clone());
        let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
        for include in includes {
            let include = IncludePath::parse(&include);
            let include_path = include.resolve(base_dir);
            if !include_path.exists() && include.optional {
                self.paths.push(config_file_id(&include_path));
                continue;
            }
            merge_toml_tables(
                document.as_table_mut(),
                &self.load_merged_document(&include_path)?.into_table(),
            );
        }
        self.stack.pop();
        self.loaded.insert(id);
        Ok(document)
    }
}

fn resolve_loaded_document(document: &mut DocumentMut, path: &Path) -> ConfigResult<BoottyConfig> {
    let compatibility_warnings = take_ghostty_compatibility_warnings(document);
    document.as_table_mut().remove("include");
    let raw = parse_raw_config_document(document.clone(), path)?;
    let config_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut config = ConfigResolver {
        path: path.to_path_buf(),
        config_dir,
    }
    .resolve(raw)?;
    config.compatibility_warnings = compatibility_warnings;
    Ok(config)
}

fn merge_toml_tables(target: &mut Table, overlay: &Table) {
    merge_toml_table_like(target, overlay);
}

fn merge_toml_table_like(target: &mut dyn TableLike, overlay: &dyn TableLike) {
    for (key, value) in overlay.iter() {
        if let Some(target_table) = target.get_mut(key).and_then(Item::as_table_like_mut)
            && let Some(overlay_table) = value.as_table_like()
        {
            merge_toml_table_like(target_table, overlay_table);
            continue;
        }
        target.insert(key, value.clone());
    }
}

const GHOSTTY_COMPATIBILITY_KEYS: &[&str] = &[
    "background-opacity",
    "background-blur-radius",
    "window-padding-x",
    "window-padding-y",
    "window-padding-balance",
    "window-save-state",
    "shell-integration",
    "shell-integration-features",
    "copy-on-select",
    "confirm-close-surface",
    "quit-after-last-window-closed",
];

fn take_ghostty_compatibility_warnings(document: &mut DocumentMut) -> Vec<String> {
    GHOSTTY_COMPATIBILITY_KEYS
        .iter()
        .filter_map(|key| {
            document
                .as_table_mut()
                .remove(key)
                .map(|_| format!("unsupported Ghostty compatibility key ignored: {key}"))
        })
        .collect()
}
fn parse_raw_config_document(document: DocumentMut, path: &Path) -> ConfigResult<RawConfig> {
    toml_edit::de::from_document(document).map_err(|error| {
        ConfigLoadError::new(format!(
            "failed to parse config file {}: {error}",
            path.display()
        ))
    })
}

fn config_document_includes(document: &DocumentMut, path: &Path) -> ConfigResult<Vec<String>> {
    let Some(item) = document.get("include") else {
        return Ok(Vec::new());
    };
    let Some(array) = item.as_array() else {
        return Err(ConfigLoadError::new(format!(
            "failed to parse config file {}: include must be an array of strings",
            path.display()
        )));
    };
    array
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ConfigLoadError::new(format!(
                    "failed to parse config file {}: include must contain only strings",
                    path.display()
                ))
            })
        })
        .collect()
}

fn config_file_id(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

struct IncludePath<'a> {
    path: &'a str,
    optional: bool,
}

impl<'a> IncludePath<'a> {
    fn parse(input: &'a str) -> Self {
        input.strip_prefix('?').map_or(
            Self {
                path: input,
                optional: false,
            },
            |path| Self {
                path,
                optional: true,
            },
        )
    }

    fn resolve(&self, base_dir: &Path) -> PathBuf {
        let path = Path::new(self.path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            base_dir.join(path)
        }
    }
}
