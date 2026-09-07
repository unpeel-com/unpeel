//! Host-minted, route-bound credentials for App Kit UI participants.
//!
//! `UNPEEL_UI_TOKEN` is a per-App-session signing key shared only by the Host
//! and authoritative App process. Renderers and neighboring agent sessions
//! receive derived credentials whose signed claims bind their identity,
//! grants, and exact attachment route.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{
    ClientId, RendererId, UiParticipant, ViewId, ui::validate_identifier_for_protocol,
    ui::validate_participant_for_protocol,
};

/// Version of the signed participant-token claims.
pub const UI_PARTICIPANT_TOKEN_VERSION: u32 = 1;
/// Visible token prefix, allowing later credential formats to coexist.
pub const UI_PARTICIPANT_TOKEN_PREFIX: &str = "upui1";

const MIN_SIGNING_KEY_BYTES: usize = 32;
const MAX_PARTICIPANT_TOKEN_BYTES: usize = 16 * 1024;
const CLOCK_SKEW_MILLIS: u64 = 30_000;

type HmacSha256 = Hmac<Sha256>;

/// Host-attested identity and scope carried inside a participant token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiParticipantTokenClaims {
    pub version: u32,
    pub app_session_id: String,
    pub participant: UiParticipant,
    pub client_id: ClientId,
    pub renderer_id: RendererId,
    pub view_id: ViewId,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
    pub token_id: String,
}

