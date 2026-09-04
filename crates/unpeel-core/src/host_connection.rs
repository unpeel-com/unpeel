//! Controller-side connection contract shared by direct, SSH, and Link
//! transports.
//!
//! A transport never guesses replay safety from the HTTP-shaped method. Some
//! legacy GET routes rotate credentials, so every call declares whether it is
//! a read or an effect. Process loss fails the call; the semantic backend may
//! then bootstrap again and resume a read from its committed cursor, while an
//! effect with a lost receipt remains outcome-unknown.

use std::fmt;
use std::time::Duration;

use crate::relay_wire::TunnelRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestSemantics {
    ReadOnly,
    Effect,
}

/// Opaque, connection-owned transport generation. A semantic backend obtains
/// this from a successful bootstrap reply and binds later calls to it. If the
/// transport has reconnected, a bound call fails before sending any bytes
/// instead of silently becoming the first effect on a new connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionGeneration {
    pub(crate) connection_id: uuid::Uuid,
    pub(crate) sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCall {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub semantics: RequestSemantics,
}

impl HostCall {
    pub fn new(
        method: impl Into<String>,
        path: impl Into<String>,
        semantics: RequestSemantics,
    ) -> Self {
        Self {
            method: method.into(),
            path: path.into(),
            query: Vec::new(),
            content_type: None,
            body: Vec::new(),
            semantics,
        }
    }

    pub fn with_query(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    pub fn with_body(mut self, content_type: impl Into<String>, body: Vec<u8>) -> Self {
        self.content_type = Some(content_type.into());
        self.body = body;
        self
    }
}

/// A connection-owned, one-use call. Its id is allocated before transport
/// work starts, cannot be changed by a caller, and cannot be accidentally
/// reused after a replay-cache response or an ambiguous disconnect.
#[derive(Debug)]
pub struct PreparedHostCall {
    pub(crate) connection_id: uuid::Uuid,
    pub(crate) request_id: u64,
    pub(crate) required_generation: Option<ConnectionGeneration>,
    pub(crate) call: HostCall,
}

impl PreparedHostCall {
    pub fn request_id(&self) -> u64 {
        self.request_id
    }

    pub fn semantics(&self) -> RequestSemantics {
        self.call.semantics
    }

