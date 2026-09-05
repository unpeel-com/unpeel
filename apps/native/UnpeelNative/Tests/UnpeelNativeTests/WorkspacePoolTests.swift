import Combine
import Foundation
import XCTest
import UnpeelShared
@testable import UnpeelNative

/// Background workspace pool (workspaces-unification phase 7): poll caching,
/// unreachable-host backoff, the remote concurrency cap, attention edge
/// detection + notification dedup, and the lend/adopt handoff that guarantees
/// the pool never keeps a live connection to a runtime-served workspace.
@MainActor
final class WorkspacePoolTests: XCTestCase {
    private var pool: WorkspacePool!
    private var backends: [String: StubPoolBackend] = [:]
    private var targets: [WorkspacePool.Target] = []
    private var excluded: Set<String> = []
    private var foreground: String?
    private var notifications: [(title: String, workspace: String, key: String, session: String)] = []
    private var factoryCalls: [String] = []

    override func tearDown() async throws {
        pool?.stop()
        pool = nil
        backends = [:]
        targets = []
        excluded = []
        foreground = nil
        notifications = []
        factoryCalls = []
    }

    private func makePool(
        pollNanoseconds: UInt64 = 25_000_000,
        backoffBaseNanoseconds: UInt64 = 30_000_000,
        backoffCapNanoseconds: UInt64 = 240_000_000,
        maxRemote: Int = 4
    ) {
        pool = WorkspacePool(
            pollIntervalNanoseconds: pollNanoseconds,
            backoffBaseNanoseconds: backoffBaseNanoseconds,
            backoffCapNanoseconds: backoffCapNanoseconds,
            // Effectively inert: tests reconcile explicitly.
            maintenanceIntervalNanoseconds: 60_000_000_000,
            immediateRefreshThrottleSeconds: 0,
            maxLiveRemoteConnections: maxRemote,
            backendFactory: { [weak self] transport in
                guard let self else { throw StubPoolError.unexpected }
                let key = Self.stubKey(for: transport)
                self.factoryCalls.append(key)
                guard let backend = self.backends[key] else {
                    throw StubPoolError.unexpected
                }
                return backend
            },
            notifyAttention: { [weak self] title, workspace, key, session in
                self?.notifications.append((title, workspace, key, session))
            }
        )
        pool.start(
            targetsProvider: { [weak self] in self?.targets ?? [] },
            excludedKeys: { [weak self] in self?.excluded ?? [] },
            foregroundKey: { [weak self] in self?.foreground }
        )
    }

    private static func stubKey(for transport: RemoteHostTransport) -> String {
        switch transport {
        case let .localGateway(home, _, _):
            return URL(fileURLWithPath: home).lastPathComponent
        case let .localService(home, _, _):
            return URL(fileURLWithPath: home).lastPathComponent
        case let .ssh(target, _, _, _):
            return String(target.dropFirst("ssh://".count))
        case .direct, .link:
            return "unsupported"
        }
    }

    private func makeTarget(
        key: String,
        remote: Bool = false,
        expectedHostID: String? = nil
    ) -> WorkspacePool.Target {
        WorkspacePool.Target(
            key: key,
            name: "Workspace \(key)",
            transport: remote
                ? .ssh(
                    target: "ssh://\(key)",
                    expectedHostID: expectedHostID,
                    mode: .command,
                    secret: nil
                )
                : .localGateway(
                    unpeelHome: "/tmp/unpeel-pool-tests/\(key)",
                    workspaceName: key,
                    expectedHostID: expectedHostID
                ),
            isRemote: remote,
            expectedHostID: expectedHostID,
            fingerprint: "fp:\(key)"
        )
    }

    // MARK: - Poll caching

