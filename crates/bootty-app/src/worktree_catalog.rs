use std::process::Command;

use crate::strings::session_name_for_path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreePickerEntry {
    pub label: String,
    pub path: Option<String>,
    pub is_new: bool,
}

pub fn discover_worktree_picker_entries(project_path: &str) -> Vec<WorktreePickerEntry> {
    let mut entries = vec![WorktreePickerEntry {
        label: "+ New worktree".to_owned(),
        path: None,
        is_new: true,
    }];
    let output = Command::new("git")
        .args(["-C", project_path, "worktree", "list", "--porcelain"])
        .output();
    let Ok(output) = output else {
        entries.push(WorktreePickerEntry {
            label: format!("{} ← main", session_name_for_path(project_path)),
            path: Some(project_path.to_owned()),
            is_new: false,
        });
        return entries;
    };
    entries.extend(parse_git_worktree_list(&String::from_utf8_lossy(
        &output.stdout,
    )));
    entries
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
                    label: format!("{} ← {branch}", session_name_for_path(&path)),
                    path: Some(path),
                    is_new: false,
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
