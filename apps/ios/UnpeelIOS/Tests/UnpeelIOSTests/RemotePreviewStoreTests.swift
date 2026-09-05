import XCTest
import UnpeelShared
@testable import UnpeelIOS

@MainActor
private final class BootstrapResponseGate {
    private var continuation: CheckedContinuation<RemoteBootstrapSnapshot, Error>?

    func response() async throws -> RemoteBootstrapSnapshot {
        try await withCheckedThrowingContinuation { continuation = $0 }
    }

    func waitUntilStarted() async {
        while continuation == nil { await Task.yield() }
    }

    func succeed(with snapshot: RemoteBootstrapSnapshot) {
        continuation?.resume(returning: snapshot)
        continuation = nil
    }
}

@MainActor
final class RemotePreviewStoreTests: XCTestCase {
    func testInitialSelectionPrefersAttentionSession() {
        let store = RemotePreviewStore.preview

        XCTAssertEqual(store.selectedSession?.activity, .blocked)
    }

    func testInitialSelectionIncludesEveryProviderTerminal() {
        let base = RemoteBootstrapSnapshot.mock
        let amp = RemoteSessionSummary(
            id: "amp-session",
            projectID: "project-unpeel",
            providerID: "amp",
            title: "Amp terminal",
            command: "amp",
            createdAtUnixMs: 1,
            status: .running,
            activity: .blocked
        )
        let codex = RemoteSessionSummary(
            id: "codex-session",
            projectID: "project-unpeel",
            providerID: "codex",
            title: "Codex terminal",
            command: "codex",
            createdAtUnixMs: 2,
            status: .running,
            activity: .idle
        )
        let snapshot = RemoteBootstrapSnapshot(
            protocolVersion: base.protocolVersion,
            macID: base.macID,
            macName: base.macName,
            folders: base.folders,
            projects: base.projects,
            presets: base.presets,
            sessions: [amp, codex],
            capturedAtUnixMs: base.capturedAtUnixMs
        )
        let store = RemotePreviewStore(snapshot: snapshot)

        XCTAssertTrue(amp.supportsIOSSessionAPI)
        XCTAssertTrue(codex.supportsIOSSessionAPI)
        XCTAssertEqual(store.selectedSessionID, amp.id)

        store.select(codex)
        XCTAssertEqual(store.selectedSessionID, codex.id)
    }

    func testSelectDismissesSessionsDrawer() {
        let store = RemotePreviewStore.preview
        let target = store.snapshot.sessions.last!
        store.sessionsDrawerPresented = true

        store.select(target)

        XCTAssertEqual(store.selectedSessionID, target.id)
        XCTAssertFalse(store.sessionsDrawerPresented)
    }

    // MARK: - MCP approvals

    func testWriteApprovalSelectsTargetAndSurfacesAttention() {
        let caller = activitySession(id: "caller", activity: .working, updatedAtUnixMs: 2)
        let target = activitySession(id: "target", activity: .idle, updatedAtUnixMs: 1)
        let store = RemotePreviewStore(
            snapshot: restoreSnapshot(
                sessions: [caller, target],
                pendingApprovals: [
                    RemotePendingApproval(
                        id: "a1",
                        kind: "write",
                        title: "Allow write?",
                        body: "body",
                        callerSessionID: "caller",
                        targetSessionID: "target",
                        requestedAtUnixMs: 1
                    )
                ]
            )
        )

        XCTAssertEqual(store.selectedSessionID, "target")
        XCTAssertTrue(store.sessionNeedsMcpApprovalAttention("target"))
        XCTAssertFalse(store.sessionNeedsMcpApprovalAttention("caller"))
        XCTAssertEqual(store.pendingApproval(forSessionID: "target")?.id, "a1")
        XCTAssertEqual(store.bellBlockedSessions.map(\.id), ["target"])
        XCTAssertEqual(store.bellActiveSessions.map(\.id), ["caller"])
    }

    func testBrowserApprovalPresentsOnCaller() {
        let caller = activitySession(id: "caller", activity: .working, updatedAtUnixMs: 1)
        let other = activitySession(id: "other", activity: .idle, updatedAtUnixMs: 2)
        let store = RemotePreviewStore(
            snapshot: restoreSnapshot(
                sessions: [other, caller],
                pendingApprovals: [
                    RemotePendingApproval(
                        id: "a1",
                        kind: "browser",
                        title: "Allow browser?",
                        body: "body",
                        callerSessionID: "caller",
                        requestedAtUnixMs: 1
                    )
                ]
            )
        )

        XCTAssertEqual(store.selectedSessionID, "caller")
        XCTAssertTrue(store.sessionNeedsMcpApprovalAttention("caller"))
        XCTAssertEqual(store.bellBlockedSessions.map(\.id), ["caller"])
        XCTAssertTrue(store.bellActiveSessions.isEmpty)
    }

