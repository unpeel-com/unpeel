import XCTest
@testable import UnpeelShared

final class RemoteControlProtocolTests: XCTestCase {
    func testHostProtocolDescriptorIsAdditiveAndMajorVersioned() throws {
        let descriptor = RemoteHostProtocolDescriptor(
            capabilities: ["host.bootstrap", "session.output.read", "future.capability"]
        )
        let decoded = try roundTrip(descriptor)

        XCTAssertEqual(decoded, descriptor)
        XCTAssertTrue(decoded.isCompatible())
        XCTAssertFalse(decoded.isCompatible(controllerMajorVersion: 2))
        XCTAssertTrue(decoded.supports("session.output.read"))
        XCTAssertFalse(decoded.supports("session.create"))
    }

    func testBootstrapDecodesWithoutHostProtocolForLegacyHosts() throws {
        let json = #"{"protocolVersion":1,"folders":[],"projects":[],"presets":[],"sessions":[],"capturedAtUnixMs":1}"#
        let decoded = try JSONDecoder().decode(RemoteBootstrapSnapshot.self, from: Data(json.utf8))
        XCTAssertNil(decoded.hostProtocol)
        // A legacy Host omits hostWorkspaces — treat missing as "just this one".
        XCTAssertNil(decoded.hostWorkspaces)
        // Pane grouping is an additive sidebar projection. Older Controllers
        // omit it and the phone keeps the legacy flat Session list.
        XCTAssertNil(decoded.paneGroups)
    }

    func testBootstrapRoundTripsSidebarPaneGroups() throws {
        let paneGroup = RemotePaneGroupSummary(
            id: "pane-group-1",
            representativeSessionID: "session-main",
            sessionIDs: ["session-main", "session-notes", "session-review"]
        )
        let snapshot = RemoteBootstrapSnapshot(
            folders: [],
            projects: [],
            presets: [],
            sessions: [],
            capturedAtUnixMs: 42,
            paneGroups: [paneGroup]
        )

        let encoded = try JSONEncoder().encode(snapshot)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        let encodedGroups = try XCTUnwrap(object["paneGroups"] as? [[String: Any]])
        XCTAssertEqual(encodedGroups.first?["representativeSessionID"] as? String, "session-main")
        XCTAssertEqual(
            encodedGroups.first?["sessionIDs"] as? [String],
            ["session-main", "session-notes", "session-review"]
        )

        let decoded = try JSONDecoder().decode(RemoteBootstrapSnapshot.self, from: encoded)
        XCTAssertEqual(decoded.paneGroups, [paneGroup])
    }

    func testBootstrapRoundTripsHostWorkspaceList() throws {
        let snapshot = RemoteBootstrapSnapshot(
            folders: [],
            projects: [],
            presets: [],
            sessions: [],
            capturedAtUnixMs: 42,
            directEndpoint: URL(string: "http://192.168.1.25:61234/mobile"),
            hostWorkspaces: [
                RemoteWorkspaceSummary(
                    id: "local:/Users/t/.unpeel",
                    name: "Personal",
                    tintHue: nil,
                    isCurrent: true,
                    isRunning: true,
                    kind: "local"
                ),
                RemoteWorkspaceSummary(
                    id: "1a2b3c",
                    name: "Client",
                    tintHue: 212,
                    isCurrent: false,
                    isRunning: false,
                    kind: "local"
                ),
                // A remote Host this Mac reaches (ssh/paired) — proxied through
                // the Mac with its own credentials; the phone tells it apart by
                // `kind`.
                RemoteWorkspaceSummary(
                    id: "ssh:1a2b3c",
                    name: "Server",
                    tintHue: 120,
                    isCurrent: false,
                    isRunning: false,
                    kind: "ssh"
                ),
            ]
        )

        let decoded = try roundTrip(snapshot)
        XCTAssertEqual(decoded.directEndpoint, snapshot.directEndpoint)
        XCTAssertEqual(decoded.hostWorkspaces, snapshot.hostWorkspaces)
        XCTAssertEqual(decoded.hostWorkspaces?.count, 3)
        let current = try XCTUnwrap(decoded.hostWorkspaces?.first)
        XCTAssertTrue(current.isCurrent)
        XCTAssertNil(current.tintHue)
        XCTAssertEqual(current.kind, "local")
        XCTAssertEqual(decoded.hostWorkspaces?.last?.kind, "ssh")

        // `kind` is additive/optional: an older Host that omits it still decodes,
        // and nil is treated as local by clients.
        let legacyJSON = #"{"id":"local:/x","name":"Legacy","isCurrent":false,"isRunning":true}"#
        let legacy = try JSONDecoder().decode(
            RemoteWorkspaceSummary.self,
            from: Data(legacyJSON.utf8)
        )
        XCTAssertNil(legacy.kind)
    }

