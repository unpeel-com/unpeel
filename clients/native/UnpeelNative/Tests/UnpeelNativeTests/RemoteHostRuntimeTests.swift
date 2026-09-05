import Combine
import Foundation
import XCTest
import UnpeelShared
@testable import UnpeelNative

@MainActor
final class RemoteHostRuntimeTests: XCTestCase {
    func testDisconnectWakesCancelledRefreshSleepAndReleasesRuntime() async {
        let backend = ControlledRemoteBackend()
        var runtime: RemoteHostRuntime? = makeRuntime(backend: backend)
        weak let weakRuntime = runtime

        runtime?.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil {
            runtime?.connectionState == .connected(name: "Host")
        }
        // The refresh loop parks immediately after publishing the bootstrap.
        // Let that suspension install its checked continuation before cancel.
        await Task.yield()

        runtime?.disconnect()
        runtime = nil
        await waitUntil { weakRuntime == nil }
        XCTAssertNil(weakRuntime)
    }

    func testDefaultSelectionPrefersBlockedThenRunning() {
        let snapshot = makeSnapshot(sessions: [
            makeSession(id: "idle", status: .exited, activity: .idle),
            makeSession(id: "running", status: .running, activity: .working),
            makeSession(id: "blocked", status: .running, activity: .blocked),
        ])

        XCTAssertEqual(RemoteHostRuntime.defaultSessionID(in: snapshot), "blocked")
    }

    func testContentEqualityIgnoresClockPreviewAndSubminuteUpdateChurn() {
        let first = makeSnapshot(
            capturedAt: 1,
            sessions: [makeSession(
                id: "session",
                updatedAt: 60_001,
                preview: "first"
            )]
        )
        let second = makeSnapshot(
            capturedAt: 999,
            sessions: [makeSession(
                id: "session",
                updatedAt: 119_999,
                preview: "second"
            )]
        )

        XCTAssertTrue(RemoteHostRuntime.snapshotContentEqual(first, second))
    }

    func testContentEqualityNoticesActiveRuntimeChanges() {
        let shell = makeSnapshot(sessions: [makeSession(id: "session")])
        let claude = makeSnapshot(sessions: [makeSession(
            id: "session",
            activeRuntimeID: "claude"
        )])

        XCTAssertFalse(RemoteHostRuntime.snapshotContentEqual(shell, claude))
    }

    func testContentEqualityNoticesAppAlertChanges() {
        let base = makeSnapshot(sessions: [makeSession(
            id: "session", updatedAt: 60_001
        )])
        let alerted = makeSnapshot(sessions: [makeSession(
            id: "session",
            updatedAt: 60_002,
            latestAlertBody: "Close to the weekly limit",
            latestAlertAt: 60_002
        )])

        XCTAssertFalse(RemoteHostRuntime.snapshotContentEqual(base, alerted))
    }

    func testContentEqualityNoticesWorkspaceSettingsChanges() {
        let first = makeSnapshot(workspaceSettings: makeWorkspaceSettings(menuAttention: true))
        let second = makeSnapshot(workspaceSettings: makeWorkspaceSettings(menuAttention: false))

        XCTAssertFalse(RemoteHostRuntime.snapshotContentEqual(first, second))
    }

