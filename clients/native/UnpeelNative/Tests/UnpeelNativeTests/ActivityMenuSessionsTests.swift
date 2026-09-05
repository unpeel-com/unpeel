import XCTest
import UnpeelShared
@testable import UnpeelNative

final class ActivityMenuSessionsTests: XCTestCase {
    func testBlockersHaveTheirOwnSectionAndCannotAlsoBeWorkingOrFinished() {
        let working = session("working", status: .busy)
        let blocked = session("blocked", status: .attention)
        let finished = session("finished", status: .idle)
        let node = projectNode(sessions: [working, blocked, finished])

        let activity = ActivityMenuSessions(
            nodes: [node],
            allSessions: [working, blocked, finished],
            jobs: [working, blocked],
            finished: [blocked, finished]
        )

        XCTAssertEqual(activity.jobs.map(\.id), ["working"])
        XCTAssertEqual(activity.blockers.map(\.id), ["blocked"])
        XCTAssertEqual(activity.finished.map(\.id), ["finished"])
        XCTAssertEqual(activity.sectionCount, 3)
    }

    func testBlockerOrderFollowsTheProjectTreeAndDuplicateRowsAreRemoved() {
        let first = session("first", status: .attention)
        let second = session("second", status: .attention)
        let child = projectNode(id: "child", sessions: [second, first])
        let parent = projectNode(id: "parent", sessions: [first], worktrees: [child])

        let activity = ActivityMenuSessions(
            nodes: [parent],
            allSessions: [second, first],
            jobs: [],
            finished: []
        )

        XCTAssertEqual(activity.blockers.map(\.id), ["first", "second"])
        XCTAssertEqual(activity.sectionCount, 1)
    }

    func testOrphanBlockersAppendAfterTreeInLifecycleAndIDOrder() {
        let rendered = session("rendered", status: .attention, lifecycleAtMs: 10)
        let orphanZ = session("orphan-z", status: .attention, lifecycleAtMs: 300)
        let orphanA = session("orphan-a", status: .attention, lifecycleAtMs: 300)
        let orphanOld = session("orphan-old", status: .attention, lifecycleAtMs: 100)
        let orphanIdle = session("orphan-idle", status: .idle, lifecycleAtMs: 500)

        let activity = ActivityMenuSessions(
            nodes: [projectNode(sessions: [rendered])],
            // Deliberately unordered, including the rendered blocker again:
            // orphan ranking must not inherit Dictionary.Values iteration.
            allSessions: [orphanOld, rendered, orphanZ, orphanIdle, orphanA],
            jobs: [],
            finished: []
        )

        XCTAssertEqual(
            activity.blockers.map(\.id),
            ["rendered", "orphan-a", "orphan-z", "orphan-old"]
        )
    }

    func testRemoteSliceBuildsProjectFolderBreadcrumbAndExclusiveBuckets() {
        let snapshot = RemoteBootstrapSnapshot(
            folders: [],
            projects: [
                RemoteProjectSummary(id: "project", name: "Project", path: "/project"),
                RemoteProjectSummary(
                    id: "folder",
                    name: "Folder",
                    path: "/project/folder",
                    parentProjectID: "project",
                    isGroup: true
                ),
            ],
            presets: [],
            sessions: [
                remoteSession("working", projectID: "folder", activity: .working),
                remoteSession("blocked", projectID: "project", activity: .blocked, unread: true),
                remoteSession(
                    "finished", projectID: "project", activity: .done,
                    unread: true, alert: "Close to the weekly limit"
                ),
                remoteSession(
                    "archived", projectID: "project", activity: .done,
                    unread: true, archived: true
                ),
            ],
            capturedAtUnixMs: 1
        )

        let slice = WorkspaceActivityMenuSlice(snapshot: snapshot)

        XCTAssertEqual(slice.jobs.map(\.sessionID), ["working"])
        XCTAssertEqual(slice.jobs.first?.projectPath, "Project › Folder")
        XCTAssertEqual(slice.blockers.map(\.sessionID), ["blocked"])
        XCTAssertEqual(slice.finished.map(\.sessionID), ["finished"])
        XCTAssertEqual(slice.finished.first?.alertBody, "Close to the weekly limit")
    }

    func testGlobalRowIdentityIncludesWorkspaceKey() {
        let session = WorkspaceActivityMenuSession(
            sessionID: "same-id",
            title: "Session",
            command: "codex",
            projectPath: "Project",
            status: "Working",
            alertBody: nil
        )
        let first = GlobalActivityMenuItem(
            workspaceKey: "local:first",
            workspaceName: "First",
            workspaceTint: .blue,
            session: session
        )
        let second = GlobalActivityMenuItem(
            workspaceKey: "host:second",
            workspaceName: "Second",
            workspaceTint: .amber,
            session: session
        )

        XCTAssertNotEqual(first.id, second.id)
    }

    private func session(
        _ id: String,
        status: SessionStatus,
        lifecycleAtMs: Int64 = 0
    ) -> SessionEntry {
        SessionEntry(
            id: id,
            projectID: "project",
            label: id,
            command: "codex",
            createdAt: 0,
            status: status,
            lifecycleAtMs: lifecycleAtMs
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

    private func remoteSession(
        _ id: String,
        projectID: String,
        activity: RemoteActivityState,
        unread: Bool = false,
        archived: Bool = false,
        alert: String? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: projectID,
            title: id,
            command: "codex",
            createdAtUnixMs: 1,
            status: .running,
            activity: activity,
            unread: unread,
            archived: archived,
            latestAlertBody: alert,
            latestAlertAtUnixMs: alert == nil ? nil : 2
        )
    }
}
