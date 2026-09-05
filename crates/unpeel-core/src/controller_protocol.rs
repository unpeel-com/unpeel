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
pub const HOST_PROTOCOL_MINOR: u16 = 15;

pub const NATIVE_HOST_CAPABILITIES: &[&str] = &[
    "approval.answer",
    "approval.list",
    "artifact.delete",
    "artifact.list",
    "artifact.read",
    "artifact.request_screenshot",
    "artifact.upload",
    "artifact.upload.resumable",
    "apps.install",
    "apps.open",
    "host.bootstrap",
    "host.mobile.tls",
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
    "settings.openers.set",
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
    "apps.install",
    "apps.open",
    "host.bootstrap",
    "host.mobile.tls",
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
    "settings.openers.set",
    "settings.workspace.set",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostProtocolDescriptor {
    pub major_version: u16,
    pub minor_version: u16,
    pub capabilities: Vec<String>,
}

/// The capability a Host advertises only when it can actually install Apps:
/// there has to be a published App release target for its platform
/// (`app_installer::release_target()`), which is a Host-side fact.
pub const APPS_INSTALL_CAPABILITY: &str = "apps.install";

impl HostProtocolDescriptor {
    /// The descriptor a native (Mac app) Host advertises in bootstrap.
    #[cfg(feature = "native-host")]
    pub fn native_v1() -> Self {
        Self::advertised_v1(
            NATIVE_HOST_CAPABILITIES,
            crate::app_installer::release_target().is_some(),
        )
    }

    /// The descriptor a headless (`unpeel serve`) Host advertises in bootstrap.
    #[cfg(feature = "native-host")]
    pub fn headless_v1() -> Self {
        Self::advertised_v1(
            HEADLESS_HOST_CAPABILITIES,
            crate::app_installer::release_target().is_some(),
        )
    }

    /// Build a protocol-1 descriptor from a capability set. Pure and portable:
    /// the only Host-side input is whether App installation is available on
    /// this platform, which decides if [`APPS_INSTALL_CAPABILITY`] is listed.
    /// Hosts use [`Self::native_v1`] / [`Self::headless_v1`]; Controllers and
    /// their tests build expected descriptors through this constructor.
    pub fn advertised_v1(capabilities: &[&str], app_install_available: bool) -> Self {
        Self {
            major_version: HOST_PROTOCOL_MAJOR,
            minor_version: HOST_PROTOCOL_MINOR,
            capabilities: capabilities
                .iter()
                .filter(|capability| {
                    **capability != APPS_INSTALL_CAPABILITY || app_install_available
                })
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

    fn ledger() -> Ledger {
        serde_json::from_str(include_str!("../../../protocol/host-capabilities-v1.json"))
            .expect("valid host capability ledger")
    }

    fn ledger_ids(ledger: Ledger, select: fn(&LedgerCapability) -> bool) -> Vec<String> {
        ledger
            .capabilities
            .into_iter()
            .filter(select)
            .map(|entry| entry.id)
            .collect()
    }

    #[test]
    fn ledger_protocol_version_matches_the_constants() {
        let ledger = ledger();
        assert_eq!(ledger.schema_version, 1);
        assert_eq!(ledger.protocol.major, HOST_PROTOCOL_MAJOR);
        assert_eq!(ledger.protocol.minor, HOST_PROTOCOL_MINOR);
    }

    #[test]
    fn headless_capability_set_matches_the_canonical_ledger() {
        let expected = ledger_ids(ledger(), |entry| entry.tui);
        let advertised =
            HostProtocolDescriptor::advertised_v1(HEADLESS_HOST_CAPABILITIES, true).capabilities;
        assert_eq!(advertised, expected);
    }

    #[test]
    fn native_capability_set_matches_the_canonical_ledger() {
        let expected = ledger_ids(ledger(), |entry| entry.native);
        let advertised =
            HostProtocolDescriptor::advertised_v1(NATIVE_HOST_CAPABILITIES, true).capabilities;
        assert_eq!(advertised, expected);
    }

    #[test]
    fn app_install_is_the_only_capability_that_depends_on_a_release_target() {
        for set in [HEADLESS_HOST_CAPABILITIES, NATIVE_HOST_CAPABILITIES] {
            let with = HostProtocolDescriptor::advertised_v1(set, true);
            let without = HostProtocolDescriptor::advertised_v1(set, false);
            assert!(with.supports(APPS_INSTALL_CAPABILITY));
            assert!(!without.supports(APPS_INSTALL_CAPABILITY));
            assert!(with.supports("apps.open") && without.supports("apps.open"));
            let mut expected = with.capabilities.clone();
            expected.retain(|id| id != APPS_INSTALL_CAPABILITY);
            assert_eq!(without.capabilities, expected);
            assert_eq!(
                (with.major_version, with.minor_version),
                (1, HOST_PROTOCOL_MINOR)
            );
        }
    }

    /// The ledger's `protocol.history` records which capability ids each
    /// additive minor introduced. The 0.6 openers and the `/mobile` TLS change
    /// ship together under minor 15; every id is advertised individually (a
    /// Controller checks ids, never the minor), so each must be in both
    /// capability sets and in the ledger's capability list.
    #[test]
    fn minor_history_capabilities_are_advertised_individually() {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../../protocol/host-capabilities-v1.json"))
                .expect("valid host capability ledger");
        let history = raw["protocol"]["history"]
            .as_array()
            .expect("protocol.history array");
        let latest = history
            .iter()
            .filter_map(|entry| entry["minor"].as_u64())
            .max()
            .expect("at least one history entry");
        assert_eq!(latest, u64::from(HOST_PROTOCOL_MINOR));

        let minor_15 = history
            .iter()
            .find(|entry| entry["minor"].as_u64() == Some(15))
            .expect("minor 15 history entry");
        let ids: Vec<&str> = minor_15["capabilities"]
            .as_array()
            .expect("capabilities array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "apps.install",
                "apps.open",
                "host.mobile.tls",
                "settings.openers.set"
            ]
        );
        let ledger_both = ledger_ids(ledger(), |entry| entry.native && entry.tui);
        for entry in history {
            for id in entry["capabilities"]
                .as_array()
                .expect("capabilities array")
                .iter()
                .filter_map(|value| value.as_str())
            {
                assert!(
                    HEADLESS_HOST_CAPABILITIES.contains(&id),
                    "{id} missing from headless set"
                );
                assert!(
                    NATIVE_HOST_CAPABILITIES.contains(&id),
                    "{id} missing from native set"
                );
                assert!(
                    ledger_both.contains(&id.to_owned()),
                    "{id} missing from the canonical ledger"
                );
            }
        }
    }

    #[cfg(feature = "native-host")]
    #[test]
    fn host_descriptors_follow_the_platform_release_target() {
        let available = crate::app_installer::release_target().is_some();
        assert_eq!(
            HostProtocolDescriptor::headless_v1(),
            HostProtocolDescriptor::advertised_v1(HEADLESS_HOST_CAPABILITIES, available)
        );
        assert_eq!(
            HostProtocolDescriptor::native_v1(),
            HostProtocolDescriptor::advertised_v1(NATIVE_HOST_CAPABILITIES, available)
        );
    }

    #[test]
    fn compatibility_is_major_version_only() {
        let descriptor = HostProtocolDescriptor::advertised_v1(HEADLESS_HOST_CAPABILITIES, false);
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
