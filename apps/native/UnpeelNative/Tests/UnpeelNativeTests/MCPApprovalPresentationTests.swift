import XCTest
@testable import UnpeelNative

final class MCPApprovalPresentationTests: XCTestCase {
    func testWritePresentsOnKnownTarget() {
        let approval = makeApproval(kind: .write, caller: "caller", target: "target")
        XCTAssertEqual(
            approval.presentationSessionID(knownIDs: ["caller", "target"]),
            "target"
        )
    }

    func testWriteFallsBackToCallerWhenTargetIsUnknown() {
        let approval = makeApproval(kind: .write, caller: "caller", target: "gone")
        XCTAssertEqual(
            approval.presentationSessionID(knownIDs: ["caller"]),
            "caller"
        )
    }

    func testBrowserPresentsOnCaller() {
        let approval = makeApproval(kind: .browser, caller: "caller")
        XCTAssertEqual(
            approval.presentationSessionID(knownIDs: ["caller"]),
            "caller"
        )
    }

    func testAttentionOverlayPromotesLiveSessionAndLeavesOthers() {
        let idle = session("idle", status: .idle)
        let busy = session("busy", status: .busy)
        let exited = session("exited", status: .exited)
        let child = projectNode(id: "child", sessions: [busy])
        let parent = projectNode(id: "parent", sessions: [idle, exited], worktrees: [child])

        let overlaid = McpApprovalAttention.applying(
            to: [parent],
            pendingSessionIDs: ["idle", "busy", "exited"]
        )

        XCTAssertEqual(overlaid[0].sessions[0].status, .attention)
        XCTAssertEqual(overlaid[0].sessions[1].status, .exited)
        XCTAssertEqual(overlaid[0].worktrees[0].sessions[0].status, .attention)
    }

    func testAttentionOnRepresentativeSuppressesSiblingSpinner() {
        let representative = session("main", status: .attention)
        let paneItems = [
            UnpeelStore.PaneSidebarItem(
                paneID: "pane",
                sessionID: "member",
                command: "codex",
                agentName: "Codex",
                status: .busy,
                isRepresentative: false
            )
        ]
        XCTAssertNil(
            sessionRowActivitySpinnerCommand(
                session: representative,
                paneItems: paneItems
            )
        )
    }

    func testEmptyPendingIDsAreIdentity() {
        let node = projectNode(sessions: [session("s", status: .idle)])
        let overlaid = McpApprovalAttention.applying(
            to: [node],
            pendingSessionIDs: []
        )
        XCTAssertEqual(overlaid, [node])
    }

    func testOverlaidAttentionAppearsAsActivityBlocker() {
        let working = session("working", status: .busy)
        let waiting = session("waiting", status: .idle)
        let overlaid = McpApprovalAttention.applying(
            to: [projectNode(sessions: [working, waiting])],
            pendingSessionIDs: ["waiting"]
        )
        let activity = ActivityMenuSessions(
            nodes: overlaid,
            allSessions: overlaid[0].sessions,
            jobs: [working],
            finished: []
        )
        XCTAssertEqual(activity.blockers.map(\.id), ["waiting"])
        XCTAssertEqual(activity.jobs.map(\.id), ["working"])
    }

    private func makeApproval(
        kind: PendingMcpApproval.Kind,
        caller: String,
        target: String? = nil
    ) -> PendingMcpApproval {
        PendingMcpApproval(
            id: "approval",
            kind: kind,
            callerSessionID: caller,
            targetSessionID: target,
            targetAppID: nil,
            targetAppName: nil,
            requestedAt: Date()
        )
    }

    private func session(_ id: String, status: SessionStatus) -> SessionEntry {
        SessionEntry(
            id: id,
            projectID: "project",
            label: id,
            command: "codex",
            createdAt: 0,
            status: status
        )
    }

    private func projectNode(
        id: String = "project",
        sessions: [SessionEntry],
        worktrees: [ProjectNode] = []
    ) -> ProjectNode {
        ProjectNode(
            project: Project(
                id: id,
                name: id,
                path: "/tmp/\(id)",
                parentProjectID: nil,
                sortOrder: 0,
                isFolder: nil,
                worktreeBranch: nil,
                workspacesEnabled: nil,
                mcpBlocked: nil
            ),
            sessions: sessions,
            worktrees: worktrees
        )
    }
}
