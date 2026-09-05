import XCTest
import UnpeelShared
@testable import UnpeelNative

final class RemoteDTOAdaptersTests: XCTestCase {
    func testSessionStatusMapsToRemoteStatus() {
        XCTAssertEqual(SessionStatus.starting.remoteStatus, .running)
        XCTAssertEqual(SessionStatus.busy.remoteStatus, .running)
        XCTAssertEqual(SessionStatus.attention.remoteStatus, .running)
        XCTAssertEqual(SessionStatus.exited.remoteStatus, .exited)
    }

    func testSessionActivityStatusMapsToRemoteActivity() {
        XCTAssertEqual(SessionActivityStatus.starting.remoteActivity, .starting)
        XCTAssertEqual(SessionActivityStatus.working.remoteActivity, .working)
        XCTAssertEqual(SessionActivityStatus.blocked.remoteActivity, .blocked)
        XCTAssertEqual(SessionActivityStatus.done.remoteActivity, .done)
        XCTAssertEqual(SessionActivityStatus.idle.remoteActivity, .idle)
        XCTAssertEqual(SessionActivityStatus.exited.remoteActivity, .idle)
    }

    func testPresetRemoteSummaryCarriesCliAndDefault() {
        let preset = Preset(
            id: "codex-custom",
            label: "Codex fast",
            command: "codex --model gpt-5.1",
            enabled: true,
            quickLaunch: true
        )

        let remote = preset.remoteSummary(defaultPresetID: "codex-custom")

        XCTAssertEqual(remote.id, "codex-custom")
        XCTAssertEqual(remote.cliID, "codex")
        XCTAssertTrue(remote.quickLaunch)
        XCTAssertTrue(remote.isDefault)
    }

    func testProjectRemoteSummaryCarriesFolderAndEffectiveBlock() {
        let project = Project(
            id: "project-1",
            name: "Unpeel",
            path: "/Users/test/Dev/unpeel",
            parentProjectID: "folder-1",
            sortOrder: 12,
            isFolder: nil,
            worktreeBranch: nil,
            workspacesEnabled: true,
            mcpBlocked: nil
        )

        let remote = project.remoteProjectSummary(folderID: "folder-1", mcpBlocked: true)

        XCTAssertEqual(remote.id, "project-1")
        XCTAssertEqual(remote.folderID, "folder-1")
        XCTAssertNil(remote.parentProjectID)
        XCTAssertTrue(remote.mcpBlocked)
        XCTAssertEqual(remote.sortOrder, 12)
    }

    func testProjectRemoteSummaryCarriesWorktreeParentSeparatelyFromFolder() {
        let project = Project(
            id: "worktree-1",
            name: "native-b",
            path: "/Users/test/.unpeel/worktrees/unpeel/native-b",
            parentProjectID: "project-unpeel",
            sortOrder: 4,
            isFolder: nil,
            worktreeBranch: "native/b",
            workspacesEnabled: nil,
            mcpBlocked: nil
        )

        let remote = project.remoteProjectSummary(
            folderID: nil,
            parentProjectID: "project-unpeel",
            mcpBlocked: false
        )

        XCTAssertNil(remote.folderID)
        XCTAssertEqual(remote.parentProjectID, "project-unpeel")
        XCTAssertEqual(remote.worktreeBranch, "native/b")
    }

    func testProjectRemoteSummaryCarriesInlineGroupPresentation() {
        let project = Project(
            id: "group-research",
            name: "Research",
            path: "/Users/test/Dev/unpeel",
            parentProjectID: "project-unpeel",
            sortOrder: 5,
            isFolder: true,
            worktreeBranch: nil,
            workspacesEnabled: nil,
            mcpBlocked: nil
        )

        let remote = project.remoteProjectSummary(
            parentProjectID: "project-unpeel",
            isGroup: true,
            colorID: "violet"
        )

        XCTAssertEqual(remote.parentProjectID, "project-unpeel")
        XCTAssertEqual(remote.isGroup, true)
        XCTAssertEqual(remote.colorID, "violet")
        XCTAssertNil(remote.worktreeBranch)
        XCTAssertNil(remote.sessionOrder)

        let mixed = project.remoteProjectSummary(
            parentProjectID: "project-unpeel",
            isGroup: true,
            sessionOrder: ["session-a", "group-research"]
        )
        XCTAssertEqual(mixed.sessionOrder, ["session-a", "group-research"])
    }

    func testFolderRemoteSummaryCarriesParentColorAndSort() {
        let folder = Project(
            id: "folder-2",
            name: "Clients",
            path: "",
            parentProjectID: "folder-1",
            sortOrder: 3,
            isFolder: true,
            worktreeBranch: nil,
            workspacesEnabled: nil,
            mcpBlocked: nil
        )

        let remote = folder.remoteFolderSummary(colorID: "blue")

        XCTAssertEqual(remote.id, "folder-2")
        XCTAssertEqual(remote.parentFolderID, "folder-1")
        XCTAssertEqual(remote.colorID, "blue")
        XCTAssertEqual(remote.sortOrder, 3)
    }

