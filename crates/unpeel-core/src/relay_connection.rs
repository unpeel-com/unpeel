//! Controller-side Link transport backed by the shipped Apple Relay client.
//!
//! The native bridge supplies an executor which owns the Swift
//! `RemoteRelayConnection` (and therefore the one canonical WebSocket/E2E
//! implementation). This module contributes only the transport-neutral
//! [`HostConnection`] rules: stable request ids, generation-bound effects,
//! delivery certainty, and bearer injection at the transport boundary.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::host_connection::{
    ConnectionGeneration, DeliveryState, HostCall, HostConnection, HostConnectionError, HostReply,
    PreparedHostCall, RequestSemantics,
};
use crate::relay_wire;

/// One response from the canonical Relay client plus the exact encrypted
/// socket generation which carried it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayTransportReply {
    pub connection_generation: u64,
    pub encoded_response: Vec<u8>,
}

/// Failure certainty reported by the canonical Relay client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayTransportError {
    GenerationChanged,
    Disconnected {
        delivery: DeliveryState,
        message: String,
    },
    TimedOut {
        delivery: DeliveryState,
    },
}

impl fmt::Display for RelayTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationChanged => write!(formatter, "Link connection generation changed"),
            Self::Disconnected { message, .. } => write!(formatter, "{message}"),
            Self::TimedOut { .. } => write!(formatter, "Link request timed out"),
        }
    }
}

/// Synchronous boundary used by Rust's semantic backend. Native callers run
/// `HostConnection::request` away from the UI actor; their executor may wait
/// for the shared Swift actor without blocking the main thread.
pub trait RelayRequestExecutor: Send + Sync {
    fn request(
        &self,
        encoded_request: &[u8],
        required_connection_generation: Option<u64>,
        timeout: Duration,
    ) -> Result<RelayTransportReply, RelayTransportError>;

    fn disconnect(&self);
}

/// A Link carrier for the same `RemoteSessionBackend` used by SSH and Direct.
pub struct RelayHostConnection {
    connection_id: uuid::Uuid,
    auth_token: String,
    executor: Arc<dyn RelayRequestExecutor>,
    closed: AtomicBool,
    current_generation: Mutex<Option<u64>>,
    next_request_id: AtomicU64,
}

impl RelayHostConnection {
    pub fn new(
        auth_token: impl Into<String>,
        executor: Arc<dyn RelayRequestExecutor>,
    ) -> Result<Self, HostConnectionError> {
        let auth_token = auth_token.into();
        if auth_token.is_empty()
            || auth_token.len() > 4096
            || auth_token.bytes().any(|byte| {
                !byte.is_ascii() || byte.is_ascii_whitespace() || byte.is_ascii_control()
            })
        {
            return Err(HostConnectionError::Configuration(
                "Link Host bearer token is empty or malformed".to_owned(),
            ));
        }
        Ok(Self {
            connection_id: uuid::Uuid::new_v4(),
            auth_token,
            executor,
            closed: AtomicBool::new(false),
            current_generation: Mutex::new(None),
            next_request_id: AtomicU64::new(1),
        })
    }

