import XCTest
@testable import UnpeelNative

@MainActor
final class SidebarSessionOrderTests: XCTestCase {
    private func pin(_ id: String, at: UInt64) -> PinnedSidebarSession {
        PinnedSidebarSession(
            key: PinnedSidebarSession.key(forSessionID: id),
            projectID: "project",
            sessionID: id,
            pinnedAt: at
        )
    }

    func testSharedPinRanksWinAndUnrankedPinsKeepBaseOrder() {
        let base = [pin("a", at: 1), pin("b", at: 2), pin("c", at: 3)]

        let ordered = UnpeelStore.orderedPinnedSessions(
            base,
            sharedOrder: ["regular", "c", "a"],
            localOrder: ["b", "a", "c"]
        )

        XCTAssertEqual(ordered.compactMap(\.sessionID), ["c", "a", "b"])
    }

    func testLegacyPinOrderRemainsFallbackWhenSharedListHasNoPins() {
        let base = [pin("a", at: 1), pin("b", at: 2), pin("c", at: 3)]

        let ordered = UnpeelStore.orderedPinnedSessions(
            base,
            sharedOrder: ["regular-a", "regular-b"],
            localOrder: ["b", "a"]
        )

        XCTAssertEqual(ordered.compactMap(\.sessionID), ["b", "a", "c"])
    }

    func testPinnedGroupRecordsRankByTheirProjectID() {
        let group = PinnedSidebarSession(
            key: PinnedSidebarSession.key(forProjectID: "ideas"),
            projectID: "project",
            sessionID: nil,
            pinnedAt: 2
        )
        XCTAssertEqual(group.pinnedProjectID, "ideas")
        XCTAssertEqual(group.orderTargetID, "ideas")

        let ordered = UnpeelStore.orderedPinnedSessions(
            [pin("a", at: 1), group, pin("b", at: 3)],
            sharedOrder: ["b", "ideas", "a"],
            localOrder: nil
        )

        XCTAssertEqual(ordered.map(\.key), [
            PinnedSidebarSession.key(forSessionID: "b"),
            PinnedSidebarSession.key(forProjectID: "ideas"),
            PinnedSidebarSession.key(forSessionID: "a"),
        ])
    }

    func testPinnedRecordOrderRewriteReordersSynthesizesAndKeepsUnrankedRows() {
        var object: [String: Any] = [
            "pinned_sessions": [
                "project": [
                    [
                        "key": "session:a", "project_id": "project",
                        "session_id": "a", "pinned_at": 1, "extra": "kept",
                    ],
                    [
                        "key": "project:ideas", "project_id": "project",
                        "pinned_at": 2,
                    ],
                    [
                        "key": "session:archived", "project_id": "project",
                        "session_id": "archived", "pinned_at": 3,
                    ],
                ],
                "other": [
                    ["key": "session:x", "project_id": "other",
                     "session_id": "x", "pinned_at": 9],
                ],
            ],
        ]

        // "tui-group" has the pinned_at marker but no record yet: the
        // rewrite synthesizes its ordering row at the dragged rank.
        XCTAssertTrue(UnpeelStore.applyPinnedRecordOrder(
            ["ideas", "tui-group", "a"],
            projectID: "project",
            groupIDs: ["ideas", "tui-group"],
            newRecordStamp: 42,
            to: &object
        ))

        let grouped = object["pinned_sessions"] as? [String: [[String: Any]]]
        let rows = grouped?["project"] ?? []
        XCTAssertEqual(
            rows.map { $0["key"] as? String },
            ["project:ideas", "project:tui-group", "session:a", "session:archived"],
            "ranked rows in the new order, the archived pin retained below"
        )
        XCTAssertEqual(
            rows.first { ($0["key"] as? String) == "session:a" }?["extra"] as? String,
            "kept",
            "unknown fields on reordered rows survive"
        )
        XCTAssertEqual(
            rows.first { ($0["key"] as? String) == "project:tui-group" }?["pinned_at"]
                as? UInt64,
            42
        )
        XCTAssertEqual(
            (grouped?["other"] ?? []).map { $0["key"] as? String },
            ["session:x"],
            "other projects' records are untouched"
        )
    }

