import XCTest
@testable import UnpeelIOS

/// Pure-logic tests for the terminal cache's LRU bookkeeping. Deliberately
/// no `TerminalSessionCache`/renderer/surface involvement — ghostty surfaces
/// cannot exist in headless test runs.
final class SessionLRUIndexTests: XCTestCase {
    func testInsertWithinCapacityEvictsNothing() {
        var index = SessionLRUIndex<String>(capacity: 3)

        XCTAssertTrue(index.insert("A", for: "a").isEmpty)
        XCTAssertTrue(index.insert("B", for: "b").isEmpty)
        XCTAssertTrue(index.insert("C", for: "c").isEmpty)
        XCTAssertEqual(index.count, 3)
        XCTAssertEqual(index.keys, ["a", "b", "c"])
    }

    func testInsertBeyondCapacityEvictsLeastRecentlyUsed() {
        var index = SessionLRUIndex<String>(capacity: 2)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        let evicted = index.insert("C", for: "c")

        XCTAssertEqual(evicted.map(\.id), ["a"])
        XCTAssertEqual(evicted.map(\.entry), ["A"])
        XCTAssertEqual(index.keys, ["b", "c"])
        XCTAssertNil(index.peek("a"))
    }

    func testLookupTouchesRecencySoEvictionSkipsIt() {
        var index = SessionLRUIndex<String>(capacity: 2)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        XCTAssertEqual(index.lookup("a"), "A")
        let evicted = index.insert("C", for: "c")

        // "b" became the oldest after the lookup touched "a".
        XCTAssertEqual(evicted.map(\.id), ["b"])
        XCTAssertEqual(index.keys, ["a", "c"])
    }

    func testLookupMissReturnsNilAndDoesNotDisturbOrder() {
        var index = SessionLRUIndex<String>(capacity: 2)
        _ = index.insert("A", for: "a")

        XCTAssertNil(index.lookup("missing"))
        XCTAssertEqual(index.keys, ["a"])
    }

    func testReinsertExistingKeyReplacesValueTouchesAndNeverSelfEvicts() {
        var index = SessionLRUIndex<String>(capacity: 2)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        let evicted = index.insert("A2", for: "a")

        XCTAssertTrue(evicted.isEmpty)
        XCTAssertEqual(index.count, 2)
        XCTAssertEqual(index.keys, ["b", "a"])
        XCTAssertEqual(index.peek("a"), "A2")
    }

    func testPeekDoesNotTouchRecency() {
        var index = SessionLRUIndex<String>(capacity: 2)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        XCTAssertEqual(index.peek("a"), "A")
        let evicted = index.insert("C", for: "c")

        // "a" stayed oldest despite the peek.
        XCTAssertEqual(evicted.map(\.id), ["a"])
    }

    func testRemoveReturnsEntryAndDropsIt() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        XCTAssertEqual(index.remove("a"), "A")
        XCTAssertNil(index.remove("a"))
        XCTAssertEqual(index.keys, ["b"])
    }

    func testRemoveAllReturnsEverythingInLRUOrder() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")
        _ = index.lookup("a")

        let removed = index.removeAll()

        XCTAssertEqual(removed.map(\.id), ["b", "a"])
        XCTAssertEqual(index.count, 0)
        XCTAssertTrue(index.keys.isEmpty)
    }

    func testRetainOnlyDropsMissingSessions() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")
        _ = index.insert("C", for: "c")

        let dropped = index.retain(only: ["a", "c"])

        XCTAssertEqual(dropped.map(\.id), ["b"])
        XCTAssertEqual(index.keys, ["a", "c"])
    }

    func testRetainOnlySparesTheKeptSessionEvenWhenMissing() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        // Transiently empty session list must not tear down the visible
        // session's terminal.
        let dropped = index.retain(only: [], keeping: "b")

        XCTAssertEqual(dropped.map(\.id), ["a"])
        XCTAssertEqual(index.keys, ["b"])
    }

    func testRemoveAllExceptKeepsOnlyTheGivenSession() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")
        _ = index.insert("C", for: "c")

        let dropped = index.removeAll(except: "b")

        XCTAssertEqual(Set(dropped.map(\.id)), ["a", "c"])
        XCTAssertEqual(index.keys, ["b"])
    }

    func testRemoveAllExceptNilDropsEverything() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("A", for: "a")
        _ = index.insert("B", for: "b")

        let dropped = index.removeAll(except: nil)

        XCTAssertEqual(dropped.map(\.id), ["a", "b"])
        XCTAssertEqual(index.count, 0)
    }

    func testCapacityIsClampedToAtLeastOne() {
        var index = SessionLRUIndex<String>(capacity: 0)

        XCTAssertTrue(index.insert("A", for: "a").isEmpty)
        let evicted = index.insert("B", for: "b")

        XCTAssertEqual(evicted.map(\.id), ["a"])
        XCTAssertEqual(index.keys, ["b"])
    }

    func testVisibilityLeaseIgnoresLateReleaseFromSameSessionRemount() {
        var visibility = TerminalVisibilityLeaseTracker()
        let oldMount = UUID()
        let replacement = UUID()

        visibility.acquire(sessionID: "same-session", owner: oldMount)
        visibility.acquire(sessionID: "same-session", owner: replacement)

        XCTAssertFalse(
            visibility.release(sessionID: "same-session", owner: oldMount)
        )
        XCTAssertEqual(visibility.sessionID, "same-session")
        XCTAssertTrue(
            visibility.release(sessionID: "same-session", owner: replacement)
        )
        XCTAssertNil(visibility.sessionID)
    }

    func testStreamLeasesOnlyStartAndStopAtOwnershipEdges() {
        var leases = TerminalStreamLeaseTracker()
        let oldMount = UUID()
        let replacement = UUID()

        XCTAssertTrue(leases.acquire(oldMount))
        XCTAssertFalse(leases.acquire(oldMount), "repeat appear must be idempotent")
        XCTAssertFalse(leases.acquire(replacement))
        XCTAssertFalse(
            leases.release(oldMount),
            "old disappear must not stop the replacement owner"
        )
        XCTAssertFalse(leases.release(oldMount), "repeat disappear must be idempotent")
        XCTAssertTrue(leases.release(replacement))
        XCTAssertTrue(leases.isEmpty)
    }

    func testSameSessionReplacementRemainsProtectedFromFlushSelection() {
        var index = SessionLRUIndex<String>(capacity: 3)
        _ = index.insert("old-entry", for: "same-session")
        _ = index.insert("parked", for: "other-session")

        var visibility = TerminalVisibilityLeaseTracker()
        let oldMount = UUID()
        let replacement = UUID()
        visibility.acquire(sessionID: "same-session", owner: oldMount)

        // An epoch change replaces the cache value before SwiftUI dismantles
        // the old view, then the replacement appears first.
        _ = index.insert("new-entry", for: "same-session")
        visibility.acquire(sessionID: "same-session", owner: replacement)
        _ = visibility.release(sessionID: "same-session", owner: oldMount)

        let dropped = index.removeAll(except: visibility.sessionID)

        XCTAssertEqual(dropped.map(\.id), ["other-session"])
        XCTAssertEqual(index.peek("same-session"), "new-entry")
    }
}
