import XCTest
@testable import UnpeelIOS

final class TerminalQueryStripTests: XCTestCase {
    private func strip(_ s: String) -> String {
        String(decoding: TerminalQueryFilter().stripRequests(Data(s.utf8)), as: UTF8.self)
    }

    /// Feeds each piece as its own chunk through one filter instance and
    /// returns the concatenated output — the split-across-chunks scenario.
    private func strip(chunks: [String]) -> String {
        let filter = TerminalQueryFilter()
        var out = Data()
        for chunk in chunks {
            out.append(filter.stripRequests(Data(chunk.utf8)))
        }
        return String(decoding: out, as: UTF8.self)
    }

    func testStripsXTVersionRequest() {
        // The observed bug: XTVERSION query (CSI > q) makes the local surface
        // reply `>|ghostty 1.3.1` into the app's input. It must be removed.
        XCTAssertEqual(strip("A\u{1B}[>qB"), "AB")
        XCTAssertEqual(strip("A\u{1B}[>0qB"), "AB")
    }

    func testStripsDeviceAttributesAndStatusRequests() {
        XCTAssertEqual(strip("x\u{1B}[cy"), "xy")       // primary DA
        XCTAssertEqual(strip("x\u{1B}[0cy"), "xy")      // primary DA, explicit 0
        XCTAssertEqual(strip("x\u{1B}[>cy"), "xy")      // secondary DA
        XCTAssertEqual(strip("x\u{1B}[6ny"), "xy")      // DSR cursor position
        XCTAssertEqual(strip("x\u{1B}[?6ny"), "xy")     // DSR, private
    }

    func testStripsDECRQMButNotDECSTR() {
        XCTAssertEqual(strip("a\u{1B}[?2026$pb"), "ab") // DECRQM query
        // DECSTR (soft reset, CSI ! p) has no '$' — must be preserved.
        XCTAssertEqual(strip("a\u{1B}[!pb"), "a\u{1B}[!pb")
    }

    func testStripsDCSQueriesXTGETTCAPAndDECRQSS() {
        XCTAssertEqual(strip("a\u{1B}P+q544e\u{1B}\\b"), "ab") // XTGETTCAP
        XCTAssertEqual(strip("a\u{1B}P$qm\u{1B}\\b"), "ab")     // DECRQSS
    }

    func testPreservesRISAndDECSCUSRAndOrdinarySequences() {
        // ESC c (RIS reset) — 'c' after a bare ESC, not a CSI: keep.
        XCTAssertEqual(strip("a\u{1B}cb"), "a\u{1B}cb")
        // DECSCUSR: CSI Ps SP q (set cursor style) — keep (only '>…q' is XTVERSION).
        XCTAssertEqual(strip("a\u{1B}[2 qb"), "a\u{1B}[2 qb")
        // Colour + cursor move must be untouched.
        XCTAssertEqual(strip("\u{1B}[31mhi\u{1B}[0m\u{1B}[2J"), "\u{1B}[31mhi\u{1B}[0m\u{1B}[2J")
    }

    // MARK: - Split across chunk boundaries (the composer leak)

    func testStripsQuerySplitAcrossChunks() {
        // The leak: a query cut at the chunk boundary passed through in two
        // innocent halves, reassembled inside the surface's parser, and was
        // answered as typed input. The trailing half-query must be withheld
        // and stripped once the boundary arrives.
        XCTAssertEqual(strip(chunks: ["ok\u{1B}[>0", "qdone"]), "okdone")
        // The observed DECRQM 2026 probe shape, split mid-parameter.
        XCTAssertEqual(strip(chunks: ["a\u{1B}[?20", "26$pb"]), "ab")
        // Split immediately after the ESC.
        XCTAssertEqual(strip(chunks: ["x\u{1B}", "[6ny"]), "xy")
    }

    func testEmitsNonQuerySplitSequenceIntact() {
        // A withheld trailing fragment that turns out to be an ordinary
        // sequence must come out whole, in order, on the next chunk.
        XCTAssertEqual(strip(chunks: ["\u{1B}[3", "1mred"]), "\u{1B}[31mred")
        XCTAssertEqual(strip(chunks: ["a\u{1B}", "cb"]), "a\u{1B}cb") // RIS survives a split
    }

    func testStripsDCSQuerySplitAcrossChunks() {
        // XTGETTCAP split three ways, including a split ST (ESC in one
        // chunk, backslash in the next).
        XCTAssertEqual(strip(chunks: ["a\u{1B}P+q54", "4e\u{1B}", "\\b"]), "ab")
        // Classification split: only 'ESC P' at the boundary.
        XCTAssertEqual(strip(chunks: ["a\u{1B}P", "$qm\u{1B}\\b"]), "ab")
        // Long unterminated query payload is discarded chunk-by-chunk, then
        // output resumes after the terminator.
        let longPayload = String(repeating: "5", count: 4096)
        XCTAssertEqual(strip(chunks: ["a\u{1B}P+q", longPayload, "\u{1B}\\b"]), "ab")
    }

    func testOversizedIncompleteCSIIsDroppedNotEmitted() {
        // A withheld run longer than any real query is not a query — drop it
        // rather than emit a reassembly hazard (mirrors the mouse tracker's
        // pending cap).
        let junk = "\u{1B}[" + String(repeating: "1;", count: 128)
        XCTAssertEqual(strip(chunks: ["ok", junk, "later"]), "oklater")
    }

    func testResetClearsCarriedState() {
        let filter = TerminalQueryFilter()
        _ = filter.stripRequests(Data("a\u{1B}[>0".utf8)) // withholds the tail
        filter.reset()
        XCTAssertEqual(
            String(decoding: filter.stripRequests(Data("fresh".utf8)), as: UTF8.self),
            "fresh"
        )
    }

    func testNoEscapeIsUnchanged() {
        XCTAssertEqual(strip("plain text 123"), "plain text 123")
    }
}
