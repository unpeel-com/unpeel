import XCTest
@testable import UnpeelNative

final class OpenURLSanitizerTests: XCTestCase {
    private func clean(_ raw: String) -> String? {
        GhosttyTerminalPane.sanitizedURL(from: raw)?.absoluteString
    }

    func testPlainURLPassesThrough() {
        XCTAssertEqual(clean("https://quiz.ai"), "https://quiz.ai")
    }

    func testStripsWrappingWhitespaceAndNewlines() {
        // Terminal-wrapped link: split across a line break with padding.
        XCTAssertEqual(
            clean("https://search.google.com/\nsearch-console/"),
            "https://search.google.com/search-console/"
        )
    }

    func testPeelsMarkdownWrappers() {
        XCTAssertEqual(clean("(https://quiz.ai)"), "https://quiz.ai")
        XCTAssertEqual(clean("<https://quiz.ai>"), "https://quiz.ai")
    }

    func testDropsTrailingSentencePunctuation() {
        XCTAssertEqual(clean("https://quiz.ai."), "https://quiz.ai")
        XCTAssertEqual(clean("https://quiz.ai,"), "https://quiz.ai")
    }

    func testAddsSchemeToBareHost() {
        XCTAssertEqual(clean("www.example.com/path"), "https://www.example.com/path")
        XCTAssertEqual(clean("example.com"), "https://example.com")
    }

    func testKeepsMailto() {
        XCTAssertEqual(clean("mailto:hi@quiz.ai"), "mailto:hi@quiz.ai")
    }

    func testRejectsUnsupportedScheme() {
        // Avoid handing LaunchServices a scheme that triggers arbitrary apps.
        XCTAssertNil(clean("file:///Applications/Calculator.app"))
        XCTAssertNil(clean("javascript:alert(1)"))
    }

    func testRejectsEmptyAndJunk() {
        XCTAssertNil(clean(""))
        XCTAssertNil(clean("   \n  "))
        XCTAssertNil(clean("just some text"))
    }

    func testLocalFileLinksBecomeDecodedDragPaths() {
        XCTAssertEqual(
            GhosttyTerminalPane.localDragPath(fromLink: "file:///tmp/a%20folder/hello.txt"),
            "/tmp/a folder/hello.txt"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.localDragPath(fromLink: "file://localhost/tmp/hello.txt"),
            "/tmp/hello.txt"
        )
    }

    func testPathDragRejectsNonFileAndRemoteFileLinks() {
        XCTAssertNil(GhosttyTerminalPane.localDragPath(fromLink: "https://example.com/file"))
        XCTAssertNil(GhosttyTerminalPane.localDragPath(fromLink: "file://remote-host/tmp/file"))
        XCTAssertNil(GhosttyTerminalPane.localDragPath(fromLink: nil))
    }

    @MainActor
    func testDropReferenceQuotingUsesSingleQuotesWhenNeeded() {
        XCTAssertEqual(
            GhosttyTerminalPane.quoteDropReference("/tmp/Screenshot.png"),
            "/tmp/Screenshot.png"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.quoteDropReference("/tmp/Screenshot 1.png"),
            "'/tmp/Screenshot 1.png'"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.quoteDropReference("/tmp/it's.png"),
            "'/tmp/it'\\''s.png'"
        )
    }

    @MainActor
    func testDroppedPathsPreferProjectThenHomeRelativeText() {
        let home = "/Users/example"
        let project = "/Users/example/Dev/unpeel"
        XCTAssertEqual(
            GhosttyTerminalPane.conciseDropReference(
                "/Users/example/Dev/unpeel/clients/native/App.swift",
                projectRoot: project,
                homeDirectory: home
            ),
            "clients/native/App.swift"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.conciseDropReference(
                "/Users/example/Documents/note.md",
                projectRoot: project,
                homeDirectory: home
            ),
            "~/Documents/note.md"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.conciseDropReference(
                "/opt/shared/file.txt",
                projectRoot: project,
                homeDirectory: home
            ),
            "/opt/shared/file.txt"
        )
        XCTAssertEqual(
            GhosttyTerminalPane.conciseDropReference(
                "/Users/example/Dev/unpeel-other/file.txt",
                projectRoot: project,
                homeDirectory: home
            ),
            "~/Dev/unpeel-other/file.txt"
        )
    }

    @MainActor
    func testDroppedImagesKeepAbsolutePathsForAgentAttachments() {
        let image = "/Users/example/Desktop/Screenshot 2026-08-28 at 16.15.11.png"
        let concise = GhosttyTerminalPane.conciseDropReference(
            image,
            projectRoot: "/Users/example/Dev/unpeel",
            homeDirectory: "/Users/example"
        )

        XCTAssertEqual(concise, image)
        XCTAssertEqual(
            GhosttyTerminalPane.quoteDropReference(concise),
            "'/Users/example/Desktop/Screenshot 2026-08-28 at 16.15.11.png'"
        )
    }
}
