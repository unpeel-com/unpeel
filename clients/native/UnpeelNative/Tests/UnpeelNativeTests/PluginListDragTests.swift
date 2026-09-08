import AppKit
import SwiftUI
import XCTest
@testable import UnpeelNative

@MainActor
final class PluginListDragTests: XCTestCase {
    private final class FlippedView: NSView {
        override var isFlipped: Bool { true }
    }

    func testVariableHeightDragCommitsOnceAndCancellationKeepsOrder() async throws {
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 600, height: 500),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let container = FlippedView(frame: NSRect(x: 0, y: 0, width: 600, height: 500))
        window.contentView = container
        let controller = PluginListDragController()
        let ids = ["claude", "codex", "markdown"]
        var commits: [[String]] = []
        controller.bind(view: container, ids: ids, enabled: true,
                        preview: { _ in AnyView(Color.gray) }, commit: { commits.append($0) })
        var y: CGFloat = 0
        for (id, height) in zip(ids, [112.0, 64.0, 86.0]) {
            let row = NSView(frame: NSRect(x: 0, y: y, width: 600, height: height))
            container.addSubview(row)
            controller.register(row, id: id)
            y += height + 16
        }
        func screen(_ y: CGFloat) -> NSPoint {
            window.convertPoint(toScreen: container.convert(NSPoint(x: 30, y: y), to: nil))
        }
        defer { controller.detach(); window.close() }
        let down = try XCTUnwrap(NSEvent.mouseEvent(
            with: .leftMouseDown, location: container.convert(NSPoint(x: 30, y: 20), to: nil),
            modifierFlags: [], timestamp: 0, windowNumber: window.windowNumber,
            context: nil, eventNumber: 0, clickCount: 1, pressure: 1
        ))
        XCTAssertNil(controller.handle(down), "Row presses must not enter competing AppKit tracking loops")
        controller.endDrag(cancelled: true)

        controller.beginDrag(id: "claude", at: screen(20))
        controller.updateDrag(at: screen(290))
        XCTAssertEqual(controller.targetIndex, 2)
        XCTAssertEqual(controller.offset(for: "codex"), -128)
        XCTAssertEqual(controller.offset(for: "markdown"), -128)
        XCTAssertTrue(commits.isEmpty, "Pointer movement must never write Host state")
        controller.endDrag(cancelled: false)
        controller.endDrag(cancelled: false)
        XCTAssertTrue(commits.isEmpty, "Keep the insertion gap until the card lands")
        XCTAssertEqual(controller.offset(for: "codex"), -128)
        try await Task.sleep(nanoseconds: 450_000_000)
        XCTAssertEqual(commits, [["codex", "markdown", "claude"]])

        controller.detach()
        controller.bind(view: container, ids: ids, enabled: true,
                        preview: { _ in AnyView(Color.gray) }, commit: { commits.append($0) })
        controller.beginDrag(id: "markdown", at: screen(230))
        controller.updateDrag(at: screen(0))
        XCTAssertEqual(controller.targetIndex, 0)
        controller.endDrag(cancelled: true)
        XCTAssertEqual(commits.count, 1, "Escape must not change the saved order")
    }
}