    func testResumeAgentUsesDedicatedCapabilityAndBackendEffect() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: [
                "host.bootstrap",
                "session.input.write",
                "session.output.read",
                RemoteControlProtocol.sessionRuntimeResumeCapability,
            ],
            sessions: [makeSession(id: "session")]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        XCTAssertTrue(runtime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.resumeAgent
        ))
        try await runtime.resumeAgent("session")
        let calls = await backend.organizationCalls
        XCTAssertEqual(calls, ["resume-agent:session"])
        runtime.disconnect()
    }

    func testGroupPinUsesDedicatedCapabilityAndOrganizationEffect() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: [
                "host.bootstrap",
                "session.input.write",
                "session.output.read",
                RemoteHostRuntime.HostOperation.projectOrganizationSet,
                RemoteHostRuntime.HostOperation.projectPinSet,
            ]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        XCTAssertTrue(runtime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.projectPinSet
        ))
        try await runtime.setProjectPinned(projectID: "group", pinned: true)
        let calls = await backend.organizationCalls
        XCTAssertEqual(calls, ["project-organization:group:-:true"])
        runtime.disconnect()
    }

    func testPairingInvitationUsesTheSelectedHostsCapabilityAndConnection() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: [
                "host.bootstrap",
                "session.input.write",
                "session.output.read",
                RemoteHostRuntime.HostOperation.pairingInvitation,
            ]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        let proxy = URL(
            string: "http://192.168.1.20:49152/mobile/pairing-proxy/INVITE-1"
        )!
        let payload = try await runtime.createPairingInvitation(proxyEndpoint: proxy)
        XCTAssertEqual(payload.macID, "host")
        XCTAssertEqual(payload.endpoint, proxy)

        let envelope = RemotePairingEnvelope(
            salt: Data(repeating: 1, count: 16),
            sealed: Data(repeating: 2, count: 28)
        )
        let response = try await runtime.completePairingInvitation(
            envelopeJSON: JSONEncoder().encode(envelope)
        )
        XCTAssertEqual(try JSONDecoder().decode(RemotePairingEnvelope.self, from: response), envelope)
        let calls = await backend.organizationCalls
        XCTAssertEqual(
            calls,
            ["pairing-invitation:create", "pairing-invitation:complete"]
        )
        runtime.disconnect()
    }

    func testRestoreAndRestartSelectsOnlyTheExactPublishedReplacement() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        let current = makeSession(
            id: "current", activity: .blocked, providerID: "claude"
        )
        let preexistingCollision = makeSession(
            id: "preexisting-collision", createdAt: 42, providerID: "claude"
        )
        let source = makeSession(
            id: "archived-source", createdAt: 42, status: .exited,
            providerID: "claude", archived: true
        )
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.output.read",
            RemoteHostRuntime.HostOperation.restore,
            RemoteHostRuntime.HostOperation.restart,
        ]

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: [current, preexistingCollision]
        )))
        await waitUntil { runtime.selectedSessionID == current.id }

        try await runtime.restoreAndRestartSession(
            source,
            knownSessionIDs: [current.id, preexistingCollision.id, source.id]
        )
        let organizationCalls = await backend.organizationCalls
        XCTAssertEqual(
            organizationCalls,
            ["restore:\(source.id)", "restart:\(source.id)"]
        )

        // Restore can publish the old id before Restart publishes its new
        // one. Keep the user's prior selection during that intermediate row.
        await waitUntil { await backend.bootstrapCount >= 2 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: [current, preexistingCollision, makeSession(
                id: source.id, createdAt: 42, status: .exited,
                providerID: "claude"
            )]
        )))
        XCTAssertEqual(runtime.selectedSessionID, current.id)

        let replacement = makeSession(
            id: "exact-replacement", command: "claude --resume thread-1",
            createdAt: 42, providerID: "claude"
        )
        await waitUntil { await backend.bootstrapCount >= 3 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: [current, preexistingCollision, replacement]
        )))
        await waitUntil { runtime.selectedSessionID == replacement.id }
        runtime.disconnect()
    }

    func testArchivedFetchSurvivesLiveBootstrapAndStillSubmitsRestoreAndRestart() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        let current = makeSession(
            id: "current", activity: .blocked, providerID: "codex"
        )
        let source = makeSession(
            id: "archived-source", createdAt: 42, status: .exited,
            providerID: "claude", archived: true
        )
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.output.read",
            RemoteHostRuntime.HostOperation.restore,
            RemoteHostRuntime.HostOperation.restart,
        ]

        // Archive fetches are page-scoped and intentionally separate from
        // the live bootstrap dictionary. A subsequent normal bootstrap only
        // prunes vanished projects; it must not erase the archived source.
        var archivedCache = RemoteArchivedSessionSummaryCache()
        archivedCache.replaceProject("project", summaries: [source])
        let liveSummaries = [current.id: current]
        archivedCache.retainProjects(["project"])
        XCTAssertNil(liveSummaries[source.id])
        let cachedSource = try XCTUnwrap(archivedCache[source.id])
        let knownSessionIDs = Set(liveSummaries.keys)
            .union(archivedCache.sessionIDs)

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: Array(liveSummaries.values)
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        try await runtime.restoreAndRestartSession(
            cachedSource,
            knownSessionIDs: knownSessionIDs
        )
        let calls = await backend.organizationCalls
        XCTAssertEqual(calls, [
            "restore:\(source.id)",
            "restart:\(source.id)",
        ])
        runtime.disconnect()
    }

    func testOrdinaryRestartSelectsOnlyItsExactReplacement() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        let source = makeSession(
            id: "stopped-source", createdAt: 42, status: .exited,
            providerID: "claude", worktreePath: "/worktree",
            worktreeBranch: "topic"
        )
        let baselineCollision = makeSession(
            id: "baseline", createdAt: 42, providerID: "claude",
            worktreePath: "/worktree", worktreeBranch: "topic"
        )
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.output.read",
            RemoteHostRuntime.HostOperation.restart,
        ]
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: [source, baselineCollision]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.selectSession(source.id)
        XCTAssertEqual(runtime.selectedSessionID, source.id)

        try await runtime.restartSession(source.id)
        let calls = await backend.organizationCalls
        XCTAssertEqual(calls, ["restart:\(source.id)"])

        let wrongRuntime = makeSession(
            id: "decoy", command: "codex resume thread", createdAt: 42,
            providerID: "codex", worktreePath: "/worktree",
            worktreeBranch: "topic"
        )
        let replacement = makeSession(
            id: "exact", command: "claude --resume thread", createdAt: 42,
            providerID: "claude", worktreePath: "/worktree",
            worktreeBranch: "topic"
        )
        await waitUntil { await backend.bootstrapCount >= 2 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities,
            sessions: [baselineCollision, wrongRuntime, replacement]
        )))
        await waitUntil { runtime.selectedSessionID == replacement.id }
        runtime.disconnect()
    }

    func testReplacementCorrelationFailsClosedForCommandCollisionAmbiguityAndAge() {
        let source = makeSession(
            id: "source", projectID: "project", command: "claude",
            createdAt: 42, status: .exited, providerID: "claude",
            worktreePath: "/worktree", worktreeBranch: "topic", archived: true
        )
        let intent = RemoteHostRuntime.ReplacementSelectionIntent(
            source: source,
            knownSessionIDs: ["source", "baseline"]
        )
        let baseline = makeSession(
            id: "baseline", createdAt: 42, providerID: "claude",
            worktreePath: "/worktree", worktreeBranch: "topic"
        )
        let exact = makeSession(
            id: "replacement", command: "claude --resume thread",
            createdAt: 42, providerID: "claude",
            worktreePath: "/worktree", worktreeBranch: "topic"
        )

        XCTAssertEqual(
            RemoteHostRuntime.replacementSelectionResolution(
                intent, sessions: [baseline, exact]
            ),
            .select(exact.id),
            "a pre-effect same-timestamp row is excluded from correlation"
        )

        let wrongCommand = makeSession(
            id: "wrong-command", command: "codex resume thread",
            createdAt: 42, providerID: "codex",
            worktreePath: "/worktree", worktreeBranch: "topic"
        )
        guard case .wait = RemoteHostRuntime.replacementSelectionResolution(
            intent, sessions: [
                wrongCommand,
                makeSession(
                    id: "wrong-project", projectID: "other", createdAt: 42,
                    providerID: "claude", worktreePath: "/worktree",
                    worktreeBranch: "topic"
                ),
                makeSession(
                    id: "wrong-worktree", createdAt: 42, providerID: "claude",
                    worktreePath: "/other", worktreeBranch: "topic"
                ),
            ]
        ) else {
            return XCTFail("runtime/project/worktree decoys must not be selected")
        }

        let secondExact = makeSession(
            id: "replacement-2", command: "claude --resume other",
            createdAt: 42, providerID: "claude",
            worktreePath: "/worktree", worktreeBranch: "topic"
        )
        XCTAssertEqual(
            RemoteHostRuntime.replacementSelectionResolution(
                intent, sessions: [exact, secondExact]
            ),
            .cancel,
            "two new matching rows are ambiguous and must never use list order"
        )

        let expiring = RemoteHostRuntime.ReplacementSelectionIntent(
            source: source,
            knownSessionIDs: ["source"],
            bootstrapObservationsRemaining: 1
        )
        XCTAssertEqual(
            RemoteHostRuntime.replacementSelectionResolution(
                expiring, sessions: []
            ),
            .cancel,
            "a stale intent must not hijack an unrelated future Session"
        )
    }

    func testAmbiguousReplacementNeverFallsBackOrLaterHijacksSelection() async throws {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        let source = makeSession(
            id: "source", createdAt: 42, status: .exited,
            providerID: "claude"
        )
        let current = makeSession(id: "current")
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.output.read",
            RemoteHostRuntime.HostOperation.restart,
        ]
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities, sessions: [source, current]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.selectSession(source.id)
        XCTAssertEqual(runtime.selectedSessionID, source.id)

        try await runtime.restartSession(source.id)
        let first = makeSession(
            id: "candidate-a", command: "claude --resume a",
            createdAt: 42, providerID: "claude"
        )
        let second = makeSession(
            id: "candidate-b", command: "claude --resume b",
            createdAt: 42, providerID: "claude"
        )
        await waitUntil { await backend.bootstrapCount >= 2 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities, sessions: [current, first, second]
        )))
        await waitUntil { runtime.snapshot?.sessions.count == 3 }
        XCTAssertNil(
            runtime.selectedSessionID,
            "ambiguous Resume must not fall through to the unrelated default row"
        )

        // Even if the collision later disappears, the canceled intent cannot
        // revive and pick the remaining candidate on a later health poll.
        await waitUntil { await backend.bootstrapCount >= 3 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: capabilities, sessions: [current, first]
        )))
        await waitUntil { runtime.snapshot?.sessions.count == 2 }
        XCTAssertNil(runtime.selectedSessionID)
        runtime.disconnect()
    }

    func testDirectTransportReachesFactoryWithoutChangingHostContract() {
        let backend = ControlledRemoteBackend()
        let endpoint = URL(string: "http://studio.local:4321/mobile")!
        var captured: RemoteHostTransport?
        let runtime = makeRuntime(backendFactory: { transport in
            captured = transport
            return backend
        })

        runtime.connectDirect(
            endpoint: endpoint,
            authToken: "secret-token",
            expectedHostID: "studio"
        )

        guard case let .direct(actualEndpoint, token, hostID) = captured else {
            return XCTFail("Expected the paired direct transport")
        }
        XCTAssertEqual(actualEndpoint, endpoint)
        XCTAssertEqual(token, "secret-token")
        XCTAssertEqual(hostID, "studio")
        runtime.disconnect()
    }

    func testLocalGatewayTransportReachesFactoryAndBootstrapsLikeAnyHost() async {
        let backend = ControlledRemoteBackend()
        var captured: RemoteHostTransport?
        let runtime = makeRuntime(backendFactory: { transport in
            captured = transport
            return backend
        })

        runtime.connectLocalWorkspace(
            home: "/Users/me/.unpeel/profiles/writing",
            name: "Writing",
            expectedHostID: nil
        )

        guard case let .localGateway(home, name, hostID) = captured else {
            return XCTFail("Expected the loopback workspace-gateway transport")
        }
        XCTAssertEqual(home, "/Users/me/.unpeel/profiles/writing")
        XCTAssertEqual(name, "Writing")
        XCTAssertNil(hostID)
        XCTAssertEqual(runtime.connectionRoute, .localGateway)

        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        XCTAssertTrue(runtime.selectionConnectionIsActive)
        runtime.disconnect()
    }

    func testLocalServiceUsesSemanticHostVerbsButNeverTerminalEffects() async throws {
        let backend = ControlledRemoteBackend()
        var captured: RemoteHostTransport?
        let runtime = makeRuntime(backendFactory: { transport in
            captured = transport
            return backend
        })

        runtime.connectLocalService(
            home: "/Users/me/.unpeel",
            name: "Personal",
            expectedHostID: "local-host"
        )

        guard case let .localService(home, name, hostID) = captured else {
            return XCTFail("Expected the required local Host-service transport")
        }
        XCTAssertEqual(home, "/Users/me/.unpeel")
        XCTAssertEqual(name, "Personal")
        XCTAssertEqual(hostID, "local-host")
        XCTAssertEqual(runtime.connectionRoute, .localGateway)

        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            hostID: "local-host",
            capabilities: [
                "host.bootstrap",
                "session.input.write",
                "session.mark_read",
                "session.output.read",
                RemoteHostRuntime.HostOperation.titleSet,
                RemoteHostRuntime.HostOperation.pinSet,
                RemoteHostRuntime.HostOperation.archive,
                RemoteHostRuntime.HostOperation.restore,
                RemoteHostRuntime.HostOperation.stop,
                RemoteHostRuntime.HostOperation.remove,
                RemoteHostRuntime.HostOperation.restart,
                RemoteHostRuntime.HostOperation.resumeAgent,
                RemoteHostRuntime.HostOperation.orderSet,
                RemoteHostRuntime.HostOperation.projectSet,
                RemoteHostRuntime.HostOperation.projectOrganizationSet,
                RemoteHostRuntime.HostOperation.projectPinSet,
            ],
            sessions: [makeSession(id: "session")]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        XCTAssertNil(runtime.selectedSessionID)
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        runtime.selectDirectDataPlaneSession("session")
        XCTAssertEqual(runtime.selectedSessionID, "session")
        try await runtime.renameSession("session", to: "Renamed")
        try await runtime.setSessionPinned("session", pinned: true)
        try await runtime.archiveSession("session")
        try await runtime.restoreSession("session")
        try await runtime.stopSession("session")
        try await runtime.removeSession("session")
        try await runtime.resumeAgent("session")
        try await runtime.setSessionOrder(
            projectID: "project", orderedSessionIDs: ["second", "session"]
        )
        try await runtime.setSessionProject("session", projectID: "project-2")
        try await runtime.setProjectSortOrder(projectID: "project", sortOrder: 3)
        try await runtime.renameProjectGroup(projectID: "group", displayName: "Ideas")
        try await runtime.setProjectFolderColor(projectID: "project", colorID: "amber")
        try await runtime.setProjectDateSorted(projectID: "group", dateSorted: true)
        try await runtime.setProjectPinned(projectID: "group", pinned: true)
        try await runtime.restartSession("session")
        try? await Task.sleep(nanoseconds: 20_000_000)
        let localEffects = await backend.effectCalls
        XCTAssertEqual(localEffects, [])
        let terminalPolls = await backend.pollCount
        XCTAssertEqual(terminalPolls, 0)
        let organizationCalls = await backend.organizationCalls
        XCTAssertEqual(organizationCalls, [
            "title:session:Renamed",
            "pin:session:true",
            "archive:session",
            "restore:session",
            "stop:session",
            "remove:session",
            "resume-agent:session",
            "order:project:second,session",
            "project:session:project-2",
            "project-organization:project:3:-",
            "project-organization:group:-:-",
            "project-organization:project:-:-",
            "project-organization:group:-:-",
            "project-organization:group:-:true",
            "restart:session",
        ])
        let projectPatches = await backend.projectOrganizationPatches
        XCTAssertEqual(projectPatches, [
            RemoteProjectOrganizationPatch(projectID: "project", sortOrder: 3),
            RemoteProjectOrganizationPatch(projectID: "group", displayName: "Ideas"),
            RemoteProjectOrganizationPatch(projectID: "project", colorID: "amber"),
            RemoteProjectOrganizationPatch(projectID: "group", dateSorted: true),
            RemoteProjectOrganizationPatch(projectID: "group", pinned: true),
        ])
        runtime.disconnect()
    }

    func testLocalServiceSelectsExplicitCreateWithoutClaimingTerminalDataPlane() async throws {
        let optimistic = makeSession(id: "created-session")
        let backend = ControlledRemoteBackend(createdSessionSummary: optimistic)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.mark_read",
            "session.output.read",
            RemoteHostRuntime.HostOperation.create,
        ]
        runtime.connectLocalService(
            home: "/Users/me/.unpeel",
            name: "Personal",
            expectedHostID: "local-host"
        )
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            hostID: "local-host",
            capabilities: capabilities,
            sessions: [makeSession(id: "current")]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.selectDirectDataPlaneSession("current")

        let created = try await runtime.createSession(
            projectID: "project",
            presetID: "preset"
        )
        XCTAssertEqual(created, "created-session")
        XCTAssertEqual(runtime.selectedSessionID, "created-session")
        XCTAssertEqual(
            runtime.directDataPlaneSelectionIntent?.sessionID,
            "created-session"
        )
        XCTAssertTrue(runtime.snapshot?.sessions.contains(where: {
            $0.id == "created-session"
        }) == true)
        XCTAssertEqual(runtime.snapshot?.sessions.first?.id, "created-session")
        await waitUntil { await backend.bootstrapCount >= 2 }
        // The refresh can race the detached Session Host's first manifest.
        // Keep the correlated starting row/selection until Host truth catches up.
        await backend.resolveBootstrap(.success(makeSnapshot(
            hostID: "local-host",
            capabilities: capabilities,
            sessions: [makeSession(id: "current")]
        )))
        await waitUntil { await backend.bootstrapCount >= 3 }
        XCTAssertEqual(runtime.selectedSessionID, "created-session")
        await backend.resolveBootstrap(.success(makeSnapshot(
            hostID: "local-host",
            capabilities: capabilities,
            sessions: [
                makeSession(id: "current"),
                makeSession(id: "created-session"),
            ]
        )))
        await waitUntil { runtime.selectedSessionID == "created-session" }
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        let pollCount = await backend.pollCount
        XCTAssertEqual(pollCount, 0)
        let terminalEffects = await backend.effectCalls
        XCTAssertEqual(terminalEffects, [])
        runtime.disconnect()
    }

    func testBackgroundCreatePublishesOptimisticRowWithoutChangingSelection() async throws {
        let optimistic = makeSession(id: "created-session")
        let backend = ControlledRemoteBackend(createdSessionSummary: optimistic)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        let capabilities = [
            "host.bootstrap",
            "session.input.write",
            "session.mark_read",
            "session.output.read",
            RemoteHostRuntime.HostOperation.create,
        ]
        runtime.connectLocalService(
            home: "/Users/me/.unpeel",
            name: "Personal",
            expectedHostID: "local-host"
        )
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            hostID: "local-host",
            capabilities: capabilities,
            sessions: [makeSession(id: "current")]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.selectDirectDataPlaneSession("current")

        let created = try await runtime.createSession(
            projectID: "project",
            presetID: "preset",
            selectOnCreate: false
        )

        XCTAssertEqual(created, "created-session")
        XCTAssertEqual(runtime.selectedSessionID, "current")
        XCTAssertNil(runtime.directDataPlaneSelectionIntent)
        XCTAssertEqual(runtime.snapshot?.sessions.first?.id, "created-session")
        runtime.disconnect()
    }

    func testStaleControllerPairingRequiresRepairInsteadOfLookingDisconnected() {
        let runtime = makeRuntime(backend: ControlledRemoteBackend())

        runtime.requirePairingRepair()

        guard case let .repairRequired(message) = runtime.connectionState else {
            return XCTFail("Expected an explicit pair-again state")
        }
        XCTAssertTrue(message.contains("different Controller identity"))
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        XCTAssertNil(runtime.connectionRoute)
    }

    func testConnectedOrConnectingHostSelectionIsAlreadyActive() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        XCTAssertFalse(runtime.selectionConnectionIsActive)

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        XCTAssertTrue(runtime.selectionConnectionIsActive)
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        XCTAssertTrue(runtime.selectionConnectionIsActive)

        runtime.requirePairingRepair()
        XCTAssertFalse(runtime.selectionConnectionIsActive)
    }

    func testFactoryFailureWithStaleSnapshotCanBeRetriedFromCheckedHost() async {
        let backend = ControlledRemoteBackend()
        var openCount = 0
        let runtime = makeRuntime { _ in
            defer { openCount += 1 }
            if openCount == 0 { return backend }
            throw NativeRemoteBackendError(
                result: -1,
                code: "remote_direct_open_failed",
                message: "Could not open the paired Host."
            )
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        XCTAssertEqual(
            runtime.connectionState,
            .reconnecting(message: "Could not open the paired Host.")
        )
        XCTAssertFalse(runtime.selectionConnectionIsActive)
    }

    func testFactoryFailureRetryKeepsPriorSameHostEffectBarrier() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let replacement = ControlledRemoteBackend(controlEffects: true)
        var openCount = 0
        let runtime = makeRuntime { _ in
            defer { openCount += 1 }
            switch openCount {
            case 0:
                return first
            case 1:
                throw NativeRemoteBackendError(
                    result: -1,
                    code: "remote_direct_open_failed",
                    message: "rotated credentials were unavailable"
                )
            default:
                return replacement
            }
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await first.effectCalls == [.markRead(sessionID: "session")] }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        XCTAssertFalse(runtime.selectionConnectionIsActive)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureBootstraps = await replacement.bootstrapCount
        XCTAssertEqual(prematureBootstraps, 0)

        await first.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await replacement.bootstrapCount == 1 }
        await replacement.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil {
            await replacement.effectCalls == [.markRead(sessionID: "session")]
        }
        await replacement.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
        await waitUntil { await replacement.closeCount == 1 }
    }

    func testLocalRoundTripKeepsOldFitClearAheadOfSameHostBootstrap() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let replacement = ControlledRemoteBackend()
        var openCount = 0
        let runtime = makeRuntime { _ in
            defer { openCount += 1 }
            return openCount == 0 ? first : replacement
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }
        runtime.fitDesktop(sessionID: "session", columns: 90, rows: 25)
        await waitUntil { await first.effectCalls.count == 1 }
        await first.resolveNextEffect(.success(.init(requestID: 1)))

        runtime.disconnect()
        await waitUntil { await first.effectCalls.count == 2 }
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureBootstraps = await replacement.bootstrapCount
        XCTAssertEqual(prematureBootstraps, 0)

        await first.resolveNextEffect(.success(.init(requestID: 2)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await replacement.bootstrapCount == 1 }
        await replacement.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.disconnect()
        await waitUntil { await replacement.closeCount == 1 }
    }

    func testRetirementBarrierRemainsTransitiveAcrossWaitingReconnectAndDisconnect() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let waitingReplacement = ControlledRemoteBackend()
        let finalReplacement = ControlledRemoteBackend()
        var openCount = 0
        let runtime = makeRuntime { _ in
            defer { openCount += 1 }
            switch openCount {
            case 0: return first
            case 1: return waitingReplacement
            default: return finalReplacement
            }
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await first.effectCalls == [.markRead(sessionID: "session")] }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        runtime.disconnect()
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let waitingCount = await waitingReplacement.bootstrapCount
        let finalCount = await finalReplacement.bootstrapCount
        XCTAssertEqual(waitingCount, 0)
        XCTAssertEqual(finalCount, 0)

        await first.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await waitingReplacement.closeCount == 1 }
        await waitUntil { await finalReplacement.bootstrapCount == 1 }
        await finalReplacement.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.disconnect()
        await waitUntil { await finalReplacement.closeCount == 1 }
    }

    func testRapidHostAToBToAWaitsForOriginalHostATailBeforeBootstrap() async {
        let firstA = ControlledRemoteBackend(controlEffects: true)
        let hostB = ControlledRemoteBackend()
        let finalA = ControlledRemoteBackend(controlEffects: true)
        var aOpenCount = 0
        let runtime = makeRuntime { transport in
            guard case let .ssh(_, expectedHostID, _, _) = transport else { return hostB }
            if expectedHostID == "B" { return hostB }
            defer { aOpenCount += 1 }
            return aOpenCount == 0 ? firstA : finalA
        }

        runtime.connectSSH(target: "ssh://a", expectedHostID: "A")
        await waitUntil { await firstA.bootstrapCount == 1 }
        await firstA.resolveBootstrap(.success(makeSnapshot(
            hostID: "A",
            sessions: [makeSession(id: "session", unread: true)]
        )))
        await waitUntil { await firstA.effectCalls == [.markRead(sessionID: "session")] }

        runtime.connectSSH(target: "ssh://b", expectedHostID: "B")
        runtime.connectSSH(target: "ssh://a", expectedHostID: "A")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureB = await hostB.bootstrapCount
        let prematureFinalA = await finalA.bootstrapCount
        XCTAssertEqual(prematureB, 0)
        XCTAssertEqual(prematureFinalA, 0)

        await firstA.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await firstA.closeCount == 1 }
        await waitUntil { await hostB.closeCount == 1 }
        await waitUntil { await finalA.bootstrapCount == 1 }
        await finalA.resolveBootstrap(.success(makeSnapshot(
            hostID: "A",
            sessions: [makeSession(id: "session", unread: true)]
        )))
        await waitUntil { await finalA.effectCalls == [.markRead(sessionID: "session")] }
        await finalA.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
        await waitUntil { await finalA.closeCount == 1 }
    }

    func testHostBFactoryFailureStillKeepsHostARetirementForImmediateReturn() async {
        let firstA = ControlledRemoteBackend(controlEffects: true)
        let finalA = ControlledRemoteBackend(controlEffects: true)
        var aOpenCount = 0
        let runtime = makeRuntime { transport in
            guard case let .ssh(_, expectedHostID, _, _) = transport else { return firstA }
            if expectedHostID == "B" {
                throw NativeRemoteBackendError(
                    result: -1,
                    code: "remote_open_failed",
                    message: "Host B could not open"
                )
            }
            defer { aOpenCount += 1 }
            return aOpenCount == 0 ? firstA : finalA
        }

        runtime.connectSSH(target: "ssh://a", expectedHostID: "A")
        await waitUntil { await firstA.bootstrapCount == 1 }
        await firstA.resolveBootstrap(.success(makeSnapshot(
            hostID: "A",
            sessions: [makeSession(id: "session", unread: true)]
        )))
        await waitUntil { await firstA.effectCalls == [.markRead(sessionID: "session")] }

        runtime.connectSSH(target: "ssh://b", expectedHostID: "B")
        XCTAssertFalse(runtime.selectionConnectionIsActive)
        runtime.connectSSH(target: "ssh://a", expectedHostID: "A")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureFinalA = await finalA.bootstrapCount
        XCTAssertEqual(prematureFinalA, 0)

        await firstA.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await firstA.closeCount == 1 }
        await waitUntil { await finalA.bootstrapCount == 1 }
        await finalA.resolveBootstrap(.success(makeSnapshot(
            hostID: "A",
            sessions: [makeSession(id: "session", unread: true)]
        )))
        await waitUntil { await finalA.effectCalls == [.markRead(sessionID: "session")] }
        await finalA.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
        await waitUntil { await finalA.closeCount == 1 }
    }

    func testSameHostReconnectWaitsForMarkReadTailBeforeFreshBootstrap() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let second = ControlledRemoteBackend(controlEffects: true)
        var openCount = 0
        let runtime = makeRuntime { _ in
            defer { openCount += 1 }
            return openCount == 0 ? first : second
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await first.effectCalls == [.markRead(sessionID: "session")] }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureSecondBootstraps = await second.bootstrapCount
        XCTAssertEqual(prematureSecondBootstraps, 0)

        await first.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await second.bootstrapCount == 1 }
        await second.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await second.effectCalls == [.markRead(sessionID: "session")] }
        await second.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
        await waitUntil { await second.closeCount == 1 }
    }

    func testPairedHostFallsBackToVerifiedLinkOnlyAfterDirectReachabilityFailure() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapRetryIntervalNanoseconds: 60_000_000_000,
            initialBootstrapFastRetryCount: 3,
            backendFactory: { transport in
                switch transport {
                case .direct: return direct
                case .link: return link
                case .ssh, .localGateway, .localService:
                    XCTFail("Paired Host must not open SSH or a local gateway")
                    return direct
                }
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }

        XCTAssertEqual(runtime.connectionRoute, .direct)
        XCTAssertNil(runtime.snapshot)
        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))

        await waitUntil {
            runtime.connectionRoute == .link
                && runtime.connectionState == .connected(name: "Host")
        }
        XCTAssertEqual(runtime.selectedSessionID, "session")
        XCTAssertEqual(RemoteHostConnectionRoute.direct.shortLabel, "Direct")
        XCTAssertEqual(RemoteHostConnectionRoute.link.shortLabel, "Via Link")
        await waitUntil { await direct.closeCount == 1 }
        runtime.disconnect()
    }

    /// A Host removed from the Unpeel Link enrollment list
    /// (`linkEnabled == false`) must never open the relay downlink: no grace
    /// race, and a Direct reachability failure stays a Direct failure.
    func testDirectOnlyHostNeverOpensLinkOnReachabilityFailure() async {
        let direct = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapRetryIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            initialDirectLinkGraceNanoseconds: 1_000_000,
            backendFactory: { transport in
                guard case .direct = transport else {
                    XCTFail("A Direct-only Host must never open a non-Direct transport")
                    return direct
                }
                return direct
            }
        )

        var record = makePairedRecord()
        record.linkEnabled = false
        runtime.connectPairedHost(
            record: record,
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))

        // Give both the fallback probe and the initial grace clock room to
        // (incorrectly) fire; the factory above fails the test if they do.
        try? await Task.sleep(nanoseconds: 20_000_000)
        XCTAssertEqual(runtime.connectionRoute, .direct)
        if case .connected = runtime.connectionState {
            XCTFail("A failed Direct-only Host must not report connected")
        }
        runtime.disconnect()
    }

    func testLinkGraceStartsBeforeBlackholedDirectDeadlineAndDirectStillWins() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            initialDirectLinkGraceNanoseconds: 1_000_000,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await waitUntil { await link.bootstrapCount == 1 }
        XCTAssertEqual(runtime.connectionState, .connecting)

        await direct.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionRoute == .direct }
        await waitUntil { await link.closeCount == 1 }
        XCTAssertEqual(runtime.connectionState, .connected(name: "Host"))

        await link.resolveBootstrap(.success(makeSnapshot()))
        try? await Task.sleep(nanoseconds: 2_000_000)
        let finalLinkCloseCount = await link.closeCount
        XCTAssertEqual(finalLinkCloseCount, 1)
        runtime.disconnect()
    }

    func testSameHostReconnectStartsNeitherDirectNorGraceLinkBeforeOldTail() async {
        let firstDirect = ControlledRemoteBackend(controlEffects: true)
        let replacementDirect = ControlledRemoteBackend()
        let link = ControlledRemoteBackend(controlEffects: true)
        var directOpenCount = 0
        let runtime = makeRuntime(
            initialDirectLinkGraceNanoseconds: 1_000_000,
            backendFactory: { transport in
                switch transport {
                case .direct:
                    defer { directOpenCount += 1 }
                    return directOpenCount == 0 ? firstDirect : replacementDirect
                case .link:
                    return link
                case .ssh, .localGateway, .localService:
                    return firstDirect
                }
            }
        )

        // Seed the same pinned Host without a paired plan, so the short grace
        // applies only to the replacement under test.
        runtime.connectDirect(
            endpoint: makePairedRecord().endpoint,
            authToken: "host-bearer",
            expectedHostID: "host"
        )
        await waitUntil { await firstDirect.bootstrapCount == 1 }
        await firstDirect.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil {
            await firstDirect.effectCalls == [.markRead(sessionID: "session")]
        }

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        try? await Task.sleep(nanoseconds: 3_000_000)
        let prematureDirect = await replacementDirect.bootstrapCount
        let prematureLink = await link.bootstrapCount
        XCTAssertEqual(prematureDirect, 0)
        XCTAssertEqual(prematureLink, 0)

        await firstDirect.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await firstDirect.closeCount == 1 }
        await waitUntil { await replacementDirect.bootstrapCount == 1 }
        await waitUntil { await link.bootstrapCount == 1 }

        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { runtime.connectionRoute == .link }
        let prematureLinkEffects = await link.effectCalls
        XCTAssertTrue(prematureLinkEffects.isEmpty)

        await waitUntil { await link.bootstrapCount == 2 }
        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await link.effectCalls == [.markRead(sessionID: "session")] }
        await link.resolveNextEffect(.success(.init(requestID: 1)))
        await replacementDirect.resolveBootstrap(.failure(reachabilityFailure()))
        runtime.disconnect()
        await waitUntil { await link.closeCount == 1 }
    }

    func testLinkCandidateWithWrongDurableHostIdentityFailsClosed() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(hostID: "expected"),
            credentials: makeRemoteCredentials(hostID: "expected")
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }
        XCTAssertEqual(runtime.connectionState, .connecting)
        await link.resolveBootstrap(.success(makeSnapshot(hostID: "attacker")))

        await waitUntil { await link.closeCount == 1 }
        XCTAssertEqual(runtime.connectionRoute, .direct)
        XCTAssertNil(runtime.snapshot)
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        let directCloseCount = await direct.closeCount
        XCTAssertEqual(directCloseCount, 0)
        runtime.disconnect()
    }

    func testAuthenticationAndProtocolFailuresNeverTriggerLinkFallback() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_bootstrap_rejected",
            message: "Host rejected authentication"
        )))
        await waitUntil {
            runtime.connectionState == .failed(message: "Host rejected authentication")
        }
        try? await Task.sleep(nanoseconds: 5_000_000)
        let linkBootstrapCount = await link.bootstrapCount
        XCTAssertEqual(linkBootstrapCount, 0)
        XCTAssertEqual(runtime.connectionRoute, .direct)
        runtime.disconnect()
    }

    func testDirectRecoveryCancelsSlowerLinkCandidate() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil {
            let directCount = await direct.bootstrapCount
            let linkCount = await link.bootstrapCount
            return directCount >= 2 && linkCount == 1
        }
        await direct.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        await link.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { await link.closeCount == 1 }
        try? await Task.sleep(nanoseconds: 2_000_000)

        XCTAssertEqual(runtime.connectionRoute, .direct)
        let directCloseCount = await direct.closeCount
        let linkCloseCount = await link.closeCount
        XCTAssertEqual(directCloseCount, 0)
        XCTAssertEqual(linkCloseCount, 1)
        runtime.disconnect()
    }

    func testDirectRecoveryReleasesMarkReadOnlyAfterOldTailAndFreshBootstrap() async {
        let direct = ControlledRemoteBackend(controlEffects: true)
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await direct.effectCalls == [.markRead(sessionID: "session")] }
        await waitUntil { await direct.bootstrapCount == 2 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }

        try? await Task.sleep(nanoseconds: 2_000_000)
        let bootstrapsWhileTailBlocked = await direct.bootstrapCount
        XCTAssertEqual(bootstrapsWhileTailBlocked, 2)

        await direct.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await direct.bootstrapCount == 3 }
        let effectsBeforeFreshState = await direct.effectCalls
        XCTAssertEqual(effectsBeforeFreshState, [.markRead(sessionID: "session")])

        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await link.closeCount == 1 }
        await waitUntil { await direct.effectCalls.count == 2 }
        let recoveredEffects = await direct.effectCalls
        XCTAssertEqual(recoveredEffects, [
            .markRead(sessionID: "session"),
            .markRead(sessionID: "session"),
        ])
        await direct.resolveNextEffect(.success(.init(requestID: 2)))

        // Let the canceled test double return and prove its candidate wrapper
        // does not close twice on a late success.
        await link.resolveBootstrap(.success(makeSnapshot()))
        try? await Task.sleep(nanoseconds: 2_000_000)
        let finalLinkCloseCount = await link.closeCount
        XCTAssertEqual(finalLinkCloseCount, 1)
        runtime.disconnect()
    }

    func testDisconnectClosesSuspendedLinkCandidateBeforeBootstrapReturns() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }

        runtime.disconnect()
        await waitUntil { await link.closeCount == 1 }
        XCTAssertEqual(runtime.connectionState, .idle)

        // Release the test double's suspended call only after proving close
        // does not depend on the FFI/bootstrap returning.
        await link.resolveBootstrap(.failure(reachabilityFailure()))
        try? await Task.sleep(nanoseconds: 2_000_000)
        let finalCloseCount = await link.closeCount
        XCTAssertEqual(finalCloseCount, 1)
    }

    func testHealthyLinkProbesBackAndPromotesVerifiedDirect() async {
        let firstDirect = ControlledRemoteBackend()
        let recoveredDirect = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        var directOpenCount = 0
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapFastRetryCount: 0,
            directProbeSuccessfulLinkRefreshes: 1,
            backendFactory: { transport in
                switch transport {
                case .direct:
                    directOpenCount += 1
                    return directOpenCount == 1 ? firstDirect : recoveredDirect
                case .link: return link
                case .ssh, .localGateway, .localService: return firstDirect
                }
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await firstDirect.bootstrapCount == 1 }
        await firstDirect.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }
        await link.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { await recoveredDirect.bootstrapCount == 1 }
        XCTAssertEqual(runtime.connectionRoute, .link)

        await recoveredDirect.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionRoute == .direct }
        XCTAssertEqual(runtime.connectionState, .connected(name: "Host"))
        await waitUntil { await link.closeCount == 1 }
        runtime.disconnect()
    }

    func testForcedLinkStartsLinkAndSkipsDirectProbe() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        var capturedLink: RemoteHostTransport?
        let credentials = makeRemoteCredentials()
        let runtime = makeRuntime(
            directProbeSuccessfulLinkRefreshes: 1,
            forceLinkForDevelopment: true,
            backendFactory: { transport in
                if case .link = transport {
                    capturedLink = transport
                    return link
                }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: credentials
        )
        await waitUntil { await link.bootstrapCount == 1 }
        guard case let .link(actualCredentials, deviceID, bearer, hostID) = capturedLink else {
            return XCTFail("Expected forced Link transport")
        }
        XCTAssertEqual(actualCredentials, credentials.relayCredentials)
        XCTAssertEqual(deviceID, "controller")
        XCTAssertEqual(bearer, credentials.authToken)
        XCTAssertEqual(hostID, "host")

        await link.resolveBootstrap(.success(makeSnapshot()))
        await waitUntil { runtime.connectionRoute == .link }
        try? await Task.sleep(nanoseconds: 5_000_000)
        let directBootstrapCount = await direct.bootstrapCount
        XCTAssertEqual(directBootstrapCount, 0)
        runtime.disconnect()
    }

    func testForcedLinkReachabilityFailureNeverFallsBackToDirect() async {
        let direct = ControlledRemoteBackend()
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            initialBootstrapFastRetryCount: 0,
            forceLinkForDevelopment: true,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await link.bootstrapCount == 1 }
        await link.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil {
            runtime.connectionState == .failed(message: "Direct Host is unreachable")
        }
        try? await Task.sleep(nanoseconds: 2_000_000)
        let directBootstrapCount = await direct.bootstrapCount
        XCTAssertEqual(directBootstrapCount, 0)
        XCTAssertEqual(runtime.connectionRoute, .link)
        runtime.disconnect()
    }

    func testFallbackNeverClosesOrReplaysPastAnInFlightDirectEffect() async {
        let direct = ControlledRemoteBackend(controlWrites: true)
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil {
            runtime.selectedSessionID == "session" && runtime.terminalEffectsEnabled
        }
        await waitUntil { await direct.bootstrapCount >= 2 }

        runtime.sendTerminalInput(Data("ambiguous".utf8), to: "session")
        await waitUntil { await direct.writePayloads.count == 1 }
        runtime.sendTerminalInput(Data("queued-before-fallback".utf8), to: "session")
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }
        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.connectionRoute == .link }

        let prematureCloseCount = await direct.closeCount
        let prematureLinkEffects = await link.effectCalls
        XCTAssertEqual(prematureCloseCount, 0)
        XCTAssertTrue(prematureLinkEffects.isEmpty)
        XCTAssertEqual(runtime.selectedSessionID, "session")

        await direct.resolveNextWrite(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_connection_disconnected",
            message: "receipt was lost",
            kind: "outcomeUnknown",
            operation: "terminal write"
        )))
        await waitUntil { await direct.closeCount == 1 }
        let finalLinkEffects = await link.effectCalls
        XCTAssertTrue(finalLinkEffects.isEmpty)
        runtime.disconnect()
    }

    func testFallbackTransfersFitCleanupToLinkWithoutReopeningDeadDirect() async {
        let direct = ControlledRemoteBackend(controlEffects: true)
        let link = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }
        runtime.fitDesktop(sessionID: "session", columns: 100, rows: 30)
        await waitUntil { await direct.effectCalls.count == 1 }
        await direct.resolveNextEffect(.success(.init(requestID: 1)))

        await waitUntil { await direct.bootstrapCount >= 2 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }
        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))

        await waitUntil { await link.effectCalls.count == 1 }
        let directCalls = await direct.effectCalls
        let firstLinkCalls = await link.effectCalls
        let directCloseCount = await direct.closeCount
        XCTAssertEqual(directCalls, [
            .fit(sessionID: "session", columns: 100, rows: 30),
        ])
        XCTAssertEqual(firstLinkCalls, [.clearFit(sessionID: "session")])
        XCTAssertEqual(directCloseCount, 1)

        await link.resolveNextEffect(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_operation_rejected",
            message: "clear was not applied",
            kind: "notApplied",
            operation: "desktop clear"
        )))
        await waitUntil { await link.effectCalls.count == 2 }
        let finalLinkCalls = await link.effectCalls
        XCTAssertEqual(finalLinkCalls[1], .fit(
            sessionID: "session",
            columns: 100,
            rows: 30
        ))
        await link.resolveNextEffect(.success(.init(requestID: 2)))
        runtime.disconnect()
        await waitUntil { await link.effectCalls.count == 3 }
        await link.resolveNextEffect(.success(.init(requestID: 3)))
        await waitUntil { await link.closeCount == 1 }
    }

    func testFallbackWaitsForPostTailBootstrapBeforeRepeatingAutomaticMarkRead() async {
        let direct = ControlledRemoteBackend(controlEffects: true)
        let link = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 0,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )

        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await direct.effectCalls == [.markRead(sessionID: "session")] }
        await waitUntil { await direct.bootstrapCount >= 2 }
        await direct.resolveBootstrap(.failure(reachabilityFailure()))
        await waitUntil { await link.bootstrapCount == 1 }
        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { runtime.connectionRoute == .link }

        let prematureEffects = await link.effectCalls
        let prematureRefreshCount = await link.bootstrapCount
        XCTAssertTrue(prematureEffects.isEmpty)
        XCTAssertEqual(prematureRefreshCount, 1)

        await direct.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await link.bootstrapCount == 2 }
        let effectsBeforeFreshState = await link.effectCalls
        XCTAssertTrue(effectsBeforeFreshState.isEmpty)

        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await link.effectCalls == [.markRead(sessionID: "session")] }
        await link.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
        await waitUntil { await link.closeCount == 1 }
    }

    func testInitialNotSentBootstrapRetriesFastWithoutFlashingFailure() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapRetryIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 1
        )
        var observedStates: [RemoteHostConnectionState] = []
        let observation = runtime.$connectionState.sink { state in
            observedStates.append(state)
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_bootstrap_failed",
            message: "paired endpoint is rebinding",
            kind: "notSent",
            operation: "bootstrap"
        )))

        await waitUntil { await backend.bootstrapCount == 2 }
        XCTAssertEqual(runtime.connectionState, .connecting)
        XCTAssertNil(runtime.snapshot)
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        XCTAssertFalse(observedStates.contains { state in
            switch state {
            case .failed, .reconnecting: true
            default: false
            }
        })

        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        XCTAssertTrue(runtime.terminalEffectsEnabled)
        withExtendedLifetime(observation) {}
        runtime.disconnect()
    }

    func testInitialBootstrapFastRetryPublishesFailureAfterBound() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 60_000_000_000,
            initialBootstrapRetryIntervalNanoseconds: 1_000_000,
            initialBootstrapFastRetryCount: 1
        )
        let failure = NativeRemoteBackendError(
            result: -1,
            code: "remote_bootstrap_failed",
            message: "Host is offline",
            kind: "notSent",
            operation: "bootstrap"
        )

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.failure(failure))
        await waitUntil { await backend.bootstrapCount == 2 }
        XCTAssertEqual(runtime.connectionState, .connecting)
        await backend.resolveBootstrap(.failure(failure))
        await waitUntil {
            runtime.connectionState == .failed(message: "Host is offline")
        }
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        try? await Task.sleep(nanoseconds: 5_000_000)
        let attemptsAfterGrace = await backend.bootstrapCount
        XCTAssertEqual(attemptsAfterGrace, 2)
        runtime.disconnect()
    }

    func testLateBootstrapFromReplacedHostCannotPublishAndOldBackendCloses() async {
        let first = ControlledRemoteBackend()
        let second = ControlledRemoteBackend()
        let runtime = makeRuntime { transport in
            guard case let .ssh(target, _, _, _) = transport else { return second }
            return target == "ssh://first" ? first : second
        }

        runtime.connectSSH(target: "ssh://first", expectedHostID: "first")
        await waitUntil { await first.bootstrapCount == 1 }
        runtime.connectSSH(target: "ssh://second", expectedHostID: "second")
        await waitUntil { await second.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(
            makeSnapshot(hostID: "first", hostName: "First")
        ))
        await second.resolveBootstrap(.success(
            makeSnapshot(hostID: "second", hostName: "Second")
        ))

        await waitUntil { runtime.snapshot?.macID == "second" }
        XCTAssertEqual(runtime.snapshot?.macID, "second")
        XCTAssertEqual(
            runtime.connectionState,
            RemoteHostConnectionState.connected(name: "Second")
        )
        await waitUntil { await first.closeCount == 1 }
        let firstCloseCount = await first.closeCount
        XCTAssertEqual(firstCloseCount, 1)
        runtime.disconnect()
    }

    func testTerminalIdentityErrorClosesAndClearsMatchingBackend() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "expected")
        await waitUntil { await backend.bootstrapCount == 1 }

        await backend.resolveBootstrap(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_identity_changed",
            message: "identity changed"
        )))

        await waitUntil {
            runtime.connectionState == .repairRequired(message: "identity changed")
        }
        await waitUntil { await backend.closeCount == 1 }
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        XCTAssertNil(runtime.terminalPane(for: "anything"))
        let closeCount = await backend.closeCount
        XCTAssertEqual(closeCount, 1)
        runtime.disconnect()
    }

    func testTerminalIdentityRepairWaitsForOldFitClearBeforeRebootstrap() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let replacement = ControlledRemoteBackend()
        var openCount = 0
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 20_000_000,
            backendFactory: { _ in
                defer { openCount += 1 }
                return openCount == 0 ? first : replacement
            }
        )

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }
        runtime.fitDesktop(sessionID: "session", columns: 100, rows: 30)
        await waitUntil { await first.effectCalls.count == 1 }
        await first.resolveNextEffect(.success(.init(requestID: 1)))

        await waitUntil { await first.bootstrapCount >= 2 }
        await first.resolveBootstrap(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_identity_changed",
            message: "The saved Host identity changed."
        )))
        await waitUntil {
            if case .repairRequired = runtime.connectionState { return true }
            return false
        }
        await waitUntil {
            await first.effectCalls == [
                .fit(sessionID: "session", columns: 100, rows: 30),
                .clearFit(sessionID: "session"),
            ]
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureBootstrapCount = await replacement.bootstrapCount
        XCTAssertEqual(prematureBootstrapCount, 0)

        await first.resolveNextEffect(.success(.init(requestID: 2)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await replacement.bootstrapCount == 1 }
        await replacement.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        runtime.disconnect()
        await waitUntil { await replacement.closeCount == 1 }
    }

    func testRefusedPageIsDiscardedAndNeverCommitted() async {
        let backend = ControlledRemoteBackend()
        let feedRecorder = FeedRecorder(accept: false)
        let runtime = makeRuntime(
            backend: backend,
            paneOutputFeeder: feedRecorder.feed
        )
        await connectWithSelectedSession(runtime, backend: backend)
        XCTAssertNotNil(runtime.terminalPane(for: "session"))
        await waitUntil { await backend.pollCount >= 1 }

        await backend.resolvePoll(.success(makePage(
            sessionID: "session",
            bytes: Data("refuse".utf8)
        )))

        await waitUntil { await backend.discardedPageCount == 1 }
        let committedPageCount = await backend.committedPageCount
        XCTAssertEqual(committedPageCount, 0)
        XCTAssertEqual(feedRecorder.feeds.map(\.bytes), [Data("refuse".utf8)])
        runtime.disconnect()
    }

    func testAcceptedPageFeedsAtomicResetThenCommits() async {
        let backend = ControlledRemoteBackend()
        let feedRecorder = FeedRecorder(accept: true)
        let runtime = makeRuntime(
            backend: backend,
            paneOutputFeeder: feedRecorder.feed
        )
        await connectWithSelectedSession(runtime, backend: backend)
        XCTAssertNotNil(runtime.terminalPane(for: "session"))
        await waitUntil { await backend.pollCount >= 1 }

        let bytes = Data("fresh tail".utf8)
        await backend.resolvePoll(.success(makePage(
            sessionID: "session",
            bytes: bytes,
            resetBeforeFeed: true
        )))

        await waitUntil { await backend.committedPageCount == 1 }
        let discardedPageCount = await backend.discardedPageCount
        XCTAssertEqual(discardedPageCount, 0)
        XCTAssertEqual(
            feedRecorder.feeds,
            [FeedRecorder.Feed(bytes: bytes, resetBeforeFeed: true)]
        )
        runtime.disconnect()
    }

    func testPresentedPanesStreamAndRouteEffectsIndependently() async {
        let backend = ControlledRemoteBackend()
        let feedRecorder = FeedRecorder(accept: true)
        let runtime = makeRuntime(
            backend: backend,
            paneOutputFeeder: feedRecorder.feed
        )
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"),
            makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }

        XCTAssertNotNil(runtime.terminalPane(for: "first"))
        XCTAssertNotNil(runtime.terminalPane(for: "second"))
        runtime.setPresentedTerminalSessions(Set(["first", "second"]))
        await waitUntil {
            Set(await backend.pollSessionIDs).isSuperset(of: ["first", "second"])
        }

        let secondBytes = Data("second pane".utf8)
        let firstBytes = Data("first pane".utf8)
        await backend.resolvePoll(
            for: "second",
            .success(makePage(sessionID: "second", bytes: secondBytes))
        )
        await backend.resolvePoll(
            for: "first",
            .success(makePage(sessionID: "first", bytes: firstBytes))
        )
        await waitUntil { await backend.committedPageCount == 2 }
        XCTAssertEqual(feedRecorder.feeds.map(\.bytes), [secondBytes, firstBytes])
        XCTAssertEqual(runtime.selectedSessionID, "first")

        // Navigation selection and renderer presentation are separate. A
        // representative/member focus round-trip must not collapse the
        // view-owned set and silently disable the other visible pane.
        runtime.selectSession("second")
        runtime.selectSession("first")
        runtime.sendTerminalInput(Data("companion input".utf8), to: "second")
        runtime.fitDesktop(sessionID: "second", columns: 91, rows: 27)
        await waitUntil { await backend.effectCalls.count == 2 }
        let presentedEffects = await backend.effectCalls
        XCTAssertEqual(presentedEffects, [
            .write(sessionID: "second", data: Data("companion input".utf8)),
            .fit(sessionID: "second", columns: 91, rows: 27),
        ])

        await waitUntil {
            let sessionIDs = await backend.pollSessionIDs
            return sessionIDs.filter { $0 == "second" }.count >= 2
                && sessionIDs.filter { $0 == "first" }.count >= 2
        }
        runtime.setPresentedTerminalSessions(Set(["first"]))
        await waitUntil { await backend.effectCalls.count == 3 }
        let detachedEffects = await backend.effectCalls
        XCTAssertEqual(
            detachedEffects.last,
            .clearFit(sessionID: "second")
        )

        runtime.sendTerminalInput(Data("ignored".utf8), to: "second")
        runtime.fitDesktop(sessionID: "second", columns: 120, rows: 40)
        try? await Task.sleep(nanoseconds: 2_000_000)
        let effectsAfterDetach = await backend.effectCalls
        XCTAssertEqual(effectsAfterDetach.count, 3)

        await backend.resolvePoll(
            for: "second",
            .success(makePage(
                sessionID: "second",
                bytes: Data("late companion".utf8)
            ))
        )
        await waitUntil { await backend.discardedPageCount == 1 }

        let continuingBytes = Data("primary continues".utf8)
        await backend.resolvePoll(
            for: "first",
            .success(makePage(sessionID: "first", bytes: continuingBytes))
        )
        await waitUntil { await backend.committedPageCount == 3 }
        XCTAssertEqual(feedRecorder.feeds.map(\.bytes), [
            secondBytes,
            firstBytes,
            continuingBytes,
        ])
        XCTAssertEqual(runtime.selectedSessionID, "first")
        runtime.disconnect()
    }

    func testPresentedPanePumpSurvivesTransientViewReparenting() async {
        let backend = ControlledRemoteBackend()
        let feedRecorder = FeedRecorder(accept: true)
        var attached = true
        let runtime = makeRuntime(
            backend: backend,
            paneAttachmentProbe: { _ in attached },
            paneOutputFeeder: feedRecorder.feed
        )
        await connectWithSelectedSession(runtime, backend: backend)
        XCTAssertNotNil(runtime.terminalPane(for: "session"))
        await waitUntil { await backend.pollCount == 1 }

        attached = false
        await backend.resolvePoll(.success(makePage(
            sessionID: "session",
            bytes: Data("during reparent".utf8)
        )))
        await waitUntil { await backend.discardedPageCount == 1 }

        attached = true
        await waitUntil { await backend.pollCount >= 2 }
        let afterReparent = Data("after reparent".utf8)
        await backend.resolvePoll(.success(makePage(
            sessionID: "session",
            bytes: afterReparent
        )))
        await waitUntil { await backend.committedPageCount == 1 }
        XCTAssertEqual(feedRecorder.feeds.map(\.bytes), [afterReparent])
        runtime.disconnect()
    }

    func testSessionSwitchDiscardsInFlightUnacceptedPage() async {
        let backend = ControlledRemoteBackend()
        let feedRecorder = FeedRecorder(accept: true)
        let runtime = makeRuntime(
            backend: backend,
            paneOutputFeeder: feedRecorder.feed
        )
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"),
            makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }
        XCTAssertNotNil(runtime.terminalPane(for: "first"))
        await waitUntil { await backend.pollCount >= 1 }

        runtime.selectSession("second")
        await backend.resolvePoll(.success(makePage(
            sessionID: "first",
            bytes: Data("late".utf8)
        )))

        await waitUntil { await backend.discardedPageCount == 1 }
        XCTAssertTrue(feedRecorder.feeds.isEmpty)
        let committedPageCount = await backend.committedPageCount
        XCTAssertEqual(committedPageCount, 0)
        runtime.disconnect()
    }

    func testTerminalWritesStayInUIOrder() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)

        runtime.sendTerminalInput(Data("a".utf8), to: "session")
        runtime.sendTerminalInput(Data("b".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 1 }
        let firstPayloads = await backend.writePayloads
        XCTAssertEqual(firstPayloads, [Data("ab".utf8)])

        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 1)))
        let orderedPayloads = await backend.writePayloads
        XCTAssertEqual(orderedPayloads, [Data("ab".utf8)])
        runtime.disconnect()
    }

    func testOutcomeUnknownDropsQueuedWritesUntilSuccessfulRebootstrap() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        await connectWithSelectedSession(runtime, backend: backend)
        await waitUntil { await backend.bootstrapCount >= 2 }

        runtime.sendTerminalInput(Data("uncertain".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 1 }
        runtime.sendTerminalInput(Data("must-not-follow".utf8), to: "session")
        await backend.resolveNextWrite(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_effect_failed",
            message: "delivery is unknown",
            kind: "outcomeUnknown",
            operation: "terminal write"
        )))

        await waitUntil { !runtime.terminalEffectsEnabled }
        try? await Task.sleep(nanoseconds: 5_000_000)
        let suspendedPayloads = await backend.writePayloads
        XCTAssertEqual(suspendedPayloads, [Data("uncertain".utf8)])

        // This bootstrap began before the effect failed. Its success is stale
        // and must not reopen input.
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { await backend.bootstrapCount >= 3 }
        XCTAssertFalse(runtime.terminalEffectsEnabled)

        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }

        runtime.sendTerminalInput(Data("after-bootstrap".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 2 }
        let resumedPayloads = await backend.writePayloads
        XCTAssertEqual(
            resumedPayloads,
            [Data("uncertain".utf8), Data("after-bootstrap".utf8)]
        )
        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 2)))
        runtime.disconnect()
    }

    func testInputTypedDuringReconnectIsDeliveredInOrderAfterRebootstrap() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        await connectWithSelectedSession(runtime, backend: backend)
        await waitUntil { await backend.bootstrapCount >= 2 }

        // A health poll fails with no write in flight: remote I/O closes
        // for a transient reason, and nothing dispatched is uncertain.
        await backend.resolveBootstrap(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_transport_failed",
            message: "socket closed",
            kind: "transport",
            operation: "bootstrap"
        )))
        await waitUntil { !runtime.terminalEffectsEnabled }

        runtime.sendTerminalInput(Data("typed-".utf8), to: "session")
        runtime.sendTerminalInput(Data("while-down".utf8), to: "session")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let suspendedPayloads = await backend.writePayloads
        XCTAssertTrue(suspendedPayloads.isEmpty)

        await waitUntil { await backend.bootstrapCount >= 3 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }
        await waitUntil { await backend.writePayloads.count == 1 }
        let resumedPayloads = await backend.writePayloads
        XCTAssertEqual(resumedPayloads, [Data("typed-while-down".utf8)])
        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 1)))
        runtime.disconnect()
    }

    func testInputTypedAfterOutcomeUnknownWriteIsNotDeliveredOnRebootstrap() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        await connectWithSelectedSession(runtime, backend: backend)
        await waitUntil { await backend.bootstrapCount >= 2 }

        runtime.sendTerminalInput(Data("uncertain".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 1 }
        await backend.resolveNextWrite(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_effect_failed",
            message: "delivery is unknown",
            kind: "outcomeUnknown",
            operation: "terminal write"
        )))
        await waitUntil { !runtime.terminalEffectsEnabled }

        // Typed after the ambiguity: it could land without "uncertain".
        runtime.sendTerminalInput(Data("must-not-follow".utf8), to: "session")

        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { await backend.bootstrapCount >= 3 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }

        runtime.sendTerminalInput(Data("after-bootstrap".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 2 }
        let resumedPayloads = await backend.writePayloads
        XCTAssertEqual(
            resumedPayloads,
            [Data("uncertain".utf8), Data("after-bootstrap".utf8)]
        )
        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 2)))
        runtime.disconnect()
    }

    func testNotAppliedWriteRetriesKnownMissingPrefixBeforeQueuedTail() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        await connectWithSelectedSession(runtime, backend: backend)
        await waitUntil { await backend.bootstrapCount >= 2 }

        runtime.sendTerminalInput(Data("first".utf8), to: "session")
        await waitUntil { await backend.writePayloads.count == 1 }
        runtime.sendTerminalInput(Data("queued".utf8), to: "session")
        await backend.resolveNextWrite(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_operation_rejected",
            message: "Host rejected this write",
            kind: "notApplied",
            operation: "terminal write"
        )))

        await waitUntil { await backend.writePayloads.count == 2 }
        XCTAssertTrue(runtime.terminalEffectsEnabled)
        XCTAssertEqual(
            runtime.connectionState,
            .connected(name: "Host")
        )
        let calls = await backend.writePayloads
        XCTAssertEqual(calls, [Data("first".utf8), Data("first".utf8)])
        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 2)))
        await waitUntil { await backend.writePayloads.count == 3 }
        let orderedCalls = await backend.writePayloads
        XCTAssertEqual(
            orderedCalls,
            [Data("first".utf8), Data("first".utf8), Data("queued".utf8)]
        )
        await backend.resolveNextWrite(.success(NativeRemoteEffectReceipt(requestID: 3)))
        runtime.disconnect()
    }

    func testTransportNotSentDropsQueuedTailAndUsesVerifiedFallback() async {
        let direct = ControlledRemoteBackend(controlWrites: true)
        let link = ControlledRemoteBackend()
        let runtime = makeRuntime(
            refreshIntervalNanoseconds: 60_000_000_000,
            backendFactory: { transport in
                if case .link = transport { return link }
                return direct
            }
        )
        runtime.connectPairedHost(
            record: makePairedRecord(),
            credentials: makeRemoteCredentials()
        )
        await waitUntil { await direct.bootstrapCount == 1 }
        await direct.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.terminalEffectsEnabled }

        runtime.sendTerminalInput(Data("not-sent".utf8), to: "session")
        await waitUntil { await direct.writePayloads.count == 1 }
        runtime.sendTerminalInput(Data("must-not-leapfrog".utf8), to: "session")
        await direct.resolveNextWrite(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_connection_disconnected",
            message: "Direct transport failed before send",
            kind: "notApplied",
            operation: "terminal write"
        )))

        await waitUntil { await link.bootstrapCount == 1 }
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        let directPayloads = await direct.writePayloads
        XCTAssertEqual(directPayloads, [Data("not-sent".utf8)])

        await link.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil { runtime.connectionRoute == .link }
        let linkEffects = await link.effectCalls
        XCTAssertTrue(linkEffects.isEmpty)
        runtime.disconnect()
    }

    func testMissingInputCapabilityConnectsReadOnlyWithoutIssuingWrite() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: ["host.bootstrap", "session.output.read"],
            sessions: [makeSession(id: "session")]
        )))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }

        XCTAssertFalse(runtime.terminalEffectsEnabled)
        runtime.sendTerminalInput(Data("must-not-send".utf8), to: "session")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let payloads = await backend.writePayloads
        XCTAssertTrue(payloads.isEmpty)
        runtime.disconnect()
    }

    func testMissingOutputCapabilityIsVisibleAndNeverPolls() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(
            capabilities: ["host.bootstrap", "session.input.write"],
            sessions: [makeSession(id: "session")]
        )))
        await waitUntil {
            runtime.connectionState == .incompatible(
                message: "This Host does not advertise remote terminal output."
            )
        }
        XCTAssertFalse(runtime.terminalEffectsEnabled)
        XCTAssertNotNil(runtime.terminalPane(for: "session"))
        runtime.sendTerminalInput(Data("no".utf8), to: "session")
        runtime.fitDesktop(sessionID: "session", columns: 80, rows: 24)
        runtime.markRead(sessionID: "session")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let polls = await backend.pollCount
        let effects = await backend.effectCalls
        XCTAssertEqual(polls, 0)
        XCTAssertTrue(effects.isEmpty)
        runtime.disconnect()
    }

    func testUTF8ScalarSplitAcrossCallbacksStartsFreshBoundedBatch() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)

        runtime.sendTerminalInput(
            Data(repeating: UInt8(ascii: "x"), count: 65_535),
            to: "session"
        )
        let emoji = Data("😀".utf8)
        runtime.sendTerminalInput(Data(emoji.prefix(2)), to: "session")
        runtime.sendTerminalInput(Data(emoji.suffix(2)), to: "session")

        await waitUntil { await backend.writePayloads.count == 1 }
        let first = await backend.writePayloads[0]
        XCTAssertEqual(first.count, 65_535)
        await backend.resolveNextWrite(.success(.init(requestID: 1)))
        await waitUntil { await backend.writePayloads.count == 2 }
        let payloads = await backend.writePayloads
        XCTAssertEqual(payloads[1], emoji)
        XCTAssertTrue(payloads.allSatisfy {
            $0.count <= RemoteHostRuntime.maximumTerminalWriteBytes
                && String(data: $0, encoding: .utf8) != nil
        })
        await backend.resolveNextWrite(.success(.init(requestID: 2)))
        runtime.disconnect()
    }

    func testMarkReadSuccessWaitsForSnapshotClearBeforeAnotherRequest() async {
        let backend = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000
        )
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await backend.effectCalls.count == 1 }
        await waitUntil { await backend.bootstrapCount >= 2 }
        await backend.resolveNextEffect(.success(.init(requestID: 1)))

        // This snapshot began before mark-read succeeded and still says
        // unread. The success latch must suppress a duplicate effect.
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: true),
        ])))
        await waitUntil { await backend.bootstrapCount >= 3 }
        let calls = await backend.effectCalls
        XCTAssertEqual(calls, [.markRead(sessionID: "session")])

        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session", unread: false),
        ])))
        runtime.disconnect()
    }

    func testCommitFailureForcesCursorResetBeforeRecoveryPoll() async {
        let backend = ControlledRemoteBackend(controlCommits: true)
        let feedRecorder = FeedRecorder(accept: true)
        let runtime = makeRuntime(
            backend: backend,
            refreshIntervalNanoseconds: 1_000_000,
            paneOutputFeeder: feedRecorder.feed
        )
        await connectWithSelectedSession(runtime, backend: backend)
        XCTAssertNotNil(runtime.terminalPane(for: "session"))
        await waitUntil { await backend.pollCount == 1 }
        await backend.resolvePoll(.success(makePage(
            sessionID: "session",
            bytes: Data("accepted".utf8)
        )))
        await waitUntil { await backend.commitAttemptCount == 1 }
        await backend.resolveNextCommit(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_output_commit_failed",
            message: "commit failed"
        )))
        await waitUntil { !runtime.terminalEffectsEnabled }

        // Discard the health poll that began before commit failure, then
        // accept a genuinely later bootstrap.
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [makeSession(id: "session")])))
        await waitUntil { await backend.bootstrapCount >= 3 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [makeSession(id: "session")])))
        await waitUntil { runtime.terminalEffectsEnabled }
        await waitUntil { await backend.resetSessionIDs.count >= 2 }
        let resets = await backend.resetSessionIDs
        XCTAssertEqual(Array(resets.prefix(2)), ["session", "session"])
        runtime.disconnect()
    }

    func testCommitFailureAfterSessionSwitchStillResetsRetainedPane() async {
        let backend = ControlledRemoteBackend(controlCommits: true)
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"), makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }
        XCTAssertNotNil(runtime.terminalPane(for: "first"))
        await waitUntil { await backend.pollCount == 1 }
        await backend.resolvePoll(.success(makePage(
            sessionID: "first",
            bytes: Data("accepted".utf8)
        )))
        await waitUntil { await backend.commitAttemptCount == 1 }

        runtime.selectSession("second")
        await backend.resolveNextCommit(.failure(NativeRemoteBackendError(
            result: -1,
            code: "remote_output_commit_failed",
            message: "late commit failed"
        )))
        runtime.selectSession("first")
        await waitUntil { await backend.resetSessionIDs.count >= 2 }
        let resets = await backend.resetSessionIDs
        XCTAssertEqual(Array(resets.prefix(2)), ["first", "first"])
        runtime.disconnect()
    }

    func testLargePasteIsSplitAt64KiBAndMixedEffectsStayOrdered() async {
        let backend = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)

        let paste = Data(repeating: UInt8(ascii: "x"), count: 100_000)
        runtime.sendTerminalInput(paste, to: "session")
        runtime.fitDesktop(sessionID: "session", columns: 120, rows: 40)
        runtime.sendTerminalInput(Data("z".utf8), to: "session")

        await waitUntil { await backend.effectCalls.count == 1 }
        await backend.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await backend.effectCalls.count == 2 }
        await backend.resolveNextEffect(.success(.init(requestID: 2)))
        await waitUntil { await backend.effectCalls.count == 3 }
        await backend.resolveNextEffect(.success(.init(requestID: 3)))
        await waitUntil { await backend.effectCalls.count == 4 }

        let calls = await backend.effectCalls
        guard case let .write(_, first) = calls[0],
              case let .write(_, second) = calls[1],
              case .fit = calls[2],
              case let .write(_, last) = calls[3]
        else { return XCTFail("Expected write chunks, fit, then final write") }
        XCTAssertLessThanOrEqual(first.count, RemoteHostRuntime.maximumTerminalWriteBytes)
        XCTAssertLessThanOrEqual(second.count, RemoteHostRuntime.maximumTerminalWriteBytes)
        XCTAssertEqual(first + second, paste)
        XCTAssertEqual(last, Data("z".utf8))
        await backend.resolveNextEffect(.success(.init(requestID: 4)))
        runtime.disconnect()
    }

    func testBurstInputCoalescesAndBackpressureFailsVisibly() async {
        let backend = ControlledRemoteBackend(controlWrites: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)

        for _ in 0..<1_000 {
            runtime.sendTerminalInput(Data("abc".utf8), to: "session")
        }
        await waitUntil { await backend.writePayloads.count == 1 }
        let burst = await backend.writePayloads
        XCTAssertEqual(burst[0], Data(String(repeating: "abc", count: 1_000).utf8))
        await backend.resolveNextWrite(.success(.init(requestID: 1)))

        runtime.sendTerminalInput(
            Data(
                repeating: UInt8(ascii: "q"),
                count: RemoteHostRuntime.maximumPendingTerminalInputBytes + 1
            ),
            to: "session"
        )
        await waitUntil { !runtime.terminalEffectsEnabled }
        guard case let .reconnecting(message) = runtime.connectionState else {
            return XCTFail("Expected visible reconnecting backpressure state")
        }
        XCTAssertTrue(message.contains("could not keep up"))
        runtime.disconnect()
    }

    func testSelectionSwitchClearsFitAfterInFlightFitInStrictOrder() async {
        let backend = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"), makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }

        runtime.fitDesktop(sessionID: "first", columns: 100, rows: 30)
        await waitUntil { await backend.effectCalls.count == 1 }
        runtime.selectSession("second")
        try? await Task.sleep(nanoseconds: 2_000_000)
        let callsWhileFitBlocked = await backend.effectCalls
        XCTAssertEqual(callsWhileFitBlocked.count, 1)
        await backend.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await backend.effectCalls.count == 2 }
        let orderedCalls = await backend.effectCalls
        XCTAssertEqual(orderedCalls, [
            .fit(sessionID: "first", columns: 100, rows: 30),
            .clearFit(sessionID: "first"),
        ])
        await backend.resolveNextEffect(.success(.init(requestID: 2)))
        runtime.disconnect()
    }

    func testRejectedReplacementFitRetainsEarlierFitForDisconnectCleanup() async {
        let backend = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)

        runtime.fitDesktop(sessionID: "session", columns: 80, rows: 24)
        await waitUntil { await backend.effectCalls.count == 1 }
        await backend.resolveNextEffect(.success(.init(requestID: 1)))

        runtime.fitDesktop(sessionID: "session", columns: 100, rows: 30)
        await waitUntil { await backend.effectCalls.count == 2 }
        runtime.markRead(sessionID: "session")
        await backend.resolveNextEffect(.failure(NativeRemoteBackendError(
            result: -1,
            code: "host_operation_rejected",
            message: "fit rejected",
            kind: "notApplied",
            operation: "desktop fit"
        )))
        await waitUntil { await backend.effectCalls.count == 3 }
        let continuedCalls = await backend.effectCalls
        XCTAssertEqual(continuedCalls[2], .markRead(sessionID: "session"))
        await backend.resolveNextEffect(.success(.init(requestID: 3)))

        runtime.disconnect()
        await waitUntil { await backend.effectCalls.count == 4 }
        let cleanupCalls = await backend.effectCalls
        XCTAssertEqual(cleanupCalls[3], .clearFit(sessionID: "session"))
        await backend.resolveNextEffect(.success(.init(requestID: 4)))
        await waitUntil { await backend.closeCount == 1 }
    }

    func testDisconnectWaitsForInFlightFitThenClearsBeforeClose() async {
        let backend = ControlledRemoteBackend(controlEffects: true)
        let runtime = makeRuntime(backend: backend)
        await connectWithSelectedSession(runtime, backend: backend)
        runtime.fitDesktop(sessionID: "session", columns: 90, rows: 25)
        await waitUntil { await backend.effectCalls.count == 1 }

        runtime.disconnect()
        let closeCountBeforeFit = await backend.closeCount
        XCTAssertEqual(closeCountBeforeFit, 0)
        await backend.resolveNextEffect(.success(.init(requestID: 1)))
        await waitUntil { await backend.effectCalls.count == 2 }
        let cleanupCalls = await backend.effectCalls
        let closeCountBeforeClear = await backend.closeCount
        XCTAssertEqual(cleanupCalls.last, .clearFit(sessionID: "session"))
        XCTAssertEqual(closeCountBeforeClear, 0)
        await backend.resolveNextEffect(.success(.init(requestID: 2)))
        await waitUntil { await backend.closeCount == 1 }
    }

    func testSameHostReconnectClearsOldFitBeforeNewGenerationRefits() async {
        let first = ControlledRemoteBackend(controlEffects: true)
        let second = ControlledRemoteBackend(controlEffects: true)
        var factoryCall = 0
        let runtime = makeRuntime { _ in
            defer { factoryCall += 1 }
            return factoryCall == 0 ? first : second
        }

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.bootstrapCount == 1 }
        await first.resolveBootstrap(.success(makeSnapshot(sessions: [makeSession(id: "session")])))
        await waitUntil { runtime.terminalEffectsEnabled }
        runtime.fitDesktop(sessionID: "session", columns: 110, rows: 35)
        await waitUntil { await first.effectCalls.count == 1 }
        await first.resolveNextEffect(.success(.init(requestID: 1)))

        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await first.effectCalls.count == 2 }
        try? await Task.sleep(nanoseconds: 2_000_000)
        let prematureBootstrapCount = await second.bootstrapCount
        let prematureNewEffects = await second.effectCalls
        XCTAssertEqual(prematureBootstrapCount, 0)
        XCTAssertTrue(prematureNewEffects.isEmpty)
        let oldEffects = await first.effectCalls
        XCTAssertEqual(oldEffects.last, .clearFit(sessionID: "session"))

        await first.resolveNextEffect(.success(.init(requestID: 2)))
        await waitUntil { await first.closeCount == 1 }
        await waitUntil { await second.bootstrapCount == 1 }
        await second.resolveBootstrap(.success(makeSnapshot(sessions: [makeSession(id: "session")])))
        await waitUntil { runtime.connectionState == .connected(name: "Host") }
        await waitUntil { await second.effectCalls.count == 1 }
        let newEffects = await second.effectCalls
        XCTAssertEqual(newEffects, [
            .fit(sessionID: "session", columns: 110, rows: 35),
        ])
        await second.resolveNextEffect(.success(.init(requestID: 1)))
        runtime.disconnect()
    }

    func testNewAndRecreatedPaneResetCursorBeforePolling() async {
        let backend = ControlledRemoteBackend(controlResets: true)
        let cache = RemoteGhosttyPaneCache(retainedPaneLimit: 1)
        let runtime = makeRuntime(
            backend: backend,
            paneCache: cache,
            paneAttachmentProbe: { _ in false }
        )
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"), makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }

        let firstPane = runtime.terminalPane(for: "first")
        await waitUntil { await backend.resetSessionIDs == ["first"] }
        let initialPollCount = await backend.pollCount
        XCTAssertEqual(initialPollCount, 0)
        await backend.resolveNextReset(.success(()))

        runtime.selectSession("second")
        XCTAssertNotNil(runtime.terminalPane(for: "second"))
        await waitUntil { await backend.resetSessionIDs == ["first", "second"] }
        await backend.resolveNextReset(.success(()))

        runtime.selectSession("first")
        let recreated = runtime.terminalPane(for: "first")
        await waitUntil { await backend.resetSessionIDs == ["first", "second", "first"] }
        XCTAssertFalse(firstPane === recreated)
        let finalPollCount = await backend.pollCount
        XCTAssertEqual(finalPollCount, 0)
        await backend.resolveNextReset(.success(()))
        runtime.disconnect()
    }

    func testOutputPagePendingDuringQuickSessionRevisitIsTransient() async {
        let backend = ControlledRemoteBackend()
        let runtime = makeRuntime(backend: backend)
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "first"), makeSession(id: "second"),
        ])))
        await waitUntil { runtime.selectedSessionID == "first" }
        XCTAssertNotNil(runtime.terminalPane(for: "first"))
        await waitUntil { await backend.pollCount == 1 }

        runtime.selectSession("second")
        runtime.selectSession("first")
        await waitUntil { await backend.pollCount == 2 }
        await backend.resolvePoll(at: 1, .failure(NativeRemoteBackendError(
            result: -1,
            code: "output_page_pending",
            message: "previous page remains pending"
        )))
        await waitUntil { await backend.pollCount >= 3 }
        XCTAssertTrue(runtime.terminalEffectsEnabled)
        XCTAssertEqual(runtime.connectionState, .connected(name: "Host"))

        await backend.resolvePoll(at: 0, .success(makePage(
            sessionID: "first",
            bytes: Data("stale".utf8)
        )))
        await waitUntil { await backend.discardedPageCount == 1 }
        runtime.disconnect()
    }

    private func makeRuntime(
        backend: ControlledRemoteBackend,
        refreshIntervalNanoseconds: UInt64 = 60_000_000_000,
        initialBootstrapRetryIntervalNanoseconds: UInt64 = 1_000_000,
        initialBootstrapFastRetryCount: Int = 3,
        paneCache: RemoteGhosttyPaneCache = RemoteGhosttyPaneCache(),
        paneAttachmentProbe: @escaping RemoteHostRuntime.PaneAttachmentProbe = { _ in true },
        paneOutputFeeder: @escaping RemoteHostRuntime.PaneOutputFeeder = {
            _, _, _ in true
        }
    ) -> RemoteHostRuntime {
        makeRuntime(
            refreshIntervalNanoseconds: refreshIntervalNanoseconds,
            initialBootstrapRetryIntervalNanoseconds:
                initialBootstrapRetryIntervalNanoseconds,
            initialBootstrapFastRetryCount: initialBootstrapFastRetryCount,
            backendFactory: { _ in backend },
            paneCache: paneCache,
            paneAttachmentProbe: paneAttachmentProbe,
            paneOutputFeeder: paneOutputFeeder
        )
    }

    private func makeRuntime(
        refreshIntervalNanoseconds: UInt64 = 60_000_000_000,
        initialBootstrapRetryIntervalNanoseconds: UInt64 = 1_000_000,
        initialBootstrapFastRetryCount: Int = 3,
        initialDirectLinkGraceNanoseconds: UInt64 = 60_000_000_000,
        directProbeSuccessfulLinkRefreshes: Int = 8,
        forceLinkForDevelopment: Bool = false,
        backendFactory: @escaping RemoteHostRuntime.BackendFactory,
        paneCache: RemoteGhosttyPaneCache = RemoteGhosttyPaneCache(),
        paneAttachmentProbe: @escaping RemoteHostRuntime.PaneAttachmentProbe = { _ in true },
        paneOutputFeeder: @escaping RemoteHostRuntime.PaneOutputFeeder = {
            _, _, _ in true
        }
    ) -> RemoteHostRuntime {
        RemoteHostRuntime(
            refreshIntervalNanoseconds: refreshIntervalNanoseconds,
            initialBootstrapRetryIntervalNanoseconds:
                initialBootstrapRetryIntervalNanoseconds,
            initialBootstrapFastRetryCount: initialBootstrapFastRetryCount,
            initialDirectLinkGraceNanoseconds: initialDirectLinkGraceNanoseconds,
            directProbeSuccessfulLinkRefreshes: directProbeSuccessfulLinkRefreshes,
            forceLinkForDevelopment: forceLinkForDevelopment,
            outputIdleIntervalNanoseconds: 1_000_000,
            resizeDebounceNanoseconds: 1_000_000,
            fitSettleNanoseconds: 0,
            fitClearDelayNanoseconds: 0,
            backendFactory: backendFactory,
            paneCache: paneCache,
            paneAttachmentProbe: paneAttachmentProbe,
            paneOutputFeeder: paneOutputFeeder
        )
    }

    private func makePairedRecord(hostID: String = "host") -> PairedHostRecord {
        PairedHostRecord(
            hostID: hostID,
            name: "Host",
            endpoint: URL(string: "http://host.local:4321/mobile")!,
            controllerDeviceID: "controller",
            pairedAtUnixMs: 1,
            certificateFingerprint: "durable-pairing-fingerprint"
        )
    }

    private func makeRemoteCredentials(hostID: String = "host") -> RemoteHostCredentials {
        RemoteHostCredentials(
            authToken: "host-bearer",
            relayCredentials: RelayCredentials(
                relayURL: URL(string: "wss://link.example.test")!,
                macID: hostID,
                relayToken: "relay-secret",
                e2eKey: Data(repeating: 7, count: 32)
            )
        )
    }

    private func reachabilityFailure() -> NativeRemoteBackendError {
        NativeRemoteBackendError(
            result: -1,
            code: "host_connection_disconnected",
            message: "Direct Host is unreachable"
        )
    }

    private func connectWithSelectedSession(
        _ runtime: RemoteHostRuntime,
        backend: ControlledRemoteBackend
    ) async {
        runtime.connectSSH(target: "ssh://host", expectedHostID: "host")
        await waitUntil { await backend.bootstrapCount == 1 }
        await backend.resolveBootstrap(.success(makeSnapshot(sessions: [
            makeSession(id: "session"),
        ])))
        await waitUntil {
            runtime.connectionState == .connected(name: "Host")
                && runtime.selectedSessionID == "session"
                && runtime.terminalEffectsEnabled
        }
    }

    private func waitUntil(
        iterations: Int = 500,
        _ predicate: @escaping () async -> Bool
    ) async {
        for _ in 0..<iterations {
            if await predicate() { return }
            await Task.yield()
            try? await Task.sleep(nanoseconds: 100_000)
        }
        XCTFail("Timed out waiting for asynchronous state")
    }

    private func makeSnapshot(
        hostID: String = "host",
        hostName: String = "Host",
        capturedAt: Int64 = 1,
        capabilities: [String]? = [
            "host.bootstrap",
            "session.input.write",
            "session.mark_read",
            "session.output.read",
            "session.resize_desktop",
        ],
        sessions: [RemoteSessionSummary] = [],
        workspaceSettings: RemoteWorkspaceSettings? = nil
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            hostProtocol: capabilities.map {
                RemoteHostProtocolDescriptor(capabilities: $0)
            },
            macID: hostID,
            macName: hostName,
            folders: [],
            projects: [],
            presets: [],
            workspaceSettings: workspaceSettings,
            sessions: sessions,
            capturedAtUnixMs: capturedAt
        )
    }

    private func makeWorkspaceSettings(menuAttention: Bool) -> RemoteWorkspaceSettings {
        RemoteWorkspaceSettings(
            notificationSettings: RemoteNotificationSettings(
                menuAttentionDetection: menuAttention
            ),
            autoStopArchiveMinutes: 120,
            sidebarStoppedLimit: 5,
            browserDefaultAccess: "on",
            mcpNonchildWriteAccess: "ask",
            computerAccess: "ask",
            mcpWorktreeAccess: false,
            mcpAutoAddBrowserScreenshots: true
        )
    }

    private func makeSession(
        id: String,
        projectID: String = "project",
        command: String = "claude",
        createdAt: Int64 = 1,
        status: RemoteSessionStatus = .running,
        activity: RemoteActivityState = .idle,
        unread: Bool = false,
        updatedAt: Int64? = nil,
        preview: String? = nil,
        activeRuntimeID: String? = nil,
        runtimeLaunchPending: Bool = false,
        providerID: String? = nil,
        worktreePath: String? = nil,
        worktreeBranch: String? = nil,
        archived: Bool = false,
        latestAlertBody: String? = nil,
        latestAlertAt: Int64? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: projectID,
            activeRuntimeID: activeRuntimeID,
            runtimeLaunchPending: runtimeLaunchPending,
            providerID: providerID,
            title: id,
            command: command,
            createdAtUnixMs: createdAt,
            updatedAtUnixMs: updatedAt,
            status: status,
            activity: activity,
            unread: unread,
            worktreePath: worktreePath,
            worktreeBranch: worktreeBranch,
            lastOutputPreview: preview,
            archived: archived,
            latestAlertBody: latestAlertBody,
            latestAlertAtUnixMs: latestAlertAt
        )
    }

    private func makePage(
        sessionID: String,
        bytes: Data,
        resetBeforeFeed: Bool = false,
        pageHandle: UInt64 = UInt64.random(in: 1...UInt64.max)
    ) -> NativeRemoteOutputPage {
        NativeRemoteOutputPage(
            metadata: NativeRemoteOutputPageMetadata(
                sessionID: sessionID,
                requestedOffset: 0,
                offset: 0,
                nextOffset: UInt64(bytes.count),
                resetBeforeFeed: resetBeforeFeed,
                truncated: false,
                capturedAtUnixMs: 1,
                byteCount: bytes.count
            ),
            bytes: bytes,
            parentHandle: 1,
            pageHandle: pageHandle
        )
    }
}

