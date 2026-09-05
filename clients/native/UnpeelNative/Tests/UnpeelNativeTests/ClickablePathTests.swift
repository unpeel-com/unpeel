import XCTest
@testable import UnpeelNative

final class ClickablePathTests: XCTestCase {
    private func match(_ row: String, column: Int = 0) -> ClickablePath.Match? {
        ClickablePath.match(inRow: row, column: column)
    }

    func testRelativePathWithLineAndColumn() {
        let m = match("  edited src/app/Home.tsx:42:5 ok", column: 12)
        XCTAssertEqual(m, .init(path: "src/app/Home.tsx", line: 42, column: 5))
    }

    func testRelativePathWithLineOnly() {
        let m = match("at src/main.rs:120", column: 6)
        XCTAssertEqual(m, .init(path: "src/main.rs", line: 120, column: nil))
    }

    func testPlainPathNoLine() {
        let m = match("see components/Footer.tsx here", column: 6)
        XCTAssertEqual(m, .init(path: "components/Footer.tsx", line: nil, column: nil))
    }

    func testAbsolutePath() {
        let m = match("/Users/x/Dev/unpeel/AGENTS.md:3", column: 4)
        XCTAssertEqual(m, .init(path: "/Users/x/Dev/unpeel/AGENTS.md", line: 3, column: nil))
    }

    func testTildePathStaysUnexpanded() {
        // Expansion happens at resolve time, not extraction.
        let m = match("~/.unpeel/app-state.json", column: 2)
        XCTAssertEqual(m, .init(path: "~/.unpeel/app-state.json", line: nil, column: nil))
    }

    func testPicksTokenUnderColumnWhenMultiple() {
        let row = "a/one.ts and b/two.ts"
        XCTAssertEqual(match(row, column: 1)?.path, "a/one.ts")
        XCTAssertEqual(match(row, column: 16)?.path, "b/two.ts")
    }

    func testSingleTokenUsedRegardlessOfColumn() {
        // Column drift from cell rounding shouldn't lose the only path.
        let m = match("   src/lib/state.swift:9   ", column: 0)
        XCTAssertEqual(m, .init(path: "src/lib/state.swift", line: 9, column: nil))
    }

    func testIgnoresURLs() {
        XCTAssertNil(match("visit https://example.com/path here", column: 10))
    }

    func testIgnoresPlainWords() {
        XCTAssertNil(match("just some words here", column: 5))
    }

    func testIgnoresBareNumbersAndTimestamps() {
        XCTAssertNil(match("12:34:56 build done", column: 2))
    }

    func testStripsTrailingSentencePunctuation() {
        let m = match("edited foo/bar.ts.", column: 8)
        XCTAssertEqual(m, .init(path: "foo/bar.ts", line: nil, column: nil))
    }

    func testParenWrappedPath() {
        let m = match("(src/Home.tsx:7)", column: 3)
        XCTAssertEqual(m, .init(path: "src/Home.tsx", line: 7, column: nil))
    }

    func testFilenameWithExtensionNoSlash() {
        let m = match("Footer.tsx changed", column: 2)
        XCTAssertEqual(m, .init(path: "Footer.tsx", line: nil, column: nil))
    }
}
