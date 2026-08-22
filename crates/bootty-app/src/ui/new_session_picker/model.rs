use std::path::{Path, PathBuf};

use bootty_extension::display_path;
use bootty_mux::project::{ProjectPickerEntry, WorktreePickerEntry};

use crate::strings::{expand_home_path, home_dir};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewMuxSessionStep {
    Project,
    Worktree,
    BranchName,
}

pub(super) fn project_entries_for_filter(
    entries: &[ProjectPickerEntry],
    filter: &str,
) -> Vec<ProjectPickerEntry> {
    let raw_filter = filter.trim();
    let filter = raw_filter.to_ascii_lowercase();
    let mut filtered = entries
        .iter()
        .filter(|entry| {
            filter.is_empty()
                || display_path(&entry.path, home_dir().as_deref())
                    .to_ascii_lowercase()
                    .contains(&filter)
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(entry) = direct_project_entry(raw_filter)
        && !filtered
            .iter()
            .any(|existing| same_project_path(&existing.path, &entry.path))
    {
        filtered.insert(0, entry);
    }
    filtered
}

fn direct_project_entry(filter: &str) -> Option<ProjectPickerEntry> {
    let filter = filter.trim();
    if !looks_like_directory_path(filter) {
        return None;
    }
    let path = expand_home_path(filter);
    path.is_dir().then(|| ProjectPickerEntry {
        path: normalize_path_for_session(&path),
        favorite: false,
    })
}

fn looks_like_directory_path(filter: &str) -> bool {
    Path::new(filter).has_root()
        || filter.starts_with("~/")
        || filter.starts_with("./")
        || filter.starts_with("../")
        || cfg!(windows)
            && (filter.starts_with(r"~\")
                || filter.starts_with(r".\")
                || filter.starts_with(r"..\"))
}

fn normalize_path_for_session(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn same_project_path(a: &str, b: &str) -> bool {
    let normalize = |path: &str| {
        PathBuf::from(path)
            .canonicalize()
            .unwrap_or_else(|_| path.into())
    };
    normalize(a) == normalize(b)
}

pub(super) fn filtered_worktree_entries<'a>(
    entries: &'a [WorktreePickerEntry],
    filter: &str,
) -> Vec<&'a WorktreePickerEntry> {
    let filter = filter.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| filter.is_empty() || entry.label.to_ascii_lowercase().contains(&filter))
        .collect()
}
