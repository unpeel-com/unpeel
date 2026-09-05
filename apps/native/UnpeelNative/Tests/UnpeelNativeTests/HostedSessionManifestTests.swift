import XCTest
@testable import UnpeelNative

final class HostedSessionManifestTests: XCTestCase {
    func testLifecycleLockPathAndLeaseMatchRustContract() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-lifecycle-lock-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let expected = root
            .appendingPathComponent("session-lifecycle-locks")
            .appendingPathComponent(
                "84c3933eeeef7825a674d522484afcb039087bf69fe534082b6ba70fdef25c82.lock"
            )
        XCTAssertEqual(
            UnpeelStore.sessionLifecycleLockURL(
                unpeelDir: root,
                sessionID: "session/../one"
            ),
            expected
        )

        let first = try XCTUnwrap(UnpeelStore.acquireSessionLifecycleLease(
            unpeelDir: root,
            sessionID: "session/../one"
        ))
        XCTAssertNil(UnpeelStore.acquireSessionLifecycleLease(
            unpeelDir: root,
            sessionID: "session/../one"
        ), "a second process/file description must not enter concurrently")
        first.release()
        XCTAssertNotNil(UnpeelStore.acquireSessionLifecycleLease(
            unpeelDir: root,
            sessionID: "session/../one"
        ))
    }

    func testReplacementResumeIsStoppedOnlyButReloadCanReplaceLiveTerminal() {
        XCTAssertTrue(UnpeelStore.replacementRestartAllowsState(
            "exited", stoppedOnly: true,
            childProcessExists: true, pidIdentity: .matches
        ))
        XCTAssertFalse(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: true,
            childProcessExists: true, pidIdentity: .matches
        ))
        XCTAssertFalse(UnpeelStore.replacementRestartAllowsState(
            nil, stoppedOnly: true,
            childProcessExists: false, pidIdentity: .notOurs
        ))
        XCTAssertTrue(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: false,
            childProcessExists: true, pidIdentity: .matches
        ))
    }

    func testReplacementResumeAcceptsDefinitivelyCrashedOrRecycledRunningManifest() {
        XCTAssertTrue(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: true,
            childProcessExists: false, pidIdentity: .unknown
        ), "an absent child is a crashed Host, even before its final manifest write")
        XCTAssertTrue(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: true,
            childProcessExists: true, pidIdentity: .notOurs
        ), "a recycled pid is not the Session child and must never be signalled")
        XCTAssertFalse(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: true,
            childProcessExists: true, pidIdentity: .unknown
        ))
        XCTAssertFalse(UnpeelStore.replacementRestartAllowsState(
            "running", stoppedOnly: true,
            childProcessExists: nil, pidIdentity: .unknown
        ))
    }

    func testDecodesProviderSessionIDFromManifest() throws {
        let json = """
        {
          "session": {
            "id": "unpeel-session",
            "project_id": "project",
            "label": "Codex",
            "command": "codex --dangerously-bypass-approvals-and-sandbox",
            "created_at": 1783511375479,
            "custom_title": false,
            "worktree_path": null,
            "worktree_branch": null,
            "parent_session_id": null,
            "spawned_by": null,
            "role": null,
            "task": null
          },
          "state": "running",
          "updated_at": 1783511380000,
          "pid": 123,
          "host_build_id": "host",
          "host_protocol_version": 1,
          "has_been_written_to": true,
          "provider_session_id": "019f41a7-20bd-74a2-bf4a-a838c0972cce",
          "provider_transcript_path": "/Users/test/.codex/sessions/session.jsonl",
          "managed_storage_path": "/Users/test/.unpeel/runtime-storage/session",
          "resume_failure_markers": ["missing", "019f41a7"],
          "runtime_launch_generation": 3,
          "runtime_launched_at": 1783511379000,
          "runtime_launch_output_offset": 4096,
          "mcp_client_registered": true,
          "browser_client_registered": true,
          "menu_prompt_active": false
        }
        """

        let manifest = try JSONDecoder().decode(
            HostedSessionManifest.self,
            from: Data(json.utf8)
        )

        XCTAssertEqual(manifest.providerSessionID, "019f41a7-20bd-74a2-bf4a-a838c0972cce")
        XCTAssertEqual(
            manifest.providerTranscriptPath,
            "/Users/test/.codex/sessions/session.jsonl"
        )
        XCTAssertEqual(
            manifest.managedStoragePath,
            "/Users/test/.unpeel/runtime-storage/session"
        )
        XCTAssertEqual(manifest.resumeFailureMarkers, ["missing", "019f41a7"])
        XCTAssertTrue(manifest.hasBeenWrittenTo)
        XCTAssertEqual(manifest.updatedAt, 1_783_511_380_000)
        XCTAssertEqual(manifest.runtimeLaunchGeneration, 3)
        XCTAssertEqual(manifest.runtimeLaunchedAt, 1_783_511_379_000)
        XCTAssertEqual(manifest.runtimeLaunchOutputOffset, 4096)
    }

    func testDurableResumeEvidenceAcceptsARealProviderTranscript() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-resume-evidence-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true
        )
        let transcript = directory.appendingPathComponent("conversation.jsonl")
        try Data("{}\n".utf8).write(to: transcript)
        let marker = try JSONSerialization.data(withJSONObject: [
            "provider_session_id": "provider-session",
            "provider_transcript_path": transcript.path,
        ])
        try marker.write(to: directory.appendingPathComponent("provider-session.json"))

        let manifest = try JSONDecoder().decode(
            HostedSessionManifest.self,
            from: Data("""
            {
              "session": {
                "id": "unpeel-session",
                "project_id": "project",
                "command": "codex"
              },
              "state": "running"
            }
            """.utf8)
        )

        XCTAssertTrue(UnpeelStore.hasDurableResumeEvidence(
            manifest: manifest, dirPath: directory.path
        ))
        try FileManager.default.removeItem(at: transcript)
        XCTAssertFalse(UnpeelStore.hasDurableResumeEvidence(
            manifest: manifest, dirPath: directory.path
        ), "a provider id and missing transcript are not enough")
        try FileManager.default.createDirectory(at: transcript, withIntermediateDirectories: true)
        XCTAssertFalse(UnpeelStore.hasDurableResumeEvidence(
            manifest: manifest, dirPath: directory.path
        ), "a provider transcript path must name a real non-empty file")

        try Data(#"{"hook_event_name":"Start"}"#.utf8).write(
            to: directory.appendingPathComponent("last-hook-event.json")
        )
        XCTAssertTrue(UnpeelStore.hasDurableResumeEvidence(
            manifest: manifest, dirPath: directory.path
        ), "a real provider lifecycle event remains valid fallback evidence")
    }

    func testMissingUpdatedAtDecodesAsLegacyZero() throws {
        let json = """
        {
          "session": { "id": "legacy", "project_id": "project" },
          "state": "exited"
        }
        """

        let manifest = try JSONDecoder().decode(
            HostedSessionManifest.self,
            from: Data(json.utf8)
        )

        XCTAssertEqual(manifest.updatedAt, 0)
        XCTAssertNil(manifest.runtime)
        XCTAssertEqual(manifest.runtimeLaunchGeneration, 0)
        XCTAssertNil(manifest.runtimeLaunchedAt)
        XCTAssertEqual(manifest.runtimeLaunchOutputOffset, 0)
        XCTAssertNil(manifest.managedStoragePath)
        XCTAssertEqual(manifest.resumeFailureMarkers, [])
    }

    func testDecodesLiveRuntimeWithoutChangingBlankLaunchPresentationContract() throws {
        let json = """
        {
          "session": {
            "id": "blank-shell",
            "project_id": "project",
            "command": ""
          },
          "state": "running",
          "runtime": {
            "currentObservation": {
              "id": "claude",
              "detectionSource": "live-process"
            }
          }
        }
        """

        let manifest = try JSONDecoder().decode(
            HostedSessionManifest.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(manifest.runtime?.currentObservation?.id, "claude")

        var entry = SessionEntry(
            id: "blank-shell",
            projectID: "project",
            label: "Terminal",
            command: "",
            createdAt: 1,
            status: .busy,
            activeRuntimeID: manifest.runtime?.currentObservation?.id
        )
        XCTAssertEqual(entry.command, "")
        XCTAssertEqual(entry.presentationCommand, "claude")

        entry.status = .exited
        XCTAssertEqual(entry.presentationCommand, "")
    }

    func testRuntimeLaunchOutputWindowExcludesPreservedScrollback() throws {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-runtime-output-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: url) }
        try Data("old conversation-not-found marker\nnew launch output".utf8).write(to: url)

        let offset = UInt64("old conversation-not-found marker\n".utf8.count)
        let window = try XCTUnwrap(UnpeelStore.readFileWindow(
            url, fromOffset: offset, maxBytes: 8_192
        ))
        XCTAssertEqual(String(decoding: window, as: UTF8.self), "new launch output")
    }

    func testResumeAgentFailureClassifierKeepsInfrastructureFailuresAt500() {
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "Refusing to restart claude: terminal foreground is codex"
        ), 409)
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "Agent restart generation changed (expected 2, current 3)"
        ), 409)
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "Refusing to resume claude: the agent is still running"
        ), 409)
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "session live host does not support shell-only agent resume"
        ), 409)
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "agent restart failed: Agent restart generation changed (expected 2, current 3)"
        ), 409)
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "Failed to submit agent relaunch command: broken pipe"
        ), 500)
        XCTAssertEqual(UnpeelStore.normalizedResumeAgentFailureMessage(
            "agent restart failed: Refusing to restart claude: terminal foreground is codex\n"
        ), "Refusing to restart claude: terminal foreground is codex")
        XCTAssertEqual(UnpeelStore.normalizedResumeAgentFailureMessage(
            "agent resume failed: Refusing to resume claude: the agent is still running\n"
        ), "Refusing to resume claude: the agent is still running")
        XCTAssertEqual(UnpeelStore.resumeAgentFailureHTTPStatus(
            "Agent relaunched, but its session manifest could not be updated: disk full"
        ), 500)
    }

    func testQueuedOldHookReceiptPredatesCommittedRuntimeLaunch() {
        let launch = Date(timeIntervalSince1970: 2_000)
        XCTAssertTrue(UnpeelStore.hookReceiptPredatesRuntimeLaunch(
            receivedAt: launch.addingTimeInterval(-0.001),
            launchedAt: launch
        ))
        XCTAssertFalse(UnpeelStore.hookReceiptPredatesRuntimeLaunch(
            receivedAt: launch,
            launchedAt: launch
        ))
        XCTAssertFalse(UnpeelStore.hookReceiptPredatesRuntimeLaunch(
            receivedAt: launch.addingTimeInterval(0.001),
            launchedAt: launch
        ))
    }

    func testDeferredStopEffectsRequireUnchangedRuntimeGeneration() {
        XCTAssertTrue(UnpeelStore.shouldPublishDeferredStopEffects(
            observedGeneration: 4,
            currentGeneration: 4
        ))
        XCTAssertFalse(UnpeelStore.shouldPublishDeferredStopEffects(
            observedGeneration: 4,
            currentGeneration: 5
        ))
        // Legacy manifests decode a concrete zero, but keep the pure helper
        // conservative and internally consistent when neither read succeeded.
        XCTAssertTrue(UnpeelStore.shouldPublishDeferredStopEffects(
            observedGeneration: nil,
            currentGeneration: nil
        ))
        XCTAssertFalse(UnpeelStore.shouldPublishDeferredStopEffects(
            observedGeneration: 4,
            currentGeneration: nil
        ))
    }

    func testPostCommitDepartingStopCannotOwnReplacementGeneration() {
        let launch = Date(timeIntervalSince1970: 2_000)

        // Generation provenance rejects the old process even when its Stop
        // POST itself lands after generation two committed.
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: 1,
            hookEventName: "Stop",
            receivedAt: launch.addingTimeInterval(3_600),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: false
        ), .reject)

        // A legacy hook has no exact provenance. After an in-place edge it
        // cannot complete a turn until the replacement established ownership.
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: nil,
            hookEventName: "StopFailure",
            receivedAt: launch.addingTimeInterval(29.999),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: false
        ), .reject)

        // Permanently old hook installs get a bounded compatibility escape:
        // exactly at 30 seconds a lone untagged Stop may settle. A tagged old
        // Stop above remains rejected forever.
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: nil,
            hookEventName: "Stop",
            receivedAt: launch.addingTimeInterval(30),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: false
        ), .accept(effectiveGeneration: 2))
    }

    func testFastReplacementHookMakesFollowingOldHookStaleBeforeManifestRescan() {
        let launch = Date(timeIntervalSince1970: 2_000)
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: 2,
            hookEventName: "Start",
            receivedAt: launch,
            currentGeneration: 1,
            runtimeLaunchedAt: launch.addingTimeInterval(-100),
            currentGenerationOwned: false
        ), .accept(effectiveGeneration: 2))
        // The caller promotes the accepted generation into its activity
        // watermark. Even if manifest.json still says one, the delayed
        // departing Stop is now unambiguously stale.
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: 1,
            hookEventName: "Stop",
            receivedAt: launch.addingTimeInterval(0.001),
            currentGeneration: 2,
            runtimeLaunchedAt: launch.addingTimeInterval(-100),
            currentGenerationOwned: true
        ), .reject)
    }

    func testGenuineReplacementStartThenStopRetainsLegacyCompatibility() {
        let launch = Date(timeIntervalSince1970: 2_000)
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: nil,
            hookEventName: "UserPromptSubmit",
            receivedAt: launch.addingTimeInterval(0.010),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: false
        ), .accept(effectiveGeneration: 2))
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: nil,
            hookEventName: "Stop",
            receivedAt: launch.addingTimeInterval(0.020),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: true
        ), .accept(effectiveGeneration: 2))

        // Current owned assets do not need the legacy opener heuristic.
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: 2,
            hookEventName: "Stop",
            receivedAt: launch.addingTimeInterval(0.001),
            currentGeneration: 2,
            runtimeLaunchedAt: launch,
            currentGenerationOwned: false
        ), .accept(effectiveGeneration: 2))
    }

    func testStaleDurableHookSeedGenerationIsRejected() throws {
        let seed = try XCTUnwrap(LastHookEvent.parse(Data(
            #"{"hook_event_name":"Stop","unpeel_runtime_generation":3}"#.utf8
        )))
        XCTAssertEqual(seed.runtimeGeneration, 3)
        XCTAssertEqual(UnpeelStore.hookRuntimeDecision(
            eventGeneration: seed.runtimeGeneration,
            hookEventName: seed.hookEventName,
            receivedAt: Date(timeIntervalSince1970: 3_001),
            currentGeneration: 4,
            runtimeLaunchedAt: Date(timeIntervalSince1970: 3_000),
            currentGenerationOwned: false
        ), .reject)
    }
}
