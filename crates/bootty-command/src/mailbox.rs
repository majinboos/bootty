use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

use crate::{Caller, CommandCancellation, CommandInvocation, CommandOutcome};

pub type WakeCallback = Arc<dyn Fn() + Send + Sync + 'static>;

pub struct AppCommandRequest {
    pub invocation: CommandInvocation,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<CommandOutcome>,
}

#[derive(Clone)]
pub struct AppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    wake: WakeCallback,
    open: Arc<Mutex<bool>>,
}

#[derive(Clone)]
pub struct BoundAppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    wake: WakeCallback,
    open: Arc<Mutex<bool>>,
    caller: Caller,
}

pub struct AppCommandReceiver {
    receiver: Receiver<AppCommandRequest>,
    open: Arc<Mutex<bool>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppCommandSendError {
    Overloaded,
    Shutdown,
}

pub fn app_command_channel(
    capacity: usize,
    wake: WakeCallback,
) -> (AppCommandSender, AppCommandReceiver) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let open = Arc::new(Mutex::new(true));
    (
        AppCommandSender {
            sender,
            wake,
            open: open.clone(),
        },
        AppCommandReceiver { receiver, open },
    )
}

impl AppCommandSender {
    /// Binds the authenticated transport identity used for every submitted invocation.
    ///
    /// Submission is non-blocking and responses arrive asynchronously. Code already running on
    /// the AppState/UI owner thread must dispatch directly; waiting there for this channel would
    /// prevent the next frame from draining the request.
    pub fn for_caller(&self, caller: Caller) -> BoundAppCommandSender {
        BoundAppCommandSender {
            sender: self.sender.clone(),
            wake: self.wake.clone(),
            open: self.open.clone(),
            caller,
        }
    }
}

impl BoundAppCommandSender {
    pub fn submit(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> Result<Receiver<CommandOutcome>, AppCommandSendError> {
        let (response, receiver) = mpsc::channel();
        self.try_send(AppCommandRequest {
            invocation,
            deadline,
            cancellation,
            response,
        })?;
        Ok(receiver)
    }

    pub fn try_send(&self, mut request: AppCommandRequest) -> Result<(), AppCommandSendError> {
        let open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*open {
            return Err(AppCommandSendError::Shutdown);
        }
        request.invocation.caller = self.caller;
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => AppCommandSendError::Overloaded,
            TrySendError::Disconnected(_) => AppCommandSendError::Shutdown,
        })?;
        (self.wake)();
        Ok(())
    }
}

impl AppCommandReceiver {
    pub fn try_recv(&self) -> Result<AppCommandRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for AppCommandReceiver {
    fn drop(&mut self) {
        let mut open = self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *open = false;
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request.response.send(CommandOutcome::Failed {
                code: "shutdown".to_owned(),
                message: "application command channel shut down".to_owned(),
            });
        }
    }
}
