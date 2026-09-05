import XCTest
@testable import UnpeelNative

@MainActor
final class SessionActivityTests: XCTestCase {
    func testOnlyHookCapableRuntimesHaveActivityAuthority() {
        XCTAssertFalse(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "", activeRuntimeID: nil
        ))
        XCTAssertFalse(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "npm run dev", activeRuntimeID: nil
        ))
        XCTAssertFalse(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "", activeRuntimeID: "unknown.runtime"
        ))

        // Runtime identity is presentation, not lifecycle authority. Pi and
        // fx have no hooks, so their repainting TUIs must never start Busy.
        XCTAssertFalse(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "pi", activeRuntimeID: "pi"
        ))
        XCTAssertFalse(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "fx", activeRuntimeID: "fx"
        ))

        // A hook-capable runtime typed into a blank terminal may acquire
        // authority after its first live hook event.
        XCTAssertTrue(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "", activeRuntimeID: "claude"
        ))
        XCTAssertTrue(SessionActivityEngine.hasHookActivityAuthority(
            launchCommand: "claude", activeRuntimeID: nil
        ))
    }

    func testSharedReadReceiptRefreshOnlyAdvancesPastNewActivity() {
        XCTAssertTrue(UnpeelStore.sharedReadReceiptNeedsRefresh(
            readAt: nil, settledAt: nil
        ))
        XCTAssertFalse(UnpeelStore.sharedReadReceiptNeedsRefresh(
            readAt: 100, settledAt: nil
        ))
        XCTAssertFalse(UnpeelStore.sharedReadReceiptNeedsRefresh(
            readAt: 100, settledAt: 100
        ))
        XCTAssertFalse(UnpeelStore.sharedReadReceiptNeedsRefresh(
            readAt: 101, settledAt: 100
        ))
        XCTAssertTrue(UnpeelStore.sharedReadReceiptNeedsRefresh(
            readAt: 100, settledAt: 101
        ))

        XCTAssertEqual(
            UnpeelStore.latestUnreadActivityAt(lifecycleAt: 90, alertAt: 120),
            120
        )
        XCTAssertEqual(
            UnpeelStore.latestUnreadActivityAt(lifecycleAt: 130, alertAt: 120),
            130
        )
        XCTAssertNil(UnpeelStore.latestUnreadActivityAt(lifecycleAt: nil, alertAt: nil))
    }

    func testHookBusyDeadlineIsRearmedByOutputGrowth() {
        let engine = SessionActivityEngine()
        let start = Date(timeIntervalSince1970: 1_000)

        XCTAssertEqual(
            engine.applyHookEvent(sessionID: "s1", hookEventName: "Start", now: start),
            .busy
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .busy)

        XCTAssertFalse(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 10,
                now: start.addingTimeInterval(295)
            )
        )
        XCTAssertFalse(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 20,
                now: start.addingTimeInterval(299)
            )
        )
        XCTAssertFalse(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 20,
                now: start.addingTimeInterval(590)
            )
        )
        XCTAssertTrue(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 20,
                now: start.addingTimeInterval(600)
            )
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)
    }

    func testDistrustedStopRearmsBusyOnSustainedGrowthOnly() {
        let engine = SessionActivityEngine()
        let start = Date(timeIntervalSince1970: 1_000)

        engine.applyHookEvent(sessionID: "s1", hookEventName: "UserPromptSubmit", now: start)
        engine.noteOutputAndSweep(
            sessionID: "s1", outputSize: 100, distrustStops: true,
            now: start.addingTimeInterval(1)
        )
        engine.applyHookEvent(
            sessionID: "s1", hookEventName: "Stop", now: start.addingTimeInterval(10)
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)

        // The turn's trailing render burst lands inside the grace: stays idle.
        engine.noteOutputAndSweep(
            sessionID: "s1", outputSize: 200, distrustStops: true,
            now: start.addingTimeInterval(12)
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)

        // Sustained growth past the grace re-arms busy (codex mid-run Stop).
        engine.noteOutputAndSweep(
            sessionID: "s1", outputSize: 300, distrustStops: true,
            now: start.addingTimeInterval(17)
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .busy)

        // Growth outside the window (a later scroll repaint) never re-arms.
        engine.applyHookEvent(
            sessionID: "s1", hookEventName: "Stop", now: start.addingTimeInterval(30)
        )
        engine.noteOutputAndSweep(
            sessionID: "s1", outputSize: 300, distrustStops: true,
            now: start.addingTimeInterval(31)
        )
        engine.noteOutputAndSweep(
            sessionID: "s1", outputSize: 400, distrustStops: true,
            now: start.addingTimeInterval(200)
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)

        // Providers without the guard keep the strict hook latch.
        engine.applyHookEvent(sessionID: "s2", hookEventName: "Stop", now: start)
        engine.noteOutputAndSweep(
            sessionID: "s2", outputSize: 100, now: start.addingTimeInterval(1)
        )
        engine.noteOutputAndSweep(
            sessionID: "s2", outputSize: 200, now: start.addingTimeInterval(10)
        )
        XCTAssertEqual(engine.hookOwnedState("s2"), .idle)
    }

    func testPermissionAttentionRequiresPostBaselineOutputGrowthToClear() {
        let engine = SessionActivityEngine()
        let start = Date(timeIntervalSince1970: 1_000)

        engine.applyHookEvent(sessionID: "s1", hookEventName: "PermissionRequest", now: start)
        XCTAssertEqual(engine.hookOwnedState("s1"), .attention)

        XCTAssertFalse(
            engine.noteOutputAndSweep(sessionID: "s1", outputSize: 100, now: start)
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .attention)

        XCTAssertFalse(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 120,
                now: start.addingTimeInterval(1)
            )
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .busy)
    }

    func testLatchOnlyPermissionEstablishesIdleHookOwnership() {
        let engine = SessionActivityEngine()

        XCTAssertEqual(
            engine.applyHookEvent(
                sessionID: "s1",
                hookEventName: "PermissionRequest",
                latchOnly: true
            ),
            .none
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)
    }

    func testUnreadReconciliationMarksCompletedUnobservedSessionWhenItSettles() {
        let first = UnreadReconciliation.reconcile(
            pendingUnreadSessions: [],
            sessionStates: ["old": .busy, "new": .idle],
            completedSessionIDs: [],
            previousObservedSessionID: "old",
            currentObservedSessionID: "new"
        )

        XCTAssertEqual(first.pendingUnreadSessions, ["old"])
        XCTAssertEqual(first.unreadToClear, ["new"])
        XCTAssertEqual(first.unreadToMark, [])

        let second = UnreadReconciliation.reconcile(
            pendingUnreadSessions: first.pendingUnreadSessions,
            sessionStates: ["old": .idle, "new": .idle],
            completedSessionIDs: ["old"],
            previousObservedSessionID: "new",
            currentObservedSessionID: "new"
        )

        XCTAssertEqual(second.pendingUnreadSessions, [])
        XCTAssertEqual(second.unreadToClear, ["new"])
        XCTAssertEqual(second.unreadToMark, ["old"])
    }

    // Leak-regression: retainSessions must drop entries for sessions that are no
    // longer live, so the per-session activity map can't accumulate across a
    // long-running app's lifetime.
    func testRetainSessionsDropsNonLiveEntries() {
        let engine = SessionActivityEngine()
        let now = Date(timeIntervalSince1970: 1_000)
        engine.applyHookEvent(sessionID: "a", hookEventName: "Start", now: now)
        engine.applyHookEvent(sessionID: "b", hookEventName: "Start", now: now)
        engine.applyHookEvent(sessionID: "c", hookEventName: "Start", now: now)
        XCTAssertEqual(Set(engine.entries.keys), ["a", "b", "c"])

        engine.retainSessions(["a"])
        XCTAssertEqual(Set(engine.entries.keys), ["a"])

        // Retaining an empty live set clears the map entirely.
        engine.retainSessions([])
        XCTAssertTrue(engine.entries.isEmpty)
    }

    func testRuntimeGenerationResetUsesExactOrTurnOpeningProvenance() {
        let engine = SessionActivityEngine()
        let launch = Date(timeIntervalSince1970: 2_000)

        engine.applyHookEvent(
            sessionID: "old", hookEventName: "Stop",
            runtimeGeneration: 1,
            now: launch.addingTimeInterval(1)
        )
        XCTAssertNil(engine.resetForRuntimeLaunch(
            "old", runtimeGeneration: 2, launchedAt: launch
        ))
        XCTAssertNil(engine.hookOwnedState("old"))

        engine.applyHookEvent(
            sessionID: "new", hookEventName: "UserPromptSubmit",
            runtimeGeneration: 2,
            now: launch.addingTimeInterval(0.001)
        )
        XCTAssertEqual(engine.resetForRuntimeLaunch(
            "new", runtimeGeneration: 2, launchedAt: launch
        ), false)
        XCTAssertEqual(engine.hookOwnedState("new"), .busy)
        XCTAssertTrue(engine.hasRuntimeOwnership("new", generation: 2))

        // Legacy replacement hooks can bind to generation two only through a
        // turn opener after the Host's launch boundary. The following Stop
        // then belongs to that same proven turn.
        engine.applyHookEvent(
            sessionID: "legacy-new", hookEventName: "UserPromptSubmit",
            now: launch.addingTimeInterval(0.001)
        )
        engine.applyHookEvent(
            sessionID: "legacy-new", hookEventName: "Stop",
            now: launch.addingTimeInterval(0.002)
        )
        XCTAssertEqual(engine.resetForRuntimeLaunch(
            "legacy-new", runtimeGeneration: 2, launchedAt: launch
        ), true)
        XCTAssertEqual(engine.hookOwnedState("legacy-new"), .idle)
        XCTAssertTrue(engine.hasRuntimeOwnership("legacy-new", generation: 2))

        // Before manifest commit, the replacement's untagged opener can be
        // delivered first and the old runtime's tagged Stop second. The edge
        // discards the tagged old event and restores the new busy turn.
        engine.applyHookEvent(
            sessionID: "precommit-order", hookEventName: "Start",
            now: launch.addingTimeInterval(0.001)
        )
        engine.applyHookEvent(
            sessionID: "precommit-order", hookEventName: "Stop",
            runtimeGeneration: 1,
            now: launch.addingTimeInterval(0.002)
        )
        XCTAssertEqual(engine.resetForRuntimeLaunch(
            "precommit-order", runtimeGeneration: 2, launchedAt: launch
        ), false)
        XCTAssertEqual(engine.hookOwnedState("precommit-order"), .busy)
        XCTAssertTrue(engine.hasRuntimeOwnership("precommit-order", generation: 2))

        // A late unversioned Stop from the departing process cannot establish
        // ownership merely because its socket receipt happened after commit.
        engine.applyHookEvent(
            sessionID: "late-stop", hookEventName: "Stop",
            now: launch.addingTimeInterval(0.002)
        )
        XCTAssertNil(engine.resetForRuntimeLaunch(
            "late-stop", runtimeGeneration: 2, launchedAt: launch
        ))
        XCTAssertNil(engine.hookOwnedState("late-stop"))

        // A latch-only new SessionStart must not inherit the old generation's
        // attention state or establish turn ownership.
        engine.applyHookEvent(
            sessionID: "seen", hookEventName: "PermissionRequest",
            now: launch.addingTimeInterval(-1)
        )
        engine.applyHookEvent(
            sessionID: "seen", hookEventName: "HookSeen", latchOnly: true,
            now: launch.addingTimeInterval(0.001)
        )
        XCTAssertEqual(engine.hookOwnedState("seen"), .attention)
        XCTAssertNil(engine.resetForRuntimeLaunch(
            "seen", runtimeGeneration: 2, launchedAt: launch
        ))
        XCTAssertNil(engine.hookOwnedState("seen"))
    }

    func testRuntimeGenerationResetWithoutLaunchStampFailsClosed() {
        let engine = SessionActivityEngine()
        engine.applyHookEvent(
            sessionID: "legacy", hookEventName: "PermissionRequest",
            now: Date(timeIntervalSince1970: 2_000)
        )

        XCTAssertNil(engine.resetForRuntimeLaunch(
            "legacy", runtimeGeneration: 2, launchedAt: nil
        ))
        XCTAssertNil(engine.hookOwnedState("legacy"))
    }

    // MARK: - LastHookEvent (last-hook-event.json seeding)

    private func parseLastHookEvent(_ json: String) -> LastHookEvent? {
        LastHookEvent.parse(Data(json.utf8))
    }

    func testLastHookEventParsesLifecycleEvents() {
        let start = parseLastHookEvent(#"{"hook_event_name":"UserPromptSubmit"}"#)
        XCTAssertEqual(start?.hookEventName, "UserPromptSubmit")
        XCTAssertEqual(start?.latchOnly, false)

        let stop = parseLastHookEvent(#"{"hook_event_name":"Stop"}"#)
        XCTAssertEqual(stop?.hookEventName, "Stop")
        XCTAssertEqual(stop?.latchOnly, false)

        let owned = parseLastHookEvent(
            #"{"hook_event_name":"Stop","unpeel_runtime_generation":7}"#
        )
        XCTAssertEqual(owned?.runtimeGeneration, 7)

        let permission = parseLastHookEvent(
            #"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#
        )
        XCTAssertEqual(permission?.hookEventName, "PermissionRequest")
        XCTAssertEqual(permission?.latchOnly, false)
    }

    func testLastHookEventNormalizesProviderEventNames() {
        // Scripts record provider-native names (Claude registers the raw hook
        // names); the seed must run them through the same normalization as
        // the live hook server.
        let start = parseLastHookEvent(#"{"hook_event_name":"session_start"}"#)
        XCTAssertEqual(start?.hookEventName, "HookSeen")
        XCTAssertEqual(start?.latchOnly, true)
    }

    func testLastHookEventAskUserQuestionAndUnknownEventsAreLatchOnly() {
        let ask = parseLastHookEvent(
            #"{"hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion"}"#
        )
        XCTAssertEqual(ask?.latchOnly, true)

        let unknown = parseLastHookEvent(#"{"hook_event_name":"PreCompact"}"#)
        XCTAssertEqual(unknown?.hookEventName, "PreCompact")
        XCTAssertEqual(unknown?.latchOnly, true)
    }

    func testLastHookEventRejectsMalformedPayloads() {
        XCTAssertNil(parseLastHookEvent("not json"))
        XCTAssertNil(parseLastHookEvent("{}"))
        XCTAssertNil(parseLastHookEvent(#"{"hook_event_name":""}"#))
    }

    // Only turn-opening events may anchor their seed on output.bin recency;
    // a recorded Stop/attention must never be revived by TUI repaints.
    func testLastHookEventStartsTurnOnlyForTurnOpeningEvents() {
        XCTAssertEqual(
            parseLastHookEvent(#"{"hook_event_name":"Start"}"#)?.startsTurn, true
        )
        XCTAssertEqual(
            parseLastHookEvent(#"{"hook_event_name":"UserPromptSubmit"}"#)?.startsTurn, true
        )
        XCTAssertEqual(
            parseLastHookEvent(#"{"hook_event_name":"Stop"}"#)?.startsTurn, false
        )
        XCTAssertEqual(
            parseLastHookEvent(
                #"{"hook_event_name":"PermissionRequest","tool_name":"Bash"}"#
            )?.startsTurn,
            false
        )
        XCTAssertEqual(
            parseLastHookEvent(#"{"hook_event_name":"PreCompact"}"#)?.startsTurn, false
        )
    }

    func testLastHookEventCanAvoidOutputAnchoringForLegacyStartEvents() {
        let legacyStart = parseLastHookEvent(#"{"hook_event_name":"Start"}"#)
        XCTAssertEqual(
            legacyStart?.shouldAnchorSeedToOutput(anchorStartEventToOutput: true),
            true
        )
        XCTAssertEqual(
            legacyStart?.shouldAnchorSeedToOutput(anchorStartEventToOutput: false),
            false
        )

        let promptSubmit = parseLastHookEvent(#"{"hook_event_name":"UserPromptSubmit"}"#)
        XCTAssertEqual(
            promptSubmit?.shouldAnchorSeedToOutput(anchorStartEventToOutput: false),
            true
        )
    }

    // Seeding a stale busy event at its file mtime must expire through the
    // ordinary 5-minute idle timeout on the next sweep instead of showing a
    // permanent spinner.
    func testSeededBusyEventOlderThanTimeoutExpiresOnFirstSweep() {
        let engine = SessionActivityEngine()
        let mtime = Date(timeIntervalSince1970: 1_000)

        engine.applyHookEvent(sessionID: "s1", hookEventName: "Start", now: mtime)
        XCTAssertEqual(engine.hookOwnedState("s1"), .busy)

        XCTAssertTrue(
            engine.noteOutputAndSweep(
                sessionID: "s1",
                outputSize: 10,
                now: mtime.addingTimeInterval(600)
            )
        )
        XCTAssertEqual(engine.hookOwnedState("s1"), .idle)
    }
}