    func testProjectGroupFieldsRoundTripAndRemainAdditive() throws {
        let group = RemoteProjectSummary(
            id: "group-research",
            name: "Research",
            path: "/dev/unpeel",
            parentProjectID: "project-unpeel",
            isGroup: true,
            colorID: "violet",
            pinned: true
        )

        XCTAssertEqual(try roundTrip(group), group)

        let legacyJSON = #"{"id":"project-unpeel","name":"Unpeel","path":"/dev/unpeel","mcpBlocked":false}"#
        let legacy = try JSONDecoder().decode(
            RemoteProjectSummary.self,
            from: Data(legacyJSON.utf8)
        )
        XCTAssertNil(legacy.isGroup)
        XCTAssertNil(legacy.pinned)
        XCTAssertNil(legacy.colorID)
        XCTAssertNil(legacy.sessionOrder)

        let mixed = RemoteProjectSummary(
            id: "project-unpeel",
            name: "Unpeel",
            path: "/dev/unpeel",
            sessionOrder: ["session-a", "group-research", "session-b"]
        )
        XCTAssertEqual(try roundTrip(mixed), mixed)
    }

    func testBootstrapCompatibilityFixtureCoversLegacyFutureAndMajorMismatch() throws {
        let root = try fixtureJSON("host-bootstrap-compatibility-v1.json")
        let cases = try XCTUnwrap(root["cases"] as? [[String: Any]])
        for item in cases {
            let id = try XCTUnwrap(item["id"] as? String)
            let object = try XCTUnwrap(item["bootstrap"] as? [String: Any])
            let data = try JSONSerialization.data(withJSONObject: object)
            let decoded = try JSONDecoder().decode(RemoteBootstrapSnapshot.self, from: data)
            let actual = decoded.hostProtocol?.isCompatible()
            let expected = item["compatible"] as? Bool
            XCTAssertEqual(actual, expected, "compatibility case \(id)")
            if id == "current-host" {
                XCTAssertEqual(decoded.sessions.first?.activeRuntimeID, "claude")
                XCTAssertNil(decoded.sessions.first?.providerID)
            }
        }
    }

    func testPairingPayloadAndResponseRoundTrip() throws {
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let payload = RemotePairingPayload(
            macID: "mac-1",
            macName: "Studio Mac",
            endpoint: endpoint,
            token: "pair-token",
            certificateFingerprint: nil,
            expiresAtUnixMs: 1_789_996_900_000
        )
        let request = RemotePairingRequest(
            token: payload.token,
            device: .init(
                id: "phone-1",
                name: "Tommy's iPhone",
                platform: "iOS",
                appVersion: "1.0"
            )
        )
        let response = RemotePairingResponse(
            macID: payload.macID,
            macName: payload.macName,
            endpoint: endpoint,
            deviceID: request.device.id,
            authToken: "auth-token",
            pairedAtUnixMs: 1_789_996_800_000,
            relayCredentials: RelayCredentials(
                relayURL: URL(string: "wss://relay.unpeel.com")!,
                macID: payload.macID,
                relayToken: "relay-token",
                e2eKey: Data(repeating: 7, count: 32)
            )
        )

        XCTAssertEqual(try roundTrip(payload), payload)
        XCTAssertEqual(try roundTrip(request), request)
        XCTAssertEqual(try roundTrip(response), response)
    }

