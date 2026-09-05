import XCTest

@testable import UnpeelIOS

final class RemoteTerminalScrollPredictionEngineTests: XCTestCase {
    private let start = Date(timeIntervalSince1970: 1_000_000)

    /// One answered slow-path gesture: confident, EWMA well above the
    /// display threshold, queue drained.
    private func earnSlowConfidence(
        _ engine: inout RemoteTerminalScrollPredictionEngine, at time: Date
    ) {
        engine.beginGesture()
        engine.wheelSent(rows: 1, at: time)
        engine.contentShifted(rows: 1, at: time.addingTimeInterval(0.3))
    }

    func testFastPathTracksButNeverDisplays() {
        var engine = RemoteTerminalScrollPredictionEngine()
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start)
        // Answered almost immediately: LAN Wi-Fi. Confidence is earned but
        // the whole-canvas translation would just fight the TUI's pinned
        // chrome, so nothing displays — this gesture or the next.
        engine.contentShifted(rows: 2, at: start.addingTimeInterval(0.05))
        XCTAssertTrue(engine.isConfident)
        engine.beginGesture()
        engine.wheelSent(rows: 4, at: start.addingTimeInterval(1))
        XCTAssertEqual(engine.pendingRows, 4)
        XCTAssertEqual(engine.offsetRows, 0)
    }

    func testSlowPathDisplaysFromTheNextGesture() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 3, at: start.addingTimeInterval(1))
        XCTAssertEqual(engine.offsetRows, -3)
    }

    func testMidGestureConfidenceDoesNotDisplayThisGesture() {
        var engine = RemoteTerminalScrollPredictionEngine()
        engine.beginGesture()
        engine.wheelSent(rows: 5, at: start)
        // First-ever answer arrives slow, mid-gesture. Displaying now would
        // snap the view by the whole backlog at once; the latch holds until
        // the next gesture starts.
        engine.contentShifted(rows: 2, at: start.addingTimeInterval(0.3))
        XCTAssertTrue(engine.isConfident)
        XCTAssertEqual(engine.pendingRows, 3)
        XCTAssertEqual(engine.offsetRows, 0)
    }

    func testQueueWaitDoesNotInflateTheLatencyEstimate() {
        var engine = RemoteTerminalScrollPredictionEngine()
        engine.beginGesture()
        engine.wheelSent(rows: 1, at: start) // probe: queue was empty
        engine.wheelSent(rows: 1, at: start.addingTimeInterval(0.02)) // queued
        engine.contentShifted(rows: 1, at: start.addingTimeInterval(0.05))
        // The queued batch stalls (the shift detector missed its chunks)
        // and drains late. Its age is queue wait, not path latency — it
        // must not push a Wi-Fi session over the display threshold.
        engine.contentShifted(rows: 1, at: start.addingTimeInterval(0.6))
        XCTAssertEqual(engine.responseLatency ?? -1, 0.05, accuracy: 0.001)
        engine.beginGesture()
        engine.wheelSent(rows: 4, at: start.addingTimeInterval(1))
        XCTAssertEqual(engine.offsetRows, 0, "queue-wait noise must never flip the gate on")
    }

    func testShiftDrainsRowsAcrossBatchesWithPartialSplit() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 3, at: start.addingTimeInterval(1))
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(1.1))
        XCTAssertEqual(engine.offsetRows, -5)
        // One coalesced chunk carrying four rows of movement drains across
        // the batch boundary and splits the second batch.
        engine.contentShifted(rows: 4, at: start.addingTimeInterval(1.4))
        XCTAssertEqual(engine.offsetRows, -1)
        engine.contentShifted(rows: 1, at: start.addingTimeInterval(1.5))
        XCTAssertEqual(engine.offsetRows, 0)
    }

    func testUnshiftedChunkDrainsNothing() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 4, at: start.addingTimeInterval(1))
        // A streaming agent frame that scrolled nothing must not eat the
        // prediction out from under the finger.
        engine.contentShifted(rows: 0, at: start.addingTimeInterval(1.2))
        XCTAssertEqual(engine.offsetRows, -4)
    }

    func testOppositeDirectionShiftIsNotAnAnswer() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 4, at: start.addingTimeInterval(1))
        engine.contentShifted(rows: -3, at: start.addingTimeInterval(1.2))
        XCTAssertEqual(engine.offsetRows, -4, "autoscroll opposite the wheels drains nothing")
        // Comfortably past the timeout: an exact-boundary probe is at the
        // mercy of Double rounding.
        let late = start.addingTimeInterval(
            1.3 + RemoteTerminalScrollPredictionEngine.responseTimeout
        )
        XCTAssertTrue(engine.expireIfUnanswered(at: late))
        XCTAssertFalse(engine.isConfident, "an unanswered gesture still closes the gate")
    }

    func testOverShiftClampsAtZeroInsteadOfFlippingSign() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(1))
        // TUI scrolls several lines per wheel step: real frames already
        // carry the extra motion, so translating past truth would overshoot.
        engine.contentShifted(rows: 6, at: start.addingTimeInterval(1.3))
        XCTAssertEqual(engine.offsetRows, 0)
    }

    func testPendingRowsAreCappedNotUnbounded() {
        var engine = RemoteTerminalScrollPredictionEngine()
        engine.beginGesture()
        engine.wheelSent(rows: RemoteTerminalScrollPredictionEngine.maximumPendingRows, at: start)
        engine.wheelSent(rows: 5, at: start)
        XCTAssertEqual(
            engine.pendingRows,
            RemoteTerminalScrollPredictionEngine.maximumPendingRows,
            "steps beyond the cap are sent but not predicted"
        )
    }

    func testUnansweredGestureExpiresAndClosesTheGate() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 3, at: start.addingTimeInterval(1))
        let late = start.addingTimeInterval(
            1.1 + RemoteTerminalScrollPredictionEngine.responseTimeout
        )
        XCTAssertTrue(engine.expireIfUnanswered(at: late))
        XCTAssertEqual(engine.pendingRows, 0)
        XCTAssertFalse(engine.isConfident, "a fully unanswered gesture closes the gate")
    }

    func testExpiryAfterAnAckKeepsTheGate() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(1))
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(1.05))
        engine.contentShifted(rows: 2, at: start.addingTimeInterval(1.3))
        let late = start.addingTimeInterval(
            1.1 + RemoteTerminalScrollPredictionEngine.responseTimeout
        )
        XCTAssertTrue(engine.expireIfUnanswered(at: late))
        XCTAssertEqual(engine.pendingRows, 0)
        XCTAssertTrue(
            engine.isConfident,
            "coalesced trailing frames must not punish a responsive TUI"
        )
    }

    func testEarlyExpiryProbeDoesNothing() {
        var engine = RemoteTerminalScrollPredictionEngine()
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start)
        XCTAssertFalse(engine.expireIfUnanswered(at: start.addingTimeInterval(0.2)))
        XCTAssertEqual(engine.pendingRows, 2)
    }

    func testCancelKeepsConfidenceResetDropsEverything() {
        var engine = RemoteTerminalScrollPredictionEngine()
        earnSlowConfidence(&engine, at: start)
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(1))
        engine.cancel()
        XCTAssertEqual(engine.pendingRows, 0)
        XCTAssertTrue(engine.isConfident)
        XCTAssertNotNil(engine.responseLatency, "cancel keeps the path estimate")
        engine.resetConfidence()
        XCTAssertFalse(engine.isConfident)
        XCTAssertNil(engine.responseLatency, "a replaced screen re-earns the path estimate")
        engine.beginGesture()
        engine.wheelSent(rows: 2, at: start.addingTimeInterval(2))
        XCTAssertEqual(engine.offsetRows, 0)
    }
}

