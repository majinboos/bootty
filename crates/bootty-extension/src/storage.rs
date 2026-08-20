use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::source_writer as extension_source_writer;

const STORAGE_KEY_LIMIT: usize = 64;
const STORAGE_KEY_BYTES_LIMIT: usize = 128;
const STORAGE_VALUE_BYTES_LIMIT: usize = 64 * 1024;
const STORAGE_BYTES_LIMIT: usize = 256 * 1024;

#[derive(Clone)]
pub(super) struct ExtensionStorage {
    path: PathBuf,
    values: Arc<Mutex<BTreeMap<String, Value>>>,
}

impl ExtensionStorage {
    pub(super) fn open(extension_root: &Path, identity: &str) -> Result<Self, String> {
        let path = storage_path(extension_root, identity)?;
        let values = match fs::read(&path) {
            Ok(bytes) => {
                if bytes.len() > STORAGE_BYTES_LIMIT {
                    return Err("extension storage exceeds 262144 bytes".to_owned());
                }
                serde_json::from_slice::<BTreeMap<String, Value>>(&bytes)
                    .map_err(|error| format!("load extension storage: {error}"))?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(format!("load extension storage: {error}")),
        };
        validate_values(&values)?;
        Ok(Self {
            path,
            values: Arc::new(Mutex::new(values)),
        })
    }

    pub(super) fn get(&self, key: &str) -> Result<Option<Value>, String> {
        validate_key(key)?;
        self.values
            .lock()
            .map(|values| values.get(key).cloned())
            .map_err(|_| "extension storage lock poisoned".to_owned())
    }

    pub(super) fn set(&self, key: String, value: Option<Value>) -> Result<(), String> {
        validate_key(&key)?;
        if let Some(value) = value.as_ref() {
            let encoded = serde_json::to_vec(value).map_err(|error| error.to_string())?;
            if encoded.len() > STORAGE_VALUE_BYTES_LIMIT {
                return Err("extension storage value exceeds 65536 bytes".to_owned());
            }
        }
        let mut values = self
            .values
            .lock()
            .map_err(|_| "extension storage lock poisoned".to_owned())?;
        let mut candidate = values.clone();
        match value {
            Some(value) => {
                candidate.insert(key, value);
            }
            None => {
                candidate.remove(&key);
            }
        }
        validate_values(&candidate)?;
        let encoded = serde_json::to_vec(&candidate).map_err(|error| error.to_string())?;
        if encoded.len() > STORAGE_BYTES_LIMIT {
            return Err("extension storage exceeds 262144 bytes".to_owned());
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "extension storage path has no parent directory".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        extension_source_writer::save_bytes(&self.path, &encoded)
            .map_err(|error| format!("write extension storage: {error}"))?;
        *values = candidate;
        Ok(())
    }
}

fn storage_path(extension_root: &Path, identity: &str) -> Result<PathBuf, String> {
    let config_root = extension_root
        .parent()
        .ok_or_else(|| "extension root has no parent directory".to_owned())?;
    let mut path = config_root.join("extension-storage");
    for component in Path::new(identity).components() {
        let std::path::Component::Normal(component) = component else {
            return Err("extension module identity is invalid".to_owned());
        };
        path.push(component);
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "extension module identity is invalid".to_owned())?;
    path.set_file_name(format!("{file_name}.json"));
    Ok(path)
}

fn validate_values(values: &BTreeMap<String, Value>) -> Result<(), String> {
    if values.len() > STORAGE_KEY_LIMIT {
        return Err("extension storage key count exceeds 64".to_owned());
    }
    for (key, value) in values {
        validate_key(key)?;
        let encoded = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        if encoded.len() > STORAGE_VALUE_BYTES_LIMIT {
            return Err("extension storage value exceeds 65536 bytes".to_owned());
        }
    }
    Ok(())
}

fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > STORAGE_KEY_BYTES_LIMIT || key.chars().any(char::is_control) {
        return Err("extension storage key is invalid".to_owned());
    }
    Ok(())
}