impl UiParticipantTokenClaims {
    /// Creates claims for one exact local socket attachment route.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        app_session_id: impl Into<String>,
        participant: UiParticipant,
        client_id: impl Into<ClientId>,
        renderer_id: impl Into<RendererId>,
        view_id: impl Into<ViewId>,
        issued_at_unix_ms: u64,
        expires_at_unix_ms: u64,
        token_id: impl Into<String>,
    ) -> Self {
        Self {
            version: UI_PARTICIPANT_TOKEN_VERSION,
            app_session_id: app_session_id.into(),
            participant,
            client_id: client_id.into(),
            renderer_id: renderer_id.into(),
            view_id: view_id.into(),
            issued_at_unix_ms,
            expires_at_unix_ms,
            token_id: token_id.into(),
        }
    }

    fn validate(&self) -> Result<(), UiParticipantTokenError> {
        if self.version != UI_PARTICIPANT_TOKEN_VERSION {
            return Err(UiParticipantTokenError::UnsupportedVersion(self.version));
        }
        for (path, value) in [
            ("appSessionId", self.app_session_id.as_str()),
            ("clientId", self.client_id.as_str()),
            ("rendererId", self.renderer_id.as_str()),
            ("viewId", self.view_id.as_str()),
            ("tokenId", self.token_id.as_str()),
        ] {
            validate_identifier_for_protocol(value, path)
                .map_err(|error| UiParticipantTokenError::InvalidClaims(error.to_string()))?;
        }
        validate_participant_for_protocol(&self.participant, "participant")
            .map_err(|error| UiParticipantTokenError::InvalidClaims(error.to_string()))?;
        if self.participant.kind == crate::UiParticipantKind::Agent
            && self.participant.source_session_id.is_none()
        {
            return Err(UiParticipantTokenError::InvalidClaims(
                "participant.sourceSessionId is required for an agent credential".to_owned(),
            ));
        }
        if self.expires_at_unix_ms <= self.issued_at_unix_ms {
            return Err(UiParticipantTokenError::InvalidClaims(
                "expiresAtUnixMs must follow issuedAtUnixMs".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Host-side helper for minting scoped credentials from a per-session key.
#[derive(Clone)]
pub struct UiParticipantTokenIssuer {
    signing_key: Vec<u8>,
    app_session_id: String,
}

impl fmt::Debug for UiParticipantTokenIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiParticipantTokenIssuer")
            .field("signing_key", &"[REDACTED]")
            .field("app_session_id", &self.app_session_id)
            .finish()
    }
}

impl Drop for UiParticipantTokenIssuer {
    fn drop(&mut self) {
        self.signing_key.fill(0);
    }
}

impl UiParticipantTokenIssuer {
    pub fn new(
        signing_key: impl AsRef<[u8]>,
        app_session_id: impl Into<String>,
    ) -> Result<Self, UiParticipantTokenError> {
        let signing_key = signing_key.as_ref();
        validate_signing_key(signing_key)?;
        let app_session_id = app_session_id.into();
        validate_identifier_for_protocol(&app_session_id, "appSessionId")
            .map_err(|error| UiParticipantTokenError::InvalidClaims(error.to_string()))?;
        Ok(Self {
            signing_key: signing_key.to_vec(),
            app_session_id,
        })
    }

    /// Signs pre-built claims after enforcing this issuer's session audience.
    pub fn sign(
        &self,
        claims: &UiParticipantTokenClaims,
    ) -> Result<String, UiParticipantTokenError> {
        claims.validate()?;
        if claims.app_session_id != self.app_session_id {
            return Err(UiParticipantTokenError::RouteMismatch("appSessionId"));
        }
        let payload = serde_json::to_vec(claims).map_err(UiParticipantTokenError::Json)?;
        let encoded_payload = URL_SAFE_NO_PAD.encode(payload);
        let signing_input = format!("{UI_PARTICIPANT_TOKEN_PREFIX}.{encoded_payload}");
        let mut mac = HmacSha256::new_from_slice(&self.signing_key)
            .map_err(|_| UiParticipantTokenError::SigningKeyTooShort)?;
        mac.update(signing_input.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        let token = format!("{signing_input}.{signature}");
        if token.len() > MAX_PARTICIPANT_TOKEN_BYTES {
            return Err(UiParticipantTokenError::TokenTooLarge);
        }
        Ok(token)
    }

    /// Mints a credential valid for `valid_for`, using the current wall clock.
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        &self,
        participant: UiParticipant,
        client_id: impl Into<ClientId>,
        renderer_id: impl Into<RendererId>,
        view_id: impl Into<ViewId>,
        token_id: impl Into<String>,
        valid_for: Duration,
    ) -> Result<String, UiParticipantTokenError> {
        self.issue_at(
            participant,
            client_id,
            renderer_id,
            view_id,
            token_id,
            unix_time_millis()?,
            valid_for,
        )
    }

    /// Deterministic variant used by Host conformance tests.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_at(
        &self,
        participant: UiParticipant,
        client_id: impl Into<ClientId>,
        renderer_id: impl Into<RendererId>,
        view_id: impl Into<ViewId>,
        token_id: impl Into<String>,
        issued_at_unix_ms: u64,
        valid_for: Duration,
    ) -> Result<String, UiParticipantTokenError> {
        let validity_ms = u64::try_from(valid_for.as_millis())
            .map_err(|_| UiParticipantTokenError::InvalidLifetime)?;
        if validity_ms == 0 {
            return Err(UiParticipantTokenError::InvalidLifetime);
        }
        let expires_at_unix_ms = issued_at_unix_ms
            .checked_add(validity_ms)
            .ok_or(UiParticipantTokenError::InvalidLifetime)?;
        self.sign(&UiParticipantTokenClaims::new(
            self.app_session_id.clone(),
            participant,
            client_id,
            renderer_id,
            view_id,
            issued_at_unix_ms,
            expires_at_unix_ms,
            token_id,
        ))
    }
}

/// App-side verifier for credentials minted by the owning Host.
#[derive(Clone)]
pub struct UiParticipantTokenVerifier {
    signing_key: Vec<u8>,
    app_session_id: String,
}

impl fmt::Debug for UiParticipantTokenVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UiParticipantTokenVerifier")
            .field("signing_key", &"[REDACTED]")
            .field("app_session_id", &self.app_session_id)
            .finish()
    }
}

impl Drop for UiParticipantTokenVerifier {
    fn drop(&mut self) {
        self.signing_key.fill(0);
    }
}