    pub(crate) fn into_tunnel(self) -> TunnelRequest {
        TunnelRequest {
            id: self.request_id,
            method: self.call.method,
            path: self.call.path,
            query: self.call.query,
            // SSH derives owner authority from the remote Unix account.
            // Direct and Link implementations attach their credentials at
            // their own transport boundary instead of accepting wire auth
            // from the semantic caller.
            auth: None,
            content_type: self.call.content_type,
            body: self.call.body,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReply {
    pub request_id: u64,
    pub generation: ConnectionGeneration,
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryState {
    /// The transport failed before this call wrote any frame bytes.
    NotSent,
    /// Some or all frame bytes reached the transport, so an effect may have
    /// landed even though no correlated response was received.
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostConnectionError {
    InvalidTarget(String),
    Configuration(String),
    Closed,
    ClosedRequest(u64),
    RequestIdExhausted,
    WrongConnection(u64),
    WrongGeneration(ConnectionGeneration),
    GenerationChanged {
        request_id: u64,
        expected: ConnectionGeneration,
    },
    RequestTooLarge {
        request_id: u64,
        encoded_bytes: usize,
        max_bytes: usize,
    },
    TooManyInFlight {
        request_id: u64,
        limit: usize,
    },
    DuplicateRequestId(u64),
    Launch {
        request_id: u64,
        message: String,
    },
    Disconnected {
        request_id: u64,
        semantics: RequestSemantics,
        delivery: DeliveryState,
        message: String,
    },
    TimedOut {
        request_id: u64,
        semantics: RequestSemantics,
        delivery: DeliveryState,
    },
}

impl HostConnectionError {
    pub fn delivery(&self) -> Option<DeliveryState> {
        match self {
            Self::Closed
            | Self::ClosedRequest(_)
            | Self::GenerationChanged { .. }
            | Self::WrongConnection(_)
            | Self::WrongGeneration(_)
            | Self::DuplicateRequestId(_)
            | Self::RequestTooLarge { .. }
            | Self::TooManyInFlight { .. }
            | Self::Launch { .. } => Some(DeliveryState::NotSent),
            Self::Disconnected { delivery, .. } | Self::TimedOut { delivery, .. } => {
                Some(*delivery)
            }
            Self::InvalidTarget(_) | Self::Configuration(_) | Self::RequestIdExhausted => None,
        }
    }

    pub fn effect_outcome_is_unknown(&self) -> bool {
        matches!(
            self,
            Self::Disconnected {
                semantics: RequestSemantics::Effect,
                delivery: DeliveryState::OutcomeUnknown,
                ..
            } | Self::TimedOut {
                semantics: RequestSemantics::Effect,
                delivery: DeliveryState::OutcomeUnknown,
                ..
            }
        )
    }
}

impl fmt::Display for HostConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(message) => write!(formatter, "invalid Host target: {message}"),
            Self::Configuration(message) => write!(formatter, "Host connection: {message}"),
            Self::Closed => write!(formatter, "Host connection is closed"),
            Self::ClosedRequest(id) => {
                write!(
                    formatter,
                    "Host connection closed before request {id} was sent"
                )
            }
            Self::RequestIdExhausted => write!(formatter, "Host request id space is exhausted"),
            Self::WrongConnection(id) => {
                write!(
                    formatter,
                    "request {id} belongs to a different Host connection"
                )
            }
            Self::WrongGeneration(_) => {
                write!(formatter, "Host generation belongs to a different connection")
            }
            Self::GenerationChanged {
                request_id,
                expected: _,
            } => write!(
                formatter,
                "Host request {request_id} was not sent: its connection generation is no longer current"
            ),
            Self::RequestTooLarge {
                request_id,
                encoded_bytes,
                max_bytes,
            } => write!(
                formatter,
                "Host request {request_id} is {encoded_bytes} bytes (maximum {max_bytes})"
            ),
            Self::TooManyInFlight { request_id, limit } => write!(
                formatter,
                "Host request {request_id} was not sent: {limit} calls are already in flight"
            ),
            Self::DuplicateRequestId(id) => {
                write!(formatter, "request id {id} is already in flight")
            }
            Self::Launch {
                request_id,
                message,
            } => write!(
                formatter,
                "could not open Host transport for request {request_id}: {message}"
            ),
            Self::Disconnected {
                request_id,
                delivery,
                message,
                ..
            } => write!(
                formatter,
                "Host connection lost for request {request_id} ({delivery:?}): {message}"
            ),
            Self::TimedOut {
                request_id,
                delivery,
                ..
            } => write!(
                formatter,
                "Host request {request_id} timed out ({delivery:?})"
            ),
        }
    }
}

impl std::error::Error for HostConnectionError {}

/// One transport-neutral Controller connection. Preparing allocates a stable,
/// connection-owned id without starting I/O; requesting consumes it exactly
/// once. Semantic non-2xx statuses are successful transport replies.
pub trait HostConnection: Send + Sync {
    /// Prepare an unconstrained call. Semantic backends use this for
    /// bootstrap, which is allowed to open a fresh transport generation.
    fn prepare(&self, call: HostCall) -> Result<PreparedHostCall, HostConnectionError>;

    /// Prepare a call that may run only on the generation which produced the
    /// backend's last accepted bootstrap. A generation change is reported as
    /// `NotSent`; the transport must never reconnect to satisfy this call.
    fn prepare_in_generation(
        &self,
        generation: ConnectionGeneration,
        call: HostCall,
    ) -> Result<PreparedHostCall, HostConnectionError>;

    fn request(
        &self,
        call: PreparedHostCall,
        timeout: Duration,
    ) -> Result<HostReply, HostConnectionError>;

    fn disconnect(&self);
}
