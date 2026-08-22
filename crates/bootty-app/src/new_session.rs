use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};

use bootty_config::config::SshRemoteConfig;
use bootty_mux::{
    controller::RepaintHandle,
    process::{CancellableCommandRunner, CommandCancellation},
    project::{ProjectPickerEntry, WorktreePickerEntry},
};

use crate::error_catalog::ErrorNotice;

pub(crate) enum RemoteEffect {
    ListProjects,
    ListWorktrees(String, Vec<String>),
    ToggleFavorite(String),
    CreateWorktree(String, String),
}

pub(crate) enum RemoteOutcome {
    Projects(Vec<ProjectPickerEntry>),
    Worktrees(Vec<WorktreePickerEntry>),
    Favorite { path: String, favorite: bool },
    CreatedWorktree(String),
}

pub(crate) struct RemoteNewSession {
    remote: SshRemoteConfig,
    repaint: RepaintHandle,
    task: Option<RemoteTask>,
}

impl RemoteNewSession {
    pub(crate) fn new(remote: SshRemoteConfig, repaint: RepaintHandle) -> Self {
        let mut owner = Self {
            remote,
            repaint,
            task: None,
        };
        owner.start(RemoteEffect::ListProjects);
        owner
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    pub(crate) fn start(&mut self, effect: RemoteEffect) {
        let (sender, receiver) = mpsc::channel();
        let cancellation = CommandCancellation::default();
        let runner = CancellableCommandRunner::new(cancellation.clone());
        let repaint = self.repaint.clone();
        let remote = self.remote.clone();
        self.task = Some(RemoteTask {
            receiver,
            cancellation,
        });
        let Some(permit) = RemoteWorkerPermit::acquire() else {
            let error = ErrorNotice::RemoteProjectOperationStopping.to_string();
            let _ = sender.send(Err(error));
            repaint();
            return;
        };
        std::thread::spawn(move || {
            let _permit = permit;
            let result = run_effect(&remote, effect, &runner).map_err(|error| error.to_string());
            let _ = sender.send(result);
            repaint();
        });
    }

    pub(crate) fn poll(&mut self) -> Option<Result<RemoteOutcome, String>> {
        let result = match self.task.as_ref()?.receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(ErrorNotice::RemoteProjectTaskStopped.to_string())
            }
        };
        self.task = None;
        Some(result)
    }
}

fn run_effect(
    remote: &SshRemoteConfig,
    effect: RemoteEffect,
    runner: &CancellableCommandRunner,
) -> Result<RemoteOutcome, anyhow::Error> {
    use crate::remote_catalog;
    Ok(match effect {
        RemoteEffect::ListProjects => RemoteOutcome::Projects(
            remote_catalog::list_remote_projects_with_runner(remote, runner)?,
        ),
        RemoteEffect::ListWorktrees(project, open_cwds) => {
            RemoteOutcome::Worktrees(remote_catalog::list_remote_worktrees_with_runner(
                remote, &project, &open_cwds, runner,
            )?)
        }
        RemoteEffect::ToggleFavorite(path) => {
            let favorite =
                remote_catalog::toggle_remote_project_favorite_with_runner(remote, &path, runner)?;
            RemoteOutcome::Favorite { path, favorite }
        }
        RemoteEffect::CreateWorktree(project, branch) => RemoteOutcome::CreatedWorktree(
            remote_catalog::create_remote_worktree_with_runner(remote, &project, &branch, runner)?,
        ),
    })
}

struct RemoteTask {
    receiver: Receiver<Result<RemoteOutcome, String>>,
    cancellation: CommandCancellation,
}

impl Drop for RemoteTask {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

static REMOTE_WORKER_ACTIVE: AtomicBool = AtomicBool::new(false);

struct RemoteWorkerPermit;

impl RemoteWorkerPermit {
    fn acquire() -> Option<Self> {
        (!REMOTE_WORKER_ACTIVE.swap(true, Ordering::AcqRel)).then_some(Self)
    }
}

impl Drop for RemoteWorkerPermit {
    fn drop(&mut self) {
        REMOTE_WORKER_ACTIVE.store(false, Ordering::Release);
    }
}
