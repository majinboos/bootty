use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

pub fn display_path(path: &str, home: Option<&Path>) -> String {
    let path = Path::new(path);
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(home)
    {
        return Path::new("~").join(relative).display().to_string();
    }
    path.display().to_string()
}

pub fn head_branch(cwd: &str) -> Option<String> {
    let head = std::fs::read_to_string(git_dir(Path::new(cwd))?.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!branch.is_empty()).then(|| branch.to_owned());
    }
    let commit = head.get(..7).unwrap_or(head);
    (!commit.is_empty()).then(|| format!("detached {commit}"))
}

const MAX_WATCHED_WORKTREES: usize = 64;

struct WorktreeWatch {
    revision: Arc<AtomicU64>,
    paths: Vec<PathBuf>,
}

type WorktreeRevisions = Arc<Mutex<HashMap<PathBuf, WorktreeWatch>>>;

fn worktree_revisions() -> &'static WorktreeRevisions {
    static REVISIONS: OnceLock<WorktreeRevisions> = OnceLock::new();
    REVISIONS.get_or_init(WorktreeRevisions::default)
}

fn worktree_watcher() -> &'static Mutex<Option<RecommendedWatcher>> {
    static WATCHER: OnceLock<Mutex<Option<RecommendedWatcher>>> = OnceLock::new();
    WATCHER.get_or_init(|| {
        let revisions = Arc::clone(worktree_revisions());
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let Ok(event) = event else { return };
            let Ok(revisions) = revisions.lock() else {
                return;
            };
            for watched in revisions.values() {
                if event
                    .paths
                    .iter()
                    .any(|path| watched.paths.iter().any(|root| path.starts_with(root)))
                {
                    watched.revision.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        Mutex::new(watcher.ok())
    })
}

pub fn worktree_revision(cwd: &str) -> u64 {
    let Some(paths) = worktree_watch_paths(Path::new(cwd)) else {
        return 0;
    };
    let root = paths[0].clone();
    if let Ok(revisions) = worktree_revisions().lock()
        && let Some(watched) = revisions.get(&root)
    {
        return watched.revision.load(Ordering::Relaxed);
    }
    let Ok(mut watcher) = worktree_watcher().lock() else {
        return 0;
    };
    let Some(watcher) = watcher.as_mut() else {
        return 0;
    };
    let Ok(mut revisions) = worktree_revisions().lock() else {
        return 0;
    };
    if revisions.len() >= MAX_WATCHED_WORKTREES {
        return 0;
    }
    let mut registered: Vec<PathBuf> = Vec::new();
    for path in &paths {
        if watcher.watch(path, RecursiveMode::Recursive).is_err() {
            for registered_path in registered {
                let _ = watcher.unwatch(&registered_path);
            }
            return 0;
        }
        registered.push(path.clone());
    }
    revisions.insert(
        root,
        WorktreeWatch {
            revision: Arc::new(AtomicU64::new(1)),
            paths,
        },
    );
    1
}

fn worktree_watch_paths(cwd: &Path) -> Option<Vec<PathBuf>> {
    let root = native_worktree_root(cwd)?;
    let git_dir = std::fs::canonicalize(git_dir(cwd)?).ok()?;
    let mut paths = vec![root.clone()];
    if !git_dir.starts_with(&root) {
        paths.push(git_dir);
    }
    Some(paths)
}

fn native_worktree_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .and_then(|dir| std::fs::canonicalize(dir).ok())
}

fn git_dir(cwd: &Path) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            let pointer = std::fs::read_to_string(&candidate).ok()?;
            let target = Path::new(pointer.trim().strip_prefix("gitdir:")?.trim()).to_path_buf();
            return Some(if target.is_absolute() {
                target
            } else {
                dir.join(target)
            });
        }
    }
    None
}