    func testPollCachesSnapshotAndKeepsConnectionAlive() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(hostID: "h1", blocked: ["blocked"]))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()

        await waitUntil { self.pool.snapshot(forKey: "a") != nil }
        XCTAssertEqual(pool.snapshot(forKey: "a")?.macID, "h1")
        XCTAssertEqual(pool.activitySlice(forKey: "a")?.blockers.map(\.sessionID), ["blocked"])
        XCTAssertNotNil(pool.lastSeenAt["a"])

        // Low-cadence polling continues on the SAME keep-alive backend: no
        // close, no re-open through the factory.
        let initialCount = await backend.bootstrapCount
        await waitUntil { await backend.bootstrapCount >= initialCount + 2 }
        let closes = await backend.closeCount
        XCTAssertEqual(closes, 0)
        XCTAssertEqual(factoryCalls, ["a"])
    }

    func testEquivalentSnapshotPollsNeverRepublish() async {
        let backend = StubPoolBackend(
            defaultResult: .success(
                Self.makeSnapshot(hostID: "h1", idle: ["s1"], capturedAtUnixMs: 1_000)
            )
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()
        await waitUntil { self.pool.snapshot(forKey: "a") != nil }
        let cachedCapturedAt = pool.snapshot(forKey: "a")?.capturedAtUnixMs
        let seenBefore = pool.lastSeenAt["a"]

        // Identical content with a fresh capture timestamp — exactly what an
        // idle host's poll returns forever. It must not wake SwiftUI.
        var publishes = 0
        let cancellable = pool.objectWillChange.sink { _ in publishes += 1 }
        await backend.setDefaultResult(.success(
            Self.makeSnapshot(hostID: "h1", idle: ["s1"], capturedAtUnixMs: 2_000)
        ))
        let count = await backend.bootstrapCount
        await waitUntil { await backend.bootstrapCount >= count + 3 }
        XCTAssertEqual(publishes, 0, "unchanged workspace must never republish")
        XCTAssertEqual(
            pool.snapshot(forKey: "a")?.capturedAtUnixMs,
            cachedCapturedAt,
            "the cached snapshot (and its timestamp-keyed derived caches) stays put"
        )
        // Contact bookkeeping still advances — silently.
        if let seenBefore, let seenNow = pool.lastSeenAt["a"] {
            XCTAssertGreaterThan(seenNow, seenBefore)
        } else {
            XCTFail("lastSeenAt must be tracked across equivalent polls")
        }

        // Real content change publishes again.
        await backend.setDefaultResult(.success(
            Self.makeSnapshot(hostID: "h1", blocked: ["s1"], capturedAtUnixMs: 3_000)
        ))
        await waitUntil { self.pool.hasAttention(forKey: "a") }
        XCTAssertGreaterThan(publishes, 0)
        cancellable.cancel()
    }

    // MARK: - Backoff

    func testUnreachableHostBacksOffExponentiallyAndRecovers() async {
        let backend = StubPoolBackend(
            defaultResult: .failure(StubPoolError.unreachable)
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()

        await waitUntil(iterations: 12_000) { await backend.bootstrapCount >= 4 }
        // A failed attempt never retains a half-open connection.
        let closes = await backend.closeCount
        XCTAssertGreaterThanOrEqual(closes, 3)
        let stamps = await backend.bootstrapTimestamps
        let firstGap = stamps[1].timeIntervalSince(stamps[0])
        let thirdGap = stamps[3].timeIntervalSince(stamps[2])
        // Base 30ms doubling: ~30ms then ~120ms. Generous margin for
        // scheduling noise.
        XCTAssertGreaterThan(thirdGap, firstGap * 1.5)

        // Reachability recovery resumes normal polling and caches.
        await backend.setDefaultResult(.success(Self.makeSnapshot(hostID: "h1")))
        await waitUntil(iterations: 12_000) { self.pool.snapshot(forKey: "a") != nil }
    }

    // MARK: - Remote concurrency cap

    func testRemoteConnectionsAreCappedAndSlotsRecycle() async {
        let a = StubPoolBackend(hold: true)
        let b = StubPoolBackend(hold: true)
        let c = StubPoolBackend(hold: true)
        backends = ["a": a, "b": b, "c": c]
        targets = [
            makeTarget(key: "a", remote: true),
            makeTarget(key: "b", remote: true),
            makeTarget(key: "c", remote: true),
        ]
        makePool(maxRemote: 2)
        pool.refreshTargets()

        await waitUntil {
            await a.bootstrapCount + b.bootstrapCount + c.bootstrapCount == 2
        }
        try? await Task.sleep(nanoseconds: 60_000_000)
        let started = await a.bootstrapCount + b.bootstrapCount + c.bootstrapCount
        XCTAssertEqual(started, 2, "third remote host must wait for a slot")
        let cCount = await c.bootstrapCount
        XCTAssertEqual(cCount, 0)

        // A failing host releases its slot during backoff; the waiter runs.
        await a.resolveHeld(.failure(StubPoolError.unreachable))
        await waitUntil { await c.bootstrapCount == 1 }
    }

    // MARK: - Attention edges + notification dedup

    func testAttentionEdgeNotifiesOncePerRise() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(idle: ["s1"]))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()
        await waitUntil { self.pool.snapshot(forKey: "a") != nil }
        XCTAssertTrue(notifications.isEmpty)
        XCTAssertFalse(pool.hasAttention(forKey: "a"))

        // Rise: blocked session → badge + exactly ONE notification, then
        // repeated blocked snapshots stay deduplicated.
        await backend.setDefaultResult(.success(Self.makeSnapshot(blocked: ["s1"])))
        await waitUntil { self.pool.hasAttention(forKey: "a") }
        await waitUntil { self.notifications.count == 1 }
        XCTAssertEqual(notifications[0].key, "a")
        XCTAssertEqual(notifications[0].session, "s1")
        XCTAssertEqual(notifications[0].workspace, "Workspace a")
        let seenCount = await backend.bootstrapCount
        await waitUntil { await backend.bootstrapCount >= seenCount + 3 }
        XCTAssertEqual(notifications.count, 1)

        // Falling edge clears the badge and re-arms the latch.
        await backend.setDefaultResult(.success(Self.makeSnapshot(idle: ["s1"])))
        await waitUntil { !self.pool.hasAttention(forKey: "a") }
        await backend.setDefaultResult(.success(Self.makeSnapshot(blocked: ["s1"])))
        await waitUntil { self.notifications.count == 2 }
    }

    func testAlreadyBlockedFirstContactSeedsBadgeWithoutNotification() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(blocked: ["s1"]))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()

        await waitUntil { self.pool.hasAttention(forKey: "a") }
        let count = await backend.bootstrapCount
        await waitUntil { await backend.bootstrapCount >= count + 2 }
        XCTAssertTrue(
            notifications.isEmpty,
            "stale attention at first contact is a badge, never a banner"
        )
    }

    func testForegroundWorkspaceNeverNotifies() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(idle: ["s1"]))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        foreground = "a"
        makePool()
        pool.refreshTargets()
        await waitUntil { self.pool.snapshot(forKey: "a") != nil }

        await backend.setDefaultResult(.success(Self.makeSnapshot(blocked: ["s1"])))
        await waitUntil { self.pool.hasAttention(forKey: "a") }
        let count = await backend.bootstrapCount
        await waitUntil { await backend.bootstrapCount >= count + 2 }
        XCTAssertTrue(notifications.isEmpty)
    }

    // MARK: - Lend / adopt handoff

    func testLendRetiresPoolConnectionAndNeverDuplicatesWhileExcluded() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(hostID: "h1"))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()
        await waitUntil { self.pool.snapshot(forKey: "a") != nil }

        // Scope entry: the cached snapshot seeds the runtime and the pool's
        // own connection retires — exactly one live connection remains.
        excluded = ["a"]
        let seed = pool.lendConnection(forKey: "a")
        XCTAssertEqual(seed?.macID, "h1")
        await waitUntil { await backend.closeCount == 1 }
        let countAfterLend = await backend.bootstrapCount
        try? await Task.sleep(nanoseconds: 120_000_000)
        let laterCount = await backend.bootstrapCount
        XCTAssertEqual(laterCount, countAfterLend, "lent workspace must not be re-polled")

        // While excluded (runtime-served) a reconcile still never reopens it,
        // and the cache remains for peeks.
        pool.refreshTargets()
        try? await Task.sleep(nanoseconds: 80_000_000)
        let excludedCount = await backend.bootstrapCount
        XCTAssertEqual(excludedCount, countAfterLend)
        XCTAssertNotNil(pool.snapshot(forKey: "a"))

        // The runtime lets go (scope left, connection retired): pooling
        // resumes on the next reconcile.
        excluded = []
        pool.refreshTargets()
        await waitUntil { await backend.bootstrapCount > countAfterLend }
    }

    func testForgottenWorkspaceDropsCacheAndConnection() async {
        let backend = StubPoolBackend(
            defaultResult: .success(Self.makeSnapshot(blocked: ["s1"]))
        )
        backends = ["a": backend]
        targets = [makeTarget(key: "a")]
        makePool()
        pool.refreshTargets()
        await waitUntil { self.pool.snapshot(forKey: "a") != nil }

        targets = []
        pool.refreshTargets()
        await waitUntil { await backend.closeCount == 1 }
        XCTAssertNil(pool.snapshot(forKey: "a"))
        XCTAssertFalse(pool.hasAttention(forKey: "a"))
    }

    // MARK: - Optimistic organization projection

    func testOrganizationHoldsKeepCarouselSnapshotInCommittedOrderUntilHostConfirms() {
        makePool()
        let initial = Self.makeOrganizationSnapshot(
            rootProjectIDs: ["p1", "p2", "p3"],
            sessionIDs: ["s1", "s2", "s3"],
            capturedAtUnixMs: 1_000
        )
        pool.noteExternalSnapshot(initial, forKey: "a", name: "Workspace a")

        pool.holdProjectOrder(
            forKey: "a", parentID: nil, orderedIDs: ["p3", "p1", "p2"]
        )
        pool.holdSessionOrder(
            forKey: "a", projectID: "p1", orderedIDs: ["s3", "s1", "s2"]
        )
        assertHeldOrganizationOrder()

        // The foreground runtime can publish one stale bootstrap immediately
        // after the effect returns. It must not roll the carousel's ghost
        // page back to its pre-drop order.
        pool.noteExternalSnapshot(
            initial.withCapturedAt(2_000), forKey: "a", name: "Workspace a"
        )
        assertHeldOrganizationOrder()

        // A naturally matching bootstrap releases both holds. A later Host
        // order is then authoritative again rather than pinned forever.
        let confirmed = Self.makeOrganizationSnapshot(
            rootProjectIDs: ["p3", "p1", "p2"],
            sessionIDs: ["s3", "s1", "s2"],
            capturedAtUnixMs: 3_000
        )
        pool.noteExternalSnapshot(confirmed, forKey: "a", name: "Workspace a")
        pool.noteExternalSnapshot(
            initial.withCapturedAt(4_000), forKey: "a", name: "Workspace a"
        )
        XCTAssertEqual(
            pool.snapshot(forKey: "a")?.projects
                .filter { $0.parentProjectID == nil }.map(\.id),
            ["p1", "p2", "p3"]
        )
        XCTAssertEqual(pool.snapshot(forKey: "a")?.sessions.map(\.id), ["s1", "s2", "s3"])
    }

    private func assertHeldOrganizationOrder(
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let snapshot = pool.snapshot(forKey: "a")
        XCTAssertEqual(
            snapshot?.projects.filter { $0.parentProjectID == nil }.map(\.id),
            ["p3", "p1", "p2"],
            file: file,
            line: line
        )
        XCTAssertEqual(
            snapshot?.sessions.map(\.id), ["s3", "s1", "s2"],
            file: file,
            line: line
        )
        XCTAssertEqual(
            snapshot?.projects.first { $0.id == "p1" }?.sessionOrder,
            ["g1", "s3", "s1", "s2"],
            "the child-group slot must not move with Session ranks",
            file: file,
            line: line
        )
    }

    // MARK: - Helpers

    private func waitUntil(
        iterations: Int = 6000,
        _ predicate: @escaping @MainActor () async -> Bool
    ) async {
        for _ in 0..<iterations {
            if await predicate() { return }
            await Task.yield()
            try? await Task.sleep(nanoseconds: 500_000)
        }
        XCTFail("Timed out waiting for asynchronous pool state")
    }

    private static func makeSnapshot(
        hostID: String = "host",
        blocked: [String] = [],
        idle: [String] = [],
        capturedAtUnixMs: Int64? = nil
    ) -> RemoteBootstrapSnapshot {
        var sessions: [RemoteSessionSummary] = []
        for id in blocked {
            sessions.append(makeSession(id: id, activity: .blocked))
        }
        for id in idle {
            sessions.append(makeSession(id: id, activity: .idle))
        }
        return RemoteBootstrapSnapshot(
            hostProtocol: RemoteHostProtocolDescriptor(capabilities: [
                "host.bootstrap",
                "session.output.read",
            ]),
            macID: hostID,
            macName: "Host",
            folders: [],
            projects: [],
            presets: [],
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs
                ?? Int64(Date().timeIntervalSince1970 * 1000)
        )
    }

    private static func makeOrganizationSnapshot(
        rootProjectIDs: [String],
        sessionIDs: [String],
        capturedAtUnixMs: Int64
    ) -> RemoteBootstrapSnapshot {
        var projects = rootProjectIDs.map { id in
            RemoteProjectSummary(
                id: id,
                name: id,
                path: "/tmp/\(id)",
                sessionOrder: id == "p1" ? ["g1"] + sessionIDs : nil
            )
        }
        projects.insert(
            RemoteProjectSummary(
                id: "g1",
                name: "Group",
                path: "/tmp/p1",
                parentProjectID: "p1",
                isGroup: true
            ),
            at: min(1, projects.endIndex)
        )
        return RemoteBootstrapSnapshot(
            hostProtocol: RemoteHostProtocolDescriptor(capabilities: [
                "host.bootstrap",
                "project.organization.set",
                "session.reorder",
            ]),
            macID: "host",
            macName: "Host",
            folders: [],
            projects: projects,
            presets: [],
            sessions: sessionIDs.map {
                makeSession(id: $0, projectID: "p1", activity: .idle)
            },
            capturedAtUnixMs: capturedAtUnixMs
        )
    }

    private static func makeSession(
        id: String,
        projectID: String = "project",
        activity: RemoteActivityState
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: projectID,
            title: id,
            command: "claude",
            createdAtUnixMs: 1,
            status: .running,
            activity: activity,
            unread: false
        )
    }
}

