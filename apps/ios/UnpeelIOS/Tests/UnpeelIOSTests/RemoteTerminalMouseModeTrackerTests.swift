import XCTest
@testable import UnpeelIOS

final class RemoteTerminalMouseModeTrackerTests: XCTestCase {
    func testEnablesAndDisablesMouseTracking() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[?1002h".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
        XCTAssertFalse(tracker.sawMouseOrAlternateDisable)

        tracker.feed(Data("\u{1B}[?1002l".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)
        XCTAssertTrue(tracker.sawMouseOrAlternateDisable)
    }

    func testTracksAlternateScreen() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[?1049h".utf8))
        XCTAssertTrue(tracker.alternateScreenEnabled)

        tracker.feed(Data("\u{1B}[?1049l".utf8))
        XCTAssertFalse(tracker.alternateScreenEnabled)
    }

    func testMultipleParamsInOneSequence() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[?1049;1002h".utf8))
        XCTAssertTrue(tracker.alternateScreenEnabled)
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testSequenceSplitAcrossChunksIsCarried() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("prefix\u{1B}[?10".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)

        tracker.feed(Data("02h suffix".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testBareEscapeSplitAcrossChunksIsCarried() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data([0x1B]))
        tracker.feed(Data("[?1000h".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testTerminalResetClearsModes() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[?1002h\u{1B}[?1049h".utf8))
        tracker.feed(Data("\u{1B}c".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)
        XCTAssertFalse(tracker.alternateScreenEnabled)
        XCTAssertTrue(tracker.sawMouseOrAlternateDisable)
    }

    func testResetClearsCarriedPrefix() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[?10".utf8))
        tracker.reset()
        tracker.feed(Data("02h".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)
    }

    func testUnterminatedOversizedSequenceIsDroppedEntirely() {
        let tracker = RemoteTerminalMouseModeTracker()
        // A pathological "CSI" that never terminates: overflow must drop the
        // pending buffer entirely (truncating it could bisect the sequence
        // into bytes that parse as something else).
        var junk = Data("\u{1B}[?".utf8)
        junk.append(Data(repeating: UInt8(ascii: "1"), count: 4096))
        tracker.feed(junk)

        // A follow-up final byte must not combine with the dropped prefix.
        tracker.feed(Data("h".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)

        // And the tracker keeps working for later well-formed sequences.
        tracker.feed(Data("\u{1B}[?1002h".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testNonPrivateSequencesAreIgnored() {
        let tracker = RemoteTerminalMouseModeTracker()
        tracker.feed(Data("\u{1B}[1002h\u{1B}[2J\u{1B}[H".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)
        XCTAssertFalse(tracker.alternateScreenEnabled)
    }
}

final class RemoteMacClientErrorTests: XCTestCase {
    func testDescriptionCarriesStatusAndServerMessage() {
        let error = RemoteMacClientError(statusCode: 404, serverMessage: "unknown session")
        XCTAssertEqual(error.description, "HTTP 404: unknown session")
        XCTAssertEqual(error.errorDescription, "HTTP 404: unknown session")
        XCTAssertEqual(error.localizedDescription, "HTTP 404: unknown session")
    }

    func testDescriptionWithoutServerMessage() {
        let error = RemoteMacClientError(statusCode: 500, serverMessage: nil)
        XCTAssertEqual(error.description, "HTTP 500")
        XCTAssertEqual(error.errorDescription, "HTTP 500")
    }

    func testHostModeSnapshotFlagIsExplicitAndClearedByReset() {
        let tracker = RemoteTerminalMouseModeTracker()
        XCTAssertFalse(tracker.hasHostModeSnapshot)
        tracker.feed(Data("\u{1B}[?1049h".utf8))
        XCTAssertFalse(tracker.hasHostModeSnapshot, "bytes alone are not a snapshot")
        tracker.markHostModeSnapshot()
        XCTAssertTrue(tracker.hasHostModeSnapshot)
        tracker.reset()
        XCTAssertFalse(tracker.hasHostModeSnapshot)
    }
}
