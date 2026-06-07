use std::{fs, path::PathBuf};

use crate::strings::{expand_home_path, home_dir, is_hidden_path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub path: String,
    pub favorite: bool,
}

pub fn discover_project_picker_entries() -> Vec<ProjectPickerEntry> {
    let mut entries = Vec::new();
    let favorites = read_favorite_project_paths();
    for path in &favorites {
        push_project_entry(&mut entries, path.clone(), true);
    }

    if let Some(home) = home_dir() {
        for path in [home.join("src"), home.join(".config")] {
            push_project_children(&mut entries, &path);
        }
        for path in [home.join("blueprints"), home.join(".claude")] {
            push_project_entry(&mut entries, path, false);
        }
    }
    entries
}

fn push_project_entry(entries: &mut Vec<ProjectPickerEntry>, path: PathBuf, favorite: bool) {
    if !path.is_dir() {
        return;
    }
    let canonical = path.canonicalize().unwrap_or(path);
    let path = canonical.to_string_lossy().into_owned();
    if let Some(existing) = entries.iter_mut().find(|entry| entry.path == path) {
        existing.favorite |= favorite;
    } else {
        entries.push(ProjectPickerEntry { path, favorite });
    }
}

fn push_project_children(entries: &mut Vec<ProjectPickerEntry>, parent: &std::path::Path) {
    let Ok(children) = fs::read_dir(parent) else {
        return;
    };
    for child in children.flatten() {
        let child_path = child.path();
        if child_path.is_dir() && !is_hidden_path(&child_path) {
            push_project_entry(entries, child_path, false);
        }
    }
}

fn read_favorite_project_paths() -> Vec<PathBuf> {
    home_dir()
        .map(|home| home.join(".config/tmux/.session-favorites"))
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(expand_home_path)
                .collect()
        })
        .unwrap_or_default()
}
