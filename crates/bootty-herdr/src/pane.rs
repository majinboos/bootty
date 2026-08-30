use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::Result;
use bootty_mux::terminal::{
    AttachLaunch, BackendPanePolicy, PaneLayoutResizeRequest, PaneStartRequest,
    ScopedMuxPaneTarget, TerminalRuntime, start_attach_terminal,
};
use bootty_mux_model::SshTarget;
use bootty_remote::ssh::SshRemote;
use serde_json::json;

use crate::{
    control::{CliHerdrApi, HerdrApi},
    remote::{RemoteHerdrApi, RemoteHerdrBridge},
};

const HERDR_PROGRAM: &str = "herdr";
const POLICY_QUEUE_CAPACITY: usize = 64;
enum PolicyCommand {
    Focus(String),
}

struct PolicyWorker {
    tx: mpsc::SyncSender<PolicyCommand>,
    errors: Arc<Mutex<VecDeque<String>>>,
}

impl PolicyWorker {
    fn new(session: String, remote: Option<Arc<RemoteHerdrBridge>>) -> Self {
        let (tx, rx) = mpsc::sync_channel(POLICY_QUEUE_CAPACITY);
        let errors = Arc::new(Mutex::new(VecDeque::new()));
        let worker_errors = Arc::clone(&errors);
        thread::spawn(move || {
            run_policy_worker(PolicyWorkerConfig {
                session,
                remote,
                rx,
                errors: worker_errors,
            });
        });
        Self { tx, errors }
    }

    fn send(&self, command: PolicyCommand) -> Result<()> {
        self.tx.try_send(command).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => anyhow::anyhow!("Herdr policy command queue is full"),
            mpsc::TrySendError::Disconnected(_) => {
                anyhow::anyhow!("Herdr policy command worker stopped")
            }
        })
    }

    fn drain_errors(&self) -> Vec<String> {
        self.errors.lock().map_or_else(
            |_| vec!["Herdr policy error lock poisoned".to_owned()],
            |mut errors| errors.drain(..).collect(),
        )
    }
}

struct PolicyWorkerConfig {
    session: String,
    remote: Option<Arc<RemoteHerdrBridge>>,
    rx: mpsc::Receiver<PolicyCommand>,
    errors: Arc<Mutex<VecDeque<String>>>,
}

fn run_policy_worker(config: PolicyWorkerConfig) {
    let PolicyWorkerConfig {
        session,
        remote,
        rx,
        errors,
    } = config;
    while let Ok(command) = rx.recv() {
        let result = match command {
            PolicyCommand::Focus(pane_id) => policy_request(
                &session,
                remote.as_ref(),
                "pane.focus",
                json!({"pane_id": pane_id}),
            ),
        };
        if let Err(error) = result {
            record_policy_error(&errors, error.to_string());
        }
    }
}

fn record_policy_error(errors: &Arc<Mutex<VecDeque<String>>>, error: String) {
    if let Ok(mut errors) = errors.lock() {
        if errors.len() >= POLICY_QUEUE_CAPACITY {
            errors.pop_front();
        }
        errors.push_back(error);
    }
}

fn policy_request(
    session: &str,
    remote: Option<&Arc<RemoteHerdrBridge>>,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    match remote {
        Some(bridge) => RemoteHerdrApi::new(Arc::clone(bridge)).request(method, params),
        None => CliHerdrApi::new(session).request(method, params),
    }
}

pub struct HerdrPanePolicy {
    session: String,
    remote: Option<Arc<RemoteHerdrBridge>>,
    focused_pane: Option<String>,
    worker: PolicyWorker,
}

impl HerdrPanePolicy {
    pub fn new(session: impl Into<String>) -> Self {
        let session = session.into();
        Self {
            worker: PolicyWorker::new(session.clone(), None),
            session,
            remote: None,
            focused_pane: None,
        }
    }

    pub fn remote(session: impl Into<String>, target: SshTarget) -> Result<Self> {
        let session = session.into();
        let remote = RemoteHerdrBridge::shared(target, session.clone())?;
        Ok(Self {
            worker: PolicyWorker::new(session.clone(), Some(Arc::clone(&remote))),
            remote: Some(remote),
            session,
            focused_pane: None,
        })
    }
}

impl BackendPanePolicy for HerdrPanePolicy {
    fn remote_target(&self) -> Option<&SshTarget> {
        self.remote.as_deref().map(RemoteHerdrBridge::target)
    }

    fn start_terminal(
        &mut self,
        request: PaneStartRequest<'_>,
    ) -> Result<Option<Box<dyn TerminalRuntime>>> {
        let args = vec!["--session".to_owned(), self.session.clone()];
        let (program, args, remote) = match self.remote.as_ref() {
            Some(bridge) => {
                let remote = SshRemote::new(bridge.target().clone());
                let (program, args) = remote.proxy_tty_command(HERDR_PROGRAM, &args)?;
                (program, args, true)
            }
            None => (HERDR_PROGRAM.to_owned(), args, false),
        };
        start_attach_terminal(
            request,
            AttachLaunch {
                program,
                args,
                env_remove: Vec::new(),
                env: Vec::new(),
                // Herdr enables its stock direct Kitty relay for known compatible terminal hosts.
                // Remove this override once Herdr recognizes Bootty itself.
                term_program: (!remote).then(|| "Ghostty".to_owned()),
                remote,
            },
        )
        .map(Some)
    }

    fn sync_target(&mut self, target: Option<&ScopedMuxPaneTarget>, _hide_tmux_status: bool) {
        let next = target
            .and_then(ScopedMuxPaneTarget::pane_id)
            .map(str::to_owned);
        if next == self.focused_pane {
            return;
        }
        self.focused_pane.clone_from(&next);
        if let Some(pane_id) = next
            && let Err(error) = self.worker.send(PolicyCommand::Focus(pane_id))
        {
            record_policy_error(&self.worker.errors, error.to_string());
        }
    }

    fn set_layout_window(&mut self, _window_id: Option<&str>) {}

    fn resize_layout_window(&mut self, _request: PaneLayoutResizeRequest<'_>) -> Result<bool> {
        Ok(false)
    }

    fn poll_async_errors(&mut self) -> Vec<String> {
        self.worker.drain_errors()
    }

    fn deactivate(&mut self) {
        self.focused_pane = None;
    }
}