    func testExistingApprovalDoesNotReselectAfterUserNavigatesAway() async {
        let caller = activitySession(id: "caller", activity: .idle, updatedAtUnixMs: 1)
        let target = activitySession(id: "target", activity: .idle, updatedAtUnixMs: 2)
        let approval = RemotePendingApproval(
            id: "a1",
            kind: "write",
            title: "Allow write?",
            body: "body",
            callerSessionID: "caller",
            targetSessionID: "target",
            requestedAtUnixMs: 1
        )
        var capturedAt: Int64 = 1
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                self.restoreSnapshot(
                    sessions: [caller, target],
                    capturedAtUnixMs: capturedAt,
                    pendingApprovals: [approval]
                )
            }
        )

        _ = await store.loadFromBridge()
        XCTAssertEqual(store.selectedSessionID, "target")

        store.select(caller)
        XCTAssertEqual(store.selectedSessionID, "caller")

        capturedAt = 2
        _ = await store.loadFromBridge()
        XCTAssertEqual(
            store.selectedSessionID, "caller",
            "a later poll with the same pending approval must not steal selection"
        )
    }

    // MARK: - Activity sheet

    func testBellActivityBucketsKeepSelectedBlockersFirstAndDeduplicated() {
        let blockedZ = activitySession(
            id: "blocked-z", activity: .blocked, updatedAtUnixMs: 300
        )
        let blockedA = activitySession(
            id: "blocked-a", activity: .blocked, updatedAtUnixMs: 300
        )
        let blockedOld = activitySession(
            id: "blocked-old", activity: .blocked, updatedAtUnixMs: 100
        )
        let working = activitySession(
            id: "working", activity: .working, updatedAtUnixMs: 200
        )
        let blockedAliasWorking = activitySession(
            id: "blocked-z", activity: .working, updatedAtUnixMs: 400
        )
        let finished = activitySession(
            id: "finished", activity: .done, updatedAtUnixMs: 500,
            unread: true, alert: "Close to the weekly limit"
        )
        let snapshot = sessionCreationSnapshot(
            hostProtocol: nil,
            sessions: [
                blockedOld,
                working,
                blockedZ,
                finished,
                blockedA,
                blockedZ,
                blockedAliasWorking,
            ]
        )
        let store = RemotePreviewStore(snapshot: snapshot)
        // The currently open blocker must still be visible in the bell sheet.
        store.selectedSessionID = blockedZ.id

        XCTAssertEqual(
            store.bellBlockedSessions.map(\.id),
            ["blocked-a", "blocked-z", "blocked-old"]
        )
        XCTAssertEqual(store.bellActiveSessions.map(\.id), ["working"])
        XCTAssertEqual(store.bellRecentSessions.map(\.id), ["finished"])
        XCTAssertEqual(
            store.bellRecentSessions.first?.latestAlertBody,
            "Close to the weekly limit"
        )

        let blockerIDs = Set(store.bellBlockedSessions.map(\.id))
        XCTAssertTrue(blockerIDs.isDisjoint(with: store.bellActiveSessions.map(\.id)))
        XCTAssertTrue(blockerIDs.isDisjoint(with: store.bellRecentSessions.map(\.id)))
    }

    // MARK: - Disconnected transport recovery

    func testDisconnectedRecoveryGoesDirectlyToRelay() async {
        var relayAttempts = 0
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                throw URLError(.cannotConnectToHost)
            }
        )
        store.attemptRelayFallback = {
            relayAttempts += 1
            return true
        }

        await store.loadFromBridge()
        XCTAssertTrue(store.isDisconnected)

        let recovered = await store.recoverDisconnectedConnection()

        XCTAssertTrue(recovered)
        XCTAssertEqual(relayAttempts, 1)
    }

    func testConnectedStateNeverStartsRelayRecovery() async {
        var relayAttempts = 0
        let store = RemotePreviewStore(snapshot: .empty)
        store.attemptRelayFallback = {
            relayAttempts += 1
            return true
        }

        let recovered = await store.recoverDisconnectedConnection()

        XCTAssertFalse(recovered)
        XCTAssertEqual(relayAttempts, 0)
    }

    func testSuccessfulPollProofCapturesExactClientAndConnectionEpoch() async {
        let client = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.1:4485")!,
            authToken: "bearer-a"
        )
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: client,
            createSessionOverride: nil,
            bootstrapOverride: { .mock }
        )
        store.adoptClient(client, connectionEpoch: 17)

        let pollResult = await store.loadFromBridge()
        guard case .success(let proof) = pollResult else {
            return XCTFail("expected a successful poll proof")
        }

        XCTAssertEqual(proof.connectionEpoch, 17)
        XCTAssertEqual(proof.hostMacID, RemoteBootstrapSnapshot.mock.macID)
        XCTAssertEqual(proof.directEndpoint, RemoteBootstrapSnapshot.mock.directEndpoint)
        XCTAssertEqual(proof.client.baseURL, client.baseURL)
        XCTAssertEqual(proof.client.authToken, "bearer-a")
        XCTAssertFalse(proof.client.isRelay)
    }

    func testPollResponseIsDiscardedAfterCrossMacClientGenerationChange() async {
        let gate = BootstrapResponseGate()
        let clientA = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.1:4485")!,
            authToken: "bearer-a"
        )
        let clientB = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.2:4485")!,
            authToken: "bearer-b"
        )
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: clientA,
            createSessionOverride: nil,
            bootstrapOverride: { try await gate.response() }
        )
        store.adoptClient(clientA, connectionEpoch: 1)

        let poll = Task { await store.loadFromBridge() }
        await gate.waitUntilStarted()
        store.adoptClient(clientB, connectionEpoch: 2)
        gate.succeed(with: .mock)

        let pollResult = await poll.value
        guard case .superseded = pollResult else {
            return XCTFail("Mac A success must not be attributed to current Mac B")
        }
        XCTAssertNil(store.snapshot.macID, "stale Mac A snapshot must not be applied")
        XCTAssertEqual(store.client.baseURL, clientB.baseURL)
        XCTAssertEqual(store.client.authToken, "bearer-b")
    }

    func testSupersededPollNeverUsesStickyDisconnectedStateForNewMacFallback() async {
        let gate = BootstrapResponseGate()
        var bootstrapCalls = 0
        let clientA = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.1:4485")!,
            authToken: "bearer-a"
        )
        let clientB = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.2:4485")!,
            authToken: "bearer-b"
        )
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: clientA,
            createSessionOverride: nil,
            bootstrapOverride: {
                bootstrapCalls += 1
                if bootstrapCalls == 1 { throw URLError(.cannotConnectToHost) }
                return try await gate.response()
            }
        )
        store.adoptClient(clientA, connectionEpoch: 1)
        var relayAttempts = 0
        store.attemptRelayFallback = {
            relayAttempts += 1
            return true
        }

        let failedPoll = await store.loadFromBridge()
        guard case .currentFailure = failedPoll else {
            return XCTFail("first Mac A poll should be a current failure")
        }
        XCTAssertTrue(store.isDisconnected)

        let stalePoll = Task { await store.loadFromBridge() }
        await gate.waitUntilStarted()
        store.adoptClient(clientB, connectionEpoch: 2)
        gate.succeed(with: .mock)
        let staleResult = await stalePoll.value
        guard case .superseded = staleResult else {
            return XCTFail("in-flight Mac A completion should be superseded")
        }

        let recovered = await store.recoverDisconnectedConnection(after: staleResult)
        XCTAssertFalse(recovered)
        XCTAssertEqual(relayAttempts, 0, "Mac B needs its own failed Direct poll first")
    }

    // MARK: - Unreachable grace period

    func testDisconnectStaysCalmConnectingUntilFailuresOutliveGrace() async {
        var now = Date(timeIntervalSinceReferenceDate: 1_000)
        var failing = true
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                if failing { throw URLError(.cannotConnectToHost) }
                return .mock
            }
        )
        store.nowProvider = { now }

        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isDisconnected)
        XCTAssertFalse(store.isUnreachable, "a young outage is ordinary connecting")
        XCTAssertEqual(store.connectionStatus, "Connecting…")

        now += 5
        _ = await store.loadFromBridge()
        XCTAssertFalse(store.isUnreachable, "still inside the grace period")

        now += 4 // 9s of continuous failure — past the ~8s grace
        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isUnreachable)
        XCTAssertEqual(store.connectionStatus, "Connection lost")

        failing = false
        now += 1
        _ = await store.loadFromBridge()
        XCTAssertFalse(store.isDisconnected)
        XCTAssertFalse(store.isUnreachable)

        failing = true
        now += 1
        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isDisconnected)
        XCTAssertFalse(store.isUnreachable, "success resets the outage clock")
    }

    func testConnectivityEventsResetTheUnreachableGrace() async {
        var now = Date(timeIntervalSinceReferenceDate: 2_000)
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { throw URLError(.cannotConnectToHost) }
        )
        store.nowProvider = { now }

        _ = await store.loadFromBridge()
        now += 9
        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isUnreachable)

        // Foreground resume restarts attempts: drop back to calm connecting.
        store.prepareForForegroundResume()
        XCTAssertFalse(store.isUnreachable)
        XCTAssertEqual(store.connectionStatus, "Connecting…")
        _ = await store.loadFromBridge() // the one-shot suppressed failure
        XCTAssertFalse(store.isUnreachable)
        now += 4
        _ = await store.loadFromBridge() // outage clock restarts here
        XCTAssertFalse(store.isUnreachable)
        now += 9
        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isUnreachable, "persisting failures fail again after the reset")

        // An epoch bump (re-pair, Bonjour reappearance, relay fallback) also
        // grants the new connection its own grace.
        store.adoptClient(store.client, connectionEpoch: 7)
        XCTAssertFalse(store.isUnreachable)
        now += 1
        _ = await store.loadFromBridge()
        XCTAssertTrue(store.isDisconnected)
        XCTAssertFalse(store.isUnreachable)
    }

    // MARK: - Host session-create capability

    func testSessionCreationCapabilityKeepsLegacyPermissiveAndRequiresAdvertisedOperation() {
        let legacy = RemotePreviewStore(snapshot: sessionCreationSnapshot(hostProtocol: nil))
        XCTAssertTrue(legacy.supportsSessionCreation)
        legacy.showPresetDrawer(for: "project-unpeel")
        XCTAssertEqual(legacy.presetDrawerProjectID, "project-unpeel")

        let advertised = RemotePreviewStore(
            snapshot: sessionCreationSnapshot(
                hostProtocol: .init(capabilities: ["session.create"])
            )
        )
        XCTAssertTrue(advertised.supportsSessionCreation)

        let omitted = RemotePreviewStore(
            snapshot: sessionCreationSnapshot(
                hostProtocol: .init(capabilities: ["host.bootstrap"])
            )
        )
        XCTAssertFalse(omitted.supportsSessionCreation)
        omitted.showPresetDrawer(for: "project-unpeel")
        XCTAssertNil(omitted.presetDrawerProjectID)
        XCTAssertNil(
            omitted.startSession(
                projectID: "project-unpeel",
                preset: omitted.snapshot.presets[0]
            )
        )

        let incompatible = RemotePreviewStore(
            snapshot: sessionCreationSnapshot(
                hostProtocol: .init(
                    majorVersion: RemoteControlProtocol.hostMajorVersion + 1,
                    capabilities: ["session.create"]
                )
            )
        )
        XCTAssertFalse(incompatible.supportsSessionCreation)
    }

    func testResumeAgentRequiresReturnedShellAndBothCapabilities() async {
        func session(
            resumeAgent: Bool?,
            activeRuntimeID: String? = nil,
            runtimeLaunchPending: Bool = false
        ) -> RemoteSessionSummary {
            RemoteSessionSummary(
                id: "live", projectID: "project-unpeel",
                activeRuntimeID: activeRuntimeID,
                runtimeLaunchPending: runtimeLaunchPending,
                providerID: "claude",
                title: "Claude", command: "claude", createdAtUnixMs: 1,
                status: .running, activity: .idle,
                capabilities: RemoteSessionCapabilities(
                    restart: false,
                    resumeAgent: resumeAgent,
                    notifyWhenDone: true
                )
            )
        }

        let oldHost = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: nil, sessions: [session(resumeAgent: true)]
        ))
        let oldHostResult = await oldHost.performSessionAction(
            sessionID: "live", action: .resumeAgent
        )
        XCTAssertFalse(oldHostResult)

        let unavailable = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: .init(capabilities: [
                RemoteControlProtocol.sessionRuntimeResumeCapability,
            ]),
            sessions: [session(resumeAgent: false)]
        ))
        let unavailableResult = await unavailable.performSessionAction(
            sessionID: "live", action: .resumeAgent
        )
        XCTAssertFalse(unavailableResult)

        let incompatible = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: .init(
                majorVersion: RemoteControlProtocol.hostMajorVersion + 1,
                capabilities: [RemoteControlProtocol.sessionRuntimeResumeCapability]
            ),
            sessions: [session(resumeAgent: true)]
        ))
        let incompatibleResult = await incompatible.performSessionAction(
            sessionID: "live", action: .resumeAgent
        )
        XCTAssertFalse(incompatibleResult)

        let active = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: .init(
                capabilities: [RemoteControlProtocol.sessionRuntimeResumeCapability]
            ),
            sessions: [session(resumeAgent: true, activeRuntimeID: "claude")]
        ))
        let activeResult = await active.performSessionAction(
            sessionID: "live", action: .resumeAgent
        )
        XCTAssertFalse(activeResult)

        let pending = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: .init(
                capabilities: [RemoteControlProtocol.sessionRuntimeResumeCapability]
            ),
            sessions: [session(resumeAgent: true, runtimeLaunchPending: true)]
        ))
        let pendingResult = await pending.performSessionAction(
            sessionID: "live", action: .resumeAgent
        )
        XCTAssertFalse(pendingResult)

        // Legacy decoding remains possible, but current phone code never
        // emits the active-runtime restart action.
        let legacyResult = await active.performSessionAction(
            sessionID: "live", action: .restartAgent
        )
        XCTAssertFalse(legacyResult)
    }

    func testSessionOrganizerResumePresentationIsHonestForEveryLifecycle() {
        func session(
            status: RemoteSessionStatus = .running,
            activeRuntimeID: String? = nil,
            runtimeLaunchPending: Bool = false,
            restart: Bool = false,
            resumeAgent: Bool? = true,
            archived: Bool = false
        ) -> RemoteSessionSummary {
            RemoteSessionSummary(
                id: "session", projectID: "project",
                activeRuntimeID: activeRuntimeID,
                runtimeLaunchPending: runtimeLaunchPending,
                providerID: "claude",
                title: "Claude", command: "claude", createdAtUnixMs: 1,
                status: status, activity: .idle,
                capabilities: RemoteSessionCapabilities(
                    restart: restart,
                    resumeAgent: resumeAgent,
                    notifyWhenDone: false
                ),
                archived: archived
            )
        }

        let currentHost = RemoteHostProtocolDescriptor(capabilities: [
            RemoteControlProtocol.sessionRuntimeResumeCapability,
        ])
        let legacyHost = RemoteHostProtocolDescriptor(
            minorVersion: 5,
            capabilities: [RemoteControlProtocol.sessionRuntimeRestartCapability]
        )

        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(activeRuntimeID: "claude"),
                hostProtocol: currentHost
            ),
            .none
        )
        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(), hostProtocol: currentHost
            ),
            .resumeAgent
        )
        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(runtimeLaunchPending: true),
                hostProtocol: currentHost
            ),
            .none
        )
        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(), hostProtocol: legacyHost
            ),
            .none
        )
        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(
                    status: .exited, restart: false,
                    resumeAgent: nil, archived: true
                ),
                hostProtocol: currentHost
            ),
            .restore
        )
        XCTAssertEqual(
            sessionOrganizeResumePresentation(
                session: session(
                    status: .exited, restart: true,
                    resumeAgent: nil, archived: true
                ),
                hostProtocol: currentHost
            ),
            .restoreAndResume
        )
    }

    func testLegacyRestartIsStoppedResumeOnly() async {
        let live = RemoteSessionSummary(
            id: "live", projectID: "project-unpeel", providerID: "claude",
            title: "Claude", command: "claude", createdAtUnixMs: 1,
            status: .running, activity: .idle,
            capabilities: RemoteSessionCapabilities(
                restart: true,
                notifyWhenDone: true
            )
        )
        let store = RemotePreviewStore(snapshot: sessionCreationSnapshot(
            hostProtocol: nil, sessions: [live]
        ))

        let result = await store.performSessionAction(
            sessionID: "live", action: .restart
        )
        XCTAssertFalse(result)
    }

    func testReplacementCorrelationIgnoresBaselineCollisionsAndDecoys() {
        let source = replacementSession(
            id: "source", status: .exited, archived: true
        )
        let baselineCollision = replacementSession(id: "baseline")
        let exact = replacementSession(
            id: "exact", command: "claude --resume thread"
        )
        let intent = RemotePreviewStore.ReplacementSelectionIntent(
            source: source,
            hostMacID: "mac",
            knownSessionIDs: [source.id, baselineCollision.id]
        )
        let decoys = [
            replacementSession(id: "wrong-project", projectID: "elsewhere"),
            replacementSession(id: "wrong-created", createdAtUnixMs: 43),
            replacementSession(
                id: "wrong-runtime", command: "codex resume thread", providerID: "codex"
            ),
            replacementSession(id: "wrong-worktree", worktreePath: "/other"),
            replacementSession(id: "still-archived", archived: true),
        ]

        XCTAssertEqual(
            RemotePreviewStore.replacementSelectionResolution(
                intent,
                sessions: [baselineCollision] + decoys + [exact]
            ),
            .select(exact.id),
            "only the unique post-effect source-correlated row may be selected"
        )

        guard case .wait = RemotePreviewStore.replacementSelectionResolution(
            intent,
            sessions: [source, exact]
        ) else {
            return XCTFail("the old id must disappear before its replacement is selectable")
        }
    }

    func testReplacementCorrelationCancelsOnAmbiguityAndBoundedExpiry() {
        let source = replacementSession(
            id: "source", status: .exited, archived: true
        )
        let intent = RemotePreviewStore.ReplacementSelectionIntent(
            source: source,
            hostMacID: "mac",
            knownSessionIDs: [source.id]
        )
        let first = replacementSession(id: "candidate-a")
        let second = replacementSession(id: "candidate-b")

        XCTAssertEqual(
            RemotePreviewStore.replacementSelectionResolution(
                intent, sessions: [first, second]
            ),
            .cancel,
            "Host row order must never break a replacement collision tie"
        )

        let expiring = RemotePreviewStore.ReplacementSelectionIntent(
            source: source,
            hostMacID: "mac",
            knownSessionIDs: [source.id],
            bootstrapObservationsRemaining: 1
        )
        XCTAssertEqual(
            RemotePreviewStore.replacementSelectionResolution(
                expiring, sessions: []
            ),
            .cancel,
            "an old intent must not hijack a future matching Session"
        )
    }

    func testAmbiguousReplacementNeverFallsBackOrLaterHijacksSelection() async throws {
        let source = replacementSession(id: "source", status: .exited)
        let first = replacementSession(id: "candidate-a")
        let second = replacementSession(id: "candidate-b")
        let initial = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [source]
        )
        let ambiguous = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [first, second], capturedAtUnixMs: 2
        )
        let laterUnique = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [first], capturedAtUnixMs: 3
        )
        var restartSessionIDs: [String] = []
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                defer { bootstrapCount += 1 }
                return bootstrapCount == 0 ? ambiguous : laterUnique
            },
            restartSessionOverride: { restartSessionIDs.append($0) }
        )

        let restart = try XCTUnwrap(store.restartSelectedSession())
        await restart.value

        XCTAssertEqual(restartSessionIDs, [source.id])
        XCTAssertEqual(bootstrapCount, 1)
        XCTAssertNil(store.selectedSessionID)
        XCTAssertFalse(store.isRestartingSelectedSession)

        _ = await store.loadFromBridge()
        XCTAssertEqual(bootstrapCount, 2)
        XCTAssertNil(
            store.selectedSessionID,
            "a canceled two-candidate intent must not revive when one candidate later remains"
        )
    }

    func testExplicitSelectionAndCreateSupersedeWaitingReplacement() async throws {
        let source = replacementSession(id: "source", status: .exited)
        let other = replacementSession(
            id: "other", command: "codex", createdAtUnixMs: 9,
            providerID: "codex", worktreePath: nil, worktreeBranch: nil
        )
        let exact = replacementSession(id: "replacement")
        let created = replacementSession(
            id: "created", command: "codex", createdAtUnixMs: 10,
            providerID: "codex", worktreePath: nil, worktreeBranch: nil
        )
        let initial = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [source, other]
        )
        let waiting = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [source, other, exact], capturedAtUnixMs: 2
        )
        let afterCreate = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [created, exact, other], capturedAtUnixMs: 3
        )
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(),
            createSessionOverride: { _ in
                RemoteCreateSessionResponse(sessionID: created.id, session: created)
            },
            bootstrapOverride: {
                defer { bootstrapCount += 1 }
                return bootstrapCount == 0 ? waiting : afterCreate
            },
            restartSessionOverride: { _ in }
        )

        let restart = try XCTUnwrap(store.restartSelectedSession())
        await restart.value
        XCTAssertEqual(store.selectedSessionID, source.id)

        store.select(other)
        XCTAssertEqual(store.selectedSessionID, other.id)
        _ = await store.loadFromBridge()
        XCTAssertEqual(
            store.selectedSessionID, other.id,
            "a replacement published after explicit selection must not steal focus"
        )

        // Stage another waiting replacement, then prove exact-id creation is
        // the newer focus intent and cannot be hijacked by that replacement.
        let restaged = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(),
            createSessionOverride: { _ in
                RemoteCreateSessionResponse(sessionID: created.id, session: created)
            },
            bootstrapOverride: { afterCreate },
            restartSessionOverride: { _ in }
        )
        let restagedRestart = try XCTUnwrap(restaged.restartSelectedSession())
        let create = try XCTUnwrap(restaged.startSession(
            projectID: "project-unpeel",
            preset: initial.presets[0]
        ))
        await create.value
        await restagedRestart.value
        XCTAssertEqual(restaged.selectedSessionID, created.id)
    }

    func testHostSwitchCancelsWaitingReplacementBeforeNewBootstrapDefaults() async throws {
        let source = replacementSession(id: "source", status: .exited)
        let oldReplacement = replacementSession(id: "old-host-replacement")
        let newHostDefault = replacementSession(
            id: "new-host-default", command: "codex", createdAtUnixMs: 9,
            providerID: "codex", worktreePath: nil, worktreeBranch: nil
        )
        let initial = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [source], macID: "host-a"
        )
        let waiting = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [source, oldReplacement],
            capturedAtUnixMs: 2, macID: "host-a"
        )
        let hostB = sessionCreationSnapshot(
            hostProtocol: nil, sessions: [newHostDefault, oldReplacement],
            capturedAtUnixMs: 3, macID: "host-b"
        )
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(
                baseURL: URL(string: "http://host-a.local")!, authToken: "a"
            ),
            createSessionOverride: nil,
            bootstrapOverride: {
                defer { bootstrapCount += 1 }
                return bootstrapCount == 0 ? waiting : hostB
            },
            restartSessionOverride: { _ in }
        )

        let restart = try XCTUnwrap(store.restartSelectedSession())
        await restart.value
        XCTAssertEqual(store.selectedSessionID, source.id)

        store.adoptClient(
            RemoteMacClient(
                baseURL: URL(string: "http://host-b.local")!, authToken: "b"
            ),
            connectionEpoch: 1
        )
        _ = await store.loadFromBridge()
        XCTAssertEqual(
            store.selectedSessionID,
            newHostDefault.id,
            "an old Host's pending replacement must not cross client identity"
        )
    }

    func testCreateWithoutSummaryPollsBootstrapUntilSessionIsSelectableWithoutRetryingMutation() async throws {
        let hostProtocol = RemoteHostProtocolDescriptor(capabilities: ["session.create"])
        let initial = sessionCreationSnapshot(hostProtocol: hostProtocol)
        let created = RemoteSessionSummary(
            id: "created-session",
            projectID: "project-unpeel",
            providerID: "codex",
            title: "New Codex session",
            command: "codex",
            createdAtUnixMs: 42,
            status: .running,
            activity: .starting
        )
        let converged = sessionCreationSnapshot(
            hostProtocol: hostProtocol,
            sessions: [created],
            capturedAtUnixMs: 3
        )
        var createRequests: [RemoteCreateSessionRequest] = []
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(),
            createSessionOverride: { request in
                createRequests.append(request)
                return RemoteCreateSessionResponse(sessionID: created.id)
            },
            bootstrapOverride: {
                bootstrapCount += 1
                // Stay stale beyond one full 1s Host fallback interval; the
                // Controller must keep polling bootstrap, never re-create.
                return bootstrapCount <= 5 ? initial : converged
            }
        )

        let task = try XCTUnwrap(
            store.startSession(
                projectID: "project-unpeel",
                preset: initial.presets[0]
            )
        )
        await task.value

        XCTAssertEqual(
            createRequests,
            [RemoteCreateSessionRequest(projectID: "project-unpeel", presetID: "preset-codex")]
        )
        XCTAssertEqual(bootstrapCount, 6)
        XCTAssertEqual(store.selectedSessionID, created.id)
        XCTAssertEqual(store.snapshot.sessions.map(\.id), [created.id])
        XCTAssertTrue(store.expandedProjectIDs.contains("project-unpeel"))
        XCTAssertNil(store.launchingPresetID)
    }

    func testCreateSummarySurvivesStaleBootstrapWithoutSelectingExistingTopSession() async throws {
        let existing = RemoteSessionSummary(
            id: "existing-top",
            projectID: "project-unpeel",
            providerID: "claude",
            title: "Existing",
            command: "claude",
            createdAtUnixMs: 1,
            status: .running,
            activity: .idle
        )
        let created = RemoteSessionSummary(
            id: "created-session",
            projectID: "project-unpeel",
            providerID: "codex",
            title: "New Codex session",
            command: "codex",
            createdAtUnixMs: 2,
            status: .running,
            activity: .starting
        )
        let initial = sessionCreationSnapshot(
            hostProtocol: .init(capabilities: ["session.create"]),
            sessions: [existing]
        )
        let converged = sessionCreationSnapshot(
            hostProtocol: initial.hostProtocol,
            sessions: [created, existing],
            capturedAtUnixMs: 3
        )
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: initial,
            client: RemoteMacClient(),
            createSessionOverride: { _ in
                RemoteCreateSessionResponse(sessionID: created.id, session: created)
            },
            bootstrapOverride: {
                defer { bootstrapCount += 1 }
                return bootstrapCount == 0 ? initial : converged
            }
        )

        let task = try XCTUnwrap(store.startSession(
            projectID: "project-unpeel",
            preset: initial.presets[0]
        ))
        await task.value

        XCTAssertEqual(bootstrapCount, 1)
        XCTAssertEqual(store.selectedSessionID, created.id)
        XCTAssertEqual(
            store.snapshot.sessions.map(\.id),
            [created.id, existing.id],
            "a pre-create bootstrap must retain the optimistic row at the top"
        )

        _ = await store.loadFromBridge()
        XCTAssertEqual(store.selectedSessionID, created.id)
        XCTAssertEqual(store.snapshot.sessions.map(\.id), [created.id, existing.id])
    }

    func testSidebarTreeNestsGroupsAndWorktreesUnderTheirProject() throws {
        let snapshot = RemoteBootstrapSnapshot(
            macID: "mac",
            macName: "Mac",
            folders: [
                .init(id: "folder-client", name: "Client", sortOrder: 0),
            ],
            projects: [
                .init(id: "project-shop", name: "Shop", path: "/work/shop", folderID: "folder-client", sortOrder: 0),
                .init(
                    id: "project-legacy",
                    name: "Legacy",
                    path: "/work/legacy",
                    folderID: "folder-client",
                    parentProjectID: "folder-client",
                    sortOrder: 1
                ),
                .init(id: "project-unpeel", name: "Unpeel", path: "/dev/unpeel", sortOrder: 2),
                .init(
                    id: "group-research",
                    name: "Research",
                    path: "/dev/unpeel",
                    parentProjectID: "project-unpeel",
                    isGroup: true,
                    colorID: "violet",
                    sortOrder: 3
                ),
                .init(
                    id: "worktree-native-b",
                    name: "native-b",
                    path: "/tmp/native-b",
                    parentProjectID: "project-unpeel",
                    worktreeBranch: "native-b",
                    sortOrder: 4
                ),
            ],
            presets: [],
            sessions: [],
            capturedAtUnixMs: 1
        )
        let tree = IOSSidebarProjectTree(snapshot: snapshot)

        XCTAssertEqual(tree.looseProjects.map(\.id), ["project-unpeel"])
        XCTAssertEqual(tree.folderGroups.map(\.folder.id), ["folder-client"])
        XCTAssertEqual(tree.folderGroups.first?.projects.map(\.id), ["project-shop", "project-legacy"])
        let root = try XCTUnwrap(snapshot.projects.first { $0.id == "project-unpeel" })
        XCTAssertEqual(
            tree.childProjects(for: root).map(\.id),
            ["group-research", "worktree-native-b"]
        )
        XCTAssertEqual(
            tree.worktreeProjects(for: root).map(\.id),
            ["worktree-native-b"]
        )
    }

    func testSidebarMixKeepsGroupsAboveSessionsUntilOrderIncludesAFolder() {
        let group = RemoteProjectSummary(
            id: "group-research",
            name: "Research",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true
        )
        let sessions = [
            activitySession(id: "session-a", activity: .idle, updatedAtUnixMs: 2),
            activitySession(id: "session-b", activity: .idle, updatedAtUnixMs: 1),
        ]

        XCTAssertEqual(
            IOSSidebarProjectTree.mixedRegularItems(
                sessions: sessions,
                folders: [group],
                order: ["session-a", "session-b"],
                dateSorted: false
            ).map(\.id),
            ["group-research", "session-a", "session-b"]
        )
        XCTAssertEqual(
            IOSSidebarProjectTree.mixedRegularItems(
                sessions: sessions,
                folders: [group],
                order: ["session-a", "group-research", "session-b"],
                dateSorted: false
            ).map(\.id),
            ["session-a", "group-research", "session-b"]
        )
        XCTAssertEqual(
            IOSSidebarProjectTree.mixedRegularItems(
                sessions: sessions,
                folders: [group],
                order: ["session-a", "group-research", "session-b"],
                dateSorted: true
            ).map(\.id),
            ["group-research", "session-a", "session-b"]
        )
    }

    func testSidebarMixMovesPinnedGroupsAheadOfOrdinaryRows() {
        let pinnedIdeas = RemoteProjectSummary(
            id: "group-ideas",
            name: "Ideas",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true,
            pinned: true
        )
        let ordinary = RemoteProjectSummary(
            id: "group-ordinary",
            name: "Ordinary",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true
        )
        let pinnedJobs = RemoteProjectSummary(
            id: "group-jobs",
            name: "Background jobs",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true,
            pinned: true
        )
        let sessions = [
            activitySession(id: "session-a", activity: .idle, updatedAtUnixMs: 2),
            activitySession(id: "session-b", activity: .idle, updatedAtUnixMs: 1),
        ]

        XCTAssertEqual(
            IOSSidebarProjectTree.mixedRegularItems(
                sessions: sessions,
                folders: [pinnedIdeas, ordinary, pinnedJobs],
                order: [
                    "session-a", "group-jobs", "group-ordinary",
                    "session-b", "group-ideas",
                ],
                dateSorted: false
            ).map(\.id),
            [
                "group-jobs", "group-ideas", "session-a",
                "group-ordinary", "session-b",
            ]
        )
    }

    func testSidebarMixAlwaysMovesArchivesBelowFoldersAndRegularSessions() {
        let group = RemoteProjectSummary(
            id: "group-research",
            name: "Research",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true
        )
        let regular = replacementSession(id: "regular")
        let archived = replacementSession(
            id: "archived", status: .exited, archived: true
        )

        XCTAssertEqual(
            IOSSidebarProjectTree.mixedRegularItems(
                sessions: [archived, regular],
                folders: [group],
                order: ["archived", "group-research", "regular"],
                dateSorted: false
            ).map(\.id),
            ["group-research", "regular", "archived"]
        )
    }

    func testSidebarPaneProjectionAttachesOrderedMembersToRepresentative() throws {
        let mainProject = RemoteProjectSummary(
            id: "project-main",
            name: "Main",
            path: "/tmp/main"
        )
        let notesProject = RemoteProjectSummary(
            id: "project-notes",
            name: "Notes",
            path: "/tmp/notes"
        )
        let representative = replacementSession(
            id: "session-main",
            projectID: mainProject.id
        )
        let notes = replacementSession(
            id: "session-notes",
            projectID: notesProject.id
        )
        let review = replacementSession(
            id: "session-review",
            projectID: mainProject.id
        )
        let snapshot = RemoteBootstrapSnapshot(
            folders: [],
            projects: [mainProject, notesProject],
            presets: [],
            // Deliberately use a different bootstrap order: pane member order
            // is owned by the pane projection, not Session list order.
            sessions: [review, representative, notes],
            capturedAtUnixMs: 1,
            paneGroups: [
                .init(
                    id: "pane-group-1",
                    representativeSessionID: representative.id,
                    sessionIDs: [representative.id, notes.id, review.id]
                ),
            ]
        )

        let tree = IOSSidebarProjectTree(snapshot: snapshot)
        XCTAssertEqual(
            tree.paneChildren(for: representative).map(\.id),
            [notes.id, review.id]
        )
        XCTAssertTrue(tree.isPaneChild(notes))
        XCTAssertTrue(tree.isPaneChild(review))
        XCTAssertEqual(tree.paneRepresentative(for: notes)?.id, representative.id)
        XCTAssertFalse(tree.isPaneChild(representative))
        // The projection changes presentation only; the child's real project
        // identity remains available for selection and organization actions.
        XCTAssertEqual(tree.sessions(for: notesProject).map(\.id), [notes.id])
    }

    func testSidebarPaneProjectionRejectsOverlappingAndIncompleteGroups() {
        let project = RemoteProjectSummary(
            id: "project-unpeel",
            name: "Unpeel",
            path: "/dev/unpeel"
        )
        let main = replacementSession(id: "main")
        let child = replacementSession(id: "child")
        let ungrouped = replacementSession(id: "ungrouped")
        let snapshot = RemoteBootstrapSnapshot(
            folders: [],
            projects: [project],
            presets: [],
            sessions: [main, child, ungrouped],
            capturedAtUnixMs: 1,
            paneGroups: [
                .init(
                    id: "accepted",
                    representativeSessionID: main.id,
                    sessionIDs: [main.id, child.id, child.id]
                ),
                .init(
                    id: "overlap",
                    representativeSessionID: ungrouped.id,
                    sessionIDs: [ungrouped.id, child.id]
                ),
                .init(
                    id: "missing-representative",
                    representativeSessionID: "stale",
                    sessionIDs: ["stale", ungrouped.id]
                ),
            ]
        )

        let tree = IOSSidebarProjectTree(snapshot: snapshot)
        XCTAssertEqual(tree.paneChildren(for: main).map(\.id), [child.id])
        XCTAssertFalse(tree.isPaneChild(ungrouped))
        XCTAssertTrue(tree.paneChildren(for: ungrouped).isEmpty)
    }

    func testInitialSelectionInWorktreeExpandsParentAndChild() {
        let snapshot = RemoteBootstrapSnapshot(
            macID: "mac",
            macName: "Mac",
            folders: [],
            projects: [
                .init(id: "project-unpeel", name: "Unpeel", path: "/dev/unpeel", sortOrder: 0),
                .init(
                    id: "worktree-native-b",
                    name: "native-b",
                    path: "/tmp/native-b",
                    parentProjectID: "project-unpeel",
                    worktreeBranch: "native-b",
                    sortOrder: 1
                ),
            ],
            presets: [],
            sessions: [
                .init(
                    id: "session-native-b",
                    projectID: "worktree-native-b",
                    providerID: "codex",
                    title: "iOS",
                    command: "codex",
                    createdAtUnixMs: 1,
                    status: .running,
                    activity: .idle
                ),
            ],
            capturedAtUnixMs: 1
        )
        let store = RemotePreviewStore(snapshot: snapshot)

        XCTAssertEqual(store.selectedSessionID, "session-native-b")
        XCTAssertTrue(store.expandedProjectIDs.contains("project-unpeel"))
        XCTAssertTrue(store.expandedProjectIDs.contains("worktree-native-b"))
    }

    func testInitialSelectionInGroupExpandsParentAndChild() {
        let snapshot = RemoteBootstrapSnapshot(
            macID: "mac",
            macName: "Mac",
            folders: [],
            projects: [
                .init(id: "project-unpeel", name: "Unpeel", path: "/dev/unpeel", sortOrder: 0),
                .init(
                    id: "group-research",
                    name: "Research",
                    path: "/dev/unpeel",
                    parentProjectID: "project-unpeel",
                    isGroup: true,
                    colorID: "violet",
                    sortOrder: 1
                ),
            ],
            presets: [],
            sessions: [
                .init(
                    id: "session-research",
                    projectID: "group-research",
                    providerID: "codex",
                    title: "Research notes",
                    command: "codex",
                    createdAtUnixMs: 1,
                    status: .running,
                    activity: .idle
                ),
            ],
            capturedAtUnixMs: 1
        )
        let store = RemotePreviewStore(snapshot: snapshot)

        XCTAssertEqual(store.selectedSessionID, "session-research")
        XCTAssertTrue(store.expandedProjectIDs.contains("project-unpeel"))
        XCTAssertTrue(store.expandedProjectIDs.contains("group-research"))
        XCTAssertEqual(
            store.sidebarTree.sessions(for: snapshot.projects[1]).map(\.id),
            ["session-research"]
        )
    }

    // MARK: - Poll equality gate

    func testSnapshotEqualityIgnoresOutputPreviewChanges() {
        let base = RemoteBootstrapSnapshot.mock
        let noisy = withSessions(base) { session in
            remaking(
                session,
                updatedAtUnixMs: session.updatedAtUnixMs,
                lastOutputPreview: "fresh tail for \(session.id)"
            )
        }

        XCTAssertTrue(RemotePreviewStore.snapshotContentEqual(base, noisy))
    }

    func testSnapshotEqualityBucketsUpdatedAtToTheMinute() {
        let base = RemoteBootstrapSnapshot.mock
        let minuteStart: Int64 = 1_789_996_920_000 // divisible by 60_000
        let early = withSessions(base) {
            remaking($0, updatedAtUnixMs: minuteStart + 5_000, lastOutputPreview: $0.lastOutputPreview)
        }
        let late = withSessions(base) {
            remaking($0, updatedAtUnixMs: minuteStart + 45_000, lastOutputPreview: $0.lastOutputPreview)
        }
        let nextMinute = withSessions(base) {
            remaking($0, updatedAtUnixMs: minuteStart + 61_000, lastOutputPreview: $0.lastOutputPreview)
        }

        // mtime churn inside one minute bucket must not publish…
        XCTAssertTrue(RemotePreviewStore.snapshotContentEqual(early, late))
        // …but crossing a minute boundary must.
        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(early, nextMinute))
    }

    func testSnapshotEqualityStillSeesRenderedSessionChanges() {
        let base = RemoteBootstrapSnapshot.mock
        let retitled = withSessions(base) {
            remaking(
                $0,
                title: $0.id == base.sessions[0].id ? "Renamed" : $0.title,
                updatedAtUnixMs: $0.updatedAtUnixMs,
                lastOutputPreview: $0.lastOutputPreview
            )
        }

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, retitled))
    }

    func testSnapshotEqualitySeesActiveRuntimeChanges() {
        let base = RemoteBootstrapSnapshot.mock
        let observed = withSessions(base) {
            remaking(
                $0,
                activeRuntimeID: $0.id == base.sessions[0].id
                    ? "com.anthropic.claude-code"
                    : nil,
                updatedAtUnixMs: $0.updatedAtUnixMs,
                lastOutputPreview: $0.lastOutputPreview
            )
        }

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, observed))
        XCTAssertEqual(observed.sessions[0].presentationProviderID, "claude")
    }

    func testSnapshotEqualitySeesTerminalBackgroundHexChanges() {
        let base = RemoteBootstrapSnapshot.mock
        let themed = withSessions(base) {
            remaking(
                $0,
                updatedAtUnixMs: $0.updatedAtUnixMs,
                lastOutputPreview: $0.lastOutputPreview,
                terminalBackgroundHex: 0x141414
            )
        }

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, themed))
    }

    func testSnapshotEqualitySeesAppAlertChanges() {
        let base = RemoteBootstrapSnapshot.mock
        let alerted = withSessions(base) {
            remaking(
                $0,
                updatedAtUnixMs: $0.updatedAtUnixMs,
                lastOutputPreview: $0.lastOutputPreview,
                latestAlertBody: $0.id == base.sessions[0].id
                    ? "Close to the weekly limit" : nil,
                latestAlertAtUnixMs: $0.id == base.sessions[0].id
                    ? 1_789_996_960_000 : nil
            )
        }

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, alerted))
    }

    func testSnapshotEqualitySeesHostCapabilityChanges() {
        let legacy = sessionCreationSnapshot(hostProtocol: nil)
        let advertised = sessionCreationSnapshot(
            hostProtocol: .init(capabilities: ["session.create"])
        )

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(legacy, advertised))
    }

    func testSnapshotEqualitySeesPaneGroupChanges() throws {
        let base = RemoteBootstrapSnapshot.mock
        let memberIDs = Array(base.sessions.prefix(2).map(\.id))
        XCTAssertEqual(memberIDs.count, 2)
        var object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: JSONEncoder().encode(base))
                as? [String: Any]
        )
        object["paneGroups"] = [[
            "id": "pane-group-1",
            "representativeSessionID": memberIDs[0],
            "sessionIDs": memberIDs,
        ]]
        let grouped = try JSONDecoder().decode(
            RemoteBootstrapSnapshot.self,
            from: JSONSerialization.data(withJSONObject: object)
        )

        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, grouped))
    }

    // MARK: - Host workspace switcher (phase 6 slice c)

    func testWorkspaceExposureReflectsBootstrapAndOnlyMultipleTriggersPicker() {
        let single = RemotePreviewStore(snapshot: workspaceSnapshot(workspaces: [
            .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true),
        ]))
        XCTAssertFalse(single.hasMultipleWorkspaces)
        XCTAssertEqual(single.currentWorkspace?.id, "ws-a")

        let legacy = RemotePreviewStore(snapshot: workspaceSnapshot(workspaces: nil))
        XCTAssertTrue(legacy.hostWorkspaces.isEmpty)
        XCTAssertFalse(legacy.hasMultipleWorkspaces)
        XCTAssertNil(legacy.currentWorkspace)

        let multi = RemotePreviewStore(snapshot: workspaceSnapshot(
            workspaces: [
                .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true),
                .init(id: "ws-b", name: "Client", tintHue: 30, isCurrent: false, isRunning: false),
            ],
            tintHue: 210
        ))
        XCTAssertTrue(multi.hasMultipleWorkspaces)
        XCTAssertEqual(multi.currentWorkspace?.id, "ws-a")
        // Host tint travels with the served workspace: A is current now.
        XCTAssertEqual(multi.hostTintHue, 210)
    }

    func testSwitchWorkspaceSelectsThenReBootstrapsToTheNewWorkspace() async {
        let a = RemoteWorkspaceSummary(
            id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true
        )
        let b = RemoteWorkspaceSummary(
            id: "ws-b", name: "Client", tintHue: 30, isCurrent: false, isRunning: true
        )
        // After the switch the Mac serves B: its tint is current, its sessions,
        // and its hostWorkspaces flips isCurrent to B.
        let bServed = workspaceSnapshot(
            workspaces: [
                .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: false, isRunning: true),
                .init(id: "ws-b", name: "Client", tintHue: 30, isCurrent: true, isRunning: true),
            ],
            tintHue: 30,
            sessions: [activitySession(id: "b-session", activity: .idle, updatedAtUnixMs: 1)],
            capturedAtUnixMs: 2
        )
        var selectedIDs: [String] = []
        let store = RemotePreviewStore(
            snapshot: workspaceSnapshot(
                workspaces: [a, b],
                tintHue: 210,
                sessions: [activitySession(id: "a-session", activity: .idle, updatedAtUnixMs: 1)]
            ),
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { bServed },
            restartSessionOverride: nil,
            selectWorkspaceOverride: { id in
                selectedIDs.append(id)
                return RemoteWorkspaceSelectResponse(workspace: b)
            }
        )
        store.selectedSessionID = "a-session"

        await store.switchWorkspace(to: b)

        XCTAssertEqual(selectedIDs, ["ws-b"], "the target workspace id must be POSTed once")
        XCTAssertEqual(store.currentWorkspace?.id, "ws-b")
        XCTAssertEqual(store.hostTintHue, 30, "chrome recolors to B's tint after the switch")
        XCTAssertEqual(
            store.selectedSessionID, "b-session",
            "the terminal rebases onto the new workspace's default session"
        )
        XCTAssertNil(store.switchingWorkspaceID)
    }

    func testWorkspaceKindDecodesAndIsSurfacedForRemoteEntries() throws {
        // A phone paired to a Mac sees local workspaces AND the Mac's remote
        // Hosts (ssh/paired). The additive `kind` lets it badge them apart.
        let local = RemoteWorkspaceSummary(
            id: "ws-a", name: "Personal", tintHue: 210, isCurrent: true, isRunning: true, kind: "local"
        )
        let ssh = RemoteWorkspaceSummary(
            id: "ssh:x", name: "Server", tintHue: 120, isCurrent: false, isRunning: false, kind: "ssh"
        )
        let paired = RemoteWorkspaceSummary(
            id: "host:y", name: "Studio", tintHue: 30, isCurrent: false, isRunning: false, kind: "paired"
        )
        let store = RemotePreviewStore(
            snapshot: workspaceSnapshot(workspaces: [local, ssh, paired], tintHue: 210),
            client: RemoteMacClient()
        )
        let kinds = store.hostWorkspaces.map { $0.kind }
        XCTAssertEqual(kinds, ["local", "ssh", "paired"])

        // A legacy Host omits `kind`; it must still decode (nil = local).
        let legacyJSON = #"{"id":"ws-z","name":"Legacy","isCurrent":false,"isRunning":true}"#
        let legacy = try JSONDecoder().decode(
            RemoteWorkspaceSummary.self, from: Data(legacyJSON.utf8)
        )
        XCTAssertNil(legacy.kind)
    }

    func testSwitchWorkspaceIgnoresCurrentAndSurfacesServerError() async {
        let a = RemoteWorkspaceSummary(
            id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true
        )
        let b = RemoteWorkspaceSummary(
            id: "ws-b", name: "Client", tintHue: 30, isCurrent: false, isRunning: true
        )
        var selectCalls = 0
        var bootstrapCalls = 0
        let store = RemotePreviewStore(
            snapshot: workspaceSnapshot(workspaces: [a, b], tintHue: 210),
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                bootstrapCalls += 1
                return self.workspaceSnapshot(workspaces: [a, b], tintHue: 210)
            },
            restartSessionOverride: nil,
            selectWorkspaceOverride: { _ in
                selectCalls += 1
                throw RemoteMacClientError(statusCode: 404, serverMessage: "unknown workspace")
            }
        )

        // Selecting the already-current workspace is a no-op (no POST).
        await store.switchWorkspace(to: a)
        XCTAssertEqual(selectCalls, 0)

        await store.switchWorkspace(to: b)
        XCTAssertEqual(selectCalls, 1)
        XCTAssertEqual(bootstrapCalls, 0, "a failed select must not re-bootstrap")
        XCTAssertEqual(store.lastError, "HTTP 404: unknown workspace")
        XCTAssertNil(store.switchingWorkspaceID)
    }

    // MARK: - Last-session restore

    func testColdLaunchRestoresLastSessionAndRevealsItsProject() async {
        let defaults = freshDefaults()
        let other = activitySession(id: "other", activity: .idle, updatedAtUnixMs: 1)
        let saved = activitySession(id: "saved", activity: .idle, updatedAtUnixMs: 2)
        defaults.set(
            "saved",
            forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        )
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { self.restoreSnapshot(sessions: [other, saved]) },
            restartSessionOverride: nil,
            defaults: defaults
        )
        XCTAssertNil(store.selectedSessionID)

        _ = await store.loadFromBridge()

        XCTAssertEqual(
            store.selectedSessionID, "saved",
            "cold launch must reopen the last-open session, not the default"
        )
        XCTAssertTrue(
            store.expandedProjectIDs.contains("project-unpeel"),
            "the restored session's project must be expanded"
        )
        XCTAssertEqual(
            store.expandedFolderID, "folder-a",
            "the sidebar must open the folder you were in"
        )
    }

    func testColdLaunchFallsBackToDefaultWhenSavedSessionIsGone() async {
        let defaults = freshDefaults()
        defaults.set(
            "long-gone",
            forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        )
        let idle = activitySession(id: "idle", activity: .idle, updatedAtUnixMs: 1)
        let blocked = activitySession(id: "attention", activity: .blocked, updatedAtUnixMs: 2)
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { self.restoreSnapshot(sessions: [idle, blocked]) },
            restartSessionOverride: nil,
            defaults: defaults
        )

        _ = await store.loadFromBridge()

        XCTAssertEqual(
            store.selectedSessionID, "attention",
            "a vanished saved session falls back to the default (attention-first) selection"
        )
    }

    func testLastSessionMemoryIsScopedPerMac() async {
        let defaults = freshDefaults()
        let keyA = RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        let keyB = RemotePreviewStore.lastSessionScopeKey(macID: "mac-b", workspaceID: nil)
        defaults.set("b-2", forKey: keyB)

        // Mac A: default-select, then explicitly open a-2. Only A's scope
        // may be written — the old single key let this clobber B's memory.
        let a1 = activitySession(id: "a-1", activity: .idle, updatedAtUnixMs: 1)
        let a2 = activitySession(id: "a-2", activity: .idle, updatedAtUnixMs: 2)
        let storeA = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { self.restoreSnapshot(macID: "mac-a", sessions: [a1, a2]) },
            restartSessionOverride: nil,
            defaults: defaults
        )
        _ = await storeA.loadFromBridge()
        XCTAssertEqual(storeA.selectedSessionID, "a-1")
        storeA.select(a2)
        XCTAssertEqual(defaults.string(forKey: keyA), "a-2")
        XCTAssertEqual(
            defaults.string(forKey: keyB), "b-2",
            "selecting on Mac A must never clobber Mac B's memory"
        )

        // Mac B (fresh launch): restores B's own last session, not A's and
        // not B's default.
        let b1 = activitySession(id: "b-1", activity: .idle, updatedAtUnixMs: 1)
        let b2 = activitySession(id: "b-2", activity: .idle, updatedAtUnixMs: 2)
        let storeB = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { self.restoreSnapshot(macID: "mac-b", sessions: [b1, b2]) },
            restartSessionOverride: nil,
            defaults: defaults
        )
        _ = await storeB.loadFromBridge()
        XCTAssertEqual(storeB.selectedSessionID, "b-2")
    }

    func testDeepLinkWinsOverColdLaunchRestore() async {
        let defaults = freshDefaults()
        defaults.set(
            "saved",
            forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        )
        let saved = activitySession(id: "saved", activity: .idle, updatedAtUnixMs: 1)
        let pushed = activitySession(id: "pushed", activity: .idle, updatedAtUnixMs: 2)
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { self.restoreSnapshot(sessions: [saved, pushed]) },
            restartSessionOverride: nil,
            defaults: defaults
        )
        // Push-notification tap before the first bootstrap arrived.
        store.selectSessionByID("pushed")

        _ = await store.loadFromBridge()

        XCTAssertEqual(
            store.selectedSessionID, "pushed",
            "a tapped deep link must not be overridden by the launch restore"
        )
    }

    func testWorkspaceSwitchRestoresThatWorkspacesLastSession() async {
        let defaults = freshDefaults()
        let a = RemoteWorkspaceSummary(
            id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true
        )
        let b = RemoteWorkspaceSummary(
            id: "ws-b", name: "Client", tintHue: 30, isCurrent: false, isRunning: true
        )
        defaults.set(
            "b-2",
            forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac", workspaceID: "ws-b")
        )
        let bServed = workspaceSnapshot(
            workspaces: [
                .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: false, isRunning: true),
                .init(id: "ws-b", name: "Client", tintHue: 30, isCurrent: true, isRunning: true),
            ],
            tintHue: 30,
            sessions: [
                activitySession(id: "b-1", activity: .idle, updatedAtUnixMs: 1),
                activitySession(id: "b-2", activity: .idle, updatedAtUnixMs: 2),
            ],
            capturedAtUnixMs: 2
        )
        let store = RemotePreviewStore(
            snapshot: workspaceSnapshot(
                workspaces: [a, b],
                tintHue: 210,
                sessions: [activitySession(id: "a-session", activity: .idle, updatedAtUnixMs: 1)]
            ),
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: { bServed },
            restartSessionOverride: nil,
            selectWorkspaceOverride: { _ in RemoteWorkspaceSelectResponse(workspace: b) },
            defaults: defaults
        )
        store.selectedSessionID = "a-session"

        await store.switchWorkspace(to: b)

        XCTAssertEqual(
            store.selectedSessionID, "b-2",
            "switching workspaces restores that workspace's last session, not its default"
        )
        XCTAssertEqual(
            defaults.string(
                forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac", workspaceID: "ws-a")
            ),
            "a-session",
            "workspace A's memory survives the switch"
        )
    }

    func testTransientNilSelectionNeverClobbersPersistedValue() {
        let defaults = freshDefaults()
        let first = activitySession(id: "s-1", activity: .idle, updatedAtUnixMs: 1)
        let second = activitySession(id: "s-2", activity: .idle, updatedAtUnixMs: 2)
        let store = RemotePreviewStore(
            snapshot: restoreSnapshot(sessions: [first, second]),
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: nil,
            restartSessionOverride: nil,
            defaults: defaults
        )
        store.select(second)
        let key = RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        XCTAssertEqual(defaults.string(forKey: key), "s-2")

        store.selectedSessionID = nil

        XCTAssertEqual(
            defaults.string(forKey: key), "s-2",
            "teardown/transient nil must never erase the remembered session"
        )
    }

    func testRestoreStaysArmedThroughAnEmptyFirstBootstrap() async {
        let defaults = freshDefaults()
        defaults.set(
            "saved",
            forKey: RemotePreviewStore.lastSessionScopeKey(macID: "mac-a", workspaceID: nil)
        )
        let other = activitySession(id: "other", activity: .idle, updatedAtUnixMs: 1)
        let saved = activitySession(id: "saved", activity: .idle, updatedAtUnixMs: 2)
        var bootstrapCount = 0
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: RemoteMacClient(),
            createSessionOverride: nil,
            bootstrapOverride: {
                defer { bootstrapCount += 1 }
                // The Host answers before its rescan finished: an empty
                // session list must not consume the launch restore.
                return bootstrapCount == 0
                    ? self.restoreSnapshot(sessions: [])
                    : self.restoreSnapshot(sessions: [other, saved], capturedAtUnixMs: 3)
            },
            restartSessionOverride: nil,
            defaults: defaults
        )

        _ = await store.loadFromBridge()
        XCTAssertNil(store.selectedSessionID)

        _ = await store.loadFromBridge()
        XCTAssertEqual(
            store.selectedSessionID, "saved",
            "the restore attempt survives a still-rescanning Host's empty first answer"
        )
    }

    func testSnapshotEqualitySeesHostWorkspaceChanges() {
        let base = workspaceSnapshot(workspaces: [
            .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true),
        ])
        let added = workspaceSnapshot(workspaces: [
            .init(id: "ws-a", name: "Default", tintHue: 210, isCurrent: true, isRunning: true),
            .init(id: "ws-b", name: "Client", tintHue: 30, isCurrent: false, isRunning: false),
        ])
        XCTAssertFalse(RemotePreviewStore.snapshotContentEqual(base, added))
    }

    // MARK: - Bundled icon SVGs

    func testBundledIconSVGsParseForCoreGraphicsRendering() {
        for icon in UnpeelChromeIcon.allCases {
            XCTAssertNotNil(
                ParsedSVGIcon.parse(icon.svgSource),
                "chrome icon \(icon) no longer parses; it would fall back to an SF Symbol"
            )
        }
        for icon in UnpeelToolIcon.allCases {
            XCTAssertNotNil(
                ParsedSVGIcon.parse(icon.svgSource),
                "tool icon \(icon) no longer parses; it would fall back to an SF Symbol"
            )
        }
    }

    // MARK: - Helpers

    private func sessionCreationSnapshot(
        hostProtocol: RemoteHostProtocolDescriptor?,
        sessions: [RemoteSessionSummary] = [],
        capturedAtUnixMs: Int64 = 1,
        macID: String = "mac"
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            hostProtocol: hostProtocol,
            macID: macID,
            macName: "Mac",
            folders: [],
            projects: [
                .init(
                    id: "project-unpeel",
                    name: "Unpeel",
                    path: "/dev/unpeel",
                    sortOrder: 0
                ),
            ],
            presets: [
                .init(
                    id: "preset-codex",
                    label: "Codex",
                    command: "codex",
                    cliID: "codex"
                ),
            ],
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs
        )
    }

    /// An isolated UserDefaults suite so last-session persistence tests never
    /// see (or leave) state in the simulator's standard defaults.
    private func freshDefaults(_ name: String = #function) -> UserDefaults {
        let suiteName = "unpeel.tests.\(name).\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)
        return defaults
    }

    /// A foldered single-project snapshot for last-session restore tests.
    private func restoreSnapshot(
        macID: String = "mac-a",
        sessions: [RemoteSessionSummary],
        capturedAtUnixMs: Int64 = 2,
        pendingApprovals: [RemotePendingApproval]? = nil
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            macID: macID,
            macName: "Mac",
            folders: [
                .init(id: "folder-a", name: "Work", colorID: "blue", sortOrder: 0),
            ],
            projects: [
                .init(
                    id: "project-unpeel",
                    name: "Unpeel",
                    path: "/dev/unpeel",
                    folderID: "folder-a",
                    mcpBlocked: false,
                    sortOrder: 0
                ),
            ],
            presets: [],
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs,
            pendingApprovals: pendingApprovals
        )
    }

    private func workspaceSnapshot(
        workspaces: [RemoteWorkspaceSummary]?,
        tintHue: Double? = nil,
        sessions: [RemoteSessionSummary] = [],
        capturedAtUnixMs: Int64 = 1
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            macID: "mac",
            macName: "Mac",
            folders: [],
            projects: [
                .init(id: "project-unpeel", name: "Unpeel", path: "/dev/unpeel", sortOrder: 0),
            ],
            presets: [],
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs,
            hostTintHue: tintHue,
            hostWorkspaces: workspaces
        )
    }

    private func activitySession(
        id: String,
        activity: RemoteActivityState,
        updatedAtUnixMs: Int64,
        unread: Bool = false,
        alert: String? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: "project-unpeel",
            providerID: "codex",
            title: id,
            command: "codex",
            createdAtUnixMs: 1,
            updatedAtUnixMs: updatedAtUnixMs,
            status: .running,
            activity: activity,
            unread: unread,
            latestAlertBody: alert,
            latestAlertAtUnixMs: alert == nil ? nil : updatedAtUnixMs
        )
    }

    private func replacementSession(
        id: String,
        projectID: String = "project-unpeel",
        command: String = "claude",
        createdAtUnixMs: Int64 = 42,
        status: RemoteSessionStatus = .running,
        providerID: String? = "claude",
        worktreePath: String? = "/worktree",
        worktreeBranch: String? = "topic",
        archived: Bool = false
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: projectID,
            providerID: providerID,
            title: id,
            command: command,
            createdAtUnixMs: createdAtUnixMs,
            status: status,
            activity: .idle,
            worktreePath: worktreePath,
            worktreeBranch: worktreeBranch,
            capabilities: RemoteSessionCapabilities(
                restart: status == .exited,
                notifyWhenDone: false
            ),
            archived: archived
        )
    }

    private func withSessions(
        _ snapshot: RemoteBootstrapSnapshot,
        _ transform: (RemoteSessionSummary) -> RemoteSessionSummary
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            protocolVersion: snapshot.protocolVersion,
            macID: snapshot.macID,
            macName: snapshot.macName,
            folders: snapshot.folders,
            projects: snapshot.projects,
            presets: snapshot.presets,
            sessions: snapshot.sessions.map(transform),
            capturedAtUnixMs: snapshot.capturedAtUnixMs + 1
        )
    }

    private func remaking(
        _ session: RemoteSessionSummary,
        title: String? = nil,
        activeRuntimeID: String? = nil,
        updatedAtUnixMs: Int64?,
        lastOutputPreview: String?,
        terminalBackgroundHex: Int? = nil,
        latestAlertBody: String? = nil,
        latestAlertAtUnixMs: Int64? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: session.id,
            projectID: session.projectID,
            activeRuntimeID: activeRuntimeID ?? session.activeRuntimeID,
            providerID: session.providerID,
            title: title ?? session.title,
            command: session.command,
            createdAtUnixMs: session.createdAtUnixMs,
            updatedAtUnixMs: updatedAtUnixMs,
            status: session.status,
            activity: session.activity,
            unread: session.unread,
            pinned: session.pinned,
            worktreePath: session.worktreePath,
            worktreeBranch: session.worktreeBranch,
            lastOutputPreview: lastOutputPreview,
            notifyWhenDone: session.notifyWhenDone,
            terminalBackgroundHex: terminalBackgroundHex ?? session.terminalBackgroundHex,
            capabilities: session.capabilities,
            latestAlertBody: latestAlertBody ?? session.latestAlertBody,
            latestAlertAtUnixMs: latestAlertAtUnixMs ?? session.latestAlertAtUnixMs
        )
    }
}
