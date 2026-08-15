use std::{
    collections::HashSet,
    env,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

mod favorite_paths;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectPickerEntry {
    pub path: String,
    pub favorite: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorktreePickerEntry {
    pub label: String,
    pub path: Option<String>,
    pub is_new: bool,
    #[serde(default)]
    pub occupied: bool,
}

pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(|name| env::var_os(name))
}

fn home_dir_from(mut var: impl FnMut(&str) -> Option<OsString>) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(profile) = non_empty_path(var("USERPROFILE")) {
            return Some(profile);
        }
        Some(non_empty_path(var("HOMEDRIVE"))?.join(non_empty_path(var("HOMEPATH"))?))
    }

    #[cfg(not(windows))]
    {
        non_empty_path(var("HOME"))
    }
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

pub fn discover_project_picker_entries(home: Option<&Path>) -> Vec<ProjectPickerEntry> {
    let mut entries = Vec::new();
    let favorites = read_favorite_project_paths(home);
    for path in &favorites {
        push_project_entry(&mut entries, path.clone(), true);
    }

    if let Some(home) = home {
        for name in ["dotfiles", ".claude", "blueprints"] {
            push_project_entry(&mut entries, home.join(name), false);
        }
        push_project_entry(&mut entries, home.to_path_buf(), false);
        for parent in [home.join("src"), home.join(".config")] {
            push_project_children(&mut entries, &parent);
        }
    }
    entries
}

pub fn toggle_favorite_project_path(home: Option<&Path>, project_path: &str) -> io::Result<bool> {
    let Some(path) = favorite_project_paths_file(home) else {
        return Ok(false);
    };
    toggle_favorite_project_path_at(&path, home, project_path)
}

pub fn discover_worktree_picker_entries(project_path: &str) -> Vec<WorktreePickerEntry> {
    let new_worktree = WorktreePickerEntry {
        label: "New worktree".to_owned(),
        path: None,
        is_new: true,
        occupied: false,
    };
    let mut command = Command::new("git");
    command.args(["-C", project_path, "worktree", "list", "--porcelain"]);
    hide_command_window(&mut command);
    let Ok(output) = command.output() else {
        return vec![main_worktree_entry(project_path)];
    };
    if !output.status.success() {
        return vec![main_worktree_entry(project_path)];
    }
    let mut entries = vec![new_worktree];
    entries.extend(parse_git_worktree_list(&String::from_utf8_lossy(
        &output.stdout,
    )));
    entries
}
pub fn mark_occupied_worktrees(entries: &mut [WorktreePickerEntry], open_cwds: &[String]) {
    let open = open_cwds
        .iter()
        .filter_map(|path| path_identity(path))
        .collect::<HashSet<_>>();
    for entry in entries {
        entry.occupied = entry
            .path
            .as_deref()
            .and_then(path_identity)
            .is_some_and(|identity| open.contains(&identity));
    }
}

fn path_identity(path: &str) -> Option<String> {
    let identity = fs::canonicalize(path).ok()?.to_string_lossy().into_owned();
    #[cfg(windows)]
    return Some(identity.to_lowercase());
    #[cfg(not(windows))]
    Some(identity)
}

pub fn add_worktree(repo_dir: &str, branch: &str) -> Result<String, String> {
    let path = new_worktree_path(repo_dir, branch)?;
    let mut command = Command::new("git");
    command.args(["-C", repo_dir, "worktree", "add", "-b", branch, &path]);
    hide_command_window(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("run git: {error}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(path)
}

fn push_project_entry(entries: &mut Vec<ProjectPickerEntry>, path: PathBuf, favorite: bool) {
    if !path.is_dir() {
        return;
    }
    let path = path.to_string_lossy().into_owned();
    if let Some(existing) = entries.iter_mut().find(|entry| entry.path == path) {
        existing.favorite |= favorite;
    } else {
        entries.push(ProjectPickerEntry { path, favorite });
    }
}

fn push_project_children(entries: &mut Vec<ProjectPickerEntry>, parent: &Path) {
    let Ok(children) = fs::read_dir(parent) else {
        return;
    };
    for child in children.flatten() {
        let child_path = child.path();
        if child_path.is_dir() && !is_hidden_path(&child_path) && !is_linked_worktree(&child_path) {
            push_project_entry(entries, child_path, false);
        }
    }
}

fn is_linked_worktree(dir: &Path) -> bool {
    dir.join(".git").is_file()
}

fn is_hidden_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.') && name != ".config")
}

fn favorite_project_paths_file(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|home| home.join(".config/tmux/.session-favorites"))
}

fn read_favorite_project_paths(home: Option<&Path>) -> Vec<PathBuf> {
    favorite_project_paths_file(home)
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| expand_home_path(home, line))
                .collect()
        })
        .unwrap_or_default()
}

