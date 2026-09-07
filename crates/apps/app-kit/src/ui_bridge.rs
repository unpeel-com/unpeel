//! App-owned, reconnectable UI endpoint for terminal-powered Apps.
//!
//! The terminal App binds a session-scoped Unix socket and remains the state
//! authority. Trusted workspace brokers attach one local connection for each
//! native or remote renderer, so renderer and GUI restarts never determine the
//! lifetime of the App model.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};

use crate::{
    AppInstanceId, AppMetadata, ClientId, EventId, MAX_SAFE_UI_INTEGER, MAX_UI_FRAME_BYTES,
    UI_DELTA_CAPABILITY, UI_PROTOCOL_MAX_VERSION, UI_PROTOCOL_MIN_VERSION, UI_PROTOCOL_NAME,
    UI_SOCKET_ENV, UI_TOKEN_ENV, UiAck, UiAckStatus, UiAttach, UiAttached, UiDelta,
    UiDeltaOperation, UiErrorMessage, UiEvent, UiEventKind, UiGrant, UiLifecycle, UiMessage,
    UiNode, UiParticipant, UiParticipantTokenError, UiParticipantTokenVerifier, UiPresence,
    UiPresenceMember, UiProtocolError, UiRendererMetadata, UiRendererState, UiRequestSnapshot,
    UiSnapshot, UiStateError, UiStateStore, ViewId, decode_ui_frame, encode_ui_frame,
    negotiate_ui_protocol_version,
};

const MAX_CONNECTIONS: usize = 256;
const MAX_FRAMES_PER_POLL: usize = 512;
const MAX_PENDING_WRITE_BYTES: usize = 32 * 1024 * 1024;
const MAX_EVENT_HISTORY: usize = 4096;
const MAX_EVENT_HISTORY_PER_CLIENT: usize = 256;
const EVENT_REPLAY_TTL: Duration = Duration::from_secs(15 * 60);
const ATTACH_DEADLINE: Duration = Duration::from_secs(5);

static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

/// App-facing input accepted from one authenticated renderer connection.
#[derive(Clone, Debug, PartialEq)]
pub enum UiBridgeEvent {
    Attached {
        participant: UiParticipant,
        client_id: ClientId,
        renderer: UiRendererMetadata,
        view_id: ViewId,
        resumed: bool,
    },
    Detached {
        participant: UiParticipant,
        client_id: ClientId,
        renderer: UiRendererMetadata,
        view_id: ViewId,
    },
    Lifecycle {
        participant: UiParticipant,
        client_id: ClientId,
        renderer: UiRendererMetadata,
        view_id: ViewId,
        state: UiRendererState,
    },
    Action {
        participant: UiParticipant,
        event: UiEvent,
    },
}

/// Final App decision for an accepted renderer event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiEventOutcome {
    Applied,
    Rejected(String),
}

/// Standalone-safe App Kit UI endpoint owned by one terminal App process.
pub struct UiBridge {
    app: AppMetadata,
    app_instance_id: AppInstanceId,
    app_session_id: Option<String>,
    participant_tokens: Option<UiParticipantTokenVerifier>,
    state_store: Option<UiStateStore>,
    #[cfg(unix)]
    server: Option<UiServer>,
    #[cfg(not(unix))]
    available: bool,
    views: HashMap<ViewId, Projection>,
    client_views: HashMap<(ClientId, ViewId), Projection>,
    pending: VecDeque<UiBridgeEvent>,
    events: HashMap<(ClientId, EventId), EventRecord>,
    event_order: VecDeque<(ClientId, EventId)>,
}

impl fmt::Debug for UiBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiBridge")
            .field("app", &self.app)
            .field("app_instance_id", &self.app_instance_id)
            .field("app_session_id", &self.app_session_id)
            .field("participant_tokens", &"[REDACTED]")
            .field("state_store", &self.state_store)
            .field("available", &self.is_available())
            .field("views", &self.views)
            .field("client_views", &self.client_views)
            .field("pending", &self.pending.len())
            .field("events", &self.events.len())
            .finish()
    }
}