    func testCompactPairingCodeRoundTrip() throws {
        let payload = RemotePairingPayload(
            macID: "mac-1",
            macName: "Studio Mac",
            endpoint: URL(string: "http://192.168.1.20:49152/mobile")!,
            token: "K7ZP2Q4RSTUVWXYZABCDEFGH23",
            certificateFingerprint: nil,
            expiresAtUnixMs: 1_789_996_900_000
        )

        let code = try XCTUnwrap(RemotePairingCode.encode(payload))
        XCTAssertEqual(code, "UNPEEL:1:192.168.1.20:49152:MAC-1:K7ZP2Q4RSTUVWXYZABCDEFGH23:1789996900")

        let decoded = try XCTUnwrap(RemotePairingCode.decode(code))
        XCTAssertEqual(decoded.endpoint, payload.endpoint)
        XCTAssertEqual(decoded.token, payload.token)
        XCTAssertEqual(decoded.expiresAtUnixMs, payload.expiresAtUnixMs)
        XCTAssertEqual(decoded.protocolVersion, payload.protocolVersion)
        XCTAssertEqual(decoded.macID, payload.macID)
    }

    func testControllerAssistedPairingCodeRoundTrip() throws {
        let payload = RemotePairingPayload(
            macID: "host-1",
            macName: "Remote Host",
            endpoint: URL(string: "http://192.168.1.20:49152/mobile/pairing-proxy/INVITE-123")!,
            token: "K7ZP2Q4RSTUVWXYZABCDEFGH23",
            expiresAtUnixMs: 1_789_996_900_000
        )

        let code = try XCTUnwrap(RemotePairingCode.encode(payload))
        XCTAssertEqual(
            code,
            "UNPEEL:1:192.168.1.20:49152:HOST-1:K7ZP2Q4RSTUVWXYZABCDEFGH23:1789996900:INVITE-123"
        )
        let decoded = try XCTUnwrap(RemotePairingCode.decode(code))
        XCTAssertEqual(decoded.endpoint, payload.endpoint)
        XCTAssertEqual(decoded.macID, payload.macID)
        XCTAssertEqual(decoded.token, payload.token)
        XCTAssertEqual(decoded.expiresAtUnixMs, payload.expiresAtUnixMs)
    }

    func testPairingEnvelopeRoundTripsAndBindsContext() throws {
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let plaintext = Data("credential-bearing response".utf8)
        let envelope = try RemotePairingCrypto.seal(
            plaintext,
            token: "SCANNED-SECRET",
            macID: "mac-1",
            endpoint: endpoint,
            direction: .response
        )
        XCTAssertEqual(
            try RemotePairingCrypto.open(
                envelope,
                token: "SCANNED-SECRET",
                macID: "mac-1",
                endpoint: endpoint,
                direction: .response
            ),
            plaintext
        )
        XCTAssertThrowsError(try RemotePairingCrypto.open(
            envelope,
            token: "SCANNED-SECRET",
            macID: "mac-2",
            endpoint: endpoint,
            direction: .response
        ))
        XCTAssertThrowsError(try RemotePairingCrypto.open(
            envelope,
            token: "SCANNED-SECRET",
            macID: "mac-1",
            endpoint: endpoint,
            direction: .request
        ))
    }

    func testPairingCodeDecodeRejectsGarbage() {
        XCTAssertNil(RemotePairingCode.decode(""))
        XCTAssertNil(RemotePairingCode.decode("hello world"))
        XCTAssertNil(RemotePairingCode.decode("UNPEEL:1:192.168.1.20:notaport:TOKEN:123"))
        XCTAssertNil(RemotePairingCode.decode("UNPEEL:1:192.168.1.20:49152:TOKEN"))
        XCTAssertNil(RemotePairingCode.decode("OTHER:1:192.168.1.20:49152:TOKEN:123"))
    }