fn expand_home_path(home: Option<&Path>, path: &str) -> PathBuf {
    path.strip_prefix("~/")
        .or_else(|| path.strip_prefix(r"~\"))
        .and_then(|path| home.map(|home| home.join(path)))
        .unwrap_or_else(|| PathBuf::from(path))
}

fn toggle_favorite_project_path_at(
    favorites_file: &Path,
    home: Option<&Path>,
    project_path: &str,
) -> io::Result<bool> {
    favorite_paths::toggle_favorite_project_path_at(favorites_file, home, project_path)
}

fn main_worktree_entry(project_path: &str) -> WorktreePickerEntry {
    WorktreePickerEntry {
        label: format!("{} (main)", session_name_for_path(project_path)),
        path: Some(project_path.to_owned()),
        is_new: false,
        occupied: false,
    }
}

fn parse_git_worktree_list(text: &str) -> Vec<WorktreePickerEntry> {
    let mut entries = Vec::new();
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    for line in text.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(path) = path.take() {
                let branch = branch
                    .take()
                    .and_then(|branch| branch.rsplit('/').next().map(str::to_owned))
                    .unwrap_or_else(|| "detached".to_owned());
                entries.push(WorktreePickerEntry {
                    label: format!("{} ({branch})", session_name_for_path(&path)),
                    path: Some(path),
                    is_new: false,
                    occupied: false,
                });
            }
        } else if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(rest.to_owned());
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = Some(rest.to_owned());
        }
    }
    entries
}

fn new_worktree_path(repo_dir: &str, branch: &str) -> Result<String, String> {
    let main = main_worktree(repo_dir).unwrap_or_else(|| repo_dir.to_owned());
    let main = Path::new(&main);
    let parent = main
        .parent()
        .ok_or_else(|| "repository has no parent directory".to_owned())?;
    let repo_name = main
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "could not read repository name".to_owned())?;
    Ok(parent
        .join(format!("{}-{}", repo_name, branch.replace('/', "-")))
        .to_string_lossy()
        .into_owned())
}

fn main_worktree(cwd: &str) -> Option<String> {
    let mut command = Command::new("git");
    command.args([
        "-C",
        cwd,
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]);
    hide_command_window(&mut command);
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    Path::new(String::from_utf8_lossy(&output.stdout).trim())
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
}

fn session_name_for_path(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("bootty")
        .trim_end_matches(".git")
}

#[cfg(windows)]
fn hide_command_window(command: &mut Command) {
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_command_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn home_directory_ignores_an_empty_home() {
        assert_eq!(
            home_dir_from(|name| (name == "HOME").then(OsString::new)),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn home_directory_prefers_a_non_empty_user_profile() {
        let home = home_dir_from(|name| match name {
            "USERPROFILE" => Some(OsString::from(r"C:\Users\dev")),
            "HOMEDRIVE" => Some(OsString::from("D:")),
            "HOMEPATH" => Some(OsString::from(r"\fallback")),
            _ => None,
        });

        assert_eq!(home, Some(PathBuf::from(r"C:\Users\dev")));
    }

    #[test]
    fn discovers_projects_with_the_local_picker_heuristics() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path();
        fs::create_dir_all(home.join("src/project")).expect("project");
        fs::create_dir_all(home.join("src/.hidden")).expect("hidden");
        fs::create_dir_all(home.join("dotfiles")).expect("dotfiles");

        let entries = discover_project_picker_entries(Some(home));

        assert!(
            entries
                .iter()
                .any(|entry| entry.path.ends_with("src/project"))
        );
        assert!(entries.iter().any(|entry| entry.path.ends_with("dotfiles")));
        assert!(!entries.iter().any(|entry| entry.path.ends_with(".hidden")));
    }

    #[test]
    fn favorite_paths_are_shared_by_local_and_remote_discovery() {
        let directory = tempfile::tempdir().expect("tempdir");
        let home = directory.path();
        let project = home.join("projects/bootty");
        fs::create_dir_all(&project).expect("project");
        let project = project.to_string_lossy().into_owned();

        assert!(toggle_favorite_project_path(Some(home), &project).expect("favorite"));
        assert!(
            discover_project_picker_entries(Some(home))
                .iter()
                .any(|entry| entry.path == project && entry.favorite)
        );
        assert!(!toggle_favorite_project_path(Some(home), &project).expect("unfavorite"));
    }

    #[test]
    fn marks_canonical_worktree_aliases_as_occupied() {
        let directory = tempfile::tempdir().expect("tempdir");
        let project = directory.path().join("project");
        fs::create_dir(&project).expect("project");
        let path = project.to_string_lossy().into_owned();
        let alias = project
            .join("..")
            .join("project")
            .to_string_lossy()
            .into_owned();
        let mut entries = vec![main_worktree_entry(&path)];

        mark_occupied_worktrees(&mut entries, &[alias]);

        assert!(entries[0].occupied);
    }

    #[test]
    fn non_git_directory_does_not_offer_new_worktree() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().to_string_lossy().into_owned();

        assert_eq!(
            discover_worktree_picker_entries(&path),
            vec![main_worktree_entry(&path)]
        );
    }
}
