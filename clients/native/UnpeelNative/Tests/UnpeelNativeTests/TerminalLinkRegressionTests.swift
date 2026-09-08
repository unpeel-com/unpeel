import Foundation
import Testing
@testable import UnpeelNative

struct TerminalLinkRegressionTests {
    @Test(arguments: [
        ("Read notes.md or https://example.com/report", "https://"),
        ("Read notes.md or https://example.com/report", "Read"),
        ("Read notes.md or https://example.com/report", " or "),
        ("a/one.md and b/two.md", "and"),
        ("Only notes.md here", "here"),
        ("mailto:person@example.com", "person"),
        ("...", "."),
    ])
    func unrelatedClicksDoNotOpenFiles(row: String, clicked: String) throws {
        let position = try #require(row.range(of: clicked)?.lowerBound)
        let offset = row.distance(from: row.startIndex, to: position)
        #expect(ClickablePath.match(inRow: row, column: offset) == nil)
    }

    @Test(arguments: [
        ("Read notes.md.", "notes.md", "notes.md", nil as Int?),
        ("Read `My Documents/Meeting notes.md`", "Meeting", "My Documents/Meeting notes.md", nil),
        ("Read \"résumés/日本語 (draft).md\"", "日本語", "résumés/日本語 (draft).md", nil),
        ("Read My\\ Documents/notes.md", "Documents", "My Documents/notes.md", nil),
        ("Read .env", ".env", ".env", nil),
        ("Read docs/notes.md#L42", "notes", "docs/notes.md", 42),
        ("[notes](docs/notes.md:42)", "docs", "docs/notes.md", 42),
        ("See café/résumé.md", "résumé", "café/résumé.md", nil),
        ("📄 日本語 a/one.md and b/two.md", "b/two", "b/two.md", nil),
    ])
    func pathsPreserveTheirNames(row: String, clicked: String, path: String, line: Int?) throws {
        let position = try #require(row.range(of: clicked)?.lowerBound)
        let offset = row.distance(from: row.startIndex, to: position)
        #expect(ClickablePath.match(inRow: row, column: offset) == .init(path: path, line: line))
    }

    @Test(arguments: [
        ("https://en.wikipedia.org/wiki/Function_(mathematics)", "https://en.wikipedia.org/wiki/Function_(mathematics)"),
        ("(https://example.com/a_(b)).", "https://example.com/a_(b)"),
        ("`<https://example.com>`", "https://example.com"),
        ("https://example.com/search?q=why?", "https://example.com/search?q=why?"),
        ("https://example.com/hello!", "https://example.com/hello!"),
        ("http://localhost:3000/path", "http://localhost:3000/path"),
        ("localhost:3000/path", "http://localhost:3000/path"),
        ("https://example.com/\n  a?x=1&y=2", "https://example.com/a?x=1&y=2"),
    ])
    func webLinksKeepSignificantPunctuation(raw: String, expected: String) {
        #expect(GhosttyTerminalPane.sanitizedURL(from: raw)?.absoluteString == expected)
    }

    @Test
    func webLinksRequireAHost() {
        #expect(GhosttyTerminalPane.sanitizedURL(from: "https://") == nil)
        #expect(GhosttyTerminalPane.sanitizedURL(from: "http:///report") == nil)
    }

    @Test
    func fileURLTargetsAreDecodedAndScoped() {
        #expect(ClickablePath.fileURLMatch("file:///tmp/Meeting%20notes.md#L42")
            == .init(path: "/tmp/Meeting notes.md", line: 42))
        #expect(ClickablePath.fileURLMatch("file://localhost/tmp/notes.md")?.path == "/tmp/notes.md")
        #expect(ClickablePath.fileURLMatch("file://other-host/tmp/notes.md") == nil)
        #expect(ClickablePath.fileURLMatch("file://other-host/srv/notes.md", allowRemoteHost: true)?.path
            == "/srv/notes.md")
        #expect(ClickablePath.fileURLMatch("https://example.com/notes.md") == nil)
        #expect(ClickablePath.fileURLMatch("file:///tmp/notes.md#heading")?.path == "/tmp/notes.md")
        #expect(ClickablePath.fileURLMatch("file:///tmp/file.")?.path == "/tmp/file.")
    }

    @Test
    func workingDirectoriesStayWithTheirPaneAcrossRefreshes() {
        var first = TerminalWorkingDirectory(seed: "/work/first")
        let second = TerminalWorkingDirectory(seed: "/work/second")
        first.reported = "/work/first/reports"
        first.seed = "/work/first"
        #expect(ClickablePath.absolutePath("notes.md", workingDirectory: first.current)
            == "/work/first/reports/notes.md")
        #expect(ClickablePath.absolutePath("notes.md", workingDirectory: second.current)
            == "/work/second/notes.md")
        first.reported = ""
        #expect(first.current == "/work/first")
    }

    @Test
    func remotePathsNeverExpandTheControllersHome() {
        #expect(ClickablePath.absolutePath("~/notes.md", workingDirectory: "/srv/project", homeDirectory: nil) == nil)
        #expect(ClickablePath.absolutePath("~/notes.md", workingDirectory: "/srv/project", homeDirectory: "/home/host")
            == "/home/host/notes.md")
        #expect(ClickablePath.absolutePath("", workingDirectory: "/work") == nil)
    }
}
