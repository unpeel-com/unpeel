import AppKit
import XCTest
@testable import UnpeelNative

/// Pure decision logic behind the ⌘V-pastes-nothing-for-a-screenshot fix:
/// `GhosttyTerminalPane.pastePayloadKind` decides, from the pasteboard's
/// current contents, whether a paste trigger should defer to ghostty's own
/// clipboard-read callback (`.text`) or paste the same image/file content
/// the drag-and-drop path accepts (`.dropReferences`); `isPasteKeyEquivalent`
/// decides whether an `NSEvent` is the ⌘V chord at all. Both are exercised
/// here directly, against a private, named scratch pasteboard that never
/// touches the real system clipboard.
@MainActor
final class GhosttyPastePayloadTests: XCTestCase {
    private var pasteboard: NSPasteboard!

    override func setUp() {
        super.setUp()
        pasteboard = NSPasteboard(name: .init(UUID().uuidString))
    }

    override func tearDown() {
        pasteboard.releaseGlobally()
        pasteboard = nil
        super.tearDown()
    }

    func testEmptyPasteboardHasNothingToPaste() {
        pasteboard.clearContents()
        XCTAssertEqual(GhosttyTerminalPane.pastePayloadKind(of: pasteboard), .none)
    }

    func testPlainTextIsText() {
        pasteboard.clearContents()
        pasteboard.setString("hello", forType: .string)
        XCTAssertEqual(GhosttyTerminalPane.pastePayloadKind(of: pasteboard), .text)
    }

    /// The exact bug this fixes: a screenshot (⌘⇧⌃4) is image data only —
    /// PNGf/TIFF/JPEG types, no string, no file URL.
    func testImageOnlyPasteboardIsDropReferences() throws {
        pasteboard.clearContents()
        let item = NSPasteboardItem()
        item.setData(try sampleTIFFData(), forType: .tiff)
        pasteboard.writeObjects([item])

        XCTAssertNil(pasteboard.string(forType: .string))
        XCTAssertEqual(GhosttyTerminalPane.pastePayloadKind(of: pasteboard), .dropReferences)
    }

    func testFileURLOnlyPasteboardIsDropReferences() {
        pasteboard.clearContents()
        pasteboard.writeObjects([NSURL(fileURLWithPath: "/tmp/unpeel-paste-test.png")])

        XCTAssertNil(pasteboard.string(forType: .string))
        XCTAssertEqual(GhosttyTerminalPane.pastePayloadKind(of: pasteboard), .dropReferences)
    }

    /// Text paste must stay exactly as it is today no matter what else is
    /// on the pasteboard — e.g. a Finder copy can carry both a file URL and
    /// a plain-text representation of the same path.
    func testTextWinsOverACoPresentFileURL() {
        pasteboard.clearContents()
        pasteboard.writeObjects([NSURL(fileURLWithPath: "/tmp/unpeel-paste-test.png")])
        let stringItem = NSPasteboardItem()
        stringItem.setString("/tmp/unpeel-paste-test.png", forType: .string)
        pasteboard.writeObjects([stringItem])

        XCTAssertTrue(
            pasteboard.canReadObject(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]),
            "sanity: the file URL really is still on the pasteboard"
        )
        XCTAssertEqual(GhosttyTerminalPane.pastePayloadKind(of: pasteboard), .text)
    }

    func testIsPasteKeyEquivalentMatchesOnlyBareCommandV() throws {
        XCTAssertTrue(
            GhosttyTerminalPane.isPasteKeyEquivalent(try keyEvent(characters: "v", modifiers: .command))
        )
        XCTAssertFalse(
            GhosttyTerminalPane.isPasteKeyEquivalent(
                try keyEvent(characters: "v", modifiers: [.command, .shift])
            ),
            "⌘⇧V (Paste and Match Style elsewhere) must not be swallowed here"
        )
        XCTAssertFalse(
            GhosttyTerminalPane.isPasteKeyEquivalent(try keyEvent(characters: "v", modifiers: []))
        )
        XCTAssertFalse(
            GhosttyTerminalPane.isPasteKeyEquivalent(try keyEvent(characters: "c", modifiers: .command)),
            "only V, not every command chord"
        )
        XCTAssertFalse(
            GhosttyTerminalPane.isPasteKeyEquivalent(
                try keyEvent(characters: "v", modifiers: .command, type: .keyUp)
            )
        )
    }

    // MARK: - Helpers

    private func sampleTIFFData() throws -> Data {
        let image = NSImage(size: NSSize(width: 1, height: 1))
        image.lockFocus()
        NSColor.red.drawSwatch(in: NSRect(x: 0, y: 0, width: 1, height: 1))
        image.unlockFocus()
        return try XCTUnwrap(image.tiffRepresentation)
    }

    private func keyEvent(
        characters: String,
        modifiers: NSEvent.ModifierFlags,
        type: NSEvent.EventType = .keyDown
    ) throws -> NSEvent {
        try XCTUnwrap(NSEvent.keyEvent(
            with: type,
            location: .zero,
            modifierFlags: modifiers,
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: 0,
            context: nil,
            characters: characters,
            charactersIgnoringModifiers: characters,
            isARepeat: false,
            keyCode: 9 // kVK_ANSI_V
        ))
    }
}
