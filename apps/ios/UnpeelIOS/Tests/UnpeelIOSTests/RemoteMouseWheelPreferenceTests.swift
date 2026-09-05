import XCTest
@testable import UnpeelIOS

/// The wheel-forwarding gate must recognise an agent by ANY identity a
/// session summary carries: a Claude started by hand in a shell, or through
/// a wrapper/preset whose command head is not literally `claude`, only
/// shows up in the Host-observed `activeRuntimeID`.
final class RemoteMouseWheelPreferenceTests: XCTestCase {
    @MainActor
    func testObservedRuntimeQualifiesWithoutProviderOrCommandHead() {
        XCTAssertTrue(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: nil,
            command: "/bin/zsh",
            activeRuntimeID: "Claude"
        ))
        XCTAssertTrue(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: nil,
            command: "~/bin/agent-wrapper --profile work",
            activeRuntimeID: "claude"
        ))
    }

    @MainActor
    func testLegacyProviderAndCommandHeadStillQualify() {
        XCTAssertTrue(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: "claude",
            command: "/bin/zsh",
            activeRuntimeID: nil
        ))
        XCTAssertTrue(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: nil,
            command: "/opt/homebrew/bin/claude --resume",
            activeRuntimeID: nil
        ))
    }

    @MainActor
    func testPlainShellDoesNotQualify() {
        XCTAssertFalse(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: nil,
            command: "/bin/zsh",
            activeRuntimeID: nil
        ))
        XCTAssertFalse(RemoteGhosttyRenderer.prefersRemoteMouseWheel(
            providerID: nil,
            command: "/bin/zsh",
            activeRuntimeID: "vim"
        ))
    }

    // MARK: - Forwarding decision once the Host's mode snapshot arrived

    @MainActor
    func testClassicClaudeWithSnapshotLeavesFlickToLocalScrollback() {
        // Classic (non-full-screen) Claude: the Host snapshot says set
        // [1004, 2004] — no alt screen, no mouse. Forwarding SGR wheel bytes
        // would be ignored by the TUI and the phone could not scroll at all.
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: true,
            mouseTrackingEnabled: false,
            alternateScreenEnabled: false,
            sawMouseOrAlternateDisable: false,
            providerPrefersRemoteMouseWheel: true
        ), .none)
    }

    @MainActor
    func testFullScreenClaudeWithSnapshotForwardsWheel() {
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: true,
            mouseTrackingEnabled: true,
            alternateScreenEnabled: true,
            sawMouseOrAlternateDisable: false,
            providerPrefersRemoteMouseWheel: true
        ), .mouse)
    }

    @MainActor
    func testNoSnapshotYetKeepsProviderHeuristicUntilDisableSeen() {
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: false,
            mouseTrackingEnabled: false,
            alternateScreenEnabled: false,
            sawMouseOrAlternateDisable: false,
            providerPrefersRemoteMouseWheel: true
        ), .mouse)
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: false,
            mouseTrackingEnabled: false,
            alternateScreenEnabled: false,
            sawMouseOrAlternateDisable: true,
            providerPrefersRemoteMouseWheel: true
        ), .none)
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: false,
            mouseTrackingEnabled: false,
            alternateScreenEnabled: false,
            sawMouseOrAlternateDisable: false,
            providerPrefersRemoteMouseWheel: false
        ), .none)
    }

    @MainActor
    func testAlternateScreenWithoutMouseEmulatesAlternateScroll() {
        XCTAssertEqual(RemoteGhosttyRenderer.wheelForwarding(
            hasHostModeSnapshot: true,
            mouseTrackingEnabled: false,
            alternateScreenEnabled: true,
            sawMouseOrAlternateDisable: false,
            providerPrefersRemoteMouseWheel: false
        ), .alternateScroll)
        XCTAssertEqual(
            RemoteGhosttyRenderer.alternateScrollSequence(
                direction: .down, steps: 2, applicationCursorKeys: false
            ),
            "\u{1B}[B\u{1B}[B"
        )
        XCTAssertEqual(
            RemoteGhosttyRenderer.alternateScrollSequence(
                direction: .up, steps: 1, applicationCursorKeys: true
            ),
            "\u{1B}OA"
        )
        XCTAssertNil(RemoteGhosttyRenderer.alternateScrollSequence(
            direction: .left, steps: 1, applicationCursorKeys: false
        ))
    }
}
