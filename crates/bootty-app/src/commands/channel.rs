use std::{
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    time::Instant,
};

use crate::mux::{RepaintHandle, controller::CommandCancellation};

use super::{Caller, CommandInvocation, CommandOutcome, CommandTarget};

pub struct AppCommandRequest {
    pub invocation: CommandInvocation,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<CommandOutcome>,
    /// Event provenance for requests that cross the control socket.
    pub completion: Option<CommandCompletionContext>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandCompletionContext {
    pub caller: Caller,
    pub owner_pid: u32,
    pub owner_generation: u64,
    pub target: Option<CommandTarget>,
}

#[derive(Clone)]
pub struct AppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    repaint: RepaintHandle,
    open: Arc<Mutex<bool>>,
}

#[derive(Clone)]
pub struct BoundAppCommandSender {
    sender: SyncSender<AppCommandRequest>,
    repaint: RepaintHandle,
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

pub fn app_command_channel(capacity: usize) -> (AppCommandSender, AppCommandReceiver) {
    app_command_channel_with_repaint(capacity, Arc::new(|| {}))
}

pub fn app_command_channel_with_repaint(
    capacity: usize,
    repaint: RepaintHandle,
) -> (AppCommandSender, AppCommandReceiver) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let open = Arc::new(Mutex::new(true));
    (
        AppCommandSender {
            sender,
            repaint,
            open: open.clone(),
        },
        AppCommandReceiver { receiver, open },
    )
}

impl AppCommandSender {
    /// Binds the authenticated transport identity used for every submitted invocation.
    ///
    /// Code on the UI owner thread must dispatch directly; waiting there would prevent the next
    /// frame from draining this channel.
    pub fn for_caller(&self, caller: Caller) -> BoundAppCommandSender {
        BoundAppCommandSender {
            sender: self.sender.clone(),
            repaint: self.repaint.clone(),
            open: self.open.clone(),
            caller,
        }
    }
}

impl BoundAppCommandSender {
    pub fn try_send(&self, mut request: AppCommandRequest) -> Result<(), AppCommandSendError> {
        let open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        if !*open {
            return Err(AppCommandSendError::Shutdown);
        }
        request.invocation.caller = self.caller;
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => AppCommandSendError::Overloaded,
            TrySendError::Disconnected(_) => AppCommandSendError::Shutdown,
        })?;
        (self.repaint)();
        Ok(())
    }
}

impl AppCommandReceiver {
    pub fn try_recv(&self) -> Result<AppCommandRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }

    /// Stop accepting new requests while the owner performs bounded teardown.
    pub fn close(&self) {
        let mut open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        *open = false;
    }
}

impl Drop for AppCommandReceiver {
    fn drop(&mut self) {
        let mut open = self.open.lock().unwrap_or_else(|error| error.into_inner());
        *open = false;
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request.response.send(CommandOutcome::Failed {
                code: "shutdown".to_owned(),
                message: "application command channel shut down".to_owned(),
            });
        }
    }
}
