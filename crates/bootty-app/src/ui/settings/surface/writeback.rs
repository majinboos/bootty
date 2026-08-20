use std::path::{Path, PathBuf};

use crate::{
    color::Color,
    config::{
        BoottyConfig, ConfigDocument, ConfigResult, load_config_document, load_config_from_path,
        update_config_document,
    },
};

pub(super) struct SettingsWriteback {
    path: PathBuf,
    last_error: Option<String>,
}

impl SettingsWriteback {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_error: None,
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub(super) fn set_error(&mut self, error: impl ToString) {
        self.last_error = Some(error.to_string());
    }

    pub(super) fn reload(&mut self) -> Option<BoottyConfig> {
        match load_config_from_path(&self.path) {
            Ok(config) => Some(config),
            Err(error) => {
                self.set_error(error);
                None
            }
        }
    }

    pub(super) fn mutate(
        &mut self,
        mutation: impl FnOnce(&mut ConfigDocument) -> ConfigResult<()>,
    ) {
        self.last_error = match update_config_document(&self.path, mutation) {
            Ok(outcome) => outcome.durability_warning().map(str::to_owned),
            Err(error) => Some(error.to_string()),
        };
    }

    pub(super) fn set_f32(&mut self, path: &[&str], value: f32) {
        self.mutate(|document| document.set_f32(path, value));
    }

    pub(super) fn set_bool(&mut self, path: &[&str], value: bool) {
        self.mutate(|document| document.set_bool(path, value));
    }

    pub(super) fn set_str(&mut self, path: &[&str], value: &str) {
        self.mutate(|document| document.set_str(path, value));
    }

    pub(super) fn set_i64(&mut self, path: &[&str], value: i64) {
        self.mutate(|document| document.set_i64(path, value));
    }

    pub(super) fn set_env(&mut self, path: &[&str], entries: &[(String, String)]) {
        self.mutate(move |document| document.set_env(path, entries));
    }

    pub(super) fn set_color(&mut self, path: &[&str], rgb: [u8; 3]) {
        self.set_color_value(
            path,
            Color {
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
                a: 0xff,
            },
        );
    }

    pub(super) fn set_color_value(&mut self, path: &[&str], color: Color) {
        let hex = color_hex(color);
        self.mutate(move |document| document.set_str(path, &hex));
    }

    pub(super) fn set_strings(&mut self, path: &[&str], values: &[String]) {
        self.mutate(|document| document.set_strings(path, values));
    }

    pub(super) fn contains(&self, path: &[&str]) -> bool {
        let Ok(Some(document)) = load_config_document(&self.path) else {
            return false;
        };
        document.contains(path)
    }

    pub(super) fn string_array(&self, path: &[&str]) -> Option<Vec<String>> {
        let Ok(Some(document)) = load_config_document(&self.path) else {
            return None;
        };
        document.string_array(path)
    }

    pub(super) fn remove(&mut self, path: &[&str]) {
        self.mutate(|document| document.remove(path));
    }
}

fn color_hex(color: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}