impl UiParticipantTokenVerifier {
    pub fn new(
        signing_key: impl AsRef<[u8]>,
        app_session_id: impl Into<String>,
    ) -> Result<Self, UiParticipantTokenError> {
        let signing_key = signing_key.as_ref();
        validate_signing_key(signing_key)?;
        let app_session_id = app_session_id.into();
        validate_identifier_for_protocol(&app_session_id, "appSessionId")
            .map_err(|error| UiParticipantTokenError::InvalidClaims(error.to_string()))?;
        Ok(Self {
            signing_key: signing_key.to_vec(),
            app_session_id,
        })
    }

    /// Verifies signature, lifetime, session audience, and the exact route.
    pub fn verify(
        &self,
        token: &str,
        client_id: &ClientId,
        renderer_id: &RendererId,
        view_id: &ViewId,
    ) -> Result<UiParticipantTokenClaims, UiParticipantTokenError> {
        self.verify_at(token, client_id, renderer_id, view_id, unix_time_millis()?)
    }

    /// Deterministic variant used by protocol and expiry tests.
    pub fn verify_at(
        &self,
        token: &str,
        client_id: &ClientId,
        renderer_id: &RendererId,
        view_id: &ViewId,
        now_unix_ms: u64,
    ) -> Result<UiParticipantTokenClaims, UiParticipantTokenError> {
        if token.is_empty() || token.len() > MAX_PARTICIPANT_TOKEN_BYTES {
            return Err(UiParticipantTokenError::MalformedToken);
        }
        let mut parts = token.split('.');
        let (Some(prefix), Some(payload), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(UiParticipantTokenError::MalformedToken);
        };
        if prefix != UI_PARTICIPANT_TOKEN_PREFIX {
            return Err(UiParticipantTokenError::MalformedToken);
        }

        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| UiParticipantTokenError::MalformedToken)?;
        let signing_input = format!("{prefix}.{payload}");
        let mut mac = HmacSha256::new_from_slice(&self.signing_key)
            .map_err(|_| UiParticipantTokenError::SigningKeyTooShort)?;
        mac.update(signing_input.as_bytes());
        mac.verify_slice(&signature)
            .map_err(|_| UiParticipantTokenError::InvalidSignature)?;

        let payload = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|_| UiParticipantTokenError::MalformedToken)?;
        let claims: UiParticipantTokenClaims =
            serde_json::from_slice(&payload).map_err(UiParticipantTokenError::Json)?;
        claims.validate()?;
        if claims.app_session_id != self.app_session_id {
            return Err(UiParticipantTokenError::RouteMismatch("appSessionId"));
        }
        if &claims.client_id != client_id {
            return Err(UiParticipantTokenError::RouteMismatch("clientId"));
        }
        if &claims.renderer_id != renderer_id {
            return Err(UiParticipantTokenError::RouteMismatch("rendererId"));
        }
        if &claims.view_id != view_id {
            return Err(UiParticipantTokenError::RouteMismatch("viewId"));
        }
        if claims.issued_at_unix_ms > now_unix_ms.saturating_add(CLOCK_SKEW_MILLIS) {
            return Err(UiParticipantTokenError::NotYetValid);
        }
        if claims.expires_at_unix_ms <= now_unix_ms {
            return Err(UiParticipantTokenError::Expired);
        }
        Ok(claims)
    }
}

