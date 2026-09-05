import XCTest
@testable import UnpeelIOS

final class StreamFrameReconcilerTests: XCTestCase {
    private func action(_ held: UInt64, _ frameOffset: UInt64, _ len: Int) -> StreamFrameAction {
        StreamFrameReconciler.action(held: held, frameOffset: frameOffset, frameLength: len)
    }

    func testContiguousFrameFeeds() {
        XCTAssertEqual(action(1000, 1000, 50), .feed)
        XCTAssertEqual(action(0, 0, 1), .feed)
    }

    // The real bug: every one of these forward gaps used to trigger a full
    // reconnect (the churn loop). They must now fill the gap and stay up.
    func testTraceForwardGapsFillInsteadOfRestart() {
        // Exact (held, frame) pairs pulled from the disconnect trace.
        let cases: [(UInt64, UInt64)] = [
            (20441339, 20445456),
            (20481496, 20484469),
            (20486528, 20487252),
            (20487810, 20490434),
            (20500391, 20502861),
            (20505283, 20505591)
        ]
        for (held, frame) in cases {
            XCTAssertEqual(
                action(held, frame, 200),
                .fillGap(from: held, upTo: frame),
                "held=\(held) frame=\(frame) must fill the gap, not restart"
            )
        }
    }

    func testStaleDuplicateFrameIsSkipped() {
        // Frame entirely below held (a replayed frame after reconnect).
        XCTAssertEqual(action(2000, 1500, 300), .skip)   // 1500+300=1800 <= 2000
        XCTAssertEqual(action(2000, 2000 - 10, 10), .skip) // ends exactly at held
    }

    func testOverlappingFrameFeedsOnlyNewTail() {
        // Frame starts before held but runs past it: keep the new bytes only.
        XCTAssertEqual(action(2000, 1950, 100), .feedSuffix(dropLeading: 50)) // covers 1950..2050
        XCTAssertEqual(action(500, 400, 250), .feedSuffix(dropLeading: 100))  // covers 400..650
    }

    func testZeroLengthFrameBelowHeldIsSkipped() {
        XCTAssertEqual(action(1000, 900, 0), .skip)
    }

    func testFillGapBoundsAreExact() {
        guard case let .fillGap(from, upTo) = action(100, 4200, 32) else {
            return XCTFail("expected fillGap")
        }
        XCTAssertEqual(from, 100)
        XCTAssertEqual(upTo, 4200)
        XCTAssertEqual(upTo - from, 4100) // the exact byte count to fetch
    }
}
