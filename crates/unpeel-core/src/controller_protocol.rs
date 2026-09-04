//! Versioned Controller ↔ Host capability contract.
//!
//! The shipped `/mobile/*` DTO remains protocol version 1. This descriptor is
//! additive: old controllers ignore it, while new controllers use capability
//! presence instead of guessing from Host kind or treating a 404 as feature
//! discovery. The canonical cross-implementation ledger lives at
//! `protocol/host-capabilities-v1.json` and tests keep these constants aligned
//! with it.

use serde::{Deserialize, Serialize};

pub const HOST_PROTOCOL_MAJOR: u16 = 1;
pub const HOST_PROTOCOL_MINOR: u16 = 14;

pub const NATIVE_HOST_CAPABILITIES: &[&str] = &[
    "approval.answer",
    "approval.list",
    "artifact.delete",
    "artifact.list",
    "artifact.read",
    "artifact.request_screenshot",
    "artifact.upload",
    "artifact.upload.resumable",
    "host.bootstrap",
    "pairing.create",
    "pairing.invitation",
    "project.organization.set",
    "project.pin.set",
    "push.register",
    "relay.credentials.recover",
    "session.archive",
    "session.archive.list",
    "session.create",
    "session.input.write",
    "session.mark_read",
    "session.metrics.read",
    "session.notify_when_done.set",
    "session.order.set",
    "session.output.read",
    "session.output.subscribe",
    "session.pin.set",
    "session.project.set",
    "session.remove",
    "session.resize",
    "session.resize_desktop",
    "session.restart",
    "session.restore",
    "session.runtime.resume",
    "session.stop",
    "session.title.set",
    "session.transcript.markdown",
    "settings.presets.set",
    "settings.workspace.set",
];

pub const HEADLESS_HOST_CAPABILITIES: &[&str] = &[
    "approval.answer",
    "approval.list",
    "artifact.delete",
    "artifact.list",
    "artifact.read",
    "artifact.request_screenshot",
    "artifact.upload.resumable",
    "host.bootstrap",
    "pairing.create",
    "pairing.invitation",
    "project.organization.set",
    "project.pin.set",
    "session.archive",
    "session.archive.list",
    "session.create",
    "session.input.write",
    "session.mark_read",
    "session.metrics.read",
    "session.order.set",
    "session.output.read",
    "session.output.subscribe",
    "session.pin.set",
    "session.project.set",
    "session.remove",
    "session.resize",
    "session.resize_desktop",
    "session.restart",
    "session.restore",
    "session.runtime.resume",
    "session.stop",
    "session.title.set",
    "session.transcript.markdown",
    "settings.presets.set",
    "settings.workspace.set",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProtocolDescriptor {
    pub major_version: u16,
    pub minor_version: u16,
    pub capabilities: Vec<String>,
}

impl HostProtocolDescriptor {
    pub fn native_v1() -> Self {
        Self::v1(NATIVE_HOST_CAPABILITIES)
    }

    pub fn headless_v1() -> Self {
        Self::v1(HEADLESS_HOST_CAPABILITIES)
    }

    fn v1(capabilities: &[&str]) -> Self {
        Self {
            major_version: HOST_PROTOCOL_MAJOR,
            minor_version: HOST_PROTOCOL_MINOR,
            capabilities: capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
        }
    }

    /// Major versions must match. Minor versions and unknown capability ids
    /// are additive; a controller checks the operation it needs explicitly.
    pub fn is_compatible_with(&self, controller_major: u16) -> bool {
        self.major_version == controller_major
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|value| value == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Ledger {
        schema_version: u16,
        protocol: LedgerProtocol,
        capabilities: Vec<LedgerCapability>,
    }

    #[derive(Deserialize)]
    struct LedgerProtocol {
        major: u16,
        minor: u16,
    }

    #[derive(Deserialize)]
    struct LedgerCapability {
        id: String,
        native: bool,
        tui: bool,
    }

    #[test]
    fn headless_descriptor_matches_the_canonical_ledger() {
        let ledger: Ledger =
            serde_json::from_str(include_str!("../../../protocol/host-capabilities-v1.json"))
                .expect("valid host capability ledger");
        assert_eq!(ledger.schema_version, 1);
        assert_eq!(ledger.protocol.major, HOST_PROTOCOL_MAJOR);
        assert_eq!(ledger.protocol.minor, HOST_PROTOCOL_MINOR);

        let expected: Vec<String> = ledger
            .capabilities
            .into_iter()
            .filter(|entry| entry.tui)
            .map(|entry| entry.id)
            .collect();
        assert_eq!(HostProtocolDescriptor::headless_v1().capabilities, expected);
    }

    #[test]
    fn native_descriptor_matches_the_canonical_ledger() {
        let ledger: Ledger =
            serde_json::from_str(include_str!("../../../protocol/host-capabilities-v1.json"))
                .expect("valid host capability ledger");
        let expected: Vec<String> = ledger
            .capabilities
            .into_iter()
            .filter(|entry| entry.native)
            .map(|entry| entry.id)
            .collect();
        assert_eq!(HostProtocolDescriptor::native_v1().capabilities, expected);
    }

    #[test]
    fn compatibility_is_major_version_only() {
        let descriptor = HostProtocolDescriptor::headless_v1();
        assert!(descriptor.is_compatible_with(1));
        assert!(!descriptor.is_compatible_with(2));
        assert!(descriptor.supports("host.bootstrap"));
        assert!(!descriptor.supports("artifact.upload"));
        assert!(descriptor.supports("artifact.upload.resumable"));
        assert!(descriptor.supports("session.runtime.resume"));
        assert!(!descriptor.supports("session.runtime.restart"));
    }

    #[test]
    fn bootstrap_compatibility_fixture_covers_legacy_future_and_major_mismatch() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../protocol/host-bootstrap-compatibility-v1.json"
        ))
        .expect("valid compatibility fixture");
        let cases = fixture["cases"].as_array().expect("fixture cases");
        for case in cases {
            let bootstrap = case["bootstrap"].clone();
            let advertised = bootstrap.get("hostProtocol").cloned();
            let compatible = advertised
                .map(serde_json::from_value::<HostProtocolDescriptor>)
                .transpose()
                .expect("descriptor decodes")
                .map(|descriptor| descriptor.is_compatible_with(HOST_PROTOCOL_MAJOR));
            assert_eq!(
                compatible,
                case["compatible"].as_bool(),
                "case {}",
                case["id"]
            );
            let typed: crate::remote_session_backend::RemoteBootstrapSnapshot =
                serde_json::from_value(bootstrap).expect("bootstrap snapshot decodes");
            if case["id"] == "current-host" {
                assert_eq!(
                    typed.sessions[0].active_runtime_id.as_deref(),
                    Some("claude")
                );
                assert_eq!(typed.sessions[0].provider_id, None);
            }
        }
    }
}
