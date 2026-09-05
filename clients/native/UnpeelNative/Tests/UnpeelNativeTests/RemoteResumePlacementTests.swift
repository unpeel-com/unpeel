import XCTest
@testable import UnpeelNative

/// The landing slot a Host-routed Resume row is moved to while the Host
/// replaces the Session. Mirrors the Host's running-row sort: unranked rows
/// newest-first, then shared-order ranks.
final class RemoteResumePlacementTests: XCTestCase {
    private struct Row {
        let running: Bool
        let createdAt: Int64
    }

    private func index(
        order: [String],
        rows: [String: Row],
        source: String,
        createdAt: Int64,
        sharedOrder: [String] = []
    ) -> Int {
        UnpeelStore.predictedResumeInsertionIndex(
            in: order,
            sourceID: source,
            sourceCreatedAt: createdAt,
            sharedOrder: sharedOrder,
            isRunningRow: { rows[$0]?.running == true },
            isSessionRow: { rows[$0] != nil },
            createdAt: { rows[$0]?.createdAt ?? 0 }
        )
    }

    func testUnrankedRowSortsNewestFirstAmongRunningRows() {
        let rows: [String: Row] = [
            "new": Row(running: true, createdAt: 300),
            "old": Row(running: true, createdAt: 100),
            "stopped": Row(running: false, createdAt: 900),
        ]
        XCTAssertEqual(
            index(order: ["new", "old", "stopped"], rows: rows, source: "s", createdAt: 200),
            1
        )
        XCTAssertEqual(
            index(order: ["new", "old", "stopped"], rows: rows, source: "s", createdAt: 400),
            0
        )
        XCTAssertEqual(
            index(order: ["new", "old", "stopped"], rows: rows, source: "s", createdAt: 50),
            2
        )
    }

    func testRankedRowTakesItsSharedRankAfterUnrankedRows() {
        let rows: [String: Row] = [
            "fresh": Row(running: true, createdAt: 900),
            "a": Row(running: true, createdAt: 100),
            "c": Row(running: true, createdAt: 100),
            "stopped": Row(running: false, createdAt: 50),
        ]
        let order = ["fresh", "a", "c", "stopped"]
        XCTAssertEqual(
            index(order: order, rows: rows, source: "b", createdAt: 999, sharedOrder: ["a", "b", "c"]),
            2
        )
        XCTAssertEqual(
            index(order: order, rows: rows, source: "z", createdAt: 999, sharedOrder: ["a", "c", "z"]),
            3
        )
    }

    func testUnrankedSourcePrecedesEveryRankedRow() {
        let rows: [String: Row] = [
            "a": Row(running: true, createdAt: 900),
            "stopped": Row(running: false, createdAt: 50),
        ]
        XCTAssertEqual(
            index(order: ["a", "stopped"], rows: rows, source: "s", createdAt: 1, sharedOrder: ["a"]),
            0
        )
    }

    func testNoRunningRowsLeadsTheStoppedRowsBelowFolders() {
        let rows: [String: Row] = [
            "stopped": Row(running: false, createdAt: 50),
        ]
        XCTAssertEqual(
            index(order: ["folder", "stopped"], rows: rows, source: "s", createdAt: 1),
            1
        )
        XCTAssertEqual(index(order: [], rows: [:], source: "s", createdAt: 1), 0)
    }
}
