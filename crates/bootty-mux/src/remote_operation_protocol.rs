use std::io::Write;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::operation::MuxBackendOperationError;
#[cfg(feature = "app")]
use crate::ssh::remote_daemon_failure;

const REMOTE_OPERATION_PROTOCOL_VERSION: u8 = 1;
const MAX_REMOTE_OPERATION_COMPLETION: usize = 1024 * 1024;
const MAX_REMOTE_OPERATION_ERROR: usize = 16 * 1024;
const MAX_REMOTE_OPERATION_ERROR_MESSAGE: usize = 1024;
const TRUNCATED_ERROR_SUFFIX: &str = "...";

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RemoteOperationStatus {
    Success,
    Error,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteOperationCompletionEnvelope<T> {
    version: u8,
    status: RemoteOperationStatus,
    completion: Option<T>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RemoteOperationErrorEnvelope {
    version: u8,
    status: RemoteOperationStatus,
    error: RemoteOperationError,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "message")]
enum RemoteOperationError {
    Unsupported(String),
    Unavailable(String),
    Denied(String),
    Stale(String),
    Failed(String),
}

impl RemoteOperationError {
    fn from_error(error: &anyhow::Error) -> Self {
        let backend_error = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<MuxBackendOperationError>());
        match backend_error {
            Some(MuxBackendOperationError::Unsupported(message)) => {
                Self::Unsupported(bounded_error_message(message))
            }
            Some(MuxBackendOperationError::Unavailable(message)) => {
                Self::Unavailable(bounded_error_message(message))
            }
            Some(MuxBackendOperationError::Denied(message)) => {
                Self::Denied(bounded_error_message(message))
            }
            Some(MuxBackendOperationError::Stale(message)) => {
                Self::Stale(bounded_error_message(message))
            }
            Some(MuxBackendOperationError::Failed(message)) => {
                Self::Failed(bounded_error_message(message))
            }
            None => Self::Failed(bounded_error_message(&error.to_string())),
        }
    }

    #[cfg(feature = "app")]
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Unsupported(message) => MuxBackendOperationError::Unsupported(message).into(),
            Self::Unavailable(message) => MuxBackendOperationError::Unavailable(message).into(),
            Self::Denied(message) => MuxBackendOperationError::Denied(message).into(),
            Self::Stale(message) => MuxBackendOperationError::Stale(message).into(),
            Self::Failed(message) => MuxBackendOperationError::Failed(message).into(),
        }
    }
}

fn bounded_error_message(message: &str) -> String {
    if message.len() <= MAX_REMOTE_OPERATION_ERROR_MESSAGE {
        return message.to_owned();
    }

    let mut end = MAX_REMOTE_OPERATION_ERROR_MESSAGE - TRUNCATED_ERROR_SUFFIX.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = message[..end].to_owned();
    bounded.push_str(TRUNCATED_ERROR_SUFFIX);
    bounded
}

fn encode_remote_operation_completion<T: Serialize>(completion: Option<T>) -> Result<String> {
    let payload = serde_json::to_string(&RemoteOperationCompletionEnvelope {
        version: REMOTE_OPERATION_PROTOCOL_VERSION,
        status: RemoteOperationStatus::Success,
        completion,
    })
    .context("encode remote operation completion")?;
    if payload.len() > MAX_REMOTE_OPERATION_COMPLETION {
        bail!("remote operation completion is too large")
    }
    Ok(payload)
}

fn encode_remote_operation_error(error: &anyhow::Error) -> Result<String> {
    let payload = serde_json::to_string(&RemoteOperationErrorEnvelope {
        version: REMOTE_OPERATION_PROTOCOL_VERSION,
        status: RemoteOperationStatus::Error,
        error: RemoteOperationError::from_error(error),
    })
    .context("encode remote operation error")?;
    if payload.len() <= MAX_REMOTE_OPERATION_ERROR {
        return Ok(payload);
    }

    serde_json::to_string(&RemoteOperationErrorEnvelope {
        version: REMOTE_OPERATION_PROTOCOL_VERSION,
        status: RemoteOperationStatus::Error,
        error: RemoteOperationError::Failed(
            "remote operation error exceeded protocol limit".to_owned(),
        ),
    })
    .context("encode bounded remote operation error")
}

pub fn write_remote_operation_completion<T: Serialize>(completion: Option<T>) -> Result<()> {
    let payload = encode_remote_operation_completion(completion)?;
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    stdout
        .write_all(payload.as_bytes())
        .context("write remote operation completion")?;
    stdout
        .write_all(b"\n")
        .context("terminate remote operation completion")?;
    stdout.flush().context("flush remote operation completion")
}

pub fn write_remote_operation_error(error: &anyhow::Error) -> Result<()> {
    let payload = encode_remote_operation_error(error)?;
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    stderr
        .write_all(payload.as_bytes())
        .context("write remote operation error")?;
    stderr
        .write_all(b"\n")
        .context("terminate remote operation error")?;
    stderr.flush().context("flush remote operation error")
}

