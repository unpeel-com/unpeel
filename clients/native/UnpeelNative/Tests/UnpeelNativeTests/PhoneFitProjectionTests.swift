import XCTest
import UnpeelShared
@testable import UnpeelNative

/// Host-service client path: the desktop letterbox override is DERIVED from
/// the Host-published phone fit in the Local projection, never set by a
/// Swift `/mobile/resize-desktop` route (that stays on the compatibility path).
final class PhoneFitProjectionTests: XCTestCase {
    private func summary(
        _ id: String,
        status: RemoteSessionStatus = .running,
        fit: (Int, Int)? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: "p",
            title: id,
            command: "claude",
            createdAtUnixMs: 1,
            status: status,
            activity: .idle,
            phoneFitColumns: fit?.0,
            phoneFitRows: fit?.1,
            phoneFitSinceUnixMs: fit == nil ? nil : 1_788_000_000_000
        )
    }

    func testPublishedFitBecomesTheLetterboxOverrideAndClampsToTheHostGridLimits() {
        let result = UnpeelStore.phoneResizeOverrides(
            fromHostPublished: [
                summary("a", fit: (60, 24)),
                summary("b"),
                summary("c", fit: (1_000, 1)),
            ],
            locallyCleared: []
        )
        XCTAssertEqual(result.overrides["a"], PhoneResizeOverride(cols: 60, rows: 24))
        XCTAssertNil(result.overrides["b"])
        XCTAssertEqual(result.overrides["c"], PhoneResizeOverride(cols: 300, rows: 2))
        XCTAssertTrue(result.locallyCleared.isEmpty)
    }

    func testAnExitedSessionNeverKeepsALetterbox() {
        let result = UnpeelStore.phoneResizeOverrides(
            fromHostPublished: [summary("a", status: .exited, fit: (60, 24))],
            locallyCleared: []
        )
        XCTAssertTrue(result.overrides.isEmpty)
    }

    func testALocallyClearedFitStaysClearedUntilTheHostStopsPublishingIt() {
        // Snapshot still carries the grid (clear in flight): no override,
        // and the local clear survives.
        let pending = UnpeelStore.phoneResizeOverrides(
            fromHostPublished: [summary("a", fit: (60, 24))],
            locallyCleared: ["a"]
        )
        XCTAssertNil(pending.overrides["a"])
        XCTAssertEqual(pending.locallyCleared, ["a"])
        // The Host dropped the grid: the local clear ends with it, so a
        // later phone fit applies again.
        let done = UnpeelStore.phoneResizeOverrides(
            fromHostPublished: [summary("a")],
            locallyCleared: ["a"]
        )
        XCTAssertTrue(done.locallyCleared.isEmpty)
        let again = UnpeelStore.phoneResizeOverrides(
            fromHostPublished: [summary("a", fit: (40, 20))],
            locallyCleared: done.locallyCleared
        )
        XCTAssertEqual(again.overrides["a"], PhoneResizeOverride(cols: 40, rows: 20))
    }

    func testSummaryDecodesTheAdditiveFieldsAndToleratesTheirAbsence() throws {
        let with = try JSONDecoder().decode(RemoteSessionSummary.self, from: Data("""
        {"id":"a","projectID":"p","title":"a","command":"claude","createdAtUnixMs":1,
         "status":"running","activity":"idle","phoneFitColumns":60,"phoneFitRows":24,
         "phoneFitSinceUnixMs":1788000000000}
        """.utf8))
        XCTAssertEqual(with.phoneFitColumns, 60)
        XCTAssertEqual(with.phoneFitRows, 24)
        XCTAssertEqual(with.phoneFitSinceUnixMs, 1_788_000_000_000)
        let without = try JSONDecoder().decode(RemoteSessionSummary.self, from: Data("""
        {"id":"a","projectID":"p","title":"a","command":"claude","createdAtUnixMs":1,
         "status":"running","activity":"idle"}
        """.utf8))
        XCTAssertNil(without.phoneFitColumns)
        XCTAssertNil(without.phoneFitRows)
    }
}
