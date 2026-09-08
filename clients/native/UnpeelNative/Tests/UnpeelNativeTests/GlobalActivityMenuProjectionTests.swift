import Testing
import UnpeelShared
@testable import UnpeelNative

struct GlobalActivityMenuProjectionTests {
    @Test(arguments: [RemoteActivityState.idle, .done, .blocked])
    func stoppedForegroundOverridesCachedWorkingRow(activity: RemoteActivityState) {
        let stale = slice(activity: .working)
        let current = slice(activity: activity, unread: activity != .idle)
        let menu = GlobalActivityMenuSessions(
            workspaces: [workspace("local", current: true)],
            foregroundKey: "local",
            foreground: current,
            cachedSlice: { _ in stale }
        )

        #expect(menu.jobs.isEmpty)
        #expect(menu.blockers.count == (activity == .blocked ? 1 : 0))
        #expect(menu.finished.count == (activity == .done ? 1 : 0))
    }

    @Test
    func switchingWorkspaceKeepsActivityWithItsOwnerAndAcceptsBackgroundStop() {
        let rows = [workspace("local", current: true), workspace("other")]
        var cached = ["local": slice(activity: .working), "other": slice(activity: .idle)]
        let foreground = slice(activity: .working)
        func menu() -> GlobalActivityMenuSessions {
            GlobalActivityMenuSessions(
                workspaces: rows, foregroundKey: "other", foreground: foreground,
                cachedSlice: { cached[$0] }
            )
        }

        // Both Hosts deliberately use the same Session id. Selection cannot
        // copy the foreground row into this app instance's Local workspace.
        #expect(menu().jobs.map(\.workspaceKey) == ["local", "other"])
        cached["local"] = slice(activity: .done, unread: true)
        #expect(menu().jobs.map(\.workspaceKey) == ["other"])
        #expect(menu().finished.map(\.workspaceKey) == ["local"])
        cached["local"] = slice(activity: .idle)
        #expect(menu().jobs.map(\.workspaceKey) == ["other"])
        #expect(menu().finished.isEmpty)
    }

    @Test
    func nextForegroundTurnOverridesAnIdleBackgroundCache() {
        let menu = GlobalActivityMenuSessions(
            workspaces: [workspace("local", current: true)],
            foregroundKey: "local", foreground: slice(activity: .working),
            cachedSlice: { _ in slice(activity: .idle) }
        )
        #expect(menu.jobs.map(\.session.sessionID) == ["same-session"])
        #expect(menu.jobs.first?.session.status == "Working")
    }

    private func slice(
        activity: RemoteActivityState, unread: Bool = false
    ) -> WorkspaceActivityMenuSlice {
        WorkspaceActivityMenuSlice(snapshot: RemoteBootstrapSnapshot(
            folders: [],
            projects: [RemoteProjectSummary(id: "p", name: "Project", path: "/project")],
            presets: [],
            sessions: [RemoteSessionSummary(
                id: "same-session", projectID: "p", title: "link detection",
                command: "codex", createdAtUnixMs: 1, status: .running,
                activity: activity, unread: unread
            )],
            capturedAtUnixMs: 1
        ))
    }

    private func workspace(_ key: String, current: Bool = false) -> WorkspaceListRowModel {
        WorkspaceListRowModel(
            id: key, name: key, detail: "", icon: "", badge: nil,
            home: "/tmp/\(key)", tint: .none,
            kind: .local(record: nil, isDefault: current, isCurrentInstance: current, isRunning: true)
        )
    }
}