    func testPairingCodeEncodeRefusesUnrepresentableEndpoints() {
        // IPv6 host — colons collide with the field separator.
        let v6 = RemotePairingPayload(
            macID: "mac-1",
            macName: "Mac",
            endpoint: URL(string: "http://[fe80::1]:49152/mobile")!,
            token: "TOKEN",
            certificateFingerprint: nil,
            expiresAtUnixMs: 1_789_996_900_000
        )
        XCTAssertNil(RemotePairingCode.encode(v6))
    }

    func testPairedDeviceSummaryRoundTripsLastSeen() throws {
        let device = RemotePairedDeviceSummary(
            id: "phone-1",
            name: "iPhone",
            platform: "iOS",
            appVersion: "1.0",
            pairedAtUnixMs: 1_789_996_800_000,
            lastSeenAtUnixMs: 1_789_996_860_000
        )

        XCTAssertEqual(try roundTrip(device), device)
    }

    func testTerminalWriteResizeAndCreateResponseRoundTrip() throws {
        let write = RemoteTerminalWriteRequest(
            sessionID: "session-1",
            data: "\u{1B}[A",
            writeID: "write-123"
        )
        let resize = RemoteTerminalResizeRequest(sessionID: "session-1", columns: 120, rows: 42)
        let created = RemoteCreateSessionResponse(
            sessionID: "session-new",
            capturedAtUnixMs: 1_789_996_800_000,
            session: RemoteSessionSummary(
                id: "session-new",
                projectID: "project-1",
                providerID: "opencode",
                title: "opencode",
                command: "opencode",
                createdAtUnixMs: 1_789_996_800_000,
                status: .running,
                activity: .starting
            )
        )

        XCTAssertEqual(try roundTrip(write), write)
        XCTAssertEqual(try roundTrip(resize), resize)
        XCTAssertEqual(try roundTrip(created), created)
    }

    func testTranscriptMarkdownRoundTrip() throws {
        let markdown = RemoteTranscriptMarkdown(
            sessionID: "session-1",
            markdown: "## User\n\nHello\n\n## Assistant\n\nHi!"
        )

        XCTAssertEqual(try roundTrip(markdown), markdown)
    }

    func testResumableArtifactUploadProgressRoundTrips() throws {
        let partial = RemoteArtifactUploadProgress(
            uploadID: "upload-1",
            nextOffset: 262_144,
            complete: false
        )
        let complete = RemoteArtifactUploadProgress(
            uploadID: "upload-1",
            nextOffset: 300_000,
            complete: true,
            path: "/host/session/artifacts/uploads/upload.jpg"
        )

        XCTAssertEqual(try roundTrip(partial), partial)
        XCTAssertEqual(try roundTrip(complete), complete)
    }

    func testCreateSessionRequestRoundTripsInitialPrompt() throws {
        let request = RemoteCreateSessionRequest(
            projectID: "project-1",
            presetID: "claude",
            worktreePath: "/tmp/unpeel-worktree",
            worktreeBranch: "feature/ios-remote",
            initialText: "Review the iOS pairing flow.",
            initialTextSubmitMode: .pasteAndSubmit
        )

        let decoded = try roundTrip(request)

        XCTAssertEqual(decoded, request)
        XCTAssertEqual(decoded.initialTextSubmitMode, .pasteAndSubmit)
    }

    func testSessionSummaryCarriesRemoteControllerState() throws {
        let session = RemoteSessionSummary(
            id: "session-1",
            projectID: "project-1",
            providerID: "codex",
            title: "iOS remote PRD",
            command: "codex",
            createdAtUnixMs: 1_789_996_800_000,
            ownerPrincipalID: "account:alice",
            createdByDeviceID: "phone-1",
            sourcePresetID: "codex-full-auto",
            updatedAtUnixMs: 1_789_996_860_000,
            status: .running,
            activity: .blocked,
            unread: true,
            pinned: true,
            lastOutputPreview: "Permission required",
            latestAlertBody: "Close to the weekly limit",
            latestAlertAtUnixMs: 1_789_996_859_000
        )

        let decoded = try roundTrip(session)

        XCTAssertEqual(decoded, session)
        XCTAssertEqual(decoded.activity, .blocked)
        XCTAssertTrue(decoded.unread)
        XCTAssertEqual(decoded.ownerPrincipalID, "account:alice")
        XCTAssertEqual(decoded.createdByDeviceID, "phone-1")
        XCTAssertEqual(decoded.sourcePresetID, "codex-full-auto")
        XCTAssertEqual(decoded.latestAlertBody, "Close to the weekly limit")
        XCTAssertEqual(decoded.latestAlertAtUnixMs, 1_789_996_859_000)
    }

