//! Typed App resource grammar shared by Hosts and Controllers.
//!
//! An `apps.open` request names a resource *kind* (`file`, `design`, …) that
//! an installed App declares in the registry (`protocol/app-registry.json`,
//! `resourceKinds`). Hosts validate the kind before dispatching an opener and
//! Controllers validate it before putting a request on the wire, so the
//! grammar lives here — compiled into the portable `controller-core` feature
//! set — while installation, catalog resolution, and opener dispatch stay in
//! the Host-only `apps_mcp` / `app_installer` / `app_open` modules.

/// The one resource kind every Host understands without an App: a file on
/// the Host's disk, opened through the user's typed opener policy.
pub const FILE_RESOURCE_KIND: &str = "file";

/// A resource kind is a short ASCII identifier: letters, digits, `.`, `_`,
/// `-`, at most 100 bytes. Anything else is rejected before it can reach a
/// registry lookup or a Host route.
pub fn valid_resource_kind(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_registry_style_kinds() {
        for kind in [
            FILE_RESOURCE_KIND,
            "design",
            "design.frame",
            "note_v2",
            "issue-42",
        ] {
            assert!(valid_resource_kind(kind), "{kind:?} should be valid");
        }
    }

    #[test]
    fn rejects_empty_oversized_and_non_identifier_kinds() {
        assert!(!valid_resource_kind(""));
        assert!(!valid_resource_kind(&"k".repeat(101)));
        assert!(valid_resource_kind(&"k".repeat(100)));
        for kind in [
            "with space",
            "slash/kind",
            "colon:kind",
            "nul\0",
            "ünïcode",
            "a\n",
        ] {
            assert!(!valid_resource_kind(kind), "{kind:?} should be rejected");
        }
    }
}