@MainActor
private final class FeedRecorder {
    struct Feed: Equatable {
        let bytes: Data
        let resetBeforeFeed: Bool
    }

    let accept: Bool
    private(set) var feeds: [Feed] = []

    init(accept: Bool) {
        self.accept = accept
    }

    func feed(
        pane _: RemoteGhosttyTerminalPane,
        bytes: Data,
        resetBeforeFeed: Bool
    ) -> Bool {
        feeds.append(Feed(bytes: bytes, resetBeforeFeed: resetBeforeFeed))
        return accept
    }
}

private actor ControlledRemoteBackend: NativeRemoteBackendProtocol {
    private var bootstrapContinuations: [
        CheckedContinuation<RemoteBootstrapSnapshot, Error>
    ] = []
    private var pendingBootstraps: [Result<RemoteBootstrapSnapshot, Error>] = []
    private var pollContinuations: [(
        sessionID: String,
        continuation: CheckedContinuation<NativeRemoteOutputPage, Error>
    )] = []
    private var pendingPolls: [Result<NativeRemoteOutputPage, Error>] = []
    private var effectContinuations: [
        (call: ControlledEffectCall, continuation: CheckedContinuation<NativeRemoteEffectReceipt, Error>)
    ] = []
    private var resetContinuations: [CheckedContinuation<Void, Error>] = []
    private var commitContinuations: [CheckedContinuation<Void, Error>] = []

    private let controlWrites: Bool
    private let controlEffects: Bool
    private let controlResets: Bool
    private let controlCommits: Bool
    private let createdSessionSummary: RemoteSessionSummary?
    private(set) var bootstrapCount = 0
    private(set) var pollCount = 0
    private(set) var pollSessionIDs: [String] = []
    private(set) var closeCount = 0
    private(set) var committedPageCount = 0
    private(set) var discardedPageCount = 0
    private(set) var commitAttemptCount = 0
    private(set) var writePayloads: [Data] = []
    private(set) var effectCalls: [ControlledEffectCall] = []
    private(set) var resetSessionIDs: [String] = []
    private(set) var organizationCalls: [String] = []
    private(set) var projectOrganizationPatches: [RemoteProjectOrganizationPatch] = []

    init(
        controlWrites: Bool = false,
        controlEffects: Bool = false,
        controlResets: Bool = false,
        controlCommits: Bool = false,
        createdSessionSummary: RemoteSessionSummary? = nil
    ) {
        self.controlWrites = controlWrites
        self.controlEffects = controlEffects
        self.controlResets = controlResets
        self.controlCommits = controlCommits
        self.createdSessionSummary = createdSessionSummary
    }

    func bootstrap() async throws -> RemoteBootstrapSnapshot {
        bootstrapCount += 1
        if !pendingBootstraps.isEmpty {
            return try pendingBootstraps.removeFirst().get()
        }
        return try await withCheckedThrowingContinuation { continuation in
            bootstrapContinuations.append(continuation)
        }
    }

    func resolveBootstrap(_ result: Result<RemoteBootstrapSnapshot, Error>) {
        if bootstrapContinuations.isEmpty {
            pendingBootstraps.append(result)
        } else {
            bootstrapContinuations.removeFirst().resume(with: result)
        }
    }

    func pollOutput(
        sessionID: String,
        limit _: Int,
        waitMilliseconds _: UInt64
    ) async throws -> NativeRemoteOutputPage {
        pollCount += 1
        pollSessionIDs.append(sessionID)
        if !pendingPolls.isEmpty {
            return try pendingPolls.removeFirst().get()
        }
        return try await withCheckedThrowingContinuation { continuation in
            pollContinuations.append((sessionID, continuation))
        }
    }

    func pollOutputFrom(
        sessionID: String,
        requestedOffset _: UInt64?,
        limit: Int,
        waitMilliseconds: UInt64
    ) async throws -> NativeRemoteOutputPage {
        try await pollOutput(
            sessionID: sessionID,
            limit: limit,
            waitMilliseconds: waitMilliseconds
        )
    }

    func resolvePoll(_ result: Result<NativeRemoteOutputPage, Error>) {
        if pollContinuations.isEmpty {
            pendingPolls.append(result)
        } else {
            pollContinuations.removeFirst().continuation.resume(with: result)
        }
    }

    func resolvePoll(
        for sessionID: String,
        _ result: Result<NativeRemoteOutputPage, Error>
    ) {
        guard let index = pollContinuations.firstIndex(where: {
            $0.sessionID == sessionID
        }) else { return }
        pollContinuations.remove(at: index).continuation.resume(with: result)
    }

    func resolvePoll(
        at index: Int,
        _ result: Result<NativeRemoteOutputPage, Error>
    ) {
        guard pollContinuations.indices.contains(index) else { return }
        pollContinuations.remove(at: index).continuation.resume(with: result)
    }

    func commitOutput(_ page: NativeRemoteOutputPage) async throws {
        guard page.claimResolution() != nil else {
            throw ControlledRemoteBackendError.pageAlreadyResolved
        }
        commitAttemptCount += 1
        if controlCommits {
            try await withCheckedThrowingContinuation { continuation in
                commitContinuations.append(continuation)
            }
        }
        committedPageCount += 1
    }

    func resolveNextCommit(_ result: Result<Void, Error>) {
        guard !commitContinuations.isEmpty else { return }
        commitContinuations.removeFirst().resume(with: result)
    }

    func discardOutput(_ page: NativeRemoteOutputPage) async {
        guard page.claimResolution() != nil else { return }
        discardedPageCount += 1
    }

    func resetOutput(sessionID: String) async throws {
        resetSessionIDs.append(sessionID)
        guard controlResets else { return }
        try await withCheckedThrowingContinuation { continuation in
            resetContinuations.append(continuation)
        }
    }

    func resolveNextReset(_ result: Result<Void, Error>) {
        guard !resetContinuations.isEmpty else { return }
        resetContinuations.removeFirst().resume(with: result)
    }

    func writeTerminal(
        sessionID: String,
        data: Data
    ) async throws -> NativeRemoteEffectReceipt {
        writePayloads.append(data)
        return try await performEffect(.write(sessionID: sessionID, data: data))
    }

    func resolveNextWrite(_ result: Result<NativeRemoteEffectReceipt, Error>) {
        guard let index = effectContinuations.firstIndex(where: {
            if case .write = $0.call { return true }
            return false
        }) else { return }
        effectContinuations.remove(at: index).continuation.resume(with: result)
    }

    func fitDesktop(
        sessionID: String,
        columns: UInt16,
        rows: UInt16
    ) async throws -> NativeRemoteEffectReceipt {
        try await performEffect(.fit(
            sessionID: sessionID,
            columns: columns,
            rows: rows
        ))
    }

    func clearDesktopFit(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performEffect(.clearFit(sessionID: sessionID))
    }

    func markRead(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performEffect(.markRead(sessionID: sessionID))
    }

    func resolveNextEffect(_ result: Result<NativeRemoteEffectReceipt, Error>) {
        guard !effectContinuations.isEmpty else { return }
        effectContinuations.removeFirst().continuation.resume(with: result)
    }

    private func performEffect(
        _ call: ControlledEffectCall
    ) async throws -> NativeRemoteEffectReceipt {
        effectCalls.append(call)
        let shouldControl: Bool
        if case .write = call {
            shouldControl = controlWrites || controlEffects
        } else {
            shouldControl = controlEffects
        }
        guard shouldControl else {
            return NativeRemoteEffectReceipt(requestID: UInt64(effectCalls.count))
        }
        return try await withCheckedThrowingContinuation { continuation in
            effectContinuations.append((call, continuation))
        }
    }

    func setSessionTitle(
        sessionID: String,
        title: String
    ) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("title:\(sessionID):\(title)")
        return NativeRemoteEffectReceipt(requestID: 900)
    }

    func setSessionPinned(
        sessionID: String,
        pinned: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("pin:\(sessionID):\(pinned)")
        return NativeRemoteEffectReceipt(requestID: 901)
    }

    func archiveSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("archive:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 902)
    }

    func restoreSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("restore:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 903)
    }

    func stopSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("stop:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 904)
    }

    func removeSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("remove:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 905)
    }

    func restartSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("restart:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 906)
    }

    func resumeAgent(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("resume-agent:\(sessionID)")
        return NativeRemoteEffectReceipt(requestID: 910)
    }

    func setSessionOrder(
        projectID: String,
        orderedSessionIDs: [String]
    ) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("order:\(projectID):\(orderedSessionIDs.joined(separator: ","))")
        return NativeRemoteEffectReceipt(requestID: 907)
    }

    func setSessionProject(
        sessionID: String,
        projectID: String
    ) async throws -> NativeRemoteEffectReceipt {
        organizationCalls.append("project:\(sessionID):\(projectID)")
        return NativeRemoteEffectReceipt(requestID: 911)
    }

    func setProjectOrganization(
        projectID: String,
        patch: RemoteProjectOrganizationPatch
    ) async throws -> NativeRemoteEffectReceipt {
        projectOrganizationPatches.append(patch)
        organizationCalls.append(
            "project-organization:\(projectID):\(patch.sortOrder.map(String.init) ?? "-")"
                + ":\(patch.pinned.map(String.init) ?? "-")"
        )
        return NativeRemoteEffectReceipt(requestID: 909)
    }

    func createSession(
        _ request: RemoteCreateSessionRequest
    ) async throws -> NativeRemoteCreatedSession {
        organizationCalls.append(
            "create:\(request.projectID):\(request.presetID ?? "-")"
        )
        return NativeRemoteCreatedSession(
            requestID: 908,
            sessionID: "created-session",
            capturedAtUnixMs: nil,
            session: createdSessionSummary
        )
    }

    func pairingInvitation(_ requestJSON: Data) async throws -> Data {
        let request = try JSONSerialization.jsonObject(with: requestJSON) as? [String: Any]
        let action = request?["action"] as? String ?? "-"
        organizationCalls.append("pairing-invitation:\(action)")
        if action == "create",
           let endpoint = request?["endpoint"] as? String {
            return try JSONEncoder().encode(RemotePairingPayload(
                macID: "host",
                macName: "Host",
                endpoint: URL(string: endpoint)!,
                token: "PAIRING-TOKEN",
                expiresAtUnixMs: 1_800_000_000_000
            ))
        }
        if action == "complete",
           let envelope = request?["envelope"] {
            return try JSONSerialization.data(withJSONObject: envelope)
        }
        throw NativeRemoteBackendError(
            result: -1,
            code: "invalid_pairing_invitation",
            message: "invalid test pairing invitation"
        )
    }

    func listArchivedSessions(projectID: String) async throws -> [RemoteSessionSummary] {
        organizationCalls.append("archived:\(projectID)")
        return []
    }

    func transcriptMarkdown(
        sessionID: String,
        entries: Int?
    ) async throws -> RemoteTranscriptMarkdown {
        organizationCalls.append("transcript:\(sessionID)")
        return RemoteTranscriptMarkdown(sessionID: sessionID, markdown: "")
    }

    func sessionMetrics(sessionID: String) async throws -> NativeRemoteSessionMetrics {
        organizationCalls.append("metrics:\(sessionID)")
        return NativeRemoteSessionMetrics(
            sessionID: sessionID,
            columns: 80,
            rows: 24,
            outputOffset: nil,
            capturedAtUnixMs: 1
        )
    }

    func close() async {
        closeCount += 1
    }
}

private enum ControlledEffectCall: Equatable, Sendable {
    case write(sessionID: String, data: Data)
    case fit(sessionID: String, columns: UInt16, rows: UInt16)
    case clearFit(sessionID: String)
    case markRead(sessionID: String)
}

private enum ControlledRemoteBackendError: Error {
    case pageAlreadyResolved
}