    func testPendingApprovalPresentsWriteOnKnownTargetOtherwiseCaller() {
        let write = RemotePendingApproval(
            id: "a1",
            kind: "write",
            title: "Allow write?",
            body: "body",
            callerSessionID: "caller",
            targetSessionID: "target",
            requestedAtUnixMs: 1
        )
        XCTAssertEqual(
            write.presentationSessionID(knownIDs: ["caller", "target"]),
            "target"
        )
        XCTAssertEqual(
            write.presentationSessionID(knownIDs: ["caller"]),
            "caller"
        )
        let browser = RemotePendingApproval(
            id: "a2",
            kind: "browser",
            title: "Allow browser?",
            body: "body",
            callerSessionID: "caller",
            requestedAtUnixMs: 1
        )
        XCTAssertEqual(
            browser.presentationSessionID(knownIDs: ["caller"]),
            "caller"
        )
    }

    func testSessionOrganizationPatchRoundTripsPartialFields() throws {
        let patch = RemoteSessionOrganizationPatch(
            sessionID: "session-1",
            title: "Renamed from phone",
            pinned: true
        )

        let decoded = try roundTrip(patch)

        XCTAssertEqual(decoded, patch)
        XCTAssertNil(decoded.archived)

        let pinOnly = try roundTrip(RemoteSessionOrganizationPatch(sessionID: "session-2", pinned: false))
        XCTAssertNil(pinOnly.title)
        XCTAssertEqual(pinOnly.pinned, false)
    }

    func testSessionActionRequestRoundTrips() throws {
        let request = RemoteSessionActionRequest(sessionID: "session-1", action: .remove)

        let decoded = try roundTrip(request)

        XCTAssertEqual(decoded, request)

        let restartAgent = RemoteSessionActionRequest(
            sessionID: "session-2", action: .restartAgent
        )
        XCTAssertEqual(try roundTrip(restartAgent), restartAgent)

        let resumeAgent = RemoteSessionActionRequest(
            sessionID: "session-3", action: .resumeAgent
        )
        XCTAssertEqual(try roundTrip(resumeAgent), resumeAgent)
    }

    func testScreenshotRequestAndAcknowledgementRoundTrip() throws {
        let request = RemoteScreenshotRequest(sessionID: "session-1")
        let response = RemoteScreenshotRequestResponse(requestedAtUnixMs: 1_789_996_800_000)

        XCTAssertEqual(try roundTrip(request), request)
        XCTAssertEqual(try roundTrip(response), response)
        XCTAssertTrue(response.accepted)
    }

    func testSessionSummaryRoundTripsCapabilities() throws {
        let session = RemoteSessionSummary(
            id: "session-1",
            projectID: "project-1",
            runtimeLaunchPending: true,
            providerID: "pi",
            title: "pi task",
            command: "pi",
            createdAtUnixMs: 1_789_996_800_000,
            status: .running,
            activity: .working,
            capabilities: RemoteSessionCapabilities(
                restart: true,
                resumeAgent: true,
                notifyWhenDone: false
            )
        )

        let decoded = try roundTrip(session)

        XCTAssertEqual(decoded, session)
        XCTAssertTrue(decoded.runtimeLaunchPending)
        XCTAssertEqual(decoded.capabilities?.restart, true)
        XCTAssertEqual(decoded.capabilities?.resumeAgent, true)
        XCTAssertNil(decoded.capabilities?.restartAgent)
        XCTAssertEqual(decoded.capabilities?.notifyWhenDone, false)
    }

