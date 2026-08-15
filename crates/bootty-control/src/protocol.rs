use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u32 = 1;
pub(crate) const REQUEST_LIMIT: u64 = 1024 * 1024;
pub(crate) const RPC_ID_LIMIT: usize = 4096;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const MAX_CONNECTIONS: usize = 32;
pub(crate) const MAX_TASKS: usize = 64;
pub(crate) const MAX_SUBSCRIPTIONS: usize = 64;
pub(crate) const MAX_TOPICS_PER_SUBSCRIPTION: usize = 16;
pub(crate) const EVENT_QUEUE_LIMIT: usize = 64;
pub(crate) const EVENT_TOPIC_LIMIT: usize = 128;
pub(crate) const TASK_WAIT_INTERVAL: Duration = Duration::from_millis(50);
pub(crate) const COMMAND_COMPLETED_TOPIC: &str = "command.completed";

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcResponse {
    pub(crate) fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn error(
        id: Value,
        code: i32,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data,
            }),
        }
    }
}

impl RpcError {
    pub(crate) fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

pub(crate) fn negotiate_protocol(params: &Value) -> Result<Value, RpcError> {
    let minimum = params
        .get("minimum_protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(PROTOCOL_VERSION));
    let maximum = params
        .get("maximum_protocol_version")
        .and_then(Value::as_u64)
        .unwrap_or(u64::from(PROTOCOL_VERSION));
    let version = u64::from(PROTOCOL_VERSION);
    if minimum > version || maximum < version || minimum > maximum {
        let mut error = RpcError::new(-32007, "no compatible protocol version");
        error.data = Some(json!({
            "server_minimum": PROTOCOL_VERSION,
            "server_maximum": PROTOCOL_VERSION,
            "client_minimum": minimum,
            "client_maximum": maximum
        }));
        return Err(error);
    }
    Ok(json!({
        "protocol_version": PROTOCOL_VERSION,
        "minimum_protocol_version": PROTOCOL_VERSION,
        "maximum_protocol_version": PROTOCOL_VERSION
    }))
}

pub(crate) fn internal_error(error: &serde_json::Error) -> RpcError {
    RpcError::new(-32603, error.to_string())
}