final class RemoteTerminalScrollShiftDetectorTests: XCTestCase {
    /// Distinct, non-repeating rows, like transcript prose.
    private func screen(_ lines: [String]) -> String {
        lines.joined(separator: "\n")
    }

    private let transcript = (0..<20).map { "line \($0) of the transcript body" }

    func testDetectsDownwardScrollShift() {
        let before = screen(transcript)
        let after = screen(Array(transcript[3...]) + ["new 0", "new 1", "new 2"])
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(before: before, after: after, maxShift: 10),
            3
        )
    }

    func testDetectsUpwardScrollShift() {
        let before = screen(Array(transcript[4...]))
        let after = screen(transcript)
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(before: before, after: after, maxShift: 10),
            -4
        )
    }

    func testStationaryScreenWithChangedTailIsZero() {
        // Streaming output: bottom rows change, everything else holds.
        var lines = transcript
        lines[18] = "streamed replacement A"
        lines[19] = "streamed replacement B"
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(
                before: screen(transcript), after: screen(lines), maxShift: 10
            ),
            0
        )
    }

    func testPinnedChromeDoesNotMaskTheScrolledRegion() {
        // Claude shape: transcript scrolls, composer + footer stay put.
        let chrome = ["╭── composer ──╮", "│ >            │", "╰──────────────╯", "? for shortcuts"]
        let before = screen(transcript + chrome)
        let after = screen(Array(transcript[2...]) + ["tail 0", "tail 1"] + chrome)
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(before: before, after: after, maxShift: 10),
            2
        )
    }

    func testUnrecognizableRepaintIsZero() {
        let before = screen(transcript)
        let after = screen((0..<20).map { "totally different row \($0)" })
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(before: before, after: after, maxShift: 10),
            0
        )
    }

    func testBlankRowsNeverVoteAndTrailingSpacesAreIgnored() {
        let before = screen(["", "alpha   ", "", "beta", ""])
        let after = screen(["", "alpha", "", "beta  ", ""])
        XCTAssertEqual(
            RemoteTerminalScrollShiftDetector.shift(before: before, after: after, maxShift: 4),
            0
        )
    }
}
