import Foundation
import XCTest
@testable import UnpeelNative

/// Pure hit-test rules for Session-drag drop targets, in AppKit window
/// coordinates (y-up: PaneEdge.up is the maxY side).
final class TerminalPaneDropTargetTests: XCTestCase {
    private let content = NSRect(x: 100, y: 0, width: 800, height: 600)

    private func resolve(
        _ point: NSPoint,
        panes: [(paneID: String, isSolo: Bool, rect: NSRect)]
    ) -> PaneDropTarget? {
        SidebarSessionDragController.dropTarget(
            at: point, contentRect: content, panes: panes
        )
    }

    func testOuterBandsTargetGroupEdges() {
        let pane = (paneID: "p1", isSolo: false, rect: content)
        XCTAssertEqual(
            resolve(NSPoint(x: 110, y: 300), panes: [pane]), .groupEdge(.left)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 890, y: 300), panes: [pane]), .groupEdge(.right)
        )
        // y-up: near maxY is visually the top.
        XCTAssertEqual(
            resolve(NSPoint(x: 500, y: 590), panes: [pane]), .groupEdge(.up)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 500, y: 10), panes: [pane]), .groupEdge(.down)
        )
    }

    func testPaneInteriorTargetsNearestEdge() {
        let pane = (
            paneID: "p1",
            isSolo: false,
            rect: NSRect(x: 200, y: 100, width: 400, height: 400)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 220, y: 300), panes: [pane]),
            .pane(paneID: "p1", edge: .left)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 580, y: 300), panes: [pane]),
            .pane(paneID: "p1", edge: .right)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 400, y: 480), panes: [pane]),
            .pane(paneID: "p1", edge: .up)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 400, y: 120), panes: [pane]),
            .pane(paneID: "p1", edge: .down)
        )
    }

    func testShortPanesRefuseVerticalSplits() {
        let short = (
            paneID: "p1",
            isSolo: false,
            rect: NSRect(x: 200, y: 250, width: 400, height: 100)
        )
        // Cursor near the visual top of a 100pt-tall pane still resolves to a
        // horizontal half: two stacked headers would leave no terminal.
        XCTAssertEqual(
            resolve(NSPoint(x: 250, y: 340), panes: [short]),
            .pane(paneID: "p1", edge: .left)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 550, y: 340), panes: [short]),
            .pane(paneID: "p1", edge: .right)
        )
    }

    func testSoloPaneResolvesToGroupEdges() {
        let solo = (
            paneID: "solo:s1",
            isSolo: true,
            rect: NSRect(x: 200, y: 100, width: 400, height: 400)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 400, y: 120), panes: [solo]),
            .groupEdge(.down)
        )
        XCTAssertEqual(
            resolve(NSPoint(x: 220, y: 300), panes: [solo]),
            .groupEdge(.left)
        )
    }

    func testOutsideContentRectIsNil() {
        XCTAssertNil(resolve(NSPoint(x: 50, y: 300), panes: []))
        XCTAssertNil(
            resolve(
                NSPoint(x: 500, y: 300),
                panes: [(
                    paneID: "p1",
                    isSolo: false,
                    rect: NSRect(x: 200, y: 100, width: 100, height: 100)
                )]
            )
        )
    }

    func testEmptyProjectSidebarPinTargetIsTopTrailingSquare() {
        let rect = SidebarSessionDragController.projectSidebarPinTargetRect(
            in: content
        )
        XCTAssertEqual(rect.width, 76)
        XCTAssertEqual(rect.height, 76)
        XCTAssertEqual(rect.maxY, content.maxY - 12)
        XCTAssertEqual(rect.maxX, content.maxX - 4)
        XCTAssertTrue(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: NSPoint(x: rect.midX, y: rect.midY),
                contentRect: content,
                trailingEdgeX: content.maxX + 6,
                projectSidebarIsOpen: false,
                canPin: true
            )
        )
        XCTAssertTrue(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: NSPoint(x: content.maxX + 5, y: content.maxY - 1),
                contentRect: content,
                trailingEdgeX: content.maxX + 6,
                projectSidebarIsOpen: false,
                canPin: true
            ),
            "a straight drag to the literal right edge reaches the inset square"
        )
        XCTAssertTrue(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: NSPoint(x: content.maxX + 20, y: content.midY),
                contentRect: content,
                trailingEdgeX: content.maxX,
                projectSidebarIsOpen: false,
                canPin: true
            ),
            "the sticky field tolerates a decisive throw just past the edge"
        )
        XCTAssertFalse(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: NSPoint(x: content.maxX + 25, y: content.midY),
                contentRect: content,
                trailingEdgeX: content.maxX,
                projectSidebarIsOpen: false,
                canPin: true
            ),
            "the edge overshoot remains bounded"
        )
    }

    func testProjectSidebarPinTargetRequiresClosedPanelAndPinnableSession() {
        let rect = SidebarSessionDragController.projectSidebarPinTargetRect(
            in: content
        )
        let point = NSPoint(x: rect.midX, y: rect.midY)
        for (isOpen, canPin) in [
            (true, true),
            (false, false),
        ] {
            XCTAssertFalse(
                SidebarSessionDragController.isProjectSidebarPinTarget(
                    at: point,
                    contentRect: content,
                    trailingEdgeX: content.maxX + 6,
                    projectSidebarIsOpen: isOpen,
                    canPin: canPin
                )
            )
        }
        XCTAssertFalse(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: NSPoint(x: content.midX, y: content.midY),
                contentRect: content,
                trailingEdgeX: content.maxX + 6,
                projectSidebarIsOpen: false,
                canPin: true
            )
        )
    }

    func testProjectSidebarPinHoverExpandsToPersistedPaneFootprint() {
        let sidebarWidth: CGFloat = 340
        let preview = SidebarSessionDragController.projectSidebarPinPreviewRect(
            in: content,
            projectSidebarWidth: sidebarWidth
        )
        XCTAssertEqual(preview.width, sidebarWidth - Theme.surfaceInset * 2)
        XCTAssertEqual(preview.minY, content.minY)
        XCTAssertEqual(preview.maxY, content.maxY)
        XCTAssertEqual(preview.maxX, content.maxX)

        // This point is well left of both the compact square and edge runway.
        // It becomes sticky only after the square has expanded, preventing
        // the full-sidebar preview from collapsing under the pointer.
        let point = NSPoint(x: preview.minX + 4, y: preview.midY)
        XCTAssertFalse(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: point,
                contentRect: content,
                trailingEdgeX: content.maxX,
                projectSidebarIsOpen: false,
                canPin: true,
                projectSidebarWidth: sidebarWidth,
                targetWasExpanded: false
            )
        )
        XCTAssertTrue(
            SidebarSessionDragController.isProjectSidebarPinTarget(
                at: point,
                contentRect: content,
                trailingEdgeX: content.maxX,
                projectSidebarIsOpen: false,
                canPin: true,
                projectSidebarWidth: sidebarWidth,
                targetWasExpanded: true
            )
        )
    }

    func testProjectSidebarPinIndicatorLeansTowardCursorWithoutMovingCard() {
        let target = NSRect(x: 400, y: 400, width: 76, height: 76)
        let belowRight = SidebarSessionDragController.projectSidebarPinSquareLean(
            cursor: NSPoint(x: 520, y: 320),
            targetRect: target
        )
        XCTAssertGreaterThan(belowRight.width, 0)
        XCTAssertGreaterThan(belowRight.height, 0)
        XCTAssertLessThanOrEqual(hypot(belowRight.width, belowRight.height), 10.001)
        XCTAssertEqual(
            SidebarSessionDragController.projectSidebarPinSquareLean(
                cursor: NSPoint(x: target.midX, y: target.midY),
                targetRect: target
            ),
            .zero
        )
    }

    func testGroupEdgePreviewMatchesFinalSplitExtent() {
        // The final renderer reserves its 8pt divider, then gives the new
        // leaf 1/(existing + 1) of the remaining root extent.
        XCTAssertEqual(
            TerminalPaneDropPreviewGeometry.groupEdgePaneExtent(
                totalExtent: 808,
                existingSessionLeafCount: 1
            ),
            400
        )
        XCTAssertEqual(
            TerminalPaneDropPreviewGeometry.groupEdgePaneExtent(
                totalExtent: 608,
                existingSessionLeafCount: 2
            ),
            200
        )
        XCTAssertEqual(
            TerminalPaneDropPreviewGeometry.groupEdgePaneExtent(
                totalExtent: 808,
                existingSessionLeafCount: 7
            ),
            100
        )
    }

    func testGroupEdgePreviewInsetKeepsItsFinalPaneFootprint() {
        XCTAssertEqual(
            TerminalPaneDropPreviewGeometry.insetHighlightExtent(
                for: 400
            ) + TerminalPaneDropPreviewGeometry.previewInset * 2,
            400
        )
        XCTAssertEqual(
            TerminalPaneDropPreviewGeometry.groupEdgePaneExtent(
                totalExtent: 4,
                existingSessionLeafCount: 1
            ),
            0
        )
    }
}