    func testCapabilitiesDecodeWithoutArchiveDefaultsToFalse() throws {
        // A Mac that predates the archive verb omits the field; the phone
        // must hide Archive rather than offer a silent no-op patch.
        let json = """
        {"restart":true,"fork":false,"appendSystemContext":true,"notifyWhenDone":true}
        """
        let decoded = try JSONDecoder().decode(
            RemoteSessionCapabilities.self, from: Data(json.utf8)
        )
        XCTAssertFalse(decoded.archive)
        XCTAssertNil(decoded.restartAgent)
        XCTAssertNil(decoded.resumeAgent)
    }

    func testCapabilitiesKeepLegacyRestartAgentDecodeOnly() throws {
        let json = """
        {"restart":false,"restartAgent":true,"fork":false,
         "appendSystemContext":false,"notifyWhenDone":true}
        """
        let decoded = try JSONDecoder().decode(
            RemoteSessionCapabilities.self, from: Data(json.utf8)
        )

        XCTAssertEqual(decoded.restartAgent, true)
        XCTAssertNil(decoded.resumeAgent)
    }

    func testSessionSummaryDecodesWithoutCapabilities() throws {
        // A Mac that predates the capabilities field simply omits it; the
        // phone must decode the summary and fall back to permissive defaults.
        let json = """
        {"id":"s1","projectID":"p1","title":"t","command":"claude",
         "createdAtUnixMs":1,"status":"running","activity":"working"}
        """
        let decoded = try JSONDecoder().decode(
            RemoteSessionSummary.self, from: Data(json.utf8)
        )
        XCTAssertFalse(decoded.runtimeLaunchPending)
        XCTAssertNil(decoded.capabilities)
        XCTAssertNil(decoded.activeRuntimeID)
        XCTAssertNil(decoded.ownerPrincipalID)
        XCTAssertNil(decoded.createdByDeviceID)
        XCTAssertNil(decoded.sourcePresetID)
    }

    func testSessionSummaryKeepsActiveRuntimeSeparateFromLegacyProvider() throws {
        let json = """
        {"id":"s1","projectID":"p1","activeRuntimeID":"claude",
         "title":"Shell","command":"","createdAtUnixMs":1,
         "status":"running","activity":"working"}
        """
        let decoded = try JSONDecoder().decode(
            RemoteSessionSummary.self, from: Data(json.utf8)
        )
        XCTAssertEqual(decoded.activeRuntimeID, "claude")
        XCTAssertNil(decoded.providerID)

        let encoded = try JSONEncoder().encode(decoded)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        XCTAssertEqual(object["activeRuntimeID"] as? String, "claude")
        XCTAssertNil(object["providerID"])
    }

    func testViewportFrameRoundTripsStyledCells() throws {
        let frame = RemoteViewportFrame(
            sessionID: "session-1",
            sequence: 42,
            rows: 1,
            columns: 2,
            cells: [
                .init(
                    text: "A",
                    foreground: .rgb(red: 255, green: 255, blue: 255),
                    background: .ansi(4),
                    style: .init(bold: true)
                ),
                .init(text: "界", foreground: .defaultForeground)
            ],
            cursor: .init(row: 0, column: 1, shape: .beam),
            alternateScreen: true,
            capturedAtUnixMs: 1_789_996_800_000
        )

        let decoded = try roundTrip(frame)

        XCTAssertEqual(decoded, frame)
        XCTAssertEqual(decoded.cells.count, 2)
        XCTAssertEqual(decoded.cursor?.shape, .beam)
    }

