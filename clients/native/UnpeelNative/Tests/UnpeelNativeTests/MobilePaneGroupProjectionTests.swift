import XCTest
@testable import UnpeelNative

final class MobilePaneGroupProjectionTests: XCTestCase {
    func testSummariesPreserveRepresentativeAndPreorderSessionIDs() throws {
        var state = PaneLayoutState()
        let location = try state.createGroup(
            representativeSessionID: "session-main",
            adding: "session-notes",
            at: .right
        )

        let summaries = try XCTUnwrap(
            MobilePaneGroupProjection.summaries(from: state)
        )
        XCTAssertEqual(summaries.count, 1)
        XCTAssertEqual(summaries[0].id, location.groupID)
        XCTAssertEqual(summaries[0].representativeSessionID, "session-main")
        XCTAssertEqual(summaries[0].sessionIDs, ["session-main", "session-notes"])
    }

    func testEmptyLayoutOmitsProjection() {
        XCTAssertNil(
            MobilePaneGroupProjection.summaries(from: PaneLayoutState())
        )
    }

    func testWorkspaceSelectionKeysMapToControllerPaneScopes() {
        XCTAssertEqual(
            MobilePaneGroupProjection.scopeID(forSelectionKey: nil),
            "local"
        )
        XCTAssertEqual(
            MobilePaneGroupProjection.scopeID(forSelectionKey: "local:/tmp/client"),
            "workspace:/tmp/client"
        )
        XCTAssertEqual(
            MobilePaneGroupProjection.scopeID(forSelectionKey: "ssh:linux-host"),
            "host:linux-host"
        )
        XCTAssertEqual(
            MobilePaneGroupProjection.scopeID(forSelectionKey: "host:paired-mac"),
            "host:paired-mac"
        )
        XCTAssertNil(MobilePaneGroupProjection.scopeID(forSelectionKey: "local:"))
        XCTAssertNil(MobilePaneGroupProjection.scopeID(forSelectionKey: "unknown:x"))
    }
}
