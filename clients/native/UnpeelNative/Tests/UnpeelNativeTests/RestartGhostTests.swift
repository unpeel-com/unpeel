import XCTest
@testable import UnpeelNative

/// Tests for `UnpeelStore.supersededRestartGhostIDs` — the rescan pass that
/// removes the dead pre-restart session that lingers as a greyed duplicate when
/// a slow provider flushes its final `state=exited` manifest after restart's
/// teardown sweep. Restart copies `created_at` exactly, so an exited session
/// sharing (project, created_at) with a live one is the leftover.
final class RestartGhostTests: XCTestCase {
    private typealias Candidate = UnpeelStore.RestartGhostCandidate

    func testExitedTwinWithLiveReplacementIsGhost() {
        // The reported case: parent's child restarted; old (exited) + new (live)
        // share created_at. The exited one is the ghost.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "old", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "new", projectID: "p", createdAt: 1000, isLive: true),
        ])
        XCTAssertEqual(ghosts, ["old"])
    }

    func testLiveSessionIsNeverAGhost() {
        // Only the exited leftover is removed; the live replacement stays.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "old", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "new", projectID: "p", createdAt: 1000, isLive: true),
        ])
        XCTAssertFalse(ghosts.contains("new"))
    }

    func testExitedSessionWithNoLiveTwinIsKept() {
        // A plain finished session (no live twin) is history, not a ghost — it
        // must survive so "Remove from list" stays the user's call.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "done", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "other", projectID: "p", createdAt: 2000, isLive: true),
        ])
        XCTAssertTrue(ghosts.isEmpty)
    }

    func testTwoExitedSessionsWithSameCreatedAtAreKept() {
        // Both dead → no live writer to defer to; leave them (conservative).
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "a", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "b", projectID: "p", createdAt: 1000, isLive: false),
        ])
        XCTAssertTrue(ghosts.isEmpty)
    }

    func testDifferentProjectSameCreatedAtIsNotAGhost() {
        // created_at collisions across unrelated projects must not cross-match.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "exited-a", projectID: "a", createdAt: 1000, isLive: false),
            Candidate(id: "live-b", projectID: "b", createdAt: 1000, isLive: true),
        ])
        XCTAssertTrue(ghosts.isEmpty)
    }

    func testZeroCreatedAtNeverGroups() {
        // Timestamp-less manifests (created_at 0) must never be treated as a
        // restart pair, even if several coincide.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "x", projectID: "p", createdAt: 0, isLive: false),
            Candidate(id: "y", projectID: "p", createdAt: 0, isLive: true),
            Candidate(id: "z", projectID: "p", createdAt: 0, isLive: false),
        ])
        XCTAssertTrue(ghosts.isEmpty)
    }

    func testDistinctCreationTimesKeepBothSessions() {
        // Independent sessions with distinct creation times never collide.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "source", projectID: "p", createdAt: 1000, isLive: true),
            Candidate(id: "other", projectID: "p", createdAt: 2000, isLive: true),
        ])
        XCTAssertTrue(ghosts.isEmpty)
    }

    func testMultipleRestartPairsEachResolveIndependently() {
        // The exact screenshot: two children each restarted once. Both old
        // instances are ghosts; both live replacements survive.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "codex-old", projectID: "unpeel", createdAt: 1783511375479, isLive: false),
            Candidate(id: "codex-new", projectID: "unpeel", createdAt: 1783511375479, isLive: true),
            Candidate(id: "claude-old", projectID: "unpeel", createdAt: 1783502687475, isLive: false),
            Candidate(id: "claude-new", projectID: "unpeel", createdAt: 1783502687475, isLive: true),
            // An unrelated live session must be untouched.
            Candidate(id: "bystander", projectID: "unpeel", createdAt: 1783500000000, isLive: true),
        ])
        XCTAssertEqual(ghosts, ["codex-old", "claude-old"])
    }

    func testDoubleRestartLeavesOnlyTheLiveOne() {
        // Restarted twice: two exited instances share created_at with one live
        // replacement. Both dead ones are ghosts.
        let ghosts = UnpeelStore.supersededRestartGhostIDs([
            Candidate(id: "gen1", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "gen2", projectID: "p", createdAt: 1000, isLive: false),
            Candidate(id: "gen3", projectID: "p", createdAt: 1000, isLive: true),
        ])
        XCTAssertEqual(ghosts, ["gen1", "gen2"])
    }
}