    fn allocate_request_id(&self) -> Result<u64, HostConnectionError> {
        self.next_request_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| HostConnectionError::RequestIdExhausted)
    }

    fn generation_token(&self, sequence: u64) -> ConnectionGeneration {
        ConnectionGeneration {
            connection_id: self.connection_id,
            sequence,
        }
    }

    fn current_generation(&self) -> Option<u64> {
        *self
            .current_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn validate_call(&self, call: &HostCall) -> Result<(), HostConnectionError> {
        if !matches!(call.method.as_str(), "GET" | "POST") {
            return Err(HostConnectionError::Configuration(
                "Link Host calls support only GET and POST".to_owned(),
            ));
        }
        if !call.path.starts_with("/mobile/") || call.path == "/mobile/pair" {
            return Err(HostConnectionError::Configuration(
                "Link Host call is outside the paired /mobile scope".to_owned(),
            ));
        }
        let route = call.path.trim_start_matches("/mobile/");
        if route.is_empty()
            || route
                .split('/')
                .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
            || call.path.bytes().any(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~'))
            })
        {
            return Err(HostConnectionError::Configuration(
                "Link Host call contains an invalid path".to_owned(),
            ));
        }
        if let Some(content_type) = &call.content_type {
            if content_type.is_empty()
                || content_type.len() > 256
                || content_type
                    .bytes()
                    .any(|byte| !byte.is_ascii() || byte.is_ascii_control())
            {
                return Err(HostConnectionError::Configuration(
                    "Link Host call contains an invalid content type".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn invalidate(&self, generation: Option<u64>) {
        let mut current = self
            .current_generation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.is_none() || *current == generation {
            *current = None;
            drop(current);
            self.executor.disconnect();
        }
    }

    pub fn disconnect(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.invalidate(None);
    }
}

impl HostConnection for RelayHostConnection {
    fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        self.validate_call(&call)?;
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id: self.allocate_request_id()?,
            required_generation: None,
            call,
        })
    }

    fn prepare_in_generation(
        &self,
        generation: ConnectionGeneration,
        call: HostCall,
    ) -> Result<PreparedHostCall, HostConnectionError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::Closed);
        }
        self.validate_call(&call)?;
        if generation.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongGeneration(generation));
        }
        let request_id = self.allocate_request_id()?;
        if self.current_generation() != Some(generation.sequence) {
            return Err(HostConnectionError::GenerationChanged {
                request_id,
                expected: generation,
            });
        }
        Ok(PreparedHostCall {
            connection_id: self.connection_id,
            request_id,
            required_generation: Some(generation),
            call,
        })
    }

    fn request(
        &self,
        call: PreparedHostCall,
        timeout: Duration,
    ) -> Result<HostReply, HostConnectionError> {
        let request_id = call.request_id;
        let semantics = call.call.semantics;
        let required_generation = call.required_generation;
        if call.connection_id != self.connection_id {
            return Err(HostConnectionError::WrongConnection(request_id));
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(HostConnectionError::ClosedRequest(request_id));
        }
        if let Some(expected) = required_generation {
            if self.current_generation() != Some(expected.sequence) {
                return Err(HostConnectionError::GenerationChanged {
                    request_id,
                    expected,
                });
            }
        }

        let mut request = call.into_tunnel();
        request.auth = Some(format!("Bearer {}", self.auth_token));
        let encoded_request = relay_wire::encode_tunnel_request(&request);
        if !relay_wire::plaintext_frame_fits(encoded_request.len()) {
            return Err(HostConnectionError::RequestTooLarge {
                request_id,
                encoded_bytes: encoded_request.len(),
                max_bytes: relay_wire::MAX_PLAINTEXT_BYTES,
            });
        }

        let required_sequence = required_generation.map(|generation| generation.sequence);
        let transport = match self
            .executor
            .request(&encoded_request, required_sequence, timeout)
        {
            Ok(reply) => reply,
            Err(RelayTransportError::GenerationChanged) => {
                self.invalidate(required_sequence);
                return Err(HostConnectionError::GenerationChanged {
                    request_id,
                    expected: required_generation
                        .unwrap_or_else(|| self.generation_token(required_sequence.unwrap_or(0))),
                });
            }
            Err(RelayTransportError::Disconnected { delivery, message }) => {
                self.invalidate(required_sequence);
                return Err(HostConnectionError::Disconnected {
                    request_id,
                    semantics,
                    delivery,
                    message,
                });
            }
            Err(RelayTransportError::TimedOut { delivery }) => {
                self.invalidate(required_sequence);
                return Err(HostConnectionError::TimedOut {
                    request_id,
                    semantics,
                    delivery,
                });
            }
        };

        if transport.connection_generation == 0 {
            self.invalidate(required_sequence);
            return Err(HostConnectionError::Disconnected {
                request_id,
                semantics,
                delivery: delivery_after_dispatch(semantics),
                message: "Link returned an invalid connection generation".to_owned(),
            });
        }
        let response = match relay_wire::parse_tunnel_response(&transport.encoded_response) {
            Ok(response) => response,
            Err(message) => {
                self.invalidate(required_sequence);
                return Err(HostConnectionError::Disconnected {
                    request_id,
                    semantics,
                    delivery: delivery_after_dispatch(semantics),
                    message: format!("invalid Link response: {message}"),
                });
            }
        };

        if required_generation.is_none() {
            *self
                .current_generation
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(transport.connection_generation);
        }
        Ok(HostReply {
            request_id: response.id,
            generation: self.generation_token(transport.connection_generation),
            status: response.status,
            body: response.body,
        })
    }

    fn disconnect(&self) {
        RelayHostConnection::disconnect(self);
    }
}