    func testTranscriptSnapshotRoundTripsSemanticBlocks() throws {
        let snapshot = RemoteTranscriptSnapshot(
            sessionID: "session-1",
            providerID: "opencode",
            source: "provider_session_id",
            resolved: true,
            startOffset: 512,
            nextOffset: 4096,
            entries: [
                RemoteTranscriptEntry(
                    id: "entry-1",
                    sequence: 1,
                    role: .user,
                    text: "hi",
                    createdAtUnixMs: 1_789_996_800_000
                ),
                RemoteTranscriptEntry(
                    id: "entry-2",
                    sequence: 2,
                    role: .assistant,
                    blocks: [
                        RemoteTranscriptBlock(
                            id: "block-1",
                            kind: .reasoning,
                            text: "Thinking through the request."
                        ),
                        RemoteTranscriptBlock(
                            id: "block-2",
                            kind: .toolCall,
                            text: "Read package manifest",
                            toolName: "Read",
                            status: "completed",
                            metadata: ["path": "Package.swift"]
                        ),
                        RemoteTranscriptBlock(
                            id: "block-3",
                            kind: .text,
                            text: "Hi. What should we work on?"
                        ),
                        RemoteTranscriptBlock(
                            id: "block-4",
                            kind: .diff,
                            text: "--- a/App.swift\n+++ b/App.swift\n@@\n-old\n+new",
                            toolName: "apply_patch",
                            status: "success",
                            metadata: ["path": "App.swift", "additions": "1", "deletions": "1"]
                        ),
                        RemoteTranscriptBlock(
                            id: "block-5",
                            kind: .planUpdate,
                            text: "Inspect session files",
                            metadata: ["status": "in_progress"]
                        ),
                    ],
                    createdAtUnixMs: 1_789_996_801_000
                ),
            ],
            updatedAtUnixMs: 1_789_996_802_000
        )

        let decoded = try roundTrip(snapshot)

        XCTAssertEqual(decoded, snapshot)
        XCTAssertEqual(decoded.providerID, "opencode")
        XCTAssertEqual(decoded.startOffset, 512)
        XCTAssertEqual(decoded.nextOffset, 4096)
        XCTAssertEqual(decoded.entries.last?.blocks.first?.kind, .reasoning)
        XCTAssertEqual(decoded.entries.last?.blocks[1].metadata["path"], "Package.swift")
        XCTAssertEqual(decoded.entries.last?.blocks[3].kind, .diff)
        XCTAssertEqual(decoded.entries.last?.blocks[4].kind, .planUpdate)
    }

    func testTranscriptHistoryPageRoundTripsBoundedOffsets() throws {
        let page = RemoteTranscriptHistoryPage(
            sessionID: "session-1",
            providerID: "codex",
            source: "manifest",
            resolved: true,
            startOffset: 256,
            endOffset: 1024,
            truncated: true,
            entries: [
                .init(id: "entry-older", sequence: 256, role: .user, text: "Earlier prompt"),
                .init(id: "entry-newer", sequence: 512, role: .assistant, text: "Earlier answer"),
            ],
            updatedAtUnixMs: 1_789_996_802_000
        )

        let decoded = try roundTrip(page)

        XCTAssertEqual(decoded, page)
        XCTAssertEqual(decoded.startOffset, 256)
        XCTAssertEqual(decoded.endOffset, 1024)
        XCTAssertTrue(decoded.truncated)
    }

    func testTranscriptStreamChunkRoundTripsOffsetsAndPartialLine() throws {
        let chunk = RemoteTranscriptStreamChunk(
            sessionID: "session-1",
            providerID: "codex",
            source: "manifest",
            resolved: true,
            offset: 1024,
            nextOffset: 2048,
            partial: "{\"type\":\"event_msg\"",
            truncated: false,
            entries: [
                .init(id: "entry-1", sequence: 1025, role: .assistant, text: "Working on it.")
            ],
            updatedAtUnixMs: 1_789_996_802_000
        )

        let decoded = try roundTrip(chunk)

        XCTAssertEqual(decoded, chunk)
        XCTAssertEqual(decoded.nextOffset, 2048)
        XCTAssertEqual(decoded.partial, "{\"type\":\"event_msg\"")
    }

