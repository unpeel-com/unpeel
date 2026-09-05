import XCTest
@testable import UnpeelNative

final class SessionTitleDefaultsTests: XCTestCase {
    func testBlankTerminalUsesAbbreviatedWorkingFolder() {
        XCTAssertEqual(
            SessionTitleDefaults.abbreviatedPath(
                "/Users/test/Dev/unpeel",
                home: "/Users/test"
            ),
            "~/Dev/unpeel"
        )
    }

    func testAgentCommandRemainsTheInitialLabel() {
        XCTAssertEqual(
            SessionTitleDefaults.initialLabel(command: "claude", cwd: "/tmp/project"),
            "claude"
        )
    }

    func testMissingWorkingFolderKeepsCompatibilityFallback() {
        XCTAssertEqual(SessionTitleDefaults.initialLabel(command: "", cwd: ""), "Terminal")
    }

    func testHomePrefixMustEndAtAPathBoundary() {
        XCTAssertEqual(
            SessionTitleDefaults.abbreviatedPath(
                "/Users/testing/project",
                home: "/Users/test"
            ),
            "/Users/testing/project"
        )
    }
}
