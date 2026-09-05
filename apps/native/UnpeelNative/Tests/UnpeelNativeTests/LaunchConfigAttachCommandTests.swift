import XCTest
@testable import UnpeelNative

final class LaunchConfigAttachCommandTests: XCTestCase {
    /// Another local workspace selected in this window keeps the direct
    /// `unpeel-attach` data plane: the surface command targets that
    /// workspace's home and `app-sessions` directory explicitly.
    func testScopedWorkspaceAttachCommandTargetsThatHome() {
        let home = URL(fileURLWithPath: "/Users/me/.unpeel/profiles/writing", isDirectory: true)
        let sessionsDir = home.appendingPathComponent("app-sessions", isDirectory: true)
        let command = LaunchConfig.attachCommand(sessionID: "abc123", sessionsDir: sessionsDir)

        XCTAssertTrue(command.hasSuffix(" abc123"), command)
        XCTAssertTrue(
            command.contains("--sessions-dir /Users/me/.unpeel/profiles/writing/app-sessions"),
            command
        )
        if ProcessInfo.processInfo.environment[LaunchConfig.attachCommandEnvVar] == nil {
            XCTAssertTrue(command.hasPrefix("direct:/usr/bin/env UNPEEL_HOME=/Users/me/.unpeel/profiles/writing "), command)
            XCTAssertTrue(command.contains("unpeel-attach"), command)
        }
    }

    /// The instance's own home keeps the byte-for-byte historical command.
    func testOwnHomeAttachCommandIsUnchanged() {
        XCTAssertEqual(
            LaunchConfig.attachCommand(sessionID: "abc123", sessionsDir: LaunchConfig.appSessionsDir),
            LaunchConfig.attachCommand(sessionID: "abc123")
        )
    }
}