    func testViewportPatchRoundTripsChangedRuns() throws {
        let patch = RemoteViewportPatch(
            sessionID: "session-1",
            baseSequence: 42,
            sequence: 43,
            rows: 24,
            columns: 80,
            changedRuns: [
                .init(row: 12, column: 4, cells: [
                    .init(text: "O", foreground: .ansi(2), style: .init(bold: true)),
                    .init(text: "K", foreground: .ansi(2), style: .init(bold: true)),
                ])
            ],
            cursor: .init(row: 12, column: 6),
            capturedAtUnixMs: 1_789_996_900_000
        )

        let decoded = try roundTrip(patch)

        XCTAssertEqual(decoded, patch)
        XCTAssertEqual(decoded.baseSequence, 42)
        XCTAssertEqual(decoded.changedRuns.first?.cells.map(\.text).joined(), "OK")
    }

    func testStreamEventCanCarryEncodedViewportFrame() throws {
        let frame = RemoteViewportFrame(
            sessionID: "session-1",
            sequence: 1,
            rows: 1,
            columns: 1,
            cells: [.init(text: "$")],
            capturedAtUnixMs: 1_789_996_800_000
        )
        let payload = try JSONEncoder().encode(frame)
        let event = RemoteStreamEvent(
            id: "event-1",
            kind: .viewportFrame,
            sessionID: "session-1",
            payload: payload,
            createdAtUnixMs: 1_789_996_800_000
        )

        let decoded = try roundTrip(event)
        let decodedFrame = try JSONDecoder().decode(RemoteViewportFrame.self, from: decoded.payload ?? Data())

        XCTAssertEqual(decoded.kind, .viewportFrame)
        XCTAssertEqual(decodedFrame, frame)
    }

    func testStreamEventCanCarryEncodedTranscriptSnapshot() throws {
        let snapshot = RemoteTranscriptSnapshot(
            sessionID: "session-1",
            providerID: "codex",
            source: "manifest",
            resolved: true,
            entries: [
                .init(id: "entry-1", sequence: 1, role: .assistant, text: "Done.")
            ],
            updatedAtUnixMs: 1_789_996_800_000
        )
        let payload = try JSONEncoder().encode(snapshot)
        let event = RemoteStreamEvent(
            id: "event-1",
            kind: .transcriptSnapshot,
            sessionID: "session-1",
            payload: payload,
            createdAtUnixMs: 1_789_996_800_000
        )

        let decoded = try roundTrip(event)
        let decodedSnapshot = try JSONDecoder().decode(RemoteTranscriptSnapshot.self, from: decoded.payload ?? Data())

        XCTAssertEqual(decoded.kind, .transcriptSnapshot)
        XCTAssertEqual(decodedSnapshot, snapshot)
    }

    private func roundTrip<T: Codable>(_ value: T) throws -> T {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(value)
        return try JSONDecoder().decode(T.self, from: data)
    }

    /// `protocol/<name>` is vendored at the repo root from the pinned server
    /// archive by `apps/native/vendor-protocol.sh` (gitignored);
    /// UNPEEL_PROTOCOL_DIR points at another copy (a server checkout's
    /// `protocol/`, or an extracted archive).
    private func fixtureJSON(_ name: String) throws -> [String: Any] {
        if let override = ProcessInfo.processInfo.environment["UNPEEL_PROTOCOL_DIR"] {
            let data = try Data(contentsOf: URL(fileURLWithPath: override).appendingPathComponent(name))
            return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        }
        var directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        for _ in 0..<10 {
            let candidate = directory
                .appendingPathComponent("protocol")
                .appendingPathComponent(name)
            if FileManager.default.fileExists(atPath: candidate.path) {
                let data = try Data(contentsOf: candidate)
                return try XCTUnwrap(
                    JSONSerialization.jsonObject(with: data) as? [String: Any]
                )
            }
            directory.deleteLastPathComponent()
        }
        XCTFail("could not locate protocol/\(name) from \(#filePath) — run apps/native/vendor-protocol.sh or set UNPEEL_PROTOCOL_DIR")
        return [:]
    }
}
