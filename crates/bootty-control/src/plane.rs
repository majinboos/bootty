use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

use bootty_extension::{
    ExtensionCatalog, ExtensionEventReceiver, ExtensionEventSender, event_queue,
};
use serde_json::{Value, json};

use crate::state::{SharedControlState, lock_control_state};

#[derive(Clone)]
pub struct ControlPlane {
    pub(crate) state: SharedControlState,
    pub(crate) instance_scope: Arc<Mutex<Option<String>>>,
    extension_events: Arc<ExtensionEventBus>,
}

struct ExtensionEventBus {
    sender: ExtensionEventSender,
    receiver: Mutex<ExtensionEventReceiver>,
}

impl Default for ControlPlane {
    fn default() -> Self {
        let (sender, receiver) = event_queue();
        Self {
            state: SharedControlState::default(),
            instance_scope: Arc::new(Mutex::new(None)),
            extension_events: Arc::new(ExtensionEventBus {
                sender,
                receiver: Mutex::new(receiver),
            }),
        }
    }
}

impl ControlPlane {
    pub fn extension_event_sender(&self) -> ExtensionEventSender {
        self.extension_events.sender.clone()
    }

    pub(crate) fn process_extension_events(&self, catalog: &ExtensionCatalog) {
        let Ok(receiver) = self.extension_events.receiver.lock() else {
            return;
        };
        for _ in 0..32 {
            let Ok(request) = receiver.try_recv() else {
                break;
            };
            let result = if request.cancellation.is_cancelled() {
                Err("extension event was cancelled".to_owned())
            } else if Instant::now() >= request.deadline {
                Err("extension event deadline expired".to_owned())
            } else {
                self.publish_scoped(
                    catalog,
                    request.identity.as_str(),
                    request.generation,
                    &request.topic,
                    &request.payload,
                )
            };
            let _ = request.response.send(result);
        }
    }

    fn publish_scoped(
        &self,
        catalog: &ExtensionCatalog,
        module: &str,
        generation: u64,
        topic: &str,
        payload: &Value,
    ) -> Result<(), String> {
        let scope = self
            .instance_scope
            .lock()
            .map_err(|_| "control plane scope is unavailable".to_owned())?
            .clone()
            .ok_or_else(|| "control plane is not bound to an instance".to_owned())?;
        catalog.with_active_topic(module, generation, topic, || {
            lock_control_state(&self.state).publish_event(
                &scope,
                topic,
                &json!({"extension": module, "generation": generation}),
                &Value::Null,
                payload,
            );
        })
    }
}
