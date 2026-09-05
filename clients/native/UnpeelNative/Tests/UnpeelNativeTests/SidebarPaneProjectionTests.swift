import XCTest
@testable import UnpeelNative

@MainActor
final class SidebarPaneProjectionTests: XCTestCase {
    func testProjectionCollapsesMembersUnderRepresentativeRow() throws {
        var state = PaneLayoutState()
        try state.createGroup(
            representativeSessionID: "main",
            adding: "member",
            at: .right
        )
        let sessions = [
            "main": session("main", label: "Main"),
            "member": session("member", label: "Member"),
            "ordinary": session("ordinary", label: "Ordinary"),
        ]

        let projection = SidebarPaneProjection(
            state: state,
            sessionsByID: sessions,
            eligibleSessionIDs: Set(sessions.keys)
        )

        XCTAssertEqual(projection.hiddenSessionIDs, ["member"])
        XCTAssertEqual(
            projection.itemsByRepresentative["main"]?.map(\.sessionID),
            ["main", "member"]
        )
        XCTAssertEqual(
            projection.itemsByRepresentative["main"]?.map(\.isRepresentative),
            [true, false]
        )
        XCTAssertEqual(projection.representativeSessionID(for: "member"), "main")
        XCTAssertEqual(
            projection.representativeSessionID(for: "ordinary"),
            "ordinary"
        )
    }

    func testProjectionFailsOpenWhenSavedGroupHasStaleMember() throws {
        var state = PaneLayoutState()
        try state.createGroup(
            representativeSessionID: "main",
            adding: "stale-member",
            at: .right
        )
        let sessions = ["main": session("main", label: "Main")]

        let projection = SidebarPaneProjection(
            state: state,
            sessionsByID: sessions,
            eligibleSessionIDs: ["main"]
        )

        XCTAssertTrue(projection.hiddenSessionIDs.isEmpty)
        XCTAssertTrue(projection.itemsByRepresentative.isEmpty)
        XCTAssertEqual(
            projection.representativeSessionID(for: "stale-member"),
            "stale-member"
        )
    }

    func testRepresentativeRowSpinsWhenAnyPaneMemberIsBusy() throws {
        var state = PaneLayoutState()
        try state.createGroup(
            representativeSessionID: "main",
            adding: "member",
            at: .right
        )
        let sessions = [
            "main": session("main", label: "Main", command: "claude"),
            "member": session(
                "member", label: "Member", command: "codex", status: .busy
            ),
        ]
        let projection = SidebarPaneProjection(
            state: state,
            sessionsByID: sessions,
            eligibleSessionIDs: Set(sessions.keys)
        )
        let paneItems = try XCTUnwrap(projection.itemsByRepresentative["main"])
        let representative = try XCTUnwrap(sessions["main"])

        XCTAssertEqual(
            sessionRowActivitySpinnerCommand(
                session: representative,
                paneItems: paneItems
            ),
            "codex"
        )
    }

    private func session(
        _ id: String,
        label: String,
        command: String = "claude",
        status: SessionStatus = .idle
    ) -> SessionEntry {
        SessionEntry(
            id: id,
            projectID: "project",
            label: label,
            command: command,
            createdAt: 1,
            status: status
        )
    }
}
