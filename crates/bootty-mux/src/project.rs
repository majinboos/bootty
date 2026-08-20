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

/// Git state of a session's working directory, used to decide which ditch
/// cleanup actions are safe to offer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeStatus {
    /// The cwd is inside a git work tree.
    pub in_repo: bool,
    /// The cwd is a *linked* worktree (not the repo's main working tree), so it
    /// can be removed without destroying the primary checkout.
    pub is_linked_worktree: bool,
    /// Current branch, or `None` when detached.
    pub branch: Option<String>,
    /// Has uncommitted changes (tracked edits or untracked files).
    pub dirty: bool,
    /// Commits on HEAD not present on its upstream (0 when no upstream).
    pub unpushed: u32,
    /// HEAD has a configured upstream branch.
    pub has_upstream: bool,
}

/// Inspect the git state of `cwd`. Any git failure yields a safe, empty status
/// (`in_repo == false`), so callers only ever offer "kill session".
pub fn status(cwd: &str) -> WorktreeStatus {
    let mut status = WorktreeStatus::default();
    if read(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return status;
    }
    status.in_repo = true;
    // A linked worktree's own git dir differs from the shared common dir.
    if let (Some(git_dir), Some(common)) = (
        read(cwd, &["rev-parse", "--absolute-git-dir"]),
        read(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ),
    ) {
        status.is_linked_worktree = git_dir != common;
    }
    status.branch = read(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]);
    status.dirty = read(cwd, &["status", "--porcelain"]).is_some_and(|out| !out.is_empty());
    if let Some(count) =
        read(cwd, &["rev-list", "--count", "@{u}..HEAD"]).and_then(|out| out.parse().ok())
    {
        status.has_upstream = true;
        status.unpushed = count;
    }
    status
}

/// Detach HEAD in `worktree_path`, freeing its branch while keeping the
/// worktree directory and every commit. Fully non-destructive — the ditch
/// "detach" action runs this before killing the session.
pub fn detach_head(worktree_path: &str) -> Result<(), String> {
    run(worktree_path, &["checkout", "--detach"])
}

/// Number of worktrees attached to the repo containing `cwd` — the main working
/// tree plus every linked worktree. `0` when `cwd` is not in a git repo. Used to
/// gate the detach action, which only earns its keep in a multi-worktree repo.
pub fn worktree_count(cwd: &str) -> usize {
    read(cwd, &["worktree", "list", "--porcelain"])
        .map(|out| {
            out.lines()
                .filter(|line| line.starts_with("worktree "))
                .count()
        })
        .unwrap_or(0)
}

/// The repo's trunk branch — the one ditch must never offer to delete. Resolved
/// from the remote's default (`origin/HEAD`); for repos without a remote it
/// falls back to the branch checked out in the main worktree. `None` when
/// neither is known, in which case no branch is treated as the trunk.
pub fn trunk_branch(cwd: &str) -> Option<String> {
    read(
        cwd,
        &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
    )
    .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))
    .or_else(|| {
        let main = main_worktree(cwd)?;
        read(&main, &["symbolic-ref", "--quiet", "--short", "HEAD"])
    })
}

/// Remove the linked worktree rooted at `worktree_path`. Runs from the main
/// working tree so git doesn't refuse to remove the tree you're standing in;
/// `force` is required when the worktree is dirty.
pub fn remove_worktree(worktree_path: &str, force: bool) -> Result<(), String> {
    let main = main_worktree(worktree_path)
        .ok_or_else(|| "could not locate the main worktree".to_owned())?;
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(worktree_path);
    run(&main, &args)
}

/// Delete `branch`, running git in `repo_dir` — any live working tree of the
/// repo. Pass the main worktree, since a just-removed linked worktree is gone.
/// `force` maps to `git branch -D` (drops unmerged commits) vs the safe `-d`.
pub fn delete_branch(repo_dir: &str, branch: &str, force: bool) -> Result<(), String> {
    run(
        repo_dir,
        &["branch", if force { "-D" } else { "-d" }, branch],
    )
}

/// The root directory of the Git worktree containing `cwd`.
pub fn worktree_root(cwd: &str) -> Option<String> {
    read(cwd, &["rev-parse", "--show-toplevel"])
}

/// Suggest a grouped session name for a worktree, or a basename for a plain directory.
pub fn suggested_session_name(cwd: &str) -> String {
    let Some(worktree) = worktree_root(cwd) else {
        return session_name_for_path(cwd).to_owned();
    };
    let status = status(&worktree);

    let group = main_worktree(&worktree)
        .as_deref()
        .map(|path| session_name_for_path(path).to_owned())
        .unwrap_or_else(|| session_name_for_path(&worktree).to_owned());
    let leaf = status
        .branch
        .as_deref()
        .and_then(|branch| branch.rsplit('/').next())
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| session_name_for_path(&worktree).to_owned());
    format!("{group}/{leaf}")
}

pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(|name| env::var_os(name))
}

pub fn home_dir_from(mut var: impl FnMut(&str) -> Option<OsString>) -> Option<PathBuf> {
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
    let Ok(output) = git_command(project_path, &["worktree", "list", "--porcelain"]).output()
    else {
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
    let output = git_command(repo_dir, &["worktree", "add", "-b", branch, &path])
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

pub fn main_worktree(cwd: &str) -> Option<String> {
    let common = read(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    Path::new(&common)
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

fn read(cwd: &str, args: &[&str]) -> Option<String> {
    record_subprocess("git read");
    let output = git_command(cwd, args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn run(cwd: &str, args: &[&str]) -> Result<(), String> {
    record_subprocess("git run");
    let output = git_command(cwd, args)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn git_command(cwd: &str, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    hide_command_window(&mut command);
    command
}

#[cfg(feature = "app")]
fn record_subprocess(what: &str) {
    bootty_runtime::perf::record_subprocess(what);
}

#[cfg(not(feature = "app"))]
fn record_subprocess(_what: &str) {}

#[cfg(windows)]
fn hide_command_window(command: &mut Command) {
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
fn hide_command_window(_command: &mut Command) {}