    func testCombinedOrderPreservesBothBucketsWithoutDuplicates() {
        XCTAssertEqual(
            UnpeelStore.combinedSessionOrder(
                pinnedIDs: ["pin-b", "pin-a"],
                regularIDs: ["regular-b", "pin-a", "regular-a"]
            ),
            ["pin-b", "pin-a", "regular-b", "regular-a"]
        )
    }

    func testRelativeSessionOrderPreservesChildGroupSlots() {
        XCTAssertEqual(
            UnpeelStore.applyingRelativeIDOrder(
                ["session-c", "session-a", "session-b"],
                to: ["group", "session-a", "session-b", "session-c"]
            ),
            ["group", "session-c", "session-a", "session-b"]
        )
    }

    func testRestartReplacementKeepsTheSameSharedRank() {
        XCTAssertEqual(
            UnpeelStore.replacingSessionID(
                in: ["a", "old", "c"], oldID: "old", newID: "new"
            ),
            ["a", "new", "c"]
        )
        XCTAssertNil(
            UnpeelStore.replacingSessionID(
                in: ["a", "c"], oldID: "old", newID: "new"
            )
        )
    }

    func testRecentSortOnlyPrivilegesWorkingThenUsesLifecycleAndStableID() {
        func session(
            _ id: String, status: SessionStatus, created: Int64, lifecycle: Int64
        ) -> SessionEntry {
            SessionEntry(
                id: id,
                projectID: "project",
                label: id,
                command: "codex",
                createdAt: created,
                status: status,
                lifecycleAtMs: lifecycle
            )
        }
        let rows = [
            session("idle-newest", status: .idle, created: 10, lifecycle: 900),
            session("attention", status: .attention, created: 20, lifecycle: 800),
            session("exited", status: .exited, created: 30, lifecycle: 700),
            session("busy", status: .busy, created: 40, lifecycle: 100),
            session("restart", status: .exited, created: 50, lifecycle: 90),
            session("tie-b", status: .idle, created: 60, lifecycle: 600),
            session("tie-a", status: .idle, created: 60, lifecycle: 600),
        ]

        let ordered = UnpeelStore.sessionsSortedByRecentActivity(
            rows,
            restartingSessionIDs: ["restart"]
        )

        XCTAssertEqual(
            ordered.map(\.id),
            ["busy", "restart", "idle-newest", "attention", "exited", "tie-a", "tie-b"]
        )
    }

    func testLifecycleStampIgnoresRepaintFallbackForHookCapableAgent() {
        XCTAssertEqual(
            UnpeelStore.resolvedLifecycleAtMs(
                createdAtMs: 100,
                command: "codex --full-auto",
                hookEventAtMs: nil,
                screenChangedAtMs: 900,
                outputAtMs: 1_000,
                finalExitedAtMs: nil
            ),
            100,
            "a hook-capable session with no hook event stays at creation"
        )
        XCTAssertEqual(
            UnpeelStore.resolvedLifecycleAtMs(
                createdAtMs: 100,
                command: "codex --full-auto",
                hookEventAtMs: 400,
                screenChangedAtMs: 900,
                outputAtMs: 1_000,
                finalExitedAtMs: nil
            ),
            400,
            "the durable hook seed is authoritative even when output repaints later"
        )
    }

    func testHooklessLifecyclePrefersScreenThenOutputAndExitedFinalStamp() {
        XCTAssertEqual(
            UnpeelStore.resolvedLifecycleAtMs(
                createdAtMs: 100,
                command: "pi",
                hookEventAtMs: nil,
                screenChangedAtMs: 300,
                outputAtMs: 900,
                finalExitedAtMs: nil
            ),
            300
        )
        XCTAssertEqual(
            UnpeelStore.resolvedLifecycleAtMs(
                createdAtMs: 100,
                command: "pi",
                hookEventAtMs: nil,
                screenChangedAtMs: nil,
                outputAtMs: 900,
                finalExitedAtMs: nil
            ),
            900
        )
        XCTAssertEqual(
            UnpeelStore.resolvedLifecycleAtMs(
                createdAtMs: 100,
                command: "pi",
                hookEventAtMs: nil,
                screenChangedAtMs: 300,
                outputAtMs: 900,
                finalExitedAtMs: 1_200
            ),
            1_200
        )
    }

}
