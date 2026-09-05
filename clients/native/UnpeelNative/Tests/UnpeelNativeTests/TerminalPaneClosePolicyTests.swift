import XCTest
@testable import UnpeelNative

final class TerminalPaneClosePolicyTests: XCTestCase {
    func testLauncherDetachesImmediately() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .launcher(projectID: "project"),
                canArchiveSession: false
            ),
            .detachPane
        )
    }

    func testPlainTerminalRemovesImmediately() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .session(id: "shell"),
                canArchiveSession: false
            ),
            .removeSession("shell")
        )
    }

    func testResumableAgentRequiresArchiveConfirmation() {
        XCTAssertEqual(
            terminalPaneCloseAction(
                for: .session(id: "agent"),
                canArchiveSession: true
            ),
            .confirmArchive("agent")
        )
    }
}
