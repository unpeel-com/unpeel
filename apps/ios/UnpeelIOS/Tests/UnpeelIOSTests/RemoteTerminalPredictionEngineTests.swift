import XCTest
@testable import UnpeelIOS

final class RemoteTerminalPredictionEngineTests: XCTestCase {
    private let start = Date(timeIntervalSinceReferenceDate: 1_000)

    private func rows(_ lines: [String]) -> [Substring] {
        lines.joined(separator: "\n").split(
            separator: "\n",
            omittingEmptySubsequences: false
        )
    }

    func testPredictionsStayHiddenUntilFirstConfirmation() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("h", cursor: (row: 1, column: 4), columns: 80, at: start)
        XCTAssertNil(
            engine.displayedText,
            "an unconfirmed context (password prompt, menu) must never show a prediction"
        )

        engine.reconcile(rows: rows(["", "  > h"]), at: start.addingTimeInterval(0.3))
        XCTAssertTrue(engine.isConfident)
        XCTAssertTrue(engine.pending.isEmpty)

        engine.keystroke("i", cursor: (row: 1, column: 5), columns: 80, at: start)
        XCTAssertEqual(engine.displayedText, ["i"], "after one confirmed echo the gate is open")
    }

    func testRapidKeystrokesChainOffTheLastPrediction() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("a", cursor: (row: 0, column: 2), columns: 80, at: start)
        engine.keystroke("b", cursor: (row: 0, column: 2), columns: 80, at: start)
        engine.keystroke("c", cursor: (row: 0, column: 2), columns: 80, at: start)
        XCTAssertEqual(engine.pending.map(\.column), [2, 3, 4])
        XCTAssertEqual(engine.pending.map(\.row), [0, 0, 0])
    }

    func testPartialEchoConfirmsPrefixAndKeepsTheRest() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("a", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.keystroke("b", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.reconcile(rows: rows(["a"]), at: start.addingTimeInterval(0.2))
        XCTAssertTrue(engine.isConfident)
        XCTAssertEqual(engine.pending.map(\.character), ["b"], "unechoed suffix keeps waiting")
    }

    func testForeignCharacterAtPredictedCellClosesTheGate() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("j", cursor: (row: 0, column: 3), columns: 80, at: start)
        engine.reconcile(rows: rows(["absX"]), at: start.addingTimeInterval(0.2))
        XCTAssertTrue(engine.pending.isEmpty)
        XCTAssertFalse(engine.isConfident, "vim-normal-mode style contradiction closes display")
    }

    func testExpiryDropsEverythingAndClosesTheGate() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("s", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.reconcile(rows: rows([""]), at: start.addingTimeInterval(0.5))
        XCTAssertFalse(engine.pending.isEmpty, "blank cell inside the expiry window waits")
        engine.reconcile(
            rows: rows([""]),
            at: start.addingTimeInterval(RemoteTerminalPredictionEngine.expiry + 0.1)
        )
        XCTAssertTrue(engine.pending.isEmpty)
        XCTAssertFalse(engine.isConfident)
    }

    func testBackspaceRemovesTheLastPrediction() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("a", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.keystroke("b", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.backspace()
        XCTAssertEqual(engine.pending.map(\.character), ["a"])
        engine.backspace()
        engine.backspace()
        XCTAssertTrue(engine.pending.isEmpty, "extra backspaces with nothing pending are no-ops")
    }

    func testLineEdgeAndUnknownCursorSuppressPrediction() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("x", cursor: (row: 0, column: 79), columns: 80, at: start)
        XCTAssertTrue(engine.pending.isEmpty, "wrap is the remote program's call")
        engine.keystroke("x", cursor: nil, columns: 80, at: start)
        XCTAssertTrue(engine.pending.isEmpty)
    }

    func testOverflowClearsInsteadOfPaintingAPhantomLine() {
        var engine = RemoteTerminalPredictionEngine()
        for _ in 0 ..< RemoteTerminalPredictionEngine.maximumPending {
            engine.keystroke("x", cursor: (row: 0, column: 0), columns: 200, at: start)
        }
        XCTAssertEqual(engine.pending.count, RemoteTerminalPredictionEngine.maximumPending)
        engine.keystroke("x", cursor: (row: 0, column: 0), columns: 200, at: start)
        XCTAssertTrue(engine.pending.isEmpty)
    }

    func testConfidenceSurvivesNonPrintableClearButNotReset() {
        var engine = RemoteTerminalPredictionEngine()
        engine.keystroke("a", cursor: (row: 0, column: 0), columns: 80, at: start)
        engine.reconcile(rows: rows(["a"]), at: start.addingTimeInterval(0.1))
        XCTAssertTrue(engine.isConfident)
        engine.clearPending()
        XCTAssertTrue(engine.isConfident, "an arrow key doesn't invalidate earned trust")
        engine.reset()
        XCTAssertFalse(engine.isConfident, "a replay/rebase does")
    }

    func testCellCharacterMapsColumnsAndBlanks() {
        let row = Substring("ab cd")
        XCTAssertEqual(RemoteTerminalPredictionEngine.cellCharacter(in: row, column: 0), "a")
        XCTAssertEqual(RemoteTerminalPredictionEngine.cellCharacter(in: row, column: 2), " ")
        XCTAssertEqual(RemoteTerminalPredictionEngine.cellCharacter(in: row, column: 4), "d")
        XCTAssertNil(
            RemoteTerminalPredictionEngine.cellCharacter(in: row, column: 5),
            "beyond the painted row reads as blank (waiting), not contradiction"
        )
    }
}