private enum StubPoolError: Error {
    case unexpected
    case unreachable
}

/// Minimal scripted backend: bootstraps answer from a queue (else the
/// default result, else park on a continuation when `hold` is set); every
/// other protocol call is unreachable for a read-only pool connection.
private actor StubPoolBackend: NativeRemoteBackendProtocol {
    private var queued: [Result<RemoteBootstrapSnapshot, Error>] = []
    private var defaultResult: Result<RemoteBootstrapSnapshot, Error>?
    private let hold: Bool
    private var held: [CheckedContinuation<RemoteBootstrapSnapshot, Error>] = []

    private(set) var bootstrapCount = 0
    private(set) var bootstrapTimestamps: [Date] = []
    private(set) var closeCount = 0

    init(
        defaultResult: Result<RemoteBootstrapSnapshot, Error>? = nil,
        hold: Bool = false
    ) {
        self.defaultResult = defaultResult
        self.hold = hold
    }

    func setDefaultResult(_ result: Result<RemoteBootstrapSnapshot, Error>) {
        defaultResult = result
    }

    func enqueue(_ result: Result<RemoteBootstrapSnapshot, Error>) {
        queued.append(result)
    }

    func resolveHeld(_ result: Result<RemoteBootstrapSnapshot, Error>) {
        guard !held.isEmpty else { return }
        held.removeFirst().resume(with: result)
    }

    func bootstrap() async throws -> RemoteBootstrapSnapshot {
        bootstrapCount += 1
        bootstrapTimestamps.append(Date())
        if !queued.isEmpty {
            return try queued.removeFirst().get()
        }
        if hold {
            return try await withCheckedThrowingContinuation { held.append($0) }
        }
        guard let defaultResult else { throw StubPoolError.unexpected }
        return try defaultResult.get()
    }

    func close() async {
        closeCount += 1
    }

    // A read-only pool connection must never issue any of these.

    func pollOutput(
        sessionID _: String,
        limit _: Int,
        waitMilliseconds _: UInt64
    ) async throws -> NativeRemoteOutputPage {
        throw StubPoolError.unexpected
    }

    func pollOutputFrom(
        sessionID _: String,
        requestedOffset _: UInt64?,
        limit _: Int,
        waitMilliseconds _: UInt64
    ) async throws -> NativeRemoteOutputPage {
        throw StubPoolError.unexpected
    }

    func commitOutput(_: NativeRemoteOutputPage) async throws {
        throw StubPoolError.unexpected
    }

    func discardOutput(_: NativeRemoteOutputPage) async {}

    func resetOutput(sessionID _: String) async throws {
        throw StubPoolError.unexpected
    }

    func writeTerminal(
        sessionID _: String,
        data _: Data
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func fitDesktop(
        sessionID _: String,
        columns _: UInt16,
        rows _: UInt16
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func clearDesktopFit(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func markRead(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func setSessionTitle(
        sessionID _: String,
        title _: String
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func setSessionPinned(
        sessionID _: String,
        pinned _: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func archiveSession(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func restoreSession(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func stopSession(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func removeSession(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func restartSession(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func resumeAgent(sessionID _: String) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func setSessionOrder(
        projectID _: String,
        orderedSessionIDs _: [String]
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func setProjectOrganization(
        projectID _: String,
        patch _: RemoteProjectOrganizationPatch
    ) async throws -> NativeRemoteEffectReceipt {
        throw StubPoolError.unexpected
    }

    func createSession(
        _: RemoteCreateSessionRequest
    ) async throws -> NativeRemoteCreatedSession {
        throw StubPoolError.unexpected
    }

    func pairingInvitation(_: Data) async throws -> Data {
        throw StubPoolError.unexpected
    }

    func listArchivedSessions(projectID _: String) async throws -> [RemoteSessionSummary] {
        throw StubPoolError.unexpected
    }

    func transcriptMarkdown(
        sessionID _: String,
        entries _: Int?
    ) async throws -> RemoteTranscriptMarkdown {
        throw StubPoolError.unexpected
    }

    func sessionMetrics(sessionID _: String) async throws -> NativeRemoteSessionMetrics {
        throw StubPoolError.unexpected
    }
}
