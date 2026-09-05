import Combine
import Foundation
import Testing
import UnpeelShared
@testable import UnpeelNative

struct StartupPerformanceTests {
    @Test @MainActor
    func unchangedManagementSnapshotsDoNotPublish() {
        let state = HostManagementState()
        var publications = 0
        let observer = state.objectWillChange.sink { publications += 1 }
        defer { observer.cancel() }
        for _ in 0..<10 { state.apply(.init()) }
        #expect(publications == 0)
        let endpoint = URL(string: "http://127.0.0.1:1234")!
        state.apply(.init(endpoint: endpoint))
        for _ in 0..<10 { state.apply(.init(endpoint: endpoint)) }
        #expect(publications == 1)
        for _ in 0..<10 { state.update(error: "Disconnected") }
        #expect(publications == 2)
        state.apply(.init(endpoint: endpoint))
        #expect(publications == 3)
    }

    @Test @MainActor
    func slowReconciliationLeavesMainActorFreeAndLaunchesOnce() async {
        let manager = HostServiceManager()
        let gate = DispatchSemaphore(value: 0)
        defer { gate.signal() }
        var launches = 0
        let ranOnMain = await withCheckedContinuation { started in
            _ = manager.prepareForLaunch {
                started.resume(returning: Thread.isMainThread)
                _ = gate.wait(timeout: .now() + 5)
                return true
            } launch: { restarted in
                #expect(restarted)
                #expect(manager.launchPreparation == nil)
                launches += 1
            }
        }
        #expect(!ranOnMain)
        #expect(launches == 0)
        let existing = manager.prepareForLaunch {
            Issue.record("A concurrent launch repeated reconciliation")
            return false
        } launch: { _ in Issue.record("A concurrent launch ran twice") }
        #expect(manager.launchPreparation != nil)
        gate.signal()
        await existing.value
        #expect(launches == 1)
    }

    @Test
    func startupCachePreservesRowsAndRejectsWrongIdentityOrCorruption() async throws {
        let home = FileManager.default.temporaryDirectory
            .appendingPathComponent("up-cache-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: home) }
        let cache = StartupPresentationCache(home: home)
        let original = StartupPresentation(
            home: home.path, hostID: "host", nodes: [Self.node("project", status: .busy)],
            pins: [:], archivedIDs: ["archive"], unreadIDs: ["session-project"]
        )
        cache.save(original)
        await cache.flush()
        #expect(cache.load(home: home.path, hostID: "host") == original)
        #expect(cache.load(home: home.path, hostID: "different") == nil)
        #expect(cache.load(home: "/different", hostID: "host") == nil)
        var future = original
        future.version = 999
        cache.save(future)
        await cache.flush()
        #expect(cache.load(home: home.path, hostID: "host") == nil)
        try Data("torn json".utf8).write(to: cache.fileURL)
        #expect(cache.load(home: home.path, hostID: "host") == nil)
        try Data(repeating: 0, count: StartupPresentationCache.maximumBytes + 1)
            .write(to: cache.fileURL)
        #expect(cache.load(home: home.path, hostID: "host") == nil)
    }

    @Test
    func onlyChangedProjectListsAreInvalidated() {
        let first = Self.node("first", status: .idle)
        let second = Self.node("second", status: .idle)
        func changes(_ next: [ProjectNode], order: [String: [String]] = [:]) -> Set<String> {
            SidebarProjectionChanges.affectedProjects(
                previous: [first, second], next: next,
                previousSummaries: [:], nextSummaries: [:],
                previousProjects: [:], nextProjects: [:],
                previousOrder: [:], nextOrder: order
            )
        }
        #expect(changes([first, second]).isEmpty)
        #expect(changes([Self.node("first", status: .busy), second]) == ["first"])
        #expect(changes([first, second], order: ["second": ["session-second"]]) == ["second"])
        #expect(changes([first]) == ["second"])
    }

    @Test
    func rowFlagsInvalidateTheirProjectButOutputPreviewsDoNot() {
        let nodes = [Self.node("first", status: .idle), Self.node("second", status: .idle)]
        func summary(unread: Bool = false, pinned: Bool = false, archived: Bool = false,
                     preview: String? = nil) -> RemoteSessionSummary {
            RemoteSessionSummary(
                id: "session-first", projectID: "first", title: "Terminal", command: "",
                createdAtUnixMs: 1, status: .running, activity: .idle,
                unread: unread, pinned: pinned, lastOutputPreview: preview, archived: archived
            )
        }
        func changes(_ next: RemoteSessionSummary) -> Set<String> {
            SidebarProjectionChanges.affectedProjects(
                previous: nodes, next: nodes,
                previousSummaries: ["session-first": summary()],
                nextSummaries: ["session-first": next],
                previousProjects: [:], nextProjects: [:],
                previousOrder: [:], nextOrder: [:]
            )
        }
        #expect(changes(summary(unread: true)) == ["first"])
        #expect(changes(summary(pinned: true)) == ["first"])
        #expect(changes(summary(archived: true)) == ["first"])
        #expect(changes(summary(preview: "New terminal output")).isEmpty)
    }

    private static func node(_ id: String, status: SessionStatus) -> ProjectNode {
        ProjectNode(
            project: Project(
                id: id, name: id, path: "/tmp/project", parentProjectID: nil,
                sortOrder: nil, isFolder: nil, worktreeBranch: nil,
                workspacesEnabled: nil, mcpBlocked: nil
            ),
            sessions: [SessionEntry(
                id: "session-\(id)", projectID: id, label: "Terminal", command: "",
                createdAt: 1, status: status, worktreePath: nil, worktreeBranch: nil
            )], worktrees: []
        )
    }
}
