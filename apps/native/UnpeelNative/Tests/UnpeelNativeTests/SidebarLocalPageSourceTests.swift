import XCTest
@testable import UnpeelNative

/// The current instance's own Local carousel page must render the worker's
/// snapshot once Local is a Host client. The Swift scan tree is frozen at
/// launch in that mode, so a page built from it omitted every Session
/// created since (most visibly the ones filed inside sidebar groups), which
/// then popped in only after the commit's first bootstrap.
final class SidebarLocalPageSourceTests: XCTestCase {
    func testHostServedLocalRendersThePooledSnapshotWhenOneExists() {
        XCTAssertTrue(SidebarListContentBuilder.currentInstanceRendersHostSnapshot(
            localHostServed: true,
            pooledSnapshotAvailable: true
        ))
    }

    func testHostServedLocalFallsBackToLocalTruthBeforeTheFirstMirror() {
        XCTAssertFalse(SidebarListContentBuilder.currentInstanceRendersHostSnapshot(
            localHostServed: true,
            pooledSnapshotAvailable: false
        ))
    }

    func testCompatibilityLocalNeverReadsAPooledSnapshot() {
        // The in-app Host keeps the Swift scan as Local truth; a stale mirror
        // from an earlier client-mode launch must not outrank it.
        XCTAssertFalse(SidebarListContentBuilder.currentInstanceRendersHostSnapshot(
            localHostServed: false,
            pooledSnapshotAvailable: true
        ))
    }
}
