use std::sync::mpsc;
use std::time::{Duration, Instant};

use bootty_command::{CommandCancellation, CommandInvocation, CommandOutcome};
use serde_json::Value;

use crate::{ExtensionUiAction, ModuleIdentity};

pub(crate) const INVOCATION_QUEUE_LIMIT: usize = 64;
pub const EVENT_QUEUE_LIMIT: usize = 64;

pub(crate) struct ExtensionInvocationRequest {
    pub(crate) invocation: CommandInvocation,
    pub(crate) deadline: Instant,
    pub(crate) cancellation: CommandCancellation,
    pub(crate) response: mpsc::Sender<CommandOutcome>,
}

#[derive(Clone)]
pub struct ExtensionInvocationSender {
    sender: mpsc::SyncSender<ExtensionWorkerMessage>,
}

pub(crate) enum ExtensionWorkerMessage {
    Invoke(ExtensionInvocationRequest),
    Render,
    Action(ExtensionUiAction),
}

#[derive(Clone)]
pub(crate) struct ExtensionWorkerSender {
    sender: mpsc::SyncSender<ExtensionWorkerMessage>,
}

pub(crate) struct ExtensionWorkerReceiver {
    receiver: mpsc::Receiver<ExtensionWorkerMessage>,
}

pub(crate) fn worker_queue() -> (ExtensionWorkerSender, ExtensionWorkerReceiver) {
    let (sender, receiver) = mpsc::sync_channel(INVOCATION_QUEUE_LIMIT);
    (
        ExtensionWorkerSender { sender },
        ExtensionWorkerReceiver { receiver },
    )
}

impl ExtensionWorkerSender {
    pub(crate) fn invocation_sender(&self) -> ExtensionInvocationSender {
        ExtensionInvocationSender {
            sender: self.sender.clone(),
        }
    }

    pub(crate) fn try_render(&self) {
        let _ = self.sender.try_send(ExtensionWorkerMessage::Render);
    }

    pub(crate) fn try_action(&self, action: ExtensionUiAction) -> Result<(), &'static str> {
        self.sender
            .try_send(ExtensionWorkerMessage::Action(action))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => "extension command queue is full",
                mpsc::TrySendError::Disconnected(_) => "extension generation is no longer active",
            })
    }
}

impl ExtensionWorkerReceiver {
    pub(crate) fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<ExtensionWorkerMessage, mpsc::RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }

    pub(crate) fn drain_shutdown(&self) {
        while let Ok(message) = self.receiver.try_recv() {
            if let ExtensionWorkerMessage::Invoke(request) = message {
                let _ = request.response.send(shutdown_outcome());
            }
        }
    }
}

impl Drop for ExtensionWorkerReceiver {
    fn drop(&mut self) {
        self.drain_shutdown();
    }
}

impl ExtensionInvocationSender {
    pub fn invoke(
        &self,
        invocation: CommandInvocation,
        deadline: Instant,
        cancellation: CommandCancellation,
    ) -> mpsc::Receiver<CommandOutcome> {
        let (response, receiver) = mpsc::channel();
        if cancellation.is_cancelled() {
            let _ = response.send(cancelled_outcome());
            return receiver;
        }
        let request = ExtensionInvocationRequest {
            invocation,
            deadline,
            cancellation,
            response,
        };
        match self
            .sender
            .try_send(ExtensionWorkerMessage::Invoke(request))
        {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(ExtensionWorkerMessage::Invoke(request))) => {
                let _ = request.response.send(overloaded_outcome());
            }
            Err(mpsc::TrySendError::Disconnected(ExtensionWorkerMessage::Invoke(request))) => {
                let _ = request.response.send(stale_outcome());
            }
            Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) => {
                unreachable!("invocation queue only receives invocation messages")
            }
        }
        receiver
    }
}

pub struct ExtensionEventRequest {
    pub identity: ModuleIdentity,
    pub generation: u64,
    pub topic: String,
    pub payload: Value,
    pub deadline: Instant,
    pub cancellation: CommandCancellation,
    pub response: mpsc::Sender<Result<(), String>>,
}

#[derive(Clone)]
pub struct ExtensionEventSender {
    sender: mpsc::SyncSender<ExtensionEventRequest>,
}

pub struct ExtensionEventReceiver {
    receiver: mpsc::Receiver<ExtensionEventRequest>,
}

pub fn event_queue() -> (ExtensionEventSender, ExtensionEventReceiver) {
    let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_LIMIT);
    (
        ExtensionEventSender { sender },
        ExtensionEventReceiver { receiver },
    )
}

impl ExtensionEventSender {
    pub fn publish(
        &self,
        identity: ModuleIdentity,
        generation: u64,
        topic: String,
        payload: Value,
        deadline: Instant,
        cancellation: &CommandCancellation,
    ) -> Result<(), String> {
        if cancellation.is_cancelled() {
            return Err("extension event was cancelled".to_owned());
        }
        let (response, receiver) = mpsc::channel();
        let request = ExtensionEventRequest {
            identity,
            generation,
            topic,
            payload,
            deadline,
            cancellation: cancellation.clone(),
            response,
        };
        match self.sender.try_send(request) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(request)) => {
                let _ = request
                    .response
                    .send(Err("extension event queue is full".to_owned()));
                return Err("extension event queue is full".to_owned());
            }
            Err(mpsc::TrySendError::Disconnected(request)) => {
                let _ = request
                    .response
                    .send(Err("extension event queue is shut down".to_owned()));
                return Err("extension event queue is shut down".to_owned());
            }
        }
        loop {
            if cancellation.is_cancelled() {
                return Err("extension event was cancelled".to_owned());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("extension event deadline expired".to_owned());
            }
            match receiver.recv_timeout(remaining.min(Duration::from_millis(5))) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("extension event control response closed".to_owned());
                }
            }
        }
    }
}

impl ExtensionEventReceiver {
    pub fn try_recv(&self) -> Result<ExtensionEventRequest, mpsc::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for ExtensionEventReceiver {
    fn drop(&mut self) {
        while let Ok(request) = self.receiver.try_recv() {
            let _ = request
                .response
                .send(Err("extension event queue shut down".to_owned()));
        }
    }
}

fn overloaded_outcome() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "extension_busy".to_owned(),
        message: "extension command queue is full".to_owned(),
    }
}

fn stale_outcome() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "stale_extension_generation".to_owned(),
        message: "extension generation is no longer active".to_owned(),
    }
}

fn shutdown_outcome() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "shutdown".to_owned(),
        message: "extension worker stopped".to_owned(),
    }
}

fn cancelled_outcome() -> CommandOutcome {
    CommandOutcome::Failed {
        code: "cancelled".to_owned(),
        message: "extension command was cancelled".to_owned(),
    }
}