    func testSessionRemoteSummaryCarriesRemoteControllerState() {
        let session = SessionEntry(
            id: "session-1",
            projectID: "project-1",
            label: "Review checkout",
            command: "claude --dangerously-skip-permissions",
            createdAt: 1_789_996_800_000,
            status: .attention,
            customTitle: false,
            worktreePath: "/tmp/unpeel-worktree",
            worktreeBranch: "feature/ios"
        )

        let remote = session.remoteSummary(
            unread: true,
            pinned: true,
            lastOutputPreview: "Approve command?",
            updatedAtUnixMs: 1_789_996_860_000,
            latestAlertBody: "Close to the weekly limit",
            latestAlertAtUnixMs: 1_789_996_859_000
        )

        XCTAssertEqual(remote.id, "session-1")
        XCTAssertEqual(remote.providerID, "claude")
        XCTAssertEqual(remote.title, "Review checkout")
        XCTAssertEqual(remote.status, .running)
        XCTAssertEqual(remote.activity, .blocked)
        XCTAssertTrue(remote.unread)
        XCTAssertTrue(remote.pinned)
        XCTAssertEqual(remote.worktreeBranch, "feature/ios")
        XCTAssertEqual(remote.lastOutputPreview, "Approve command?")
        XCTAssertEqual(remote.latestAlertBody, "Close to the weekly limit")
        XCTAssertEqual(remote.latestAlertAtUnixMs, 1_789_996_859_000)
    }

    func testBlankShellSummaryCarriesActiveRuntimeWithoutPromotingLaunchProvider() {
        let session = SessionEntry(
            id: "blank-shell",
            projectID: "project-1",
            label: "Terminal",
            command: "",
            createdAt: 1_789_996_800_000,
            status: .busy,
            activeRuntimeID: "claude"
        )

        let remote = session.remoteSummary()

        XCTAssertEqual(remote.command, "")
        XCTAssertNil(remote.providerID)
        XCTAssertEqual(remote.activeRuntimeID, "claude")
        XCTAssertEqual(remote.spinnerColorHex, 0xD97757)
        XCTAssertNil(remote.capabilities?.restartAgent)
        XCTAssertEqual(remote.capabilities?.resumeAgent, false)
    }

    func testManagedSummaryAdvertisesResumeAgentOnlyAfterRuntimeReturnsToShell() {
        var session = SessionEntry(
            id: "managed", projectID: "project-1", label: "Claude",
            command: "claude", createdAt: 1, status: .idle,
            activeRuntimeID: "claude", hostProtocolVersion: 3,
            hasResumableState: true
        )
        XCTAssertNil(session.remoteSummary().capabilities?.restartAgent)
        XCTAssertEqual(session.remoteSummary().capabilities?.resumeAgent, false)

        session.activeRuntimeID = nil
        XCTAssertEqual(session.remoteSummary().capabilities?.resumeAgent, true)

        session.runtimeLaunchPending = true
        XCTAssertTrue(session.remoteSummary().runtimeLaunchPending)
        XCTAssertEqual(session.remoteSummary().capabilities?.resumeAgent, false)
    }

    func testSessionRemoteSummaryUsesEffectiveGroupProjectID() {
        let session = SessionEntry(
            id: "session-1",
            projectID: "project-unpeel",
            label: "Grouped session",
            command: "codex",
            createdAt: 1_789_996_800_000,
            status: .idle,
            customTitle: false,
            worktreePath: nil,
            worktreeBranch: nil
        )

        let remote = session.remoteSummary(projectID: "group-research")

        XCTAssertEqual(remote.projectID, "group-research")
    }

    func testSessionRemoteSummaryDefaultsUpdatedAtToLifecycleStamp() {
        let session = SessionEntry(
            id: "session-recent",
            projectID: "project-unpeel",
            label: "Recent session",
            command: "codex",
            createdAt: 100,
            status: .idle,
            lifecycleAtMs: 900
        )

        XCTAssertEqual(session.remoteSummary().updatedAtUnixMs, 900)
        XCTAssertEqual(
            session.remoteSummary(
                latestAlertBody: "Available again",
                latestAlertAtUnixMs: 1_200
            ).updatedAtUnixMs,
            1_200
        )
    }

    func testIdleUnreadSessionMapsToDoneActivity() {
        let session = SessionEntry(
            id: "session-2",
            projectID: "project-1",
            label: "Run tests",
            command: "codex",
            createdAt: 1_789_996_800_000,
            status: .idle,
            customTitle: false,
            worktreePath: nil,
            worktreeBranch: nil
        )

        let remote = session.remoteSummary(
            unread: true,
            pinned: false,
            lastOutputPreview: "All tests passed",
            updatedAtUnixMs: 1_789_996_860_000
        )

        XCTAssertEqual(remote.activity, .done)
    }
}