pub(crate) fn decode_remote_operation_completion<T: DeserializeOwned>(
    payload: &str,
) -> Result<Option<T>> {
    if payload.len() > MAX_REMOTE_OPERATION_COMPLETION {
        bail!("remote operation completion is too large")
    }
    let envelope: RemoteOperationCompletionEnvelope<T> =
        serde_json::from_str(payload).context("decode remote operation completion")?;
    if envelope.version != REMOTE_OPERATION_PROTOCOL_VERSION
        || envelope.status != RemoteOperationStatus::Success
    {
        bail!("unsupported remote operation completion envelope")
    }
    Ok(envelope.completion)
}

#[cfg(feature = "app")]
pub(crate) fn remote_operation_failure(host: &str, detail: &str) -> anyhow::Error {
    decode_remote_operation_error(detail)
        .unwrap_or_else(|| anyhow::Error::msg(remote_daemon_failure(host, detail)))
}

#[cfg(feature = "app")]
fn decode_remote_operation_error(detail: &str) -> Option<anyhow::Error> {
    let detail = detail.trim();
    if detail.is_empty() || detail.len() > MAX_REMOTE_OPERATION_ERROR {
        return None;
    }
    let envelope: RemoteOperationErrorEnvelope = serde_json::from_str(detail).ok()?;
    (envelope.version == REMOTE_OPERATION_PROTOCOL_VERSION
        && envelope.status == RemoteOperationStatus::Error)
        .then(|| envelope.error.into_error())
}

#[cfg(all(test, feature = "app"))]
mod tests {
    use super::*;
    use crate::backend::{
        MuxAllocatedResources, MuxAllocatedWindow, MuxBackendCommandCompletion,
        MuxBackendOperationError, MuxEventTarget,
    };

    #[test]
    fn typed_operation_errors_round_trip_through_remote_stderr() {
        let cases = [
            MuxBackendOperationError::Unsupported("unsupported operation".to_owned()),
            MuxBackendOperationError::Unavailable("daemon unavailable".to_owned()),
            MuxBackendOperationError::Denied("policy denied operation".to_owned()),
            MuxBackendOperationError::Stale("pane generation changed".to_owned()),
            MuxBackendOperationError::Failed("backend failed operation".to_owned()),
        ];

        for expected in cases {
            let source: anyhow::Error = expected.clone().into();
            let payload = encode_remote_operation_error(&source).expect("encode error envelope");
            let actual = remote_operation_failure("devbox", &payload);

            assert_eq!(
                actual.downcast_ref::<MuxBackendOperationError>(),
                Some(&expected),
                "{expected:?} must survive the remote error envelope"
            );
        }
    }

    #[test]
    fn malformed_or_legacy_remote_stderr_remains_a_generic_transport_failure() {
        for detail in [
            "legacy daemon failure",
            r#"{"version":1,"status":"error","error":{"kind":"stale"}}"#,
            r#"{"version":2,"status":"error","error":{"kind":"stale","message":"old pane"}}"#,
        ] {
            let error = remote_operation_failure("devbox", detail);
            assert!(
                error.downcast_ref::<MuxBackendOperationError>().is_none(),
                "{detail:?} must not masquerade as a typed backend failure"
            );
            assert!(
                error
                    .to_string()
                    .starts_with("Could not run the Bootty daemon on devbox"),
                "{detail:?} must retain legacy daemon failure handling"
            );
        }
    }

    #[test]
    fn remote_completion_round_trips_exact_recursive_allocation_ids() {
        let expected = Some(MuxBackendCommandCompletion {
            allocated: Some(MuxAllocatedResources {
                session_id: "$41".to_owned(),
                windows: vec![
                    MuxAllocatedWindow {
                        window_id: "@7".to_owned(),
                        pane_ids: vec!["%12".to_owned(), "%13".to_owned(), "%14".to_owned()],
                    },
                    MuxAllocatedWindow {
                        window_id: "@8".to_owned(),
                        pane_ids: vec!["%15".to_owned(), "%16".to_owned()],
                    },
                ],
            }),
            target: Some(MuxEventTarget::session("$41")),
        });

        let payload = encode_remote_operation_completion(expected.clone())
            .expect("encode recursive completion");
        let actual = decode_remote_operation_completion::<MuxBackendCommandCompletion>(&payload)
            .expect("decode recursive completion");

        assert_eq!(actual, expected);
    }

    #[test]
    fn remote_completion_envelopes_are_bounded() {
        assert!(
            encode_remote_operation_completion(Some("x".repeat(MAX_REMOTE_OPERATION_COMPLETION)))
                .is_err()
        );
    }

    #[test]
    fn error_envelopes_bound_long_messages() {
        let source: anyhow::Error =
            MuxBackendOperationError::Failed("x".repeat(MAX_REMOTE_OPERATION_ERROR_MESSAGE * 2))
                .into();

        let payload = encode_remote_operation_error(&source).expect("encode bounded error");
        let decoded = decode_remote_operation_error(&payload).expect("decode bounded error");

        assert!(payload.len() <= MAX_REMOTE_OPERATION_ERROR);
        assert!(matches!(
            decoded.downcast_ref::<MuxBackendOperationError>(),
            Some(MuxBackendOperationError::Failed(message))
                if message.len() <= MAX_REMOTE_OPERATION_ERROR_MESSAGE
                    && message.ends_with(TRUNCATED_ERROR_SUFFIX)
        ));
    }
}