/// Credential creation or verification failure.
#[derive(Debug)]
pub enum UiParticipantTokenError {
    SigningKeyTooShort,
    InvalidLifetime,
    Clock,
    TokenTooLarge,
    MalformedToken,
    InvalidSignature,
    UnsupportedVersion(u32),
    InvalidClaims(String),
    RouteMismatch(&'static str),
    NotYetValid,
    Expired,
    Json(serde_json::Error),
}

impl fmt::Display for UiParticipantTokenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SigningKeyTooShort => write!(
                formatter,
                "UI participant signing key must contain at least {MIN_SIGNING_KEY_BYTES} bytes"
            ),
            Self::InvalidLifetime => formatter.write_str("invalid UI participant token lifetime"),
            Self::Clock => formatter.write_str("system clock is before the Unix epoch"),
            Self::TokenTooLarge => formatter.write_str("UI participant token exceeds its limit"),
            Self::MalformedToken => formatter.write_str("malformed UI participant token"),
            Self::InvalidSignature => formatter.write_str("invalid UI participant token signature"),
            Self::UnsupportedVersion(version) => {
                write!(
                    formatter,
                    "unsupported UI participant token version {version}"
                )
            }
            Self::InvalidClaims(message) => {
                write!(formatter, "invalid UI participant claims: {message}")
            }
            Self::RouteMismatch(field) => {
                write!(formatter, "UI participant token does not match {field}")
            }
            Self::NotYetValid => formatter.write_str("UI participant token is not yet valid"),
            Self::Expired => formatter.write_str("UI participant token expired"),
            Self::Json(error) => write!(formatter, "invalid UI participant claims JSON: {error}"),
        }
    }
}

impl std::error::Error for UiParticipantTokenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}

fn validate_signing_key(signing_key: &[u8]) -> Result<(), UiParticipantTokenError> {
    if signing_key.len() < MIN_SIGNING_KEY_BYTES {
        Err(UiParticipantTokenError::SigningKeyTooShort)
    } else {
        Ok(())
    }
}

fn unix_time_millis() -> Result<u64, UiParticipantTokenError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UiParticipantTokenError::Clock)?
        .as_millis();
    u64::try_from(millis).map_err(|_| UiParticipantTokenError::Clock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UiGrant, UiParticipantKind};

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn agent() -> UiParticipant {
        UiParticipant::new("agent:session-neighbor")
            .kind(UiParticipantKind::Agent)
            .source_session_id("session-neighbor")
            .display_name("Review agent")
            .grants([UiGrant::VIEW, UiGrant::EDIT])
    }

    #[test]
    fn scoped_token_round_trips_and_binds_every_route_field() {
        let issuer = UiParticipantTokenIssuer::new(KEY, "session-app").unwrap();
        let token = issuer
            .issue_at(
                agent(),
                "client-agent",
                "renderer-agent",
                "main",
                "token-1",
                1_000_000,
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(!token.contains("agent:session-neighbor"));

        let verifier = UiParticipantTokenVerifier::new(KEY, "session-app").unwrap();
        let claims = verifier
            .verify_at(
                &token,
                &"client-agent".into(),
                &"renderer-agent".into(),
                &"main".into(),
                1_010_000,
            )
            .unwrap();
        assert_eq!(claims.participant.kind, UiParticipantKind::Agent);
        assert!(claims.participant.allows(UiGrant::EDIT));
        assert!(!claims.participant.allows(UiGrant::ADMIN));

        assert!(matches!(
            verifier.verify_at(
                &token,
                &"other-client".into(),
                &"renderer-agent".into(),
                &"main".into(),
                1_010_000,
            ),
            Err(UiParticipantTokenError::RouteMismatch("clientId"))
        ));
    }

    #[test]
    fn tampering_and_expiry_fail_closed() {
        let issuer = UiParticipantTokenIssuer::new(KEY, "session-app").unwrap();
        let token = issuer
            .issue_at(
                agent(),
                "client-agent",
                "renderer-agent",
                "main",
                "token-1",
                1_000_000,
                Duration::from_secs(1),
            )
            .unwrap();
        let verifier = UiParticipantTokenVerifier::new(KEY, "session-app").unwrap();
        assert!(matches!(
            verifier.verify_at(
                &token,
                &"client-agent".into(),
                &"renderer-agent".into(),
                &"main".into(),
                1_001_000,
            ),
            Err(UiParticipantTokenError::Expired)
        ));

        let mut tampered = token.into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'A' { b'B' } else { b'A' };
        assert!(matches!(
            verifier.verify_at(
                std::str::from_utf8(&tampered).unwrap(),
                &"client-agent".into(),
                &"renderer-agent".into(),
                &"main".into(),
                1_000_500,
            ),
            Err(UiParticipantTokenError::InvalidSignature)
                | Err(UiParticipantTokenError::MalformedToken)
        ));
    }
}