impl UiBridge {
    /// Binds the Host-provided endpoint, or remains inert in a normal terminal.
    ///
    /// A hosted endpoint requires both [`UI_SOCKET_ENV`] and [`UI_TOKEN_ENV`].
    /// The token is a per-session signing key used to verify scoped participant
    /// credentials minted by the Host; renderers never receive that key. Both
    /// variables are scrubbed before this function returns so subsequently
    /// spawned children cannot inherit the endpoint credential.
    ///
    /// Call this during single-threaded App startup. Rust 2024 environment
    /// mutation cannot be soundly synchronized with arbitrary foreign code.
    pub fn detect(app: AppMetadata) -> Result<Self, UiBridgeError> {
        let path = crate::process_security::take_var_os(UI_SOCKET_ENV);
        let token = crate::process_security::take_var(UI_TOKEN_ENV);
        let Some(path) = path.filter(|value| !value.is_empty()) else {
            return Ok(Self::disabled(app));
        };
        let token = token.map_err(|_| UiBridgeError::MissingToken)?;
        let path = PathBuf::from(path);
        let app_session_id = std::env::var("UNPEEL_SESSION_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| session_id_from_socket_path(&path));
        Self::listen_for_session(path, token, app_session_id, app)
    }

    /// Binds an explicit app-owned socket with a participant-token signing key.
    ///
    /// The App Session id is inferred from the socket's parent directory. Hosts
    /// with another layout should call [`Self::listen_for_session`].
    pub fn listen(
        path: impl AsRef<Path>,
        signing_key: impl Into<String>,
        app: AppMetadata,
    ) -> Result<Self, UiBridgeError> {
        let path = path.as_ref();
        let app_session_id = session_id_from_socket_path(path);
        Self::listen_for_session(path, signing_key, app_session_id, app)
    }

    /// Binds an explicit endpoint and participant-token audience.
    pub fn listen_for_session(
        path: impl AsRef<Path>,
        signing_key: impl Into<String>,
        app_session_id: impl Into<String>,
        app: AppMetadata,
    ) -> Result<Self, UiBridgeError> {
        let signing_key = signing_key.into();
        if signing_key.is_empty() {
            return Err(UiBridgeError::MissingToken);
        }
        let app_session_id = app_session_id.into();
        let participant_tokens =
            UiParticipantTokenVerifier::new(signing_key.as_bytes(), app_session_id.clone())?;
        #[cfg(unix)]
        {
            let path = path.as_ref();
            let state_store =
                UiStateStore::beside_socket(path, app.id.clone(), app.version.clone())?;
            let server = UiServer::bind(path)?;
            Ok(Self {
                app,
                app_instance_id: new_app_instance_id(),
                app_session_id: Some(app_session_id),
                participant_tokens: Some(participant_tokens),
                state_store: Some(state_store),
                server: Some(server),
                views: HashMap::new(),
                client_views: HashMap::new(),
                pending: VecDeque::new(),
                events: HashMap::new(),
                event_order: VecDeque::new(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            let _ = participant_tokens;
            Err(UiBridgeError::UnsupportedPlatform)
        }
    }

    /// Creates an inert bridge for an ordinary terminal run.
    #[must_use]
    pub fn disabled(app: AppMetadata) -> Self {
        Self {
            app,
            app_instance_id: new_app_instance_id(),
            app_session_id: None,
            participant_tokens: None,
            state_store: None,
            #[cfg(unix)]
            server: None,
            #[cfg(not(unix))]
            available: false,
            views: HashMap::new(),
            client_views: HashMap::new(),
            pending: VecDeque::new(),
            events: HashMap::new(),
            event_order: VecDeque::new(),
        }
    }

    /// Identity that stays stable for the lifetime of this terminal process.
    #[must_use]
    pub fn app_instance_id(&self) -> &AppInstanceId {
        &self.app_instance_id
    }

    /// Host Session whose scoped participant tokens this endpoint accepts.
    #[must_use]
    pub fn app_session_id(&self) -> Option<&str> {
        self.app_session_id.as_deref()
    }

    /// Crash-safe state file next to `ui.sock` when running under a Host.
    #[must_use]
    pub fn state_store(&self) -> Option<&UiStateStore> {
        self.state_store.as_ref()
    }

    /// Whether the App successfully owns a renderer attachment endpoint.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        #[cfg(unix)]
        {
            self.server.is_some()
        }
        #[cfg(not(unix))]
        {
            self.available
        }
    }

    /// Bound endpoint path when hosted.
    #[must_use]
    pub fn socket_path(&self) -> Option<&Path> {
        #[cfg(unix)]
        {
            self.server
                .as_ref()
                .map(|server| server.guard.path.as_path())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Whether at least one visible projection still needs Ratatui output.
    ///
    /// When this becomes `false`, the App should stop calling `Terminal::draw`
    /// and wait on model/UI events. It must not stop the whole process.
    #[must_use]
    pub fn should_render_terminal(&self) -> bool {
        #[cfg(unix)]
        {
            let Some(server) = &self.server else {
                return true;
            };
            let states = server
                .connections
                .iter()
                .filter_map(|connection| connection.attachment.as_ref())
                .map(|attachment| attachment.state);
            should_render_terminal(states)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    /// Publishes one shared projection to every attached client for a view.
    ///
    /// Revisions are immutable and monotonically increasing. A same-revision
    /// retry is accepted only when the component tree is identical.
    pub fn publish(
        &mut self,
        view_id: impl Into<ViewId>,
        revision: u64,
        root: UiNode,
    ) -> Result<usize, UiBridgeError> {
        let view_id = view_id.into();
        validate_projection(revision, &root)?;
        store_projection(&mut self.views, view_id.clone(), revision, root)?;
        self.client_views.retain(|(_, targeted_view), projection| {
            targeted_view != &view_id || projection.revision >= revision
        });
        self.broadcast_view(&view_id)
    }

    /// Applies and publishes a compact shared change from `base_revision`.
    ///
    /// Delta-capable renderers receive the operations only when the bridge
    /// knows their queued state is exactly the base shared projection. Any
    /// renderer that is behind, targeted, or lacks delta support receives the
    /// resulting complete snapshot instead.
    pub fn publish_delta(
        &mut self,
        view_id: impl Into<ViewId>,
        base_revision: u64,
        revision: u64,
        operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        let view_id = view_id.into();
        let Some(previous) = self.views.get(&view_id).cloned() else {
            return Err(UiBridgeError::MissingBaseProjection(view_id));
        };
        validate_delta_base(&view_id, &previous, base_revision, revision, &operations)?;
        let mut root = previous.root;
        root.apply_delta_operations(&operations)
            .map_err(UiProtocolError::InvalidView)?;
        validate_projection(revision, &root)?;
        self.views
            .insert(view_id.clone(), Projection { revision, root });
        self.client_views.retain(|(_, targeted_view), projection| {
            targeted_view != &view_id || projection.revision >= revision
        });
        self.broadcast_shared_delta(&view_id, base_revision, revision, operations)
    }

    /// Publishes a participant-specific projection for focus, selection, or
    /// other state that should not be shared with every collaborator.
    pub fn publish_to(
        &mut self,
        client_id: impl Into<ClientId>,
        view_id: impl Into<ViewId>,
        revision: u64,
        root: UiNode,
    ) -> Result<usize, UiBridgeError> {
        let client_id = client_id.into();
        let view_id = view_id.into();
        validate_projection(revision, &root)?;
        if let Some(shared) = self.views.get(&view_id)
            && revision < shared.revision
        {
            return Err(UiBridgeError::RevisionRegressed {
                view_id,
                previous: shared.revision,
                received: revision,
            });
        }
        store_projection(
            &mut self.client_views,
            (client_id.clone(), view_id.clone()),
            revision,
            root,
        )?;
        self.send_projection_to(&client_id, &view_id)
    }

    /// Applies a compact participant-specific change to the currently resolved
    /// projection for one stable client.
    pub fn publish_delta_to(
        &mut self,
        client_id: impl Into<ClientId>,
        view_id: impl Into<ViewId>,
        base_revision: u64,
        revision: u64,
        operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        let client_id = client_id.into();
        let view_id = view_id.into();
        let Some(previous) = self.projection_for(&client_id, &view_id).cloned() else {
            return Err(UiBridgeError::MissingBaseProjection(view_id));
        };
        validate_delta_base(&view_id, &previous, base_revision, revision, &operations)?;
        let mut root = previous.root;
        root.apply_delta_operations(&operations)
            .map_err(UiProtocolError::InvalidView)?;
        validate_projection(revision, &root)?;
        self.client_views.insert(
            (client_id.clone(), view_id.clone()),
            Projection { revision, root },
        );
        self.send_delta_to(&client_id, &view_id, base_revision, revision, operations)
    }

    /// Polls one accepted attachment, lifecycle transition, action, or detach.
    ///
    /// One call pumps all currently readable clients and queues their inputs,
    /// allowing an App to drain same-revision edits before publishing the next
    /// revision.
    pub fn poll(&mut self) -> Result<Option<UiBridgeEvent>, UiBridgeError> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        #[cfg(unix)]
        if self.server.is_some() {
            self.pump()?;
        }
        Ok(self.pending.pop_front())
    }

    /// Records a final result for one previously accepted idempotent event.
    ///
    /// The acknowledgement is cached and replayed if the renderer reconnects
    /// and resends the same `(clientId, eventId)` pair.
    pub fn acknowledge(
        &mut self,
        event: &UiEvent,
        outcome: UiEventOutcome,
        revision: u64,
    ) -> Result<(), UiBridgeError> {
        if revision > MAX_SAFE_UI_INTEGER {
            return Err(UiBridgeError::InvalidRevision(revision));
        }
        let key = (event.client_id.clone(), event.event_id.clone());
        let Some(record) = self.events.get_mut(&key) else {
            return Err(UiBridgeError::UnknownEvent {
                client_id: event.client_id.clone(),
                event_id: event.event_id.clone(),
            });
        };
        if record.event != *event {
            return Err(UiBridgeError::EventIdentityCollision {
                client_id: event.client_id.clone(),
                event_id: event.event_id.clone(),
            });
        }
        let (status, message) = match outcome {
            UiEventOutcome::Applied => (UiAckStatus::Applied, None),
            UiEventOutcome::Rejected(message) => (UiAckStatus::Rejected, Some(message)),
        };
        let ack = make_ack(event, status, revision, message);
        record.ack = Some(ack.clone());
        record.recorded_at = Instant::now();
        self.send_ack(&ack)?;
        Ok(())
    }

    #[cfg(unix)]
    fn pump(&mut self) -> Result<(), UiBridgeError> {
        self.expire_unattached_connections();
        self.reap_connections()?;
        self.accept_connections()?;

        let connection_ids: Vec<u64> = self
            .server
            .as_ref()
            .expect("checked above")
            .connections
            .iter()
            .map(|connection| connection.id)
            .collect();

        for connection_id in connection_ids {
            let messages = {
                let Some(connection) = self.connection_mut(connection_id) else {
                    continue;
                };
                if connection.flush().is_err() {
                    connection.disconnected = true;
                    Vec::new()
                } else {
                    match connection.read_messages() {
                        Ok(messages) => messages,
                        Err(UiProtocolError::Io(_)) => {
                            connection.disconnected = true;
                            Vec::new()
                        }
                        Err(error) => {
                            let _ = connection.queue(
                                UiErrorMessage::new("invalidMessage", error.to_string()).into(),
                            );
                            connection.close_after_flush = true;
                            Vec::new()
                        }
                    }
                }
            };

            for message in messages {
                self.handle_message(connection_id, message)?;
            }
        }

        let connection_ids: Vec<u64> = self
            .server
            .as_ref()
            .expect("checked above")
            .connections
            .iter()
            .map(|connection| connection.id)
            .collect();
        for connection_id in connection_ids {
            let Some(connection) = self.connection_mut(connection_id) else {
                continue;
            };
            if connection.flush().is_err() {
                connection.disconnected = true;
            }
        }
        self.reap_connections()?;
        Ok(())
    }

    #[cfg(unix)]
    fn accept_connections(&mut self) -> Result<(), UiBridgeError> {
        let server = self.server.as_mut().expect("called only when available");
        loop {
            match server.listener.accept() {
                Ok((stream, _)) => {
                    if server.connections.len() >= MAX_CONNECTIONS {
                        drop(stream);
                        continue;
                    }
                    match crate::process_security::peer_has_current_effective_uid(&stream) {
                        Ok(Some(true) | None) => {}
                        Ok(Some(false)) | Err(_) => {
                            drop(stream);
                            continue;
                        }
                    }
                    if stream.set_nonblocking(true).is_err() {
                        drop(stream);
                        continue;
                    }
                    let id = server.next_connection_id;
                    server.next_connection_id = server.next_connection_id.wrapping_add(1).max(1);
                    server.connections.push(UiConnection::new(id, stream));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
    }

    #[cfg(unix)]
    fn expire_unattached_connections(&mut self) {
        let Some(server) = &mut self.server else {
            return;
        };
        let now = Instant::now();
        for connection in &mut server.connections {
            if connection.attachment.is_none()
                && now.saturating_duration_since(connection.accepted_at) >= ATTACH_DEADLINE
            {
                connection.disconnected = true;
            }
        }
    }

    #[cfg(unix)]
    fn handle_message(
        &mut self,
        connection_id: u64,
        message: UiMessage,
    ) -> Result<(), UiBridgeError> {
        if self
            .connection(connection_id)
            .is_none_or(|connection| connection.disconnected || connection.close_after_flush)
        {
            return Ok(());
        }
        let attached = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.as_ref())
            .cloned();
        if attached.is_none() {
            if let UiMessage::Attach(attach) = message {
                return self.handle_attach(connection_id, attach);
            }
            self.reject_connection(
                connection_id,
                "attachRequired",
                "the first renderer frame must be attach",
            )?;
            return Ok(());
        }
        let attachment = attached.expect("checked above");
        if message_protocol_version(&message) != Some(attachment.protocol_version) {
            self.reject_connection(
                connection_id,
                "protocolVersionMismatch",
                "message protocolVersion differs from the negotiated connection version",
            )?;
            return Ok(());
        }

        match message {
            UiMessage::Event(event) => self.handle_event(connection_id, event),
            UiMessage::Lifecycle(lifecycle) => self.handle_lifecycle(connection_id, lifecycle),
            UiMessage::RequestSnapshot(request) => {
                self.handle_snapshot_request(connection_id, request)
            }
            UiMessage::Attach(_)
            | UiMessage::Attached(_)
            | UiMessage::Snapshot(_)
            | UiMessage::Delta(_)
            | UiMessage::Ack(_)
            | UiMessage::Presence(_)
            | UiMessage::Error(_) => {
                self.reject_connection(
                    connection_id,
                    "unexpectedMessage",
                    "renderer sent an App-owned UI frame",
                )?;
                Ok(())
            }
        }
    }

    #[cfg(unix)]
    fn handle_attach(&mut self, connection_id: u64, attach: UiAttach) -> Result<(), UiBridgeError> {
        let Some(protocol_version) =
            negotiate_ui_protocol_version(attach.min_protocol_version, attach.max_protocol_version)
        else {
            self.reject_connection(
                connection_id,
                "unsupportedProtocolVersion",
                "renderer and App do not share a UI protocol version",
            )?;
            return Ok(());
        };
        let participant = match self
            .participant_tokens
            .as_ref()
            .expect("hosted verifier")
            .verify(
                &attach.participant_token,
                &attach.client_id,
                &attach.renderer.id,
                &attach.view_id,
            ) {
            Ok(claims) => claims.participant,
            Err(_) => {
                self.reject_connection(
                    connection_id,
                    "unauthorized",
                    "participant credential verification failed",
                )?;
                return Ok(());
            }
        };
        if !participant_allows(&participant, UiGrant::VIEW) {
            self.reject_connection(
                connection_id,
                "forbidden",
                "participant lacks the view grant",
            )?;
            return Ok(());
        }

        let resumed = attach
            .expected_app_instance_id
            .as_ref()
            .is_some_and(|expected| expected == &self.app_instance_id);
        let attachment = Attachment {
            participant,
            client_id: attach.client_id,
            renderer: attach.renderer,
            view_id: attach.view_id,
            state: attach.state,
            protocol_version,
            last_sent_revision: None,
            last_sent_was_targeted: false,
        };

        let duplicate_ids: Vec<u64> = self
            .server
            .as_ref()
            .expect("available")
            .connections
            .iter()
            .filter(|connection| connection.id != connection_id)
            .filter_map(|connection| {
                let existing = connection.attachment.as_ref()?;
                (existing.client_id == attachment.client_id
                    && existing.renderer.id == attachment.renderer.id)
                    .then_some(connection.id)
            })
            .collect();
        for duplicate_id in duplicate_ids {
            if let Some(connection) = self.connection_mut(duplicate_id) {
                connection.attachment = None;
                connection.close_after_flush = true;
            }
        }

        let current_revision = self
            .projection_for(&attachment.client_id, &attachment.view_id)
            .map(|projection| projection.revision);
        let attached = UiAttached {
            protocol: UI_PROTOCOL_NAME.to_owned(),
            protocol_version,
            min_protocol_version: UI_PROTOCOL_MIN_VERSION,
            max_protocol_version: UI_PROTOCOL_MAX_VERSION,
            app: self.app.clone(),
            app_instance_id: self.app_instance_id.clone(),
            participant_id: attachment.participant.id.clone(),
            client_id: attachment.client_id.clone(),
            renderer_id: attachment.renderer.id.clone(),
            view_id: attachment.view_id.clone(),
            resumed,
            current_revision,
        };
        let Some(connection) = self.connection_mut(connection_id) else {
            return Ok(());
        };
        connection.attachment = Some(attachment.clone());
        if !self.queue_message(connection_id, attached.into())? {
            return Ok(());
        }
        self.queue_projection(connection_id)?;
        self.pending.push_back(UiBridgeEvent::Attached {
            participant: attachment.participant.clone(),
            client_id: attachment.client_id.clone(),
            renderer: attachment.renderer.clone(),
            view_id: attachment.view_id.clone(),
            resumed,
        });
        self.broadcast_presence(&attachment.view_id)?;
        Ok(())
    }

    #[cfg(unix)]
    fn handle_event(&mut self, connection_id: u64, event: UiEvent) -> Result<(), UiBridgeError> {
        let Some(attachment) = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.clone())
        else {
            return Ok(());
        };
        if !event_matches_attachment(&event, &attachment, &self.app_instance_id) {
            self.reject_connection(
                connection_id,
                "routeMismatch",
                "event identity does not match its authenticated attachment",
            )?;
            return Ok(());
        }

        let key = (event.client_id.clone(), event.event_id.clone());
        if let Some(record) = self.events.get(&key) {
            if record.event != event {
                self.reject_connection(
                    connection_id,
                    "eventIdCollision",
                    "eventId was reused with different content",
                )?;
                return Ok(());
            }
            let ack = record.ack.clone().unwrap_or_else(|| {
                make_ack(&event, UiAckStatus::Pending, event.base_revision, None)
            });
            let _ = self.queue_message(connection_id, ack.into())?;
            return Ok(());
        }

        let Some(current_revision) = self
            .projection_for(&event.client_id, &event.view_id)
            .map(|projection| projection.revision)
        else {
            let ack = make_ack(
                &event,
                UiAckStatus::Rejected,
                0,
                Some("view has not published a snapshot".to_owned()),
            );
            if !self.remember_final_event(event, ack.clone()) {
                return self.reject_event_overflow(connection_id);
            }
            let _ = self.queue_message(connection_id, ack.into())?;
            return Ok(());
        };

        if event.base_revision < current_revision {
            let ack = make_ack(
                &event,
                UiAckStatus::Stale,
                current_revision,
                Some("renderer must resync before retrying".to_owned()),
            );
            if !self.remember_final_event(event, ack.clone()) {
                return self.reject_event_overflow(connection_id);
            }
            let _ = self.queue_message(connection_id, ack.into())?;
            self.queue_projection(connection_id)?;
            return Ok(());
        }
        if event.base_revision > current_revision {
            let ack = make_ack(
                &event,
                UiAckStatus::Rejected,
                current_revision,
                Some("event revision is ahead of the App".to_owned()),
            );
            if !self.remember_final_event(event, ack.clone()) {
                return self.reject_event_overflow(connection_id);
            }
            let _ = self.queue_message(connection_id, ack.into())?;
            return Ok(());
        }

        let required_grant = required_grant(event.action.kind);
        if !participant_allows(&attachment.participant, required_grant) {
            let ack = make_ack(
                &event,
                UiAckStatus::Rejected,
                current_revision,
                Some(format!("participant lacks the {required_grant} grant")),
            );
            if !self.remember_final_event(event, ack.clone()) {
                return self.reject_event_overflow(connection_id);
            }
            let _ = self.queue_message(connection_id, ack.into())?;
            return Ok(());
        }

        if !self.remember_pending_event(event.clone()) {
            return self.reject_event_overflow(connection_id);
        }
        self.pending.push_back(UiBridgeEvent::Action {
            participant: attachment.participant,
            event,
        });
        Ok(())
    }

    #[cfg(unix)]
    fn handle_lifecycle(
        &mut self,
        connection_id: u64,
        lifecycle: UiLifecycle,
    ) -> Result<(), UiBridgeError> {
        let app_instance_id = self.app_instance_id.clone();
        let Some(connection) = self.connection_mut(connection_id) else {
            return Ok(());
        };
        let Some(attachment) = connection.attachment.as_mut() else {
            return Ok(());
        };
        if lifecycle.app_instance_id != app_instance_id
            || lifecycle.client_id != attachment.client_id
            || lifecycle.renderer_id != attachment.renderer.id
            || lifecycle.view_id != attachment.view_id
        {
            self.reject_connection(
                connection_id,
                "routeMismatch",
                "lifecycle identity does not match its authenticated attachment",
            )?;
            return Ok(());
        }
        attachment.state = lifecycle.state;
        let attachment = attachment.clone();
        self.pending.push_back(UiBridgeEvent::Lifecycle {
            participant: attachment.participant,
            client_id: attachment.client_id,
            renderer: attachment.renderer,
            view_id: attachment.view_id.clone(),
            state: attachment.state,
        });
        self.broadcast_presence(&attachment.view_id)?;
        Ok(())
    }

    #[cfg(unix)]
    fn handle_snapshot_request(
        &mut self,
        connection_id: u64,
        request: UiRequestSnapshot,
    ) -> Result<(), UiBridgeError> {
        let Some(attachment) = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.as_ref())
        else {
            return Ok(());
        };
        if request.app_instance_id != self.app_instance_id
            || request.client_id != attachment.client_id
            || request.renderer_id != attachment.renderer.id
            || request.view_id != attachment.view_id
        {
            self.reject_connection(
                connection_id,
                "routeMismatch",
                "snapshot request does not match its authenticated attachment",
            )?;
            return Ok(());
        }
        self.queue_projection(connection_id).map(drop)
    }

    #[cfg(unix)]
    fn broadcast_view(&mut self, view_id: &ViewId) -> Result<usize, UiBridgeError> {
        let recipients: Vec<(u64, ClientId)> = self
            .server
            .as_ref()
            .map(|server| {
                server
                    .connections
                    .iter()
                    .filter_map(|connection| {
                        let attachment = connection.attachment.as_ref()?;
                        (&attachment.view_id == view_id)
                            .then(|| (connection.id, attachment.client_id.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut queued = 0;
        for (connection_id, _) in recipients {
            if self.queue_projection(connection_id)? {
                queued += 1;
            }
        }
        self.flush_available();
        Ok(queued)
    }

    #[cfg(not(unix))]
    fn broadcast_view(&mut self, _view_id: &ViewId) -> Result<usize, UiBridgeError> {
        Ok(0)
    }

    #[cfg(unix)]
    fn broadcast_shared_delta(
        &mut self,
        view_id: &ViewId,
        base_revision: u64,
        revision: u64,
        operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        let recipients: Vec<u64> = self
            .server
            .as_ref()
            .map(|server| {
                server
                    .connections
                    .iter()
                    .filter_map(|connection| {
                        let attachment = connection.attachment.as_ref()?;
                        (&attachment.view_id == view_id).then_some(connection.id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut queued = 0;
        for connection_id in recipients {
            let can_apply = self
                .connection(connection_id)
                .and_then(|connection| connection.attachment.as_ref())
                .is_some_and(|attachment| {
                    attachment.renderer.supports(UI_DELTA_CAPABILITY)
                        && attachment.last_sent_revision == Some(base_revision)
                        && !attachment.last_sent_was_targeted
                });
            let did_queue = if can_apply {
                self.queue_delta_message(
                    connection_id,
                    base_revision,
                    revision,
                    operations.clone(),
                    false,
                )?
            } else {
                self.queue_projection(connection_id)?
            };
            queued += usize::from(did_queue);
        }
        self.flush_available();
        Ok(queued)
    }

    #[cfg(not(unix))]
    fn broadcast_shared_delta(
        &mut self,
        _view_id: &ViewId,
        _base_revision: u64,
        _revision: u64,
        _operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        Ok(0)
    }

    #[cfg(unix)]
    fn send_delta_to(
        &mut self,
        client_id: &ClientId,
        view_id: &ViewId,
        base_revision: u64,
        revision: u64,
        operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        let recipients: Vec<u64> = self
            .server
            .as_ref()
            .map(|server| {
                server
                    .connections
                    .iter()
                    .filter_map(|connection| {
                        let attachment = connection.attachment.as_ref()?;
                        (&attachment.client_id == client_id && &attachment.view_id == view_id)
                            .then_some(connection.id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut queued = 0;
        for connection_id in recipients {
            let can_apply = self
                .connection(connection_id)
                .and_then(|connection| connection.attachment.as_ref())
                .is_some_and(|attachment| {
                    attachment.renderer.supports(UI_DELTA_CAPABILITY)
                        && attachment.last_sent_revision == Some(base_revision)
                });
            let did_queue = if can_apply {
                self.queue_delta_message(
                    connection_id,
                    base_revision,
                    revision,
                    operations.clone(),
                    true,
                )?
            } else {
                self.queue_projection(connection_id)?
            };
            queued += usize::from(did_queue);
        }
        self.flush_available();
        Ok(queued)
    }

    #[cfg(not(unix))]
    fn send_delta_to(
        &mut self,
        _client_id: &ClientId,
        _view_id: &ViewId,
        _base_revision: u64,
        _revision: u64,
        _operations: Vec<UiDeltaOperation>,
    ) -> Result<usize, UiBridgeError> {
        Ok(0)
    }

    #[cfg(unix)]
    fn send_projection_to(
        &mut self,
        client_id: &ClientId,
        view_id: &ViewId,
    ) -> Result<usize, UiBridgeError> {
        let recipients: Vec<u64> = self
            .server
            .as_ref()
            .map(|server| {
                server
                    .connections
                    .iter()
                    .filter_map(|connection| {
                        let attachment = connection.attachment.as_ref()?;
                        (&attachment.client_id == client_id && &attachment.view_id == view_id)
                            .then_some(connection.id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut queued = 0;
        for connection_id in recipients {
            if self.queue_projection(connection_id)? {
                queued += 1;
            }
        }
        self.flush_available();
        Ok(queued)
    }

    #[cfg(not(unix))]
    fn send_projection_to(
        &mut self,
        _client_id: &ClientId,
        _view_id: &ViewId,
    ) -> Result<usize, UiBridgeError> {
        Ok(0)
    }

    #[cfg(unix)]
    fn queue_projection(&mut self, connection_id: u64) -> Result<bool, UiBridgeError> {
        let Some(attachment) = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.clone())
        else {
            return Ok(false);
        };
        let Some((projection, was_targeted)) = self
            .projection_for_with_source(&attachment.client_id, &attachment.view_id)
            .map(|(projection, targeted)| (projection.clone(), targeted))
        else {
            return Ok(false);
        };
        let snapshot = UiSnapshot::new(
            self.app_instance_id.clone(),
            attachment.client_id,
            attachment.view_id,
            projection.revision,
            projection.root,
        );
        let mut snapshot = snapshot;
        snapshot.protocol_version = attachment.protocol_version;
        let queued = self.queue_message(connection_id, snapshot.into())?;
        if queued
            && let Some(attachment) = self
                .connection_mut(connection_id)
                .and_then(|connection| connection.attachment.as_mut())
        {
            attachment.last_sent_revision = Some(projection.revision);
            attachment.last_sent_was_targeted = was_targeted;
        }
        Ok(queued)
    }

    #[cfg(unix)]
    fn queue_delta_message(
        &mut self,
        connection_id: u64,
        base_revision: u64,
        revision: u64,
        operations: Vec<UiDeltaOperation>,
        targeted: bool,
    ) -> Result<bool, UiBridgeError> {
        let Some(attachment) = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.clone())
        else {
            return Ok(false);
        };
        let mut delta = UiDelta::new(
            self.app_instance_id.clone(),
            attachment.client_id,
            attachment.view_id,
            base_revision,
            revision,
            operations,
        );
        delta.protocol_version = attachment.protocol_version;
        let queued = self.queue_message(connection_id, delta.into())?;
        if queued
            && let Some(attachment) = self
                .connection_mut(connection_id)
                .and_then(|connection| connection.attachment.as_mut())
        {
            attachment.last_sent_revision = Some(revision);
            attachment.last_sent_was_targeted = targeted;
        }
        Ok(queued)
    }

    fn projection_for(&self, client_id: &ClientId, view_id: &ViewId) -> Option<&Projection> {
        self.projection_for_with_source(client_id, view_id)
            .map(|(projection, _)| projection)
    }

    fn projection_for_with_source(
        &self,
        client_id: &ClientId,
        view_id: &ViewId,
    ) -> Option<(&Projection, bool)> {
        let shared = self.views.get(view_id);
        let targeted = self.client_views.get(&(client_id.clone(), view_id.clone()));
        match (shared, targeted) {
            (Some(shared), Some(targeted)) if targeted.revision >= shared.revision => {
                Some((targeted, true))
            }
            (Some(shared), _) => Some((shared, false)),
            (None, Some(targeted)) => Some((targeted, true)),
            (None, None) => None,
        }
    }

    #[cfg(unix)]
    fn broadcast_presence(&mut self, view_id: &ViewId) -> Result<(), UiBridgeError> {
        let Some(server) = &self.server else {
            return Ok(());
        };
        let mut members: Vec<UiPresenceMember> = server
            .connections
            .iter()
            .filter_map(|connection| {
                let attachment = connection.attachment.as_ref()?;
                (&attachment.view_id == view_id).then(|| UiPresenceMember {
                    participant: attachment.participant.clone(),
                    client_id: attachment.client_id.clone(),
                    renderer: attachment.renderer.clone(),
                    state: attachment.state,
                })
            })
            .collect();
        members.sort_by(|left, right| {
            left.participant
                .id
                .cmp(&right.participant.id)
                .then_with(|| left.client_id.cmp(&right.client_id))
                .then_with(|| left.renderer.id.cmp(&right.renderer.id))
        });
        let recipients: Vec<(u64, u32)> = server
            .connections
            .iter()
            .filter_map(|connection| {
                let attachment = connection.attachment.as_ref()?;
                (&attachment.view_id == view_id)
                    .then_some((connection.id, attachment.protocol_version))
            })
            .collect();
        for (connection_id, protocol_version) in recipients {
            let message = UiPresence {
                protocol: UI_PROTOCOL_NAME.to_owned(),
                protocol_version,
                app_instance_id: self.app_instance_id.clone(),
                view_id: view_id.clone(),
                members: members.clone(),
            };
            let _ = self.queue_message(connection_id, message.into())?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn send_ack(&mut self, ack: &UiAck) -> Result<(), UiBridgeError> {
        let recipients: Vec<u64> = self
            .server
            .as_ref()
            .map(|server| {
                server
                    .connections
                    .iter()
                    .filter_map(|connection| {
                        let attachment = connection.attachment.as_ref()?;
                        (attachment.client_id == ack.client_id
                            && attachment.renderer.id == ack.renderer_id
                            && attachment.view_id == ack.view_id)
                            .then_some(connection.id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        for connection_id in recipients {
            let _ = self.queue_message(connection_id, ack.clone().into())?;
        }
        self.flush_available();
        Ok(())
    }

    #[cfg(not(unix))]
    fn send_ack(&mut self, _ack: &UiAck) -> Result<(), UiBridgeError> {
        Ok(())
    }

    fn remember_pending_event(&mut self, event: UiEvent) -> bool {
        if !self.make_event_room(&event.client_id) {
            return false;
        }
        let key = (event.client_id.clone(), event.event_id.clone());
        self.event_order.push_back(key.clone());
        self.events.insert(
            key,
            EventRecord {
                event,
                ack: None,
                recorded_at: Instant::now(),
            },
        );
        true
    }

    fn remember_final_event(&mut self, event: UiEvent, ack: UiAck) -> bool {
        if !self.make_event_room(&event.client_id) {
            return false;
        }
        let key = (event.client_id.clone(), event.event_id.clone());
        self.event_order.push_back(key.clone());
        self.events.insert(
            key,
            EventRecord {
                event,
                ack: Some(ack),
                recorded_at: Instant::now(),
            },
        );
        true
    }

    fn make_event_room(&mut self, client_id: &ClientId) -> bool {
        self.prune_expired_events();
        while self.events.len() >= MAX_EVENT_HISTORY
            || self
                .events
                .keys()
                .filter(|(candidate, _)| candidate == client_id)
                .count()
                >= MAX_EVENT_HISTORY_PER_CLIENT
        {
            let Some(index) = self.event_order.iter().position(|key| {
                &key.0 == client_id
                    && self
                        .events
                        .get(key)
                        .is_some_and(|record| record.ack.is_some())
            }) else {
                return false;
            };
            if let Some(key) = self.event_order.remove(index) {
                self.events.remove(&key);
            }
        }
        true
    }

    fn prune_expired_events(&mut self) {
        let now = Instant::now();
        let mut index = 0;
        while index < self.event_order.len() {
            let key = &self.event_order[index];
            let expired = self.events.get(key).is_some_and(|record| {
                record.ack.is_some()
                    && now.saturating_duration_since(record.recorded_at) >= EVENT_REPLAY_TTL
            });
            if expired {
                if let Some(key) = self.event_order.remove(index) {
                    self.events.remove(&key);
                }
            } else {
                index += 1;
            }
        }
    }

    #[cfg(unix)]
    fn reject_event_overflow(&mut self, connection_id: u64) -> Result<(), UiBridgeError> {
        self.reject_connection(
            connection_id,
            "eventLimit",
            "renderer exceeded its pending event or acknowledgement quota",
        )
    }

    #[cfg(unix)]
    fn reject_connection(
        &mut self,
        connection_id: u64,
        code: &str,
        message: &str,
    ) -> Result<(), UiBridgeError> {
        let mut error = UiErrorMessage::new(code, message);
        if let Some(version) = self
            .connection(connection_id)
            .and_then(|connection| connection.attachment.as_ref())
            .map(|attachment| attachment.protocol_version)
        {
            error.protocol_version = version;
        }
        let queued = self.queue_message(connection_id, error.into())?;
        if queued && let Some(connection) = self.connection_mut(connection_id) {
            connection.close_after_flush = true;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn flush_available(&mut self) {
        let Some(server) = &mut self.server else {
            return;
        };
        for connection in &mut server.connections {
            if connection.flush().is_err() {
                connection.disconnected = true;
            }
        }
    }

    #[cfg(unix)]
    fn reap_connections(&mut self) -> Result<(), UiBridgeError> {
        let server = self.server.as_mut().expect("available");
        let mut removed = Vec::new();
        let mut index = 0;
        while index < server.connections.len() {
            let connection = &server.connections[index];
            if connection.disconnected
                || (connection.close_after_flush && connection.outgoing.is_empty())
            {
                let mut connection = server.connections.swap_remove(index);
                if let Some(attachment) = connection.attachment.take() {
                    removed.push(attachment);
                }
            } else {
                index += 1;
            }
        }
        for attachment in removed {
            self.pending.push_back(UiBridgeEvent::Detached {
                participant: attachment.participant.clone(),
                client_id: attachment.client_id.clone(),
                renderer: attachment.renderer.clone(),
                view_id: attachment.view_id.clone(),
            });
            self.broadcast_presence(&attachment.view_id)?;
        }
        Ok(())
    }

    #[cfg(unix)]
    fn connection(&self, id: u64) -> Option<&UiConnection> {
        self.server
            .as_ref()?
            .connections
            .iter()
            .find(|connection| connection.id == id)
    }

    #[cfg(unix)]
    fn connection_mut(&mut self, id: u64) -> Option<&mut UiConnection> {
        self.server
            .as_mut()?
            .connections
            .iter_mut()
            .find(|connection| connection.id == id)
    }

    #[cfg(unix)]
    fn queue_message(
        &mut self,
        connection_id: u64,
        message: UiMessage,
    ) -> Result<bool, UiBridgeError> {
        let Some(connection) = self.connection_mut(connection_id) else {
            return Ok(false);
        };
        connection.queue(message).map_err(UiBridgeError::Protocol)
    }
}

/// App-owned endpoint, projection, or protocol failure.
#[derive(Debug)]
pub enum UiBridgeError {
    Io(io::Error),
    Protocol(UiProtocolError),
    ParticipantToken(UiParticipantTokenError),
    State(UiStateError),
    MissingToken,
    RelativeSocketPath(PathBuf),
    InvalidSocketPath(PathBuf),
    EndpointInUse(PathBuf),
    UnsupportedPlatform,
    RevisionRegressed {
        view_id: ViewId,
        previous: u64,
        received: u64,
    },
    RevisionConflict {
        view_id: ViewId,
        revision: u64,
    },
    MissingBaseProjection(ViewId),
    DeltaBaseMismatch {
        view_id: ViewId,
        current: u64,
        received: u64,
    },
    InvalidDeltaRevision {
        view_id: ViewId,
        base: u64,
        received: u64,
    },
    InvalidRevision(u64),
    UnknownEvent {
        client_id: ClientId,
        event_id: EventId,
    },
    EventIdentityCollision {
        client_id: ClientId,
        event_id: EventId,
    },
}

impl fmt::Display for UiBridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "App Kit UI endpoint I/O error: {error}"),
            Self::Protocol(error) => write!(formatter, "App Kit UI protocol error: {error}"),
            Self::ParticipantToken(error) => {
                write!(formatter, "App Kit participant credential error: {error}")
            }
            Self::State(error) => write!(formatter, "App Kit persistence error: {error}"),
            Self::MissingToken => formatter
                .write_str("hosted App Kit UI requires a strong UNPEEL_UI_TOKEN signing key"),
            Self::RelativeSocketPath(path) => write!(
                formatter,
                "App Kit UI socket path must be absolute: {}",
                path.display()
            ),
            Self::InvalidSocketPath(path) => write!(
                formatter,
                "App Kit UI endpoint exists and is not a socket: {}",
                path.display()
            ),
            Self::EndpointInUse(path) => write!(
                formatter,
                "App Kit UI endpoint is already in use: {}",
                path.display()
            ),
            Self::UnsupportedPlatform => {
                formatter.write_str("App Kit UI Unix sockets are unsupported on this platform")
            }
            Self::RevisionRegressed {
                view_id,
                previous,
                received,
            } => write!(
                formatter,
                "view {view_id} revision regressed from {previous} to {received}"
            ),
            Self::RevisionConflict { view_id, revision } => write!(
                formatter,
                "view {view_id} published different state for immutable revision {revision}"
            ),
            Self::MissingBaseProjection(view_id) => {
                write!(
                    formatter,
                    "view {view_id} has no base projection for a delta"
                )
            }
            Self::DeltaBaseMismatch {
                view_id,
                current,
                received,
            } => write!(
                formatter,
                "view {view_id} delta base {received} does not match current revision {current}"
            ),
            Self::InvalidDeltaRevision {
                view_id,
                base,
                received,
            } => write!(
                formatter,
                "view {view_id} delta revision {received} must be greater than base revision {base}"
            ),
            Self::InvalidRevision(revision) => {
                write!(formatter, "revision {revision} is not cross-platform safe")
            }
            Self::UnknownEvent {
                client_id,
                event_id,
            } => {
                write!(formatter, "unknown event {client_id}/{event_id}")
            }
            Self::EventIdentityCollision {
                client_id,
                event_id,
            } => write!(
                formatter,
                "event identity {client_id}/{event_id} has different content"
            ),
        }
    }
}

impl std::error::Error for UiBridgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::ParticipantToken(error) => Some(error),
            Self::State(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for UiBridgeError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<UiProtocolError> for UiBridgeError {
    fn from(value: UiProtocolError) -> Self {
        Self::Protocol(value)
    }
}

impl From<UiParticipantTokenError> for UiBridgeError {
    fn from(value: UiParticipantTokenError) -> Self {
        Self::ParticipantToken(value)
    }
}

impl From<UiStateError> for UiBridgeError {
    fn from(value: UiStateError) -> Self {
        Self::State(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Projection {
    revision: u64,
    root: UiNode,
}

#[derive(Clone, Debug)]
struct EventRecord {
    event: UiEvent,
    ack: Option<UiAck>,
    recorded_at: Instant,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct Attachment {
    participant: UiParticipant,
    client_id: ClientId,
    renderer: UiRendererMetadata,
    view_id: ViewId,
    state: UiRendererState,
    protocol_version: u32,
    last_sent_revision: Option<u64>,
    last_sent_was_targeted: bool,
}

#[cfg(unix)]
#[derive(Debug)]
struct UiServer {
    listener: UnixListener,
    guard: SocketGuard,
    connections: Vec<UiConnection>,
    next_connection_id: u64,
}

#[cfg(unix)]
impl UiServer {
    fn bind(path: &Path) -> Result<Self, UiBridgeError> {
        if !path.is_absolute() {
            return Err(UiBridgeError::RelativeSocketPath(path.to_owned()));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(path) {
            if !metadata.file_type().is_socket() {
                return Err(UiBridgeError::InvalidSocketPath(path.to_owned()));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(UiBridgeError::EndpointInUse(path.to_owned()));
            }
            std::fs::remove_file(path)?;
        }
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            listener,
            guard: SocketGuard {
                path: path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            connections: Vec::new(),
            next_connection_id: 1,
        })
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct UiConnection {
    id: u64,
    stream: UnixStream,
    accepted_at: Instant,
    read_buffer: Vec<u8>,
    outgoing: VecDeque<Vec<u8>>,
    write_offset: usize,
    pending_write_bytes: usize,
    attachment: Option<Attachment>,
    close_after_flush: bool,
    disconnected: bool,
}

#[cfg(unix)]
impl UiConnection {
    fn new(id: u64, stream: UnixStream) -> Self {
        Self {
            id,
            stream,
            accepted_at: Instant::now(),
            read_buffer: Vec::new(),
            outgoing: VecDeque::new(),
            write_offset: 0,
            pending_write_bytes: 0,
            attachment: None,
            close_after_flush: false,
            disconnected: false,
        }
    }

    fn queue(&mut self, message: UiMessage) -> Result<bool, UiProtocolError> {
        let frame = encode_ui_frame(&message)?;
        if self.pending_write_bytes.saturating_add(frame.len()) > MAX_PENDING_WRITE_BYTES {
            self.disconnected = true;
            return Ok(false);
        }
        self.pending_write_bytes += frame.len();
        self.outgoing.push_back(frame);
        Ok(true)
    }

    fn flush(&mut self) -> io::Result<()> {
        while let Some(frame) = self.outgoing.front() {
            match self.stream.write(&frame[self.write_offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "renderer socket accepted zero output bytes",
                    ));
                }
                Ok(written) => {
                    self.write_offset += written;
                    self.pending_write_bytes -= written;
                    if self.write_offset == frame.len() {
                        self.outgoing.pop_front();
                        self.write_offset = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_messages(&mut self) -> Result<Vec<UiMessage>, UiProtocolError> {
        let mut messages = Vec::new();
        self.decode_buffered_messages(&mut messages)?;
        if messages.len() >= MAX_FRAMES_PER_POLL {
            return Ok(messages);
        }

        let mut chunk = [0u8; 8192];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.disconnected = true;
                    break;
                }
                Ok(read) => {
                    self.read_buffer.extend_from_slice(&chunk[..read]);
                    self.decode_buffered_messages(&mut messages)?;
                    if messages.len() >= MAX_FRAMES_PER_POLL {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }

        Ok(messages)
    }

    fn decode_buffered_messages(
        &mut self,
        messages: &mut Vec<UiMessage>,
    ) -> Result<(), UiProtocolError> {
        while messages.len() < MAX_FRAMES_PER_POLL {
            let Some(newline) = self.read_buffer.iter().position(|byte| *byte == b'\n') else {
                break;
            };
            let mut frame: Vec<u8> = self.read_buffer.drain(..=newline).collect();
            frame.pop();
            if frame.ends_with(b"\r") {
                frame.pop();
            }
            messages.push(decode_ui_frame(&frame)?);
        }
        if self.read_buffer.len() > MAX_UI_FRAME_BYTES + 2 {
            return Err(UiProtocolError::FrameTooLarge {
                max_bytes: MAX_UI_FRAME_BYTES,
            });
        }
        Ok(())
    }
}

fn validate_projection(revision: u64, root: &UiNode) -> Result<(), UiBridgeError> {
    if revision > MAX_SAFE_UI_INTEGER {
        return Err(UiBridgeError::InvalidRevision(revision));
    }
    root.validate().map_err(UiProtocolError::InvalidView)?;
    Ok(())
}

fn validate_delta_base(
    view_id: &ViewId,
    previous: &Projection,
    base_revision: u64,
    revision: u64,
    operations: &[UiDeltaOperation],
) -> Result<(), UiBridgeError> {
    if base_revision > MAX_SAFE_UI_INTEGER || revision > MAX_SAFE_UI_INTEGER {
        return Err(UiBridgeError::InvalidRevision(base_revision.max(revision)));
    }
    if previous.revision != base_revision {
        return Err(UiBridgeError::DeltaBaseMismatch {
            view_id: view_id.clone(),
            current: previous.revision,
            received: base_revision,
        });
    }
    if revision <= base_revision {
        return Err(UiBridgeError::InvalidDeltaRevision {
            view_id: view_id.clone(),
            base: base_revision,
            received: revision,
        });
    }
    if operations.is_empty() || operations.len() > 4096 {
        return Err(UiProtocolError::InvalidMessage(
            "delta operations must contain 1..=4096 entries".to_owned(),
        )
        .into());
    }
    Ok(())
}

fn store_projection<K>(
    projections: &mut HashMap<K, Projection>,
    key: K,
    revision: u64,
    root: UiNode,
) -> Result<(), UiBridgeError>
where
    K: std::hash::Hash + Eq + ProjectionKey,
{
    if let Some(previous) = projections.get(&key) {
        if revision < previous.revision {
            return Err(UiBridgeError::RevisionRegressed {
                view_id: key.view_id().clone(),
                previous: previous.revision,
                received: revision,
            });
        }
        if revision == previous.revision && root != previous.root {
            return Err(UiBridgeError::RevisionConflict {
                view_id: key.view_id().clone(),
                revision,
            });
        }
    }
    projections.insert(key, Projection { revision, root });
    Ok(())
}

trait ProjectionKey {
    fn view_id(&self) -> &ViewId;
}

impl ProjectionKey for ViewId {
    fn view_id(&self) -> &ViewId {
        self
    }
}

impl ProjectionKey for (ClientId, ViewId) {
    fn view_id(&self) -> &ViewId {
        &self.1
    }
}

#[cfg(unix)]
fn event_matches_attachment(
    event: &UiEvent,
    attachment: &Attachment,
    app_instance_id: &AppInstanceId,
) -> bool {
    &event.app_instance_id == app_instance_id
        && event.participant_id == attachment.participant.id
        && event.client_id == attachment.client_id
        && event.renderer_id == attachment.renderer.id
        && event.view_id == attachment.view_id
}

#[cfg(unix)]
fn message_protocol_version(message: &UiMessage) -> Option<u32> {
    match message {
        UiMessage::Attach(_) => None,
        UiMessage::Attached(message) => Some(message.protocol_version),
        UiMessage::Snapshot(message) => Some(message.protocol_version),
        UiMessage::Delta(message) => Some(message.protocol_version),
        UiMessage::Event(message) => Some(message.protocol_version),
        UiMessage::Ack(message) => Some(message.protocol_version),
        UiMessage::Lifecycle(message) => Some(message.protocol_version),
        UiMessage::RequestSnapshot(message) => Some(message.protocol_version),
        UiMessage::Presence(message) => Some(message.protocol_version),
        UiMessage::Error(message) => Some(message.protocol_version),
    }
}

fn required_grant(kind: UiEventKind) -> &'static str {
    match kind {
        UiEventKind::Activate | UiEventKind::Select | UiEventKind::Cancel => UiGrant::INTERACT,
        UiEventKind::Change | UiEventKind::Submit => UiGrant::EDIT,
        UiEventKind::Command => UiGrant::COMMAND,
    }
}

fn participant_allows(participant: &UiParticipant, grant: &str) -> bool {
    participant.allows(grant)
        || participant.allows(UiGrant::ADMIN)
        || (grant == UiGrant::INTERACT
            && (participant.allows(UiGrant::EDIT) || participant.allows(UiGrant::COMMAND)))
        || (grant == UiGrant::COMMAND && participant.allows(UiGrant::EDIT))
}

fn make_ack(event: &UiEvent, status: UiAckStatus, revision: u64, message: Option<String>) -> UiAck {
    UiAck {
        protocol: UI_PROTOCOL_NAME.to_owned(),
        protocol_version: event.protocol_version,
        app_instance_id: event.app_instance_id.clone(),
        client_id: event.client_id.clone(),
        renderer_id: event.renderer_id.clone(),
        view_id: event.view_id.clone(),
        event_id: event.event_id.clone(),
        status,
        revision,
        message,
    }
}

fn should_render_terminal(states: impl Iterator<Item = UiRendererState>) -> bool {
    let mut any_attachment = false;
    for state in states {
        any_attachment = true;
        if state.terminal_visible {
            return true;
        }
    }
    !any_attachment
}

fn session_id_from_socket_path(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("app-session")
        .to_owned()
}

fn new_app_instance_id() -> AppInstanceId {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let counter = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
    AppInstanceId::new(format!(
        "app-{:x}-{timestamp:x}-{counter:x}",
        std::process::id()
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::BufReader;
    use std::thread;
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::{
        MarkdownEditorSpec, TextEdit, TextPosition, TextRange, TextSelection, UiAction,
        UiComponent, UiParticipantTokenIssuer, read_ui_message, write_ui_message,
    };

    const TEST_SIGNING_KEY: &str = "0123456789abcdef0123456789abcdef";
    const TEST_SESSION_ID: &str = "app-session-test";

    struct TestClient {
        writer: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl TestClient {
        fn connect(path: &Path, attach: UiAttach) -> Self {
            let mut writer = UnixStream::connect(path).unwrap();
            writer
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let reader = BufReader::new(writer.try_clone().unwrap());
            write_ui_message(&mut writer, &attach.into()).unwrap();
            Self { writer, reader }
        }

        fn send(&mut self, message: UiMessage) {
            write_ui_message(&mut self.writer, &message).unwrap();
        }

        fn read(&mut self) -> UiMessage {
            read_ui_message(&mut self.reader).unwrap().unwrap()
        }
    }

    fn server() -> (TempDir, UiBridge) {
        let directory = tempfile::tempdir().unwrap();
        let bridge = UiBridge::listen_for_session(
            directory.path().join("app-ui.sock"),
            TEST_SIGNING_KEY,
            TEST_SESSION_ID,
            AppMetadata::new("com.unpeel.markdown", "Markdown", "0.1.0"),
        )
        .unwrap();
        (directory, bridge)
    }

    fn node(text: &str, column: u32) -> UiNode {
        UiNode::markdown_editor(
            "editor",
            MarkdownEditorSpec::new(text, TextSelection::caret(TextPosition::new(0, column))),
        )
    }

    fn participant(id: &str, grants: &[&str]) -> UiParticipant {
        UiParticipant::new(id)
            .display_name(id)
            .grants(grants.iter().copied())
    }

    fn attach(
        participant_id: &str,
        client_id: &str,
        renderer_id: &str,
        state: UiRendererState,
    ) -> UiAttach {
        attach_with_grants(
            participant_id,
            client_id,
            renderer_id,
            state,
            &[UiGrant::VIEW, UiGrant::EDIT, UiGrant::COMMAND],
        )
    }

    fn attach_with_grants(
        participant_id: &str,
        client_id: &str,
        renderer_id: &str,
        state: UiRendererState,
        grants: &[&str],
    ) -> UiAttach {
        let renderer =
            UiRendererMetadata::new(renderer_id, "web").capabilities([UI_DELTA_CAPABILITY]);
        let token = UiParticipantTokenIssuer::new(TEST_SIGNING_KEY, TEST_SESSION_ID)
            .unwrap()
            .issue(
                participant(participant_id, grants),
                client_id,
                renderer_id,
                "main",
                format!("token-{client_id}-{renderer_id}"),
                Duration::from_secs(60),
            )
            .unwrap();
        UiAttach::new(token, client_id, renderer, "main").state(state)
    }

    fn attach_without_delta(participant_id: &str, client_id: &str, renderer_id: &str) -> UiAttach {
        let token = UiParticipantTokenIssuer::new(TEST_SIGNING_KEY, TEST_SESSION_ID)
            .unwrap()
            .issue(
                participant(
                    participant_id,
                    &[UiGrant::VIEW, UiGrant::EDIT, UiGrant::COMMAND],
                ),
                client_id,
                renderer_id,
                "main",
                format!("token-{client_id}-{renderer_id}"),
                Duration::from_secs(60),
            )
            .unwrap();
        UiAttach::new(
            token,
            client_id,
            UiRendererMetadata::new(renderer_id, "legacyWeb"),
            "main",
        )
    }

    fn poll_until(bridge: &mut UiBridge) -> UiBridgeEvent {
        for _ in 0..100 {
            if let Some(event) = bridge.poll().unwrap() {
                return event;
            }
            thread::yield_now();
        }
        panic!("bridge did not produce an event");
    }

    fn expect_initial(client: &mut TestClient, expected_revision: u64) -> UiAttached {
        let UiMessage::Attached(attached) = client.read() else {
            panic!("first server frame must attach the renderer");
        };
        let UiMessage::Snapshot(snapshot) = client.read() else {
            panic!("second server frame must contain the current snapshot");
        };
        assert_eq!(snapshot.revision, expected_revision);
        let UiMessage::Presence(presence) = client.read() else {
            panic!("third server frame must contain presence");
        };
        assert!(!presence.members.is_empty());
        attached
    }

    fn event(
        bridge: &UiBridge,
        participant_id: &str,
        client_id: &str,
        renderer_id: &str,
        event_id: &str,
        revision: u64,
    ) -> UiEvent {
        UiEvent::new(
            bridge.app_instance_id().clone(),
            participant_id,
            client_id,
            renderer_id,
            "main",
            event_id,
            revision,
            UiAction::replace_range(
                "editor",
                TextEdit::new(
                    TextRange::new(TextPosition::new(0, 0), TextPosition::new(0, 0)),
                    "x",
                ),
            ),
        )
    }

    #[test]
    fn app_owned_endpoint_reconnects_and_controls_terminal_visibility() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 7, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );

        let UiBridgeEvent::Attached { resumed, .. } = poll_until(&mut bridge) else {
            panic!("expected renderer attachment");
        };
        assert!(!resumed);
        let attached = expect_initial(&mut client, 7);
        assert!(!attached.resumed);
        assert!(!bridge.should_render_terminal());

        client.send(
            UiLifecycle::new(
                bridge.app_instance_id().clone(),
                "client-1",
                "renderer-1",
                "main",
                UiRendererState::terminal(),
            )
            .into(),
        );
        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Lifecycle { .. }
        ));
        assert!(bridge.should_render_terminal());
        let _presence = client.read();

        drop(client);
        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Detached { .. }
        ));
        assert!(bridge.should_render_terminal());

        let resumed_attach = attach(
            "person-1",
            "client-1",
            "renderer-2",
            UiRendererState::component(),
        )
        .resume(bridge.app_instance_id().clone(), 7);
        let mut resumed_client = TestClient::connect(&path, resumed_attach);
        let UiBridgeEvent::Attached { resumed, .. } = poll_until(&mut bridge) else {
            panic!("expected renderer reattachment");
        };
        assert!(resumed);
        assert!(expect_initial(&mut resumed_client, 7).resumed);
    }

    #[test]
    fn multiple_participants_receive_presence_and_personalized_projections() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 4, node("shared", 0)).unwrap();
        bridge
            .publish_to("client-1", "main", 4, node("shared", 3))
            .unwrap();
        let path = bridge.socket_path().unwrap().to_owned();

        let mut first = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );
        let _ = poll_until(&mut bridge);
        let _ = first.read();
        let UiMessage::Snapshot(first_snapshot) = first.read() else {
            panic!("first client needs a snapshot");
        };
        let UiComponent::MarkdownEditor(first_editor) = first_snapshot.root.element else {
            panic!("fixture must contain a Markdown editor");
        };
        assert_eq!(first_editor.selection.head.utf16_column, 3);
        let _ = first.read();

        let mut second = TestClient::connect(
            &path,
            attach(
                "person-2",
                "client-2",
                "renderer-2",
                UiRendererState::component(),
            ),
        );
        let _ = poll_until(&mut bridge);
        let _ = second.read();
        let UiMessage::Snapshot(second_snapshot) = second.read() else {
            panic!("second client needs a snapshot");
        };
        let UiComponent::MarkdownEditor(second_editor) = second_snapshot.root.element else {
            panic!("fixture must contain a Markdown editor");
        };
        assert_eq!(second_editor.selection.head.utf16_column, 0);
        let UiMessage::Presence(second_presence) = second.read() else {
            panic!("second client needs presence");
        };
        assert_eq!(second_presence.members.len(), 2);
        let UiMessage::Presence(first_presence) = first.read() else {
            panic!("first client needs updated presence");
        };
        assert_eq!(first_presence.members.len(), 2);
    }

    #[test]
    fn duplicate_events_are_delivered_once_and_replay_final_acknowledgements() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 7, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );
        let _ = poll_until(&mut bridge);
        let _ = expect_initial(&mut client, 7);

        let event = event(&bridge, "person-1", "client-1", "renderer-1", "event-1", 7);
        client.send(event.clone().into());
        client.send(event.clone().into());
        let UiBridgeEvent::Action {
            event: received, ..
        } = poll_until(&mut bridge)
        else {
            panic!("expected one accepted action");
        };
        assert_eq!(received, event);
        assert_eq!(bridge.poll().unwrap(), None);

        let UiMessage::Ack(pending) = client.read() else {
            panic!("duplicate pending event needs an acknowledgement");
        };
        assert_eq!(pending.status, UiAckStatus::Pending);
        bridge
            .acknowledge(&event, UiEventOutcome::Applied, 8)
            .unwrap();
        bridge.publish("main", 8, node("xhello", 1)).unwrap();
        let UiMessage::Ack(applied) = client.read() else {
            panic!("event needs a final acknowledgement");
        };
        assert_eq!(applied.status, UiAckStatus::Applied);
        assert_eq!(applied.revision, 8);
        let UiMessage::Snapshot(snapshot) = client.read() else {
            panic!("accepted edit needs a new snapshot");
        };
        assert_eq!(snapshot.revision, 8);

        client.send(event.clone().into());
        assert_eq!(bridge.poll().unwrap(), None);
        let UiMessage::Ack(replayed) = client.read() else {
            panic!("resend needs the cached final acknowledgement");
        };
        assert_eq!(replayed.status, UiAckStatus::Applied);
    }

    #[test]
    fn contiguous_delta_updates_authoritative_snapshot_and_delta_capable_renderer() {
        let (_directory, mut bridge) = server();
        let root = UiNode::markdown_editor(
            "editor",
            MarkdownEditorSpec::new(
                "# Hello\n🙂 world",
                TextSelection::caret(TextPosition::new(1, 2)),
            ),
        );
        bridge.publish("main", 7, root).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );
        let _ = poll_until(&mut bridge);
        let UiMessage::Attached(_) = client.read() else {
            panic!("expected attached");
        };
        let UiMessage::Snapshot(initial) = client.read() else {
            panic!("expected initial snapshot");
        };
        let UiMessage::Presence(_) = client.read() else {
            panic!("expected presence");
        };

        bridge
            .publish_delta(
                "main",
                7,
                8,
                vec![
                    UiDeltaOperation::markdown_replace_range(
                        "editor",
                        TextEdit::new(
                            TextRange::new(TextPosition::new(1, 0), TextPosition::new(1, 2)),
                            "Hello",
                        ),
                    ),
                    UiDeltaOperation::markdown_set_selection(
                        "editor",
                        TextSelection::caret(TextPosition::new(1, 5)),
                    ),
                ],
            )
            .unwrap();
        let UiMessage::Delta(delta) = client.read() else {
            panic!("delta-capable renderer must receive a delta");
        };
        let updated = initial.applying(&delta).unwrap();
        let UiComponent::MarkdownEditor(editor) = &updated.root.element else {
            panic!("fixture must contain a Markdown editor");
        };
        assert_eq!(updated.revision, 8);
        assert_eq!(editor.text, "# Hello\nHello world");
        assert_eq!(editor.selection.head, TextPosition::new(1, 5));

        let stored = bridge
            .projection_for(&"client-1".into(), &"main".into())
            .unwrap();
        assert_eq!(stored.revision, 8);
        assert_eq!(stored.root, updated.root);
    }

    #[test]
    fn renderer_without_delta_capability_receives_resulting_snapshot() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 1, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach_without_delta("person-1", "client-1", "renderer-legacy"),
        );
        let _ = poll_until(&mut bridge);
        let _ = expect_initial(&mut client, 1);

        bridge
            .publish_delta(
                "main",
                1,
                2,
                vec![UiDeltaOperation::MarkdownSetDirty {
                    node_id: "editor".into(),
                    dirty: true,
                }],
            )
            .unwrap();
        let UiMessage::Snapshot(snapshot) = client.read() else {
            panic!("legacy renderer must receive a complete snapshot");
        };
        assert_eq!(snapshot.revision, 2);
        let UiComponent::MarkdownEditor(editor) = snapshot.root.element else {
            panic!("fixture must contain a Markdown editor");
        };
        assert!(editor.dirty);
    }

    #[test]
    fn stale_and_unauthorized_edits_never_reach_the_app_reducer() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 5, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let view_only = attach_with_grants(
            "person-1",
            "client-1",
            "renderer-1",
            UiRendererState::terminal(),
            &[UiGrant::VIEW],
        );
        let mut client = TestClient::connect(&path, view_only);
        let _ = poll_until(&mut bridge);
        let _ = expect_initial(&mut client, 5);

        let forbidden = event(
            &bridge,
            "person-1",
            "client-1",
            "renderer-1",
            "event-forbidden",
            5,
        );
        client.send(forbidden.into());
        assert_eq!(bridge.poll().unwrap(), None);
        let UiMessage::Ack(rejected) = client.read() else {
            panic!("forbidden edit needs an acknowledgement");
        };
        assert_eq!(rejected.status, UiAckStatus::Rejected);

        let stale = event(
            &bridge,
            "person-1",
            "client-1",
            "renderer-1",
            "event-stale",
            4,
        );
        client.send(stale.into());
        assert_eq!(bridge.poll().unwrap(), None);
        let UiMessage::Ack(stale_ack) = client.read() else {
            panic!("stale edit needs an acknowledgement");
        };
        assert_eq!(stale_ack.status, UiAckStatus::Stale);
        let UiMessage::Snapshot(snapshot) = client.read() else {
            panic!("stale edit needs a resync snapshot");
        };
        assert_eq!(snapshot.revision, 5);
    }

    #[test]
    fn signing_keys_are_redacted_and_invalid_participant_tokens_are_rejected() {
        let (_directory, mut bridge) = server();
        assert!(!format!("{bridge:?}").contains(TEST_SIGNING_KEY));
        let path = bridge.socket_path().unwrap().to_owned();
        let wrong = UiAttach::new(
            "wrong-token",
            "client-1",
            UiRendererMetadata::new("renderer-1", "web"),
            "main",
        );
        let mut client = TestClient::connect(&path, wrong);
        assert_eq!(bridge.poll().unwrap(), None);
        let UiMessage::Error(error) = client.read() else {
            panic!("wrong broker needs a generic rejection");
        };
        assert_eq!(error.code, "unauthorized");
        assert!(!error.message.contains(TEST_SIGNING_KEY));
    }

    #[test]
    fn attach_negotiates_the_highest_shared_protocol_version() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 1, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let offered = attach(
            "person-1",
            "client-1",
            "renderer-1",
            UiRendererState::component(),
        )
        .protocol_versions(1, 3);
        let mut client = TestClient::connect(&path, offered);

        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Attached { .. }
        ));
        let attached = expect_initial(&mut client, 1);
        assert_eq!(attached.protocol_version, UI_PROTOCOL_MAX_VERSION);
        assert_eq!(attached.min_protocol_version, UI_PROTOCOL_MIN_VERSION);
        assert_eq!(attached.max_protocol_version, UI_PROTOCOL_MAX_VERSION);
    }

    #[test]
    fn incompatible_protocol_ranges_are_rejected_per_connection() {
        let (_directory, mut bridge) = server();
        let path = bridge.socket_path().unwrap().to_owned();
        let offered = attach(
            "person-1",
            "client-1",
            "renderer-1",
            UiRendererState::component(),
        )
        .protocol_versions(UI_PROTOCOL_MAX_VERSION + 1, UI_PROTOCOL_MAX_VERSION + 2);
        let mut client = TestClient::connect(&path, offered);

        assert_eq!(bridge.poll().unwrap(), None);
        let UiMessage::Error(error) = client.read() else {
            panic!("incompatible renderer needs a connection-level rejection");
        };
        assert_eq!(error.code, "unsupportedProtocolVersion");
        assert_eq!(bridge.poll().unwrap(), None);
    }

    #[test]
    fn unattached_connections_expire_without_consuming_a_permanent_slot() {
        let (_directory, mut bridge) = server();
        let path = bridge.socket_path().unwrap().to_owned();
        let client = UnixStream::connect(path).unwrap();

        assert_eq!(bridge.poll().unwrap(), None);
        let server = bridge.server.as_mut().unwrap();
        assert_eq!(server.connections.len(), 1);
        server.connections[0].accepted_at = Instant::now() - ATTACH_DEADLINE;

        assert_eq!(bridge.poll().unwrap(), None);
        assert!(bridge.server.as_ref().unwrap().connections.is_empty());
        drop(client);
    }

    #[test]
    fn same_user_unix_peer_credentials_are_accepted() {
        let (first, _second) = UnixStream::pair().unwrap();
        assert_ne!(
            crate::process_security::peer_has_current_effective_uid(&first).unwrap(),
            Some(false)
        );
    }

    #[test]
    fn renderer_backpressure_detaches_only_that_renderer() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 1, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );
        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Attached { .. }
        ));
        let _ = expect_initial(&mut client, 1);

        let connection = bridge
            .server
            .as_mut()
            .unwrap()
            .connections
            .iter_mut()
            .find(|connection| connection.attachment.is_some())
            .unwrap();
        connection.pending_write_bytes = MAX_PENDING_WRITE_BYTES;

        assert_eq!(bridge.publish("main", 2, node("updated", 0)).unwrap(), 0);
        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Detached { .. }
        ));
    }

    #[test]
    fn flooding_renderer_is_detached_without_failing_poll() {
        let (_directory, mut bridge) = server();
        bridge.publish("main", 1, node("hello", 0)).unwrap();
        let path = bridge.socket_path().unwrap().to_owned();
        let mut client = TestClient::connect(
            &path,
            attach(
                "person-1",
                "client-1",
                "renderer-1",
                UiRendererState::component(),
            ),
        );
        assert!(matches!(
            poll_until(&mut bridge),
            UiBridgeEvent::Attached { .. }
        ));
        let _ = expect_initial(&mut client, 1);

        let mut actions = 0;
        let mut detached = false;
        for index in 0..=MAX_EVENT_HISTORY_PER_CLIENT {
            client.send(
                event(
                    &bridge,
                    "person-1",
                    "client-1",
                    "renderer-1",
                    &format!("event-{index}"),
                    1,
                )
                .into(),
            );
            if index % 8 == 7 || index == MAX_EVENT_HISTORY_PER_CLIENT {
                loop {
                    match bridge.poll().unwrap() {
                        Some(UiBridgeEvent::Action { .. }) => actions += 1,
                        Some(UiBridgeEvent::Detached { .. }) => {
                            detached = true;
                            break;
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
            }
            if detached {
                break;
            }
        }
        assert_eq!(actions, MAX_EVENT_HISTORY_PER_CLIENT);
        assert!(detached);
    }

    #[test]
    fn one_clients_replay_quota_never_evicts_another_clients_records() {
        let (_directory, mut bridge) = server();
        let protected = event(
            &bridge,
            "person-2",
            "client-2",
            "renderer-2",
            "protected-event",
            1,
        );
        let protected_key = (protected.client_id.clone(), protected.event_id.clone());
        assert!(bridge.remember_final_event(
            protected.clone(),
            make_ack(&protected, UiAckStatus::Applied, 2, None),
        ));

        for index in 0..(MAX_EVENT_HISTORY_PER_CLIENT + 16) {
            let candidate = event(
                &bridge,
                "person-1",
                "client-1",
                "renderer-1",
                &format!("final-{index}"),
                1,
            );
            assert!(bridge.remember_final_event(
                candidate.clone(),
                make_ack(&candidate, UiAckStatus::Applied, 2, None),
            ));
        }
        assert!(bridge.events.contains_key(&protected_key));
        assert_eq!(
            bridge
                .events
                .keys()
                .filter(|(client_id, _)| client_id.as_str() == "client-1")
                .count(),
            MAX_EVENT_HISTORY_PER_CLIENT
        );

        for index in 0..MAX_EVENT_HISTORY_PER_CLIENT {
            let pending = event(
                &bridge,
                "person-3",
                "client-3",
                "renderer-3",
                &format!("pending-{index}"),
                1,
            );
            assert!(bridge.remember_pending_event(pending));
        }
        let overflow = event(
            &bridge,
            "person-3",
            "client-3",
            "renderer-3",
            "pending-overflow",
            1,
        );
        assert!(!bridge.remember_pending_event(overflow));
        assert!(bridge.events.contains_key(&protected_key));
    }

    #[test]
    fn terminal_rendering_follows_terminal_visibility_and_connection_fallback() {
        let hidden = UiRendererState::hidden();
        assert!(should_render_terminal(std::iter::empty()));
        assert!(!should_render_terminal([hidden].into_iter()));
        assert!(!should_render_terminal(
            [hidden, UiRendererState::component()].into_iter()
        ));
        assert!(should_render_terminal(
            [hidden, UiRendererState::terminal()].into_iter()
        ));
    }
}