impl Drop for RelayHostConnection {
    fn drop(&mut self) {
        self.disconnect();
    }
}

fn delivery_after_dispatch(semantics: RequestSemantics) -> DeliveryState {
    match semantics {
        RequestSemantics::ReadOnly => DeliveryState::OutcomeUnknown,
        RequestSemantics::Effect => DeliveryState::OutcomeUnknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct ScriptedExecutor {
        replies: Mutex<VecDeque<Result<RelayTransportReply, RelayTransportError>>>,
        calls: Mutex<Vec<(relay_wire::TunnelRequest, Option<u64>, Duration)>>,
        disconnects: AtomicU64,
    }

    impl RelayRequestExecutor for ScriptedExecutor {
        fn request(
            &self,
            encoded_request: &[u8],
            required_connection_generation: Option<u64>,
            timeout: Duration,
        ) -> Result<RelayTransportReply, RelayTransportError> {
            let request = relay_wire::parse_tunnel_request_strict(encoded_request).unwrap();
            self.calls
                .lock()
                .unwrap()
                .push((request, required_connection_generation, timeout));
            self.replies.lock().unwrap().pop_front().unwrap()
        }

        fn disconnect(&self) {
            self.disconnects.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn reply(id: u64, generation: u64, status: u16, body: &[u8]) -> RelayTransportReply {
        RelayTransportReply {
            connection_generation: generation,
            encoded_response: relay_wire::encode_tunnel_response(id, status, body),
        }
    }

    #[test]
    fn bootstrap_opens_generation_and_bound_effect_reuses_it_with_bearer() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().extend([
            Ok(reply(1, 41, 200, b"{}")),
            Ok(reply(2, 41, 200, br#"{"ok":true}"#)),
        ]);
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();

        let bootstrap = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(bootstrap.generation.sequence, 41);

        let effect = connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect)
                    .with_body("application/json", br#"{"data":"x"}"#.to_vec()),
            )
            .unwrap();
        let effect = connection.request(effect, Duration::from_secs(35)).unwrap();
        assert_eq!(effect.request_id, 2);
        let calls = executor.calls.lock().unwrap();
        assert_eq!(calls[0].1, None);
        assert_eq!(calls[1].1, Some(41));
        assert_eq!(calls[1].0.auth.as_deref(), Some("Bearer paired-secret"));
        assert_eq!(calls[1].2, Duration::from_secs(35));
    }

    #[test]
    fn semantic_status_is_a_correlated_reply_not_a_transport_failure() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().push_back(Ok(reply(
            1,
            9,
            409,
            br#"{"error":"conflict"}"#,
        )));
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();
        let response = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(response.status, 409);
        assert_eq!(response.generation.sequence, 9);
        assert_eq!(executor.disconnects.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn bound_generation_change_is_proven_not_sent() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().extend([
            Ok(reply(1, 7, 200, b"{}")),
            Err(RelayTransportError::GenerationChanged),
        ]);
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();
        let bootstrap = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();
        let call = connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
            )
            .unwrap();
        let error = connection
            .request(call, Duration::from_secs(10))
            .unwrap_err();
        assert!(matches!(
            error,
            HostConnectionError::GenerationChanged { .. }
        ));
    }

    #[test]
    fn post_send_timeout_preserves_outcome_unknown() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().extend([
            Ok(reply(1, 5, 200, b"{}")),
            Err(RelayTransportError::TimedOut {
                delivery: DeliveryState::OutcomeUnknown,
            }),
        ]);
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();
        let bootstrap = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();
        let call = connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
            )
            .unwrap();
        let error = connection
            .request(call, Duration::from_secs(10))
            .unwrap_err();
        assert!(error.effect_outcome_is_unknown());
        assert_eq!(executor.disconnects.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pre_send_timeout_preserves_not_sent_and_invalidates_the_generation() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().extend([
            Ok(reply(1, 5, 200, b"{}")),
            Err(RelayTransportError::TimedOut {
                delivery: DeliveryState::NotSent,
            }),
        ]);
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();
        let bootstrap = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();
        let call = connection
            .prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
            )
            .unwrap();
        let error = connection
            .request(call, Duration::from_secs(35))
            .unwrap_err();
        assert_eq!(error.delivery(), Some(DeliveryState::NotSent));
        assert!(!error.effect_outcome_is_unknown());
        assert_eq!(executor.disconnects.load(Ordering::Relaxed), 1);
        assert!(matches!(
            connection.prepare_in_generation(
                bootstrap.generation,
                HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
            ),
            Err(HostConnectionError::GenerationChanged { .. })
        ));
    }

    #[test]
    fn correlated_effect_rejection_keeps_the_generation_callable() {
        let executor = Arc::new(ScriptedExecutor::default());
        executor.replies.lock().unwrap().extend([
            Ok(reply(1, 12, 200, b"{}")),
            Ok(reply(2, 12, 503, br#"{"error":"busy"}"#)),
            Ok(reply(3, 12, 200, br#"{"ok":true}"#)),
        ]);
        let connection = RelayHostConnection::new(
            "paired-secret",
            Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
        )
        .unwrap();
        let bootstrap = connection
            .request(
                connection
                    .prepare(HostCall::new(
                        "GET",
                        "/mobile/bootstrap",
                        RequestSemantics::ReadOnly,
                    ))
                    .unwrap(),
                Duration::from_secs(10),
            )
            .unwrap();

        let rejected = connection
            .request(
                connection
                    .prepare_in_generation(
                        bootstrap.generation,
                        HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
                    )
                    .unwrap(),
                Duration::from_secs(35),
            )
            .unwrap();
        assert_eq!(rejected.status, 503);
        assert_eq!(executor.disconnects.load(Ordering::Relaxed), 0);

        let accepted = connection
            .request(
                connection
                    .prepare_in_generation(
                        bootstrap.generation,
                        HostCall::new("POST", "/mobile/write", RequestSemantics::Effect),
                    )
                    .unwrap(),
                Duration::from_secs(35),
            )
            .unwrap();
        assert_eq!(accepted.status, 200);
        assert_eq!(accepted.generation, bootstrap.generation);
    }

    #[test]
    fn malformed_bearers_are_rejected_without_secret_echo() {
        for secret in ["", "secret with spaces", "secret\nwith-newline"] {
            let executor = Arc::new(ScriptedExecutor::default());
            let error = RelayHostConnection::new(
                secret,
                Arc::clone(&executor) as Arc<dyn RelayRequestExecutor>,
            )
            .err()
            .expect("malformed bearer must be rejected");
            let rendered = format!("{error:?}: {error}");
            if !secret.is_empty() {
                assert!(!rendered.contains(secret));
            }
            assert_eq!(executor.calls.lock().unwrap().len(), 0);
            assert_eq!(executor.disconnects.load(Ordering::Relaxed), 0);
        }
    }
}
