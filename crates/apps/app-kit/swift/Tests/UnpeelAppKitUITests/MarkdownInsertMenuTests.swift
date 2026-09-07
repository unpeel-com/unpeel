import AppKit
import Foundation
import SwiftUI
import Testing

@testable import UnpeelAppKitUI

@Test
func markdownCommandHintAndMenuTriggersComeFromTheSpec() {
    let editor = MarkdownEditorSpec(
        text: "title\n\nbody",
        selection: .caret(UITextPosition(line: 1, utf16Column: 0)),
        commandHint: MarkdownCommandHint(text: "Type '/' for commands"),
        actions: MarkdownEditorActions(openMenu: "open-menu")
    )
    #expect(editor.commandHintVisible)
    #expect(editor.menuTrigger(forTextInput: "/") == .slash)
    #expect(editor.menuTrigger(forTextInput: "\\") == .palette)
    let preview = MarkdownEditorSpec(
        text: editor.text,
        selection: editor.selection,
        presentation: .preview,
        commandHint: editor.commandHint,
        actions: editor.actions
    )
    #expect(!preview.commandHintVisible)

    let ordinaryLine = MarkdownEditorSpec(
        text: "not blank",
        selection: .caret(UITextPosition(line: 0, utf16Column: 9)),
        actions: MarkdownEditorActions(openMenu: "open-menu")
    )
    #expect(
        ordinaryLine.menuTrigger(forTextInput: "/") == .slash,
        "the Rust reducer, not the renderer, decides whether slash opens a Menu"
    )
}

@Test
func markdownTaskMarkersToggleOnlyAtTheCheckbox() {
    let text = "- [ ] first\n10. [x] second"
    #expect(
        markdownTaskToggleEdit(text: text, utf16Offset: 3)
            == MarkdownTaskToggleEdit(
                range: NSRange(location: 3, length: 1),
                replacement: "x"
            ))
    #expect(
        markdownTaskToggleEdit(text: text, utf16Offset: 17)
            == MarkdownTaskToggleEdit(
                range: NSRange(location: 17, length: 1),
                replacement: " "
            ))
    #expect(markdownTaskToggleEdit(text: text, utf16Offset: 7) == nil)
}

@Test
func markdownBackspaceRemovesMarkers() {
    let task = "- [ ] write tests"
    let edit = markdownBackspaceEdit(
        text: task,
        selection: NSRange(location: 4, length: 0)
    )
    #expect(edit?.replacement == "write tests")
    #expect(edit?.caretUTF16Offset == 0)
}

@MainActor
@Test
func nativeMarkdownSlashRoundTripPresentsAndActivatesTheAuthoritativeMenu() throws {
    let messages = try markdownSlashRoundTripMessages()
    guard case let .snapshot(initial) = messages[0],
          case let .event(slashEvent) = messages[1],
          case let .delta(openMenuDelta) = messages[2],
          case let .event(selectionEvent) = messages[3],
          case let .delta(selectionDelta) = messages[4]
    else {
        Issue.record("shared slash fixture must contain snapshot/event/delta/event/delta")
        return
    }
    let recorder = MarkdownActionRecorder()
    let hosting = NSHostingView(
        rootView: MarkdownEditorView(snapshot: initial) { recorder.actions.append($0) }
    )
    let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 640, height: 480),
        styleMask: [.titled],
        backing: .buffered,
        defer: false
    )
    window.contentView = hosting
    window.makeKeyAndOrderFront(nil)
    defer { window.close() }
    drainMainRunLoop()
    let existingWindows = Set(NSApplication.shared.windows.map(ObjectIdentifier.init))

    let textView = try #require(firstSubview(of: NSTextView.self, in: hosting))
    window.makeFirstResponder(textView)
    textView.insertText("/", replacementRange: textView.selectedRange())
    #expect(recorder.actions.last == slashEvent.action)
    #expect(textView.string.isEmpty, "the Rust app owns insertion of the slash")

    let menuSnapshot = try initial.applying(openMenuDelta)
    hosting.rootView = MarkdownEditorView(
        snapshot: menuSnapshot
    ) { recorder.actions.append($0) }
    drainMainRunLoop()
    let popover = NSApplication.shared.windows.first {
        !existingWindows.contains(ObjectIdentifier($0)) && $0.isVisible
    }
    #expect(popover != nil, "the caret-anchored semantic Menu must be visibly presented")

    textView.doCommand(by: #selector(NSResponder.insertNewline(_:)))
    #expect(recorder.actions.last == selectionEvent.action)

    let selectedSnapshot = try menuSnapshot.applying(selectionDelta)
    hosting.rootView = MarkdownEditorView(snapshot: selectedSnapshot) {
        recorder.actions.append($0)
    }
    drainMainRunLoop()
    #expect(textView.string == "# ")
    #expect(popover?.isVisible == false)
}

@MainActor
private final class MarkdownActionRecorder {
    var actions: [UIAction] = []
}

private func markdownSlashRoundTripMessages() throws -> [UIMessage] {
    let testDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    let fixture = testDirectory
        .appendingPathComponent("../../../protocol/unpeel-ui-v1.ndjson")
        .standardizedFileURL
    let stream = try String(contentsOf: fixture, encoding: .utf8)
    return try stream
        .split(separator: "\n")
        .suffix(5)
        .map { try JSONDecoder().decode(UIMessage.self, from: Data($0.utf8)) }
}

@MainActor
private func firstSubview<View: NSView>(of type: View.Type, in root: NSView) -> View? {
    if let match = root as? View { return match }
    for child in root.subviews {
        if let match = firstSubview(of: type, in: child) { return match }
    }
    return nil
}

@MainActor
private func drainMainRunLoop() {
    RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.05))
}
